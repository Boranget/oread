#!/usr/bin/env node
// 资源预算门禁：产物二进制体积 + 前端首屏关键资源（gzip）。
// 阈值唯一来源是 scripts/budget.json；本地与 CI 用同一个脚本，避免两套口径。
//
// 用法：
//   node scripts/check-budget.mjs                    # 能测的都测，缺产物记为 SKIP
//   node scripts/check-budget.mjs --frontend-only    # 只测 dist
//   node scripts/check-budget.mjs --binaries-only --strict   # CI 发版门禁，缺产物即失败
// 产物路径可用环境变量覆盖：BUDGET_BIN_AARCH64 / BUDGET_BIN_X86_64 / BUDGET_BIN_WIN

import { readFileSync, existsSync, statSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const DIST = join(ROOT, 'server', 'web', 'dist');

const cfg = JSON.parse(readFileSync(join(HERE, 'budget.json'), 'utf8'));
const baselinePath = join(HERE, 'budget-baseline.json');
const baseline = existsSync(baselinePath) ? JSON.parse(readFileSync(baselinePath, 'utf8')) : null;

const argv = process.argv.slice(2);
const frontendOnly = argv.includes('--frontend-only');
const binariesOnly = argv.includes('--binaries-only');
const strict = argv.includes('--strict');

const MiB = 1024 * 1024;
const rows = [];
let failed = 0;

function record(name, actual, limit, unit, kind) {
  const ok = actual !== null && actual <= limit;
  if (actual === null) {
    if (strict) failed += 1;
    rows.push({ name, kind: 'SKIP', text: `${name}: 未找到产物（${strict ? 'strict 模式下计为失败' : '跳过'}）` });
    return;
  }
  if (!ok) failed += 1;
  const base = baseline && baseline.binaries ? baseline.binaries[name] : undefined;
  const fmt = unit === 'bytes' ? (v) => (v / MiB).toFixed(2) + ' MiB' : (v) => v.toFixed(1) + ' kB';
  const margin = ((limit - actual) / limit) * 100;
  const baseText = base ? ` | 上次通过 ${fmt(base)}` : '';
  rows.push({
    name,
    kind: ok ? 'PASS' : 'FAIL',
    text: `${ok ? 'PASS' : 'FAIL'} ${name}: ${fmt(actual)} / 上限 ${fmt(limit)}（余量 ${margin.toFixed(1)}%）${baseText}`,
  });
}

function binPath(envKey, fallback) {
  return process.env[envKey] || fallback;
}

if (!frontendOnly) {
  const bins = [
    ['boranlib-linux-aarch64', 'BUDGET_BIN_AARCH64', join(ROOT, 'artifacts', 'boranlib-linux-aarch64')],
    ['boranlib-linux-x86_64', 'BUDGET_BIN_X86_64', join(ROOT, 'artifacts', 'boranlib-linux-x86_64')],
    ['boranlib-windows-x86_64.exe', 'BUDGET_BIN_WIN', join(ROOT, 'artifacts', 'boranlib-windows-x86_64.exe')],
  ];
  for (const [name, envKey, fallback] of bins) {
    const p = binPath(envKey, fallback);
    const limit = cfg.binaries[name] * MiB;
    const size = existsSync(p) ? statSync(p).size : null;
    record(name, size, limit, 'bytes', 'binary');
    if (size !== null) rows[rows.length - 1].text += ` | ${p}`;
  }
}

if (!binariesOnly) {
  if (!existsSync(join(DIST, 'index.html'))) {
    failed += 1;
    rows.push({ name: 'frontend', kind: 'FAIL', text: `FAIL frontend: 未找到 ${DIST}/index.html，先跑 npm run build` });
  } else {
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
      const deps = [...text.matchAll(/(?:from|import)\s*["']\.\/([^"']+\.js)["']/g)].map((m) => m[1]);
      for (const d of deps) queue.push(d);
    }

    const gz = (rel) => gzipSync(readFileSync(join(DIST, rel)), { level: 9 }).length;
    const jsKb = [...seen].reduce((a, rel) => a + gz(rel), 0) / 1024;
    const cssKb = cssLinks.reduce((a, rel) => a + gz(rel.replace(/^\.?\//, '')), 0) / 1024;

    const jsLimit = cfg.frontend.entryJsGzipKb;
    const cssLimit = cfg.frontend.entryCssGzipKb;
    for (const [name, actual, limit] of [['frontend entry JS (gzip)', jsKb, jsLimit], ['frontend CSS (gzip)', cssKb, cssLimit]]) {
      const ok = actual <= limit;
      if (!ok) failed += 1;
      rows.push({
        name,
        kind: ok ? 'PASS' : 'FAIL',
        text: `${ok ? 'PASS' : 'FAIL'} ${name}: ${actual.toFixed(1)} kB / 上限 ${limit} kB（余量 ${(((limit - actual) / limit) * 100).toFixed(1)}%）`,
      });
    }
    rows.push({ name: 'detail', kind: 'INFO', text: `  首屏 JS 含 ${seen.size} 个 chunk: ${[...seen].join(', ')}` });
    rows.push({ name: 'detail', kind: 'INFO', text: `  首屏 CSS: ${cssLinks.join(', ') || '(无)'}` });
  }
}

for (const r of rows) console.log(r.text);
console.log(failed ? `\nBUDGET FAIL：${failed} 项超限或缺失` : '\nBUDGET PASS：全部在预算内');
process.exit(failed ? 1 : 0);
