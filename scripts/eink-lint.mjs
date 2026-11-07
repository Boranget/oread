#!/usr/bin/env node
// /k（kindle 路由）内联 CSS 门禁：只允许真机实测可用的写法。
// 规范见 docs/EINK_STYLE.md（该清单只约束 /k，不约束 SPA 的 web/src）。
//
// 做法：从 Rust 源码里抽取字符串字面量内容（普通字符串与 r#"..."# 原始字符串），
// 跳过行注释/块注释与 Rust 代码，因此 `.clamp(` 之类的代码不会误报。
// 同时排除 fn selftest（诊断页故意展示不可用项）与 #[cfg(test)] 之后的内容。
//
// 默认扫描 `server/src/routes/kindle.rs` 与 `server/src/routes/kindle/**/*.rs`；
// 传文件或目录时只扫指定目标。这样把 kindle.rs 拆成子模块后门禁不会失守。
//
// 用法：node scripts/eink-lint.mjs [file-or-dir]

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const KINDLE_FILE = path.join(ROOT, 'server', 'src', 'routes', 'kindle.rs');
const KINDLE_DIR = path.join(ROOT, 'server', 'src', 'routes', 'kindle');
const ROUTES_DIR = path.dirname(KINDLE_FILE);

/// 拆分后的 `kindle_*.rs` 与 `kindle.rs` 同级；默认扫描全部，避免门禁漏掉新文件。
const KINDLE_SIBLINGS = fs
  .readdirSync(ROUTES_DIR)
  .filter((name) => name !== 'kindle.rs' && /^kindle.*\.rs$/.test(name))
  .map((name) => path.join(ROUTES_DIR, name))
  .sort();

function collectRustFiles(target) {
  if (!fs.existsSync(target)) return [];
  const stat = fs.statSync(target);
  if (stat.isFile()) return [target];
  if (!stat.isDirectory()) return [];
  const out = [];
  for (const entry of fs.readdirSync(target, { withFileTypes: true })) {
    const full = path.join(target, entry.name);
    if (entry.isDirectory()) out.push(...collectRustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) out.push(full);
  }
  return out.sort();
}

const cliTarget = process.argv[2] ? path.resolve(process.argv[2]) : null;
const targets = cliTarget
  ? collectRustFiles(cliTarget)
  : [
      ...collectRustFiles(KINDLE_FILE),
      ...collectRustFiles(KINDLE_DIR),
      ...KINDLE_SIBLINGS,
    ];

if (!targets.length) {
  console.error(
    `eink-lint: 找不到可扫描的 Rust 文件${cliTarget ? `：${cliTarget}` : `（${KINDLE_FILE} / ${KINDLE_DIR}）`}`
  );
  process.exit(2);
}

// ---- 抽取字符串字面量（带行号） ----
function extractStrings(text) {
  const out = []; // { line, text }
  let i = 0;
  let line = 1;
  let segLine = 1;
  let buf = '';
  const n = text.length;

  const flush = () => {
    if (buf) out.push({ line: segLine, text: buf });
    buf = '';
  };

  while (i < n) {
    const ch = text[i];

    if (ch === '/' && text[i + 1] === '/') {
      while (i < n && text[i] !== '\n') i++;
      continue;
    }
    if (ch === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < n && !(text[i] === '*' && text[i + 1] === '/')) {
        if (text[i] === '\n') line++;
        i++;
      }
      i += 2;
      continue;
    }

    // 原始字符串 r"..." / r#"..."#
    if (ch === 'r' && (text[i + 1] === '"' || text[i + 1] === '#')) {
      let j = i + 1;
      let hashes = 0;
      while (text[j] === '#') {
        hashes++;
        j++;
      }
      if (text[j] === '"') {
        const close = '"' + '#'.repeat(hashes);
        let k = j + 1;
        segLine = line;
        while (k < n) {
          if (text.startsWith(close, k)) {
            k += close.length;
            break;
          }
          if (text[k] === '\n') {
            flush();
            line++;
            segLine = line;
          } else {
            buf += text[k];
          }
          k++;
        }
        flush();
        i = k;
        continue;
      }
    }

    if (ch === '"') {
      let k = i + 1;
      segLine = line;
      while (k < n) {
        const c = text[k];
        if (c === '\\') {
          if (text[k + 1] === '\n') {
            // Rust 字符串续行：本行结束，内容继续到下一行
            flush();
            line++;
            segLine = line;
            k += 2;
            continue;
          }
          buf += c + (text[k + 1] ?? '');
          k += 2;
          continue;
        }
        if (c === '"') {
          k++;
          break;
        }
        if (c === '\n') {
          flush();
          line++;
          segLine = line;
          k++;
          continue;
        }
        buf += c;
        k++;
      }
      flush();
      i = k;
      continue;
    }

    if (ch === '\n') line++;
    i++;
  }

  return out;
}

// ---- 规则 ----
// reset: 允许 none/0 这类“清零型”声明（用于抵消第三方样式），不算命中。
const isReset = (v) => /^(?:none|0(?:px)?|normal|initial|unset|inherit)\b/i.test(v.trim());

const RULES = [
  { name: 'display:flex/grid', re: /display\s*:\s*(?:inline-)?(?:flex|grid)\b/gi },
  { name: 'grid-*', re: /\bgrid-(?:template|column|row|auto|gap|area)\b/gi },
  {
    name: 'vw/vh 单位',
    re: /\d+(?:\.\d+)?(?:vw|vh)\b/gi,
  },
  { name: 'calc()', re: /\bcalc\s*\(/gi },
  { name: 'clamp()', re: /\bclamp\s*\(/gi },
  { name: 'position:sticky', re: /position\s*:\s*sticky\b/gi },
  {
    name: 'transform',
    re: /(?:-webkit-)?transform\s*:\s*([^;}"'\n]*)/gi,
    reset: (m) => isReset(m[1] || ''),
  },
  {
    name: 'transition',
    re: /(?:-webkit-)?transition\s*:\s*([^;}"'\n]*)/gi,
    reset: (m) => isReset(m[1] || ''),
  },
  { name: 'animation', re: /(?:-webkit-)?animation\s*:\s*/gi },
  { name: '@keyframes', re: /@(?:-webkit-)?keyframes\b/gi },
  { name: 'object-fit', re: /object-fit\s*:/gi },
  {
    name: 'box-shadow',
    re: /box-shadow\s*:\s*([^;}"'\n]*)/gi,
    reset: (m) => isReset(m[1] || ''),
  },
  {
    name: 'border-radius',
    re: /border-radius\s*:\s*([^;}"'\n]*)/gi,
    reset: (m) => isReset(m[1] || ''),
  },
  {
    name: 'background-image',
    re: /background-image\s*:\s*([^;}"'\n]*)/gi,
    reset: (m) => isReset(m[1] || ''),
  },
  { name: '@media', re: /@media\b/gi },
];

function lintFile(file) {
  const src = fs.readFileSync(file, 'utf8');
  const lines = src.split('\n');

  // ---- 排除区间：fn selftest ... 到 #[cfg(test)]；#[cfg(test)] 到 EOF ----
  let selftestAt = -1;
  let testsAt = lines.length;
  lines.forEach((text, i) => {
    if (selftestAt < 0 && /fn\s+selftest\s*\(/.test(text)) selftestAt = i;
    if (testsAt === lines.length && /^#\[cfg\(test\)\]/.test(text.trimStart())) testsAt = i;
  });
  const excluded = (lineNo) => {
    // lineNo 为 1-based
    const idx = lineNo - 1;
    if (selftestAt >= 0 && idx >= selftestAt && idx < testsAt) return true;
    return idx >= testsAt;
  };

  const segments = extractStrings(src).filter((s) => !excluded(s.line));
  const hits = [];
  for (const seg of segments) {
    for (const rule of RULES) {
      rule.re.lastIndex = 0;
      let m;
      while ((m = rule.re.exec(seg.text)) !== null) {
        if (rule.reset && rule.reset(m)) continue;
        const keyword = m[0].replace(/\s+/g, ' ').trim();
        hits.push({
          line: seg.line,
          name: rule.name,
          keyword,
          context: seg.text.trim().slice(0, 160),
        });
      }
    }
  }

  // 同一行同一命中只报一次
  const seen = new Set();
  const unique = hits.filter((h) => {
    const key = `${h.line}|${h.name}|${h.keyword}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });

  return { file, rel: path.relative(ROOT, file) || file, hits: unique, segments: segments.length };
}

const reports = targets.map(lintFile);
const failing = reports.filter((r) => r.hits.length > 0);
const totalHits = failing.reduce((n, r) => n + r.hits.length, 0);

if (failing.length) {
  console.error(`eink-lint: FAIL（${failing.length}/${reports.length} 个文件命中 ${totalHits} 处禁用写法）`);
  for (const report of failing) {
    for (const h of report.hits) {
      console.error(`  ${report.rel}:${h.line}  [${h.name}]  ${h.keyword}`);
      if (h.context) console.error(`      ${h.context}`);
    }
  }
  console.error('规范见 docs/EINK_STYLE.md 第 4 节（仅约束 /k，SPA 不受此限）。');
  process.exit(1);
}

const totalSegments = reports.reduce((n, r) => n + r.segments, 0);
console.log(
  `eink-lint: PASS（扫描 ${reports.length} 个文件 / ${totalSegments} 段 CSS 字符串，0 命中）`
);
