#!/usr/bin/env node
// 阅读器边界门禁：`server/web/src/reader/**` 只能依赖 `src/lib/**` 与同目录文件。
//
// 为什么要有这条规则：阅读器要跑在 KPW3 的残缺浏览器上，任何管理端共享组件里
// 随手写下的 `object-fit` / `transform` / `vw` 都可能让阅读页白屏，而且是
// 「改了别处、坏在阅读器」这种最难定位的故障。目录隔离是目前唯一低成本的防线。
//
// 用法：node scripts/check-reader-boundary.mjs

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const READER_DIR = path.join(ROOT, 'server', 'web', 'src', 'reader');

const IMPORT_RE = /(?:from\s+|import\s*\(\s*)['"]([^'"]+)['"]/g;

function walk(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (/\.(svelte|js|mjs)$/.test(entry.name)) out.push(full);
  }
  return out.sort();
}

const problems = [];
for (const file of walk(READER_DIR)) {
  const text = fs.readFileSync(file, 'utf8');
  const rel = path.relative(ROOT, file).replaceAll('\\', '/');
  for (const match of text.matchAll(IMPORT_RE)) {
    const spec = match[1];
    if (!spec.startsWith('.')) continue; // 第三方包不在此规则内
    const target = path.resolve(path.dirname(file), spec).replaceAll('\\', '/');
    if (!target.startsWith(READER_DIR.replaceAll('\\', '/') + '/') && !target.includes('/web/src/lib/')) {
      problems.push(`${rel}: 非法依赖 '${spec}'（阅读器只能引用 ./ 或 ../lib/）`);
    }
  }
}

if (problems.length) {
  for (const p of problems) console.error(`::error::${p}`);
  console.error(`\nreader boundary: FAIL（${problems.length} 处）`);
  process.exit(1);
}
console.log('reader boundary: PASS');
