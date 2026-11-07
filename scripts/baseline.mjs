#!/usr/bin/env node
// Phase 0 基线采集：仓库/代码/测试/前端/二进制/构建耗时/接口延迟。
// 只读，不改代码；所有数字写进 JSON，供后续优化前后对照。
//
// 用法：
//   node scripts/baseline.mjs
//   node scripts/baseline.mjs --frontend --json docs/baseline/2026-11-07.json
//   node scripts/baseline.mjs --build --build-release --test
//   node scripts/baseline.mjs --url http://127.0.0.1:18080 --user admin --pass '<口令>'
//
// 环境变量：
//   CARGO_TARGET_DIR       复用本地 target，默认 server/target
//   BORANLIB_BASELINE_BIN  额外记录一个二进制的体积（重复传参不支持，取一个即可）

import {
  readFileSync, existsSync, statSync, readdirSync, writeFileSync, mkdirSync, statfsSync,
} from 'node:fs';
import { join, dirname, extname, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { gzipSync } from 'node:zlib';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const SERVER = join(ROOT, 'server');
const WEB = join(SERVER, 'web');
const DIST = join(WEB, 'dist');

const argv = process.argv.slice(2);
const has = (name) => argv.includes(name);
const valueOf = (name, fallback = null) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const opt = {
  frontend: has('--frontend'),
  build: has('--build'),
  buildRelease: has('--build-release'),
  test: has('--test'),
  json: valueOf('--json'),
  url: valueOf('--url'),
  user: valueOf('--user', process.env.BORANLIB_BASELINE_USER || 'admin'),
  pass: valueOf('--pass', process.env.BORANLIB_BASELINE_PASS || null),
};

const out = {
  schema: 1,
  generatedAt: new Date().toISOString(),
  git: {},
  code: {},
  tests: {},
  frontend: null,
  binaries: {},
  build: null,
  runtime: null,
  disk: null,
};

function run(cmd, args, cwd = ROOT) {
  return execFileSync(cmd, args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

function tryRun(cmd, args, cwd = ROOT) {
  try { return run(cmd, args, cwd); } catch { return null; }
}

function walk(dir, exts, acc = []) {
  if (!existsSync(dir)) return acc;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, exts, acc);
    else if (exts.includes(extname(e.name))) acc.push(p);
  }
  return acc;
}

function countLines(file) {
  const text = readFileSync(file, 'utf8');
  if (!text) return 0;
  return text.split('\n').length - (text.endsWith('\n') ? 1 : 0);
}

function dirSize(dir) {
  if (!existsSync(dir)) return 0;
  let total = 0;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    total += e.isDirectory() ? dirSize(p) : statSync(p).size;
  }
  return total;
}

function codeStats(dir, exts) {
  const files = walk(dir, exts);
  const rows = files.map((f) => ({ path: relative(ROOT, f), lines: countLines(f) }));
  rows.sort((a, b) => b.lines - a.lines);
  return {
    files: files.length,
    lines: rows.reduce((a, r) => a + r.lines, 0),
    top: rows.slice(0, 8),
  };
}

// ---- 仓库 ----
out.git.branch = tryRun('git', ['branch', '--show-current']) || '(detached)';
out.git.commit = tryRun('git', ['rev-parse', '--short', 'HEAD']) || '(none)';
out.git.subject = tryRun('git', ['log', '-1', '--pretty=%s']) || '';
out.git.dirty = (tryRun('git', ['status', '--porcelain']) || '').split('\n').filter(Boolean).length;
out.git.trackedFiles = (tryRun('git', ['ls-files']) || '').split('\n').filter(Boolean).length;
out.git.objects = tryRun('git', ['count-objects', '-vH']) || '';
out.git.sizeBytes = dirSize(join(ROOT, '.git'));

try {
  const s = statfsSync(ROOT);
  out.disk = { freeBytes: Number(s.bfree) * Number(s.bsize), totalBytes: Number(s.blocks) * Number(s.bsize) };
} catch { /* 平台不支持时留空 */ }

// ---- 代码 ----
out.code.rust = codeStats(join(SERVER, 'src'), ['.rs']);
out.code.svelte = codeStats(join(WEB, 'src'), ['.svelte']);
out.code.js = codeStats(join(WEB, 'src'), ['.js', '.mjs', '.ts']);

// ---- 测试 ----
const rustFiles = walk(join(SERVER, 'src'), ['.rs']);
let testCount = 0;
const testFiles = [];
for (const f of rustFiles) {
  const n = (readFileSync(f, 'utf8').match(/#\[(?:tokio::)?test\]/g) || []).length;
  if (n) { testCount += n; testFiles.push({ path: relative(ROOT, f), tests: n }); }
}
testFiles.sort((a, b) => b.tests - a.tests);
out.tests = {
  rustTests: testCount,
  rustFilesWithTests: testFiles.length,
  top: testFiles.slice(0, 8),
  integrationTestFiles: walk(join(SERVER, 'tests'), ['.rs']).map((f) => relative(ROOT, f)),
  frontendTestFiles: walk(join(WEB, 'src'), ['.js', '.ts', '.svelte']).filter((f) => /\.test\.(js|ts)$/.test(f)).map((f) => relative(ROOT, f)),
};

// ---- 前端 ----
function measureFrontend() {
  if (!existsSync(join(DIST, 'index.html'))) return { error: 'dist/index.html 不存在，先 --frontend 构建' };
  const html = readFileSync(join(DIST, 'index.html'), 'utf8');
  const entryJs = [...html.matchAll(/<script[^>]+src="([^"]+\.js)"/g)].map((m) => m[1]);
  const cssLinks = [...html.matchAll(/<link[^>]+rel="stylesheet"[^>]+href="([^"]+\.css)"|<link[^>]+href="([^"]+\.css)"[^>]+rel="stylesheet"/g)]
    .map((m) => m[1] || m[2]);
  const seen = new Set();
  const queue = entryJs.map((p) => p.replace(/^\.?\//, ''));
  while (queue.length) {
    const rel = queue.shift();
    if (seen.has(rel)) continue;
    const abs = join(DIST, rel);
    if (!existsSync(abs)) continue;
    seen.add(rel);
    const text = readFileSync(abs, 'utf8').replace(/import\s*\([^)]*\)/g, '');
    for (const m of text.matchAll(/(?:from|import)\s*["']\.\/([^"']+\.js)["']/g)) queue.push(m[1]);
  }
  const gzKb = (rel) => gzipSync(readFileSync(join(DIST, rel)), { level: 9 }).length / 1024;
  return {
    entryJsGzipKb: [...seen].reduce((a, rel) => a + gzKb(rel), 0),
    cssGzipKb: cssLinks.reduce((a, rel) => a + gzKb(rel.replace(/^\.?\//, '')), 0),
    chunks: [...seen],
    css: cssLinks,
  };
}

if (opt.frontend) {
  const t0 = performance.now();
  try {
    const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
    execFileSync(npm, ['run', 'build'], { cwd: WEB, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
    out.frontend = { ...measureFrontend(), buildSeconds: (performance.now() - t0) / 1000 };
  } catch (e) {
    out.frontend = { error: String(e.message || e) };
  }
} else if (existsSync(join(DIST, 'index.html'))) {
  out.frontend = measureFrontend();
}

// ---- 二进制 ----
const binCandidates = [
  ['env', process.env.BORANLIB_BASELINE_BIN],
  ['aarch64', join(ROOT, 'artifacts', 'boranlib-linux-aarch64')],
  ['x86_64', join(ROOT, 'artifacts', 'boranlib-linux-x86_64')],
  ['windows', join(ROOT, 'artifacts', 'boranlib-windows-x86_64.exe')],
];
for (const [name, p] of binCandidates) {
  if (p && existsSync(p)) out.binaries[name] = { path: p, bytes: statSync(p).size };
}

// ---- 构建/测试耗时 ----
function timeCommand(label, cmd, args, cwd) {
  const t0 = performance.now();
  try {
    execFileSync(cmd, args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
    return { ok: true, seconds: (performance.now() - t0) / 1000 };
  } catch (e) {
    return { ok: false, seconds: (performance.now() - t0) / 1000, error: String(e.message || e) };
  }
}

if (opt.build || opt.buildRelease || opt.test) {
  out.build = {};
  if (opt.build) out.build.debug = timeCommand('debug', 'cargo', ['build', '--locked'], SERVER);
  if (opt.buildRelease) out.build.release = timeCommand('release', 'cargo', ['build', '--release', '--locked'], SERVER);
  if (opt.test) out.build.test = timeCommand('test', 'cargo', ['test', '--locked'], SERVER);
}

// ---- 运行时 ----
function percentile(sorted, p) {
  if (!sorted.length) return null;
  const i = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[i];
}

async function measureRuntime(baseUrl) {
  const base = baseUrl.replace(/\/$/, '');
  const login = await fetch(`${base}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: opt.user, password: opt.pass }),
  });
  const loginBody = await login.json().catch(() => ({}));
  const token = loginBody.token;
  if (!login.ok || !token) {
    return { error: `login failed: HTTP ${login.status}`, body: loginBody };
  }
  const headers = { authorization: `Bearer ${token}` };
  const endpoints = [
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
  const results = {};
  let health = null;
  for (const ep of endpoints) {
    for (let i = 0; i < 2; i += 1) await fetch(`${base}${ep}`, { headers }).catch(() => {});
    const times = [];
    let status = 0;
    for (let i = 0; i < 10; i += 1) {
      const t0 = performance.now();
      try {
        const r = await fetch(`${base}${ep}`, { headers });
        status = r.status;
        await r.text();
        times.push(performance.now() - t0);
      } catch (e) {
        status = -1;
        break;
      }
    }
    times.sort((a, b) => a - b);
    results[ep] = {
      status,
      p50Ms: percentile(times, 50),
      p95Ms: percentile(times, 95),
      minMs: times[0] ?? null,
      maxMs: times[times.length - 1] ?? null,
    };
    if (ep === '/api/healthz') {
      try {
        const r = await fetch(`${base}${ep}`, { headers });
        health = await r.json();
      } catch { /* ignore */ }
    }
  }
  return { baseUrl: base, endpoints: results, health };
}

if (opt.url) {
  if (!opt.pass) {
    console.error('缺少登录口令：使用 --url 时需传 --pass 或设置 BORANLIB_BASELINE_PASS');
    process.exit(2);
  }
  out.runtime = await measureRuntime(opt.url);
}

// ---- 输出 ----
const mb = (n) => `${(n / 1024 / 1024).toFixed(1)} MB`;
const kb = (n) => `${n.toFixed(1)} kB`;

console.log(`# Boranlib baseline ${out.generatedAt}`);
console.log(`git: ${out.git.branch} @ ${out.git.commit}${out.git.dirty ? ` (dirty: ${out.git.dirty})` : ''} | tracked ${out.git.trackedFiles} | .git ${mb(out.git.sizeBytes)}`);
if (out.disk) console.log(`disk: free ${mb(out.disk.freeBytes)} / total ${mb(out.disk.totalBytes)}`);
console.log(`code: rust ${out.code.rust.files}f/${out.code.rust.lines}L | svelte ${out.code.svelte.files}f/${out.code.svelte.lines}L | js ${out.code.js.files}f/${out.code.js.lines}L`);
console.log(`tests: rust ${out.tests.rustTests} in ${out.tests.rustFilesWithTests}f | integration ${out.tests.integrationTestFiles.length} | frontend ${out.tests.frontendTestFiles.length}`);
if (out.frontend?.error) console.log(`frontend: ${out.frontend.error}`);
else if (out.frontend) console.log(`frontend: entry JS ${kb(out.frontend.entryJsGzipKb)} gzip | CSS ${kb(out.frontend.cssGzipKb)} gzip | ${out.frontend.chunks.length} chunks`);
if (Object.keys(out.binaries).length) {
  for (const [k, v] of Object.entries(out.binaries)) console.log(`binary ${k}: ${mb(v.bytes)} (${v.path})`);
} else {
  console.log('binary: 无 artifacts/ 产物（发版 job 或手动指定 BORANLIB_BASELINE_BIN）');
}
if (out.build) {
  for (const [k, v] of Object.entries(out.build)) console.log(`build ${k}: ${v.seconds.toFixed(1)}s ${v.ok ? 'ok' : `FAIL ${v.error}`}`);
}
if (out.runtime?.error) console.log(`runtime: ${out.runtime.error}`);
else if (out.runtime) {
  console.log(`runtime: ${out.runtime.baseUrl}`);
  for (const [ep, r] of Object.entries(out.runtime.endpoints)) {
    console.log(`  ${ep}: ${r.status} p50 ${r.p50Ms?.toFixed(1)}ms p95 ${r.p95Ms?.toFixed(1)}ms`);
  }
  if (out.runtime.health?.rss_bytes) console.log(`  rss: ${mb(out.runtime.health.rss_bytes)}`);
}

if (opt.json) {
  mkdirSync(dirname(opt.json), { recursive: true });
  writeFileSync(opt.json, `${JSON.stringify(out, null, 2)}\n`);
  console.log(`\nwrote ${opt.json}`);
}
