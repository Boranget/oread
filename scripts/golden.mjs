#!/usr/bin/env node
// Phase 0 golden 快照：对只读接口记录状态码 + 归一化后的 JSON，拆分前后做 diff。
// 只读；建议指向临时实例（18080 + 临时 data 目录），不要指向生产 :3000。
//
// 用法：
//   node scripts/golden.mjs snapshot --base-url http://127.0.0.1:18080 --out scripts/golden/baseline
//   node scripts/golden.mjs diff --expected scripts/golden/baseline --actual scripts/golden/current
//
// 登录凭据需显式提供：--user/--pass 或 BORANLIB_BASELINE_USER/PASS。

import { readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');

const argv = process.argv.slice(2);
const command = argv[0];
const valueOf = (name, fallback = null) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const baseUrl = (valueOf('--base-url', process.env.BORANLIB_BASE_URL || 'http://127.0.0.1:18080')).replace(/\/$/, '');
const user = valueOf('--user', process.env.BORANLIB_BASELINE_USER || 'admin');
const pass = valueOf('--pass', process.env.BORANLIB_BASELINE_PASS || null);
const outDir = valueOf('--out', join(ROOT, 'scripts', 'golden', 'baseline'));
const expectedDir = valueOf('--expected', join(ROOT, 'scripts', 'golden', 'baseline'));
const actualDir = valueOf('--actual', join(ROOT, 'scripts', 'golden', 'current'));

// 只读、结构稳定、不触发外网抓取的接口。
const ENDPOINTS = [
  '/healthz',
  '/api/healthz',
  '/api/sources',
  '/api/sources/health',
  '/api/bookshelf',
  '/api/settings',
  '/api/stats/overview',
  '/api/replace-rules',
  '/api/bookmarks',
  '/api/admin/users',
];

const VOLATILE = new Set([
  'updated_at', 'created_at', 'last_check_time', 'last_login', 'last_login_at', 'checked_at',
  'accessed_at', 'imported_at', 'generated_at', 'started_at', 'finished_at', 'expires_at',
  'at', 'now', 'server_time', 'timestamp', 'elapsed_ms', 'took_ms', 'duration_ms',
  'rss', 'rss_bytes', 'rss_mb', 'uptime', 'uptime_secs', 'open_user_dbs',
  'size_bytes', 'bytes', 'rows', 'free_bytes', 'total_bytes', 'image_cache_bytes',
  'items', 'last_ms', 'max_ms', 'source_limit_wait_ms', 'source_limit_waits',
  'hits', 'misses', 'hit_rate', 'saves', 'evictions', 'evicted_rows',
  'token', 'password',
]);

function scrub(value) {
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === 'object') {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      out[k] = VOLATILE.has(k) ? '<volatile>' : scrub(v);
    }
    return out;
  }
  return value;
}

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === 'object') {
    const out = {};
    for (const k of Object.keys(value).sort()) out[k] = stable(value[k]);
    return out;
  }
  return value;
}

function nameOf(path) {
  return path.replace(/^\//, '').replace(/[^a-zA-Z0-9._-]+/g, '_') || 'root';
}

async function login() {
  if (!pass) throw new Error('缺少登录口令：请传 --pass 或设置 BORANLIB_BASELINE_PASS');
  const r = await fetch(`${baseUrl}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: user, password: pass }),
  });
  const body = await r.json().catch(() => ({}));
  if (!r.ok || !body.token) throw new Error(`login failed: HTTP ${r.status} ${JSON.stringify(body)}`);
  return body.token;
}

async function snapshot() {
  const token = await login();
  mkdirSync(outDir, { recursive: true });
  const headers = { authorization: `Bearer ${token}` };
  const index = { baseUrl, capturedAt: new Date().toISOString(), endpoints: [] };

  for (const ep of ENDPOINTS) {
    const r = await fetch(`${baseUrl}${ep}`, { headers });
    const contentType = r.headers.get('content-type') || '';
    const text = await r.text();
    let body = text;
    if (contentType.includes('application/json')) {
      try { body = stable(scrub(JSON.parse(text))); } catch { body = text; }
    }
    const file = join(outDir, `${nameOf(ep)}.json`);
    writeFileSync(file, `${JSON.stringify({ status: r.status, contentType, body }, null, 2)}\n`);
    index.endpoints.push({ path: ep, status: r.status, file: `${nameOf(ep)}.json` });
    console.log(`${r.status} ${ep}`);
  }
  writeFileSync(join(outDir, 'index.json'), `${JSON.stringify(index, null, 2)}\n`);
  console.log(`\nwrote ${index.endpoints.length} snapshots to ${outDir}`);
}

function readJson(file) {
  return JSON.parse(readFileSync(file, 'utf8'));
}

function diffValue(a, b, path, out) {
  if (out.length >= 20) return;
  if (a === b) return;
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') {
    out.push(`${path}: ${JSON.stringify(a)} -> ${JSON.stringify(b)}`);
    return;
  }
  if (Array.isArray(a) !== Array.isArray(b)) {
    out.push(`${path}: type changed`);
    return;
  }
  if (Array.isArray(a)) {
    if (a.length !== b.length) out.push(`${path}.length: ${a.length} -> ${b.length}`);
    const n = Math.min(a.length, b.length);
    for (let i = 0; i < n && out.length < 20; i += 1) diffValue(a[i], b[i], `${path}[${i}]`, out);
    return;
  }
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const k of keys) {
    if (out.length >= 20) return;
    if (!(k in a)) out.push(`${path}.${k}: <missing> -> ${JSON.stringify(b[k])}`);
    else if (!(k in b)) out.push(`${path}.${k}: ${JSON.stringify(a[k])} -> <missing>`);
    else diffValue(a[k], b[k], `${path}.${k}`, out);
  }
}

function diff() {
  if (!existsSync(expectedDir)) throw new Error(`expected dir not found: ${expectedDir}`);
  if (!existsSync(actualDir)) throw new Error(`actual dir not found: ${actualDir}`);
  const files = readdirSync(actualDir).filter((f) => f.endsWith('.json') && f !== 'index.json');
  let changed = 0;
  for (const f of files) {
    const ePath = join(expectedDir, f);
    if (!existsSync(ePath)) {
      console.log(`NEW  ${f}`);
      changed += 1;
      continue;
    }
    const a = readJson(ePath).body;
    const b = readJson(join(actualDir, f)).body;
    const diffs = [];
    diffValue(a, b, '$', diffs);
    if (diffs.length) {
      changed += 1;
      console.log(`DIFF ${f}`);
      for (const d of diffs) console.log(`  ${d}`);
    } else {
      console.log(`OK   ${f}`);
    }
  }
  console.log(changed ? `\nGOLDEN FAIL: ${changed} 个快照有变化` : '\nGOLDEN PASS: 快照一致');
  process.exit(changed ? 1 : 0);
}

if (command === 'snapshot') {
  await snapshot();
} else if (command === 'diff') {
  diff();
} else {
  console.error('用法: node scripts/golden.mjs snapshot|diff [--base-url URL] [--out DIR]');
  process.exit(2);
}
