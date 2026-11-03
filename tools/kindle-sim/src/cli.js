#!/usr/bin/env node
// KPW3 仿真测试台 CLI。
// 用法示例：
//   node src/cli.js http://127.0.0.1:3000/ fixture:simple.html
import fs from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { launchBrowser, runTarget, KPW3_VIEWPORT, KPW3_SCREEN, KPW3_UA } from './runner.js';
import { startFixtureServer } from './fixture-server.js';
import { DEFAULT_THRESHOLDS, assessMode, compareSSR } from './judge.js';
import { renderReport } from './report.js';

const HELP = `KPW3 仿真测试台（Kindle Paperwhite 3 / mesquite WebKit 531.2-534.26 近似）

用法：
  node src/cli.js [options] <url1> [url2 ...]
  node src/cli.js --config kpw3.config.json
  node src/cli.js http://127.0.0.1:3000/,http://127.0.0.1:3000/assets/...

URL 可用 fixture: 前缀指向 tools/kindle-sim/fixtures/ 下的本地页面：
  node src/cli.js fixture:simple.html fixture:reader-page.html

选项：
  -c, --config <file>          批量配置（JSON），与命令行 URL 合并
  -o, --out <dir>              输出目录（默认 out/run-<时间戳>）
      --wait <ms>              页面 load 后额外等待（默认 1500）
      --timeout <ms>           单次导航超时（默认 30000）
      --browser <edge|chrome|绝对路径>  默认 edge
      --headed                 显示浏览器窗口（默认 headless）
      --no-degrade-css         关闭 CSS 降级样式注入（只审计不改写）
      --visible-chars <n>      “白屏”可见字符阈值（默认 ${DEFAULT_THRESHOLDS.visibleChars}）
      --first-screen-chars <n> “首屏正文”阈值（默认 ${DEFAULT_THRESHOLDS.firstScreenChars}）
      --shell-chars <n>        无主内容区时的首屏正文最低字数（默认 ${DEFAULT_THRESHOLDS.shellChars}）
      --user-agent <string>    覆盖默认 KPW3 UA
  -h, --help                   显示帮助

输出：
  <out>/summary.md             Markdown 汇总 + 明细
  <out>/results.json           全部原始数据
  <out>/screens/*.png          每 URL 两种模式的整页/首屏截图
`;

function parseArgs(argv) {
  const opts = {
    urls: [],
    config: null,
    out: null,
    waitMs: 1500,
    timeoutMs: 30000,
    browser: 'edge',
    headless: true,
    degradeCss: true,
    userAgent: null,
    thresholds: { ...DEFAULT_THRESHOLDS },
    cliThresholds: {},
  };
  const aliases = {
    '-c': '--config', '-o': '--out', '-h': '--help',
  };
  for (let i = 0; i < argv.length; i += 1) {
    let arg = argv[i];
    if (aliases[arg]) arg = aliases[arg];
    if (arg === '--help') {
      opts.help = true;
      continue;
    }
    if (arg === '--config') { opts.config = argv[++i]; continue; }
    if (arg === '--out') { opts.out = argv[++i]; continue; }
    if (arg === '--wait') { opts.waitMs = Number(argv[++i]); continue; }
    if (arg === '--timeout') { opts.timeoutMs = Number(argv[++i]); continue; }
    if (arg === '--browser') { opts.browser = argv[++i]; continue; }
    if (arg === '--headed') { opts.headless = false; continue; }
    if (arg === '--no-degrade-css') { opts.degradeCss = false; continue; }
    if (arg === '--visible-chars') { opts.thresholds.visibleChars = Number(argv[++i]); opts.cliThresholds.visibleChars = true; continue; }
    if (arg === '--first-screen-chars') { opts.thresholds.firstScreenChars = Number(argv[++i]); opts.cliThresholds.firstScreenChars = true; continue; }
    if (arg === '--shell-chars') { opts.thresholds.shellChars = Number(argv[++i]); opts.cliThresholds.shellChars = true; continue; }
    if (arg === '--user-agent') { opts.userAgent = argv[++i]; continue; }
    if (arg.startsWith('-')) {
      throw new Error(`未知参数：${arg}`);
    }
    // 支持逗号分隔；URL 里的逗号罕见，批量场景按逗号拆。
    for (const part of arg.split(',')) {
      if (part.trim()) opts.urls.push(part.trim());
    }
  }
  return opts;
}

async function loadConfig(file) {
  const full = path.resolve(process.cwd(), file);
  const data = JSON.parse(await fs.readFile(full, 'utf8'));
  return data;
}

function autoName(url) {
  if (url.startsWith('fixture:')) return url.slice('fixture:'.length).replace(/\.[a-z]+$/i, '');
  try {
    const u = new URL(url);
    const p = u.pathname === '/' ? 'home' : u.pathname.replace(/^\//, '').replace(/[^\w.-]+/g, '-');
    return `${u.hostname}-${p}`.replace(/-+/g, '-').replace(/-$/, '');
  } catch (_e) {
    return url.slice(0, 40);
  }
}

function normalizeTargets(raw) {
  return raw.map((t) => {
    if (typeof t === 'string') return { name: autoName(t), url: t };
    const url = t.url || t.href;
    if (!url) throw new Error('配置中的 target 缺少 url');
    return { name: t.name || autoName(url), url, waitMs: t.waitMs, timeoutMs: t.timeoutMs, cookie: t.cookie };
  });
}

function stamp() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(HELP);
    return;
  }

  let config = null;
  if (opts.config) config = await loadConfig(opts.config);
  const cfgTargets = config ? await normalizeTargets(config.targets || []) : [];
  const cliTargets = normalizeTargets(opts.urls);
  const targets = [...cfgTargets, ...cliTargets];

  if (config) {
    if (config.waitMs != null && !process.argv.includes('--wait')) opts.waitMs = config.waitMs;
    if (config.timeoutMs != null && !process.argv.includes('--timeout')) opts.timeoutMs = config.timeoutMs;
    if (config.browser && !process.argv.includes('--browser')) opts.browser = config.browser;
    if (config.degradeCss === false && !process.argv.includes('--no-degrade-css')) opts.degradeCss = false;
    if (config.userAgent && !opts.userAgent) opts.userAgent = config.userAgent;
    if (config.thresholds) {
      // 命令行显式传入的阈值优先于配置文件。
      for (const [key, value] of Object.entries(config.thresholds)) {
        if (!opts.cliThresholds[key]) opts.thresholds[key] = value;
      }
    }
  }

  if (targets.length === 0) {
    console.error('没有目标 URL。用 --help 查看用法。');
    process.exitCode = 2;
    return;
  }

  const runDir = path.resolve(opts.out || path.join('out', `run-${stamp()}`));
  await fs.mkdir(path.join(runDir, 'screens'), { recursive: true });

  let fixtureServer = null;
  try {
    const needsFixture = targets.some((t) => t.url.startsWith('fixture:'));
    if (needsFixture) {
      fixtureServer = await startFixtureServer();
      for (const t of targets) {
        if (t.url.startsWith('fixture:')) {
          const rel = t.url.slice('fixture:'.length).replace(/^\/+/, '');
          t.url = fixtureServer.base + rel;
        }
      }
    }

    console.log(`KPW3 仿真：${targets.length} 个目标，输出 ${runDir}`);
    const { browser, meta } = await launchBrowser({ browser: opts.browser, headless: opts.headless });
    console.log(`浏览器：${meta.name} ${meta.version}`);

    const results = [];
    const startedAt = new Date().toISOString();
    for (let i = 0; i < targets.length; i += 1) {
      const target = targets[i];
      console.log(`\n[${i + 1}/${targets.length}] ${target.name} → ${target.url}`);
      const t0 = Date.now();
      const targetOptions = {
        ...opts,
        waitMs: target.waitMs != null ? target.waitMs : opts.waitMs,
        timeoutMs: target.timeoutMs != null ? target.timeoutMs : opts.timeoutMs,
      };
      const r = await runTarget({ browserMeta: { browser, ...meta }, target, options: targetOptions, runDir, index: i });
      r.js.assessment = assessMode(r.js, opts.thresholds);
      r.nojs.assessment = assessMode(r.nojs, opts.thresholds);
      r.ssr = compareSSR(r.js, r.nojs, r.js.assessment, r.nojs.assessment, opts.thresholds);
      results.push(r);
      console.log(`  JS=${r.js.assessment.verdict}  无JS=${r.nojs.assessment.verdict}  ` +
        `可见字符=${r.js.metrics.visibleChars}/${r.nojs.metrics.visibleChars}  ` +
        `拦截脚本=${r.js.blockedScripts.length}  SSR=${r.ssr.bodyInHtml ? '是' : '否'}  ${Date.now() - t0}ms`);
    }

    await browser.close();

    const runInfo = {
      startedAt,
      finishedAt: new Date().toISOString(),
      browser: meta,
      userAgent: opts.userAgent || KPW3_UA,
      viewport: KPW3_VIEWPORT,
      screen: KPW3_SCREEN,
      thresholds: opts.thresholds,
    };
    const summary = renderReport({ runInfo, results, options: opts, thresholds: opts.thresholds });
    await fs.writeFile(path.join(runDir, 'results.json'), JSON.stringify({ runInfo, results }, null, 2), 'utf8');
    await fs.writeFile(path.join(runDir, 'summary.md'), summary, 'utf8');
    console.log(`\n报告：${path.join(runDir, 'summary.md')}`);
    console.log(`原始数据：${path.join(runDir, 'results.json')}`);
    console.log(summary.split('\n').slice(0, 40).join('\n'));
  } finally {
    if (fixtureServer) await fixtureServer.close();
  }
}

main().catch((e) => {
  console.error(e.stack || e);
  process.exitCode = 1;
});
