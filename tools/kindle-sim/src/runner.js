// 单 URL 的仿真执行：JS 模式 + 完全禁用 JS 模式。
import fs from 'node:fs/promises';
import path from 'node:path';
import { chromium } from 'playwright-core';
import { analyzeScript } from './es5.js';
import { auditCSS, degradeCSS } from './css.js';
import { rewriteHTML } from './html.js';
import { measurePage } from './measure.js';

export const KPW3_VIEWPORT = { width: 1072, height: 1268 };
export const KPW3_SCREEN = { width: 1072, height: 1448 };
// mesquite（KPW3）真机 UA（来自上报 JSON）；可在配置里覆盖。
export const KPW3_UA = 'Mozilla/5.0 (X11; ; U; Linux armv7l; zh-cn) AppleWebKit/534.26+ (KHTML, like Gecko) Version/5.0 Safari/534.26+';
export const KPW3_DPR = 1.7964;

const MISSING_GLOBALS = `(() => {
  try { window.Promise = undefined; } catch (e) {}
  try { window.fetch = undefined; } catch (e) {}
})();`;

const BROWSER_PATHS = {
  edge: [
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  ],
  chrome: [
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  ],
};

export async function launchBrowser(options = {}) {
  const choice = options.browser || 'edge';
  let executablePath = choice;
  if (choice === 'edge' || choice === 'chrome') {
    executablePath = null;
    for (const p of BROWSER_PATHS[choice]) {
      try {
        await fs.access(p);
        executablePath = p;
        break;
      } catch (_e) {
        /* try next */
      }
    }
    if (!executablePath) throw new Error(`找不到本机 ${choice}，请用 --browser <绝对路径> 指定 Chromium 内核浏览器`);
  }
  const browser = await chromium.launch({
    executablePath,
    headless: options.headless !== false,
    args: ['--no-first-run', '--disable-dev-shm-usage', '--disable-gpu', '--force-color-profile=srgb'],
  });
  const version = browser.version();
  return {
    browser,
    meta: { name: choice, path: executablePath, version },
  };
}

function headersWithoutEncoding(resp, contentType) {
  const headers = { ...resp.headers() };
  delete headers['content-length'];
  delete headers['content-encoding'];
  delete headers['transfer-encoding'];
  // KPW3 的 WebKit 不认识现代 CSP；保留 CSP 会误伤本应在旧设备执行的脚本。
  delete headers['content-security-policy'];
  delete headers['content-security-policy-report-only'];
  if (contentType) headers['content-type'] = contentType;
  return headers;
}

function isCss(url, response) {
  const ct = (response.headers()['content-type'] || '').toLowerCase();
  return ct.includes('text/css') || /\.css(?:[?#]|$)/i.test(url);
}

function slugify(name, url, index) {
  const raw = name || url;
  const slug = raw
    .replace(/^https?:\/\//, '')
    .replace(/[^\w\u4e00-\u9fa5.-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 70);
  return `${String(index + 1).padStart(2, '0')}-${slug || 'target'}`;
}

function shortUrl(url, max = 110) {
  return url.length > max ? `${url.slice(0, max - 1)}…` : url;
}

export async function runTarget({ browserMeta, target, options, runDir, index }) {
  const slug = slugify(target.name, target.url, index);
  const shotDir = path.join(runDir, 'screens');
  await fs.mkdir(shotDir, { recursive: true });

  const js = await runOne({ browserMeta, target, options, mode: 'js', slug, shotDir });
  const nojs = await runOne({ browserMeta, target, options, mode: 'nojs', slug, shotDir });
  // cookie 属于会话凭据，不写入 results.json
  const { cookie: _omit, ...safeTarget } = target;
  return { target: { ...safeTarget, slug }, js, nojs };
}

async function runOne({ browserMeta, target, options, mode, slug, shotDir }) {
  const { browser } = browserMeta;
  const jsEnabled = mode === 'js';
  const context = await browser.newContext({
    viewport: KPW3_VIEWPORT,
    screen: KPW3_SCREEN,
    deviceScaleFactor: KPW3_DPR,
    isMobile: false,
    hasTouch: false,
    userAgent: options.userAgent || KPW3_UA,
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
    javaScriptEnabled: jsEnabled,
    colorScheme: 'light',
  });

  // 可选：target.cookie = "name=value"（或 "name=value; name2=value2"），注入到目标源。
  // 也支持 `env:VAR`，从环境变量读取，避免把会话凭据写进配置文件。
  if (target.cookie) {
    const rawCookie = String(target.cookie).startsWith('env:')
      ? (process.env[String(target.cookie).slice(4)] || '')
      : String(target.cookie);
    const origin = new URL(target.url).origin;
    const cookies = rawCookie
      .split(';')
      .map((p) => p.trim())
      .filter(Boolean)
      .map((pair) => {
        const eq = pair.indexOf('=');
        return { name: pair.slice(0, eq).trim(), value: pair.slice(eq + 1).trim(), url: origin };
      });
    if (cookies.length) await context.addCookies(cookies);
  }

  const state = {
    scripts: [],
    css: [],
    styleAttrs: [],
    failures: [],
    httpErrors: [],
    console: [],
    pageErrors: [],
    blockedUrls: new Map(),
    moduleSrcs: new Set(),
    inlineModuleSrcs: [],
    navError: null,
  };

  if (jsEnabled) await context.addInitScript(MISSING_GLOBALS);

  await context.route('**/*', async (route) => {
    const req = route.request();
    const url = req.url();
    const type = req.resourceType();
    try {
      if (type === 'document') {
        const resp = await route.fetch();
        const ct = (resp.headers()['content-type'] || '').toLowerCase();
        if (!ct.includes('text/html') && !ct.includes('application/xhtml')) {
          await route.fulfill({ status: resp.status(), headers: headersWithoutEncoding(resp), body: await resp.body() });
          return;
        }
        const html = await resp.text();
        const rewritten = rewriteHTML(html, url, { degrade: options.degradeCss !== false });
        for (const s of rewritten.inlineScripts) state.scripts.push(s);
        for (const s of rewritten.inlineStyles) state.css.push({ ...s, source: 'inline' });
        for (const s of rewritten.moduleSrcs) state.moduleSrcs.add(s);
        if (rewritten.styleAttrs.length) state.styleAttrs.push(...rewritten.styleAttrs);
        await route.fulfill({
          status: resp.status(),
          headers: headersWithoutEncoding(resp, resp.headers()['content-type']),
          body: rewritten.html,
        });
        return;
      }

      if (isCssCandidate(url, type)) {
        const resp = await route.fetch();
        if (!isCss(url, resp)) {
          await route.fulfill({ status: resp.status(), headers: headersWithoutEncoding(resp), body: await resp.body() });
          return;
        }
        const cssText = await resp.text();
        const audit = auditCSS(cssText);
        const degraded = options.degradeCss !== false ? degradeCSS(cssText) : { css: cssText, changes: null };
        state.css.push({
          url,
          source: 'external',
          bytes: Buffer.byteLength(cssText, 'utf8'),
          audit,
          changes: degraded.changes,
        });
        await route.fulfill({
          status: resp.status(),
          headers: headersWithoutEncoding(resp, resp.headers()['content-type']),
          body: degraded.css,
        });
        return;
      }

      if (!jsEnabled) {
        await route.continue();
        return;
      }

      if (type === 'script' || type === 'worker' || /\.(?:js|mjs)(?:[?#]|$)/i.test(url)) {
        const resp = await route.fetch();
        const script = await resp.text();
        const isModule = state.moduleSrcs.has(url) || /\.mjs(?:[?#]|$)/i.test(url);
        const entry = analyzeScript(script, url, type === 'worker' ? 'worker' : 'external');
        entry.status = resp.status();
        entry.blocked = false;
        entry.reason = null;
        if (resp.status() >= 400) {
          entry.parseable = false;
          entry.error = { message: `HTTP ${resp.status()}`, line: 0, column: 0, position: -1, sourceUrl: url };
          entry.blocked = true;
          entry.reason = 'http-error';
        } else if (isModule) {
          entry.blocked = true;
          entry.reason = 'module';
          entry.parseable = false;
          entry.error = entry.error || { message: 'type=module 在 KPW3 WebKit 中不被支持', line: 0, column: 0, position: -1, sourceUrl: url };
        } else if (!entry.parseable) {
          entry.blocked = true;
          entry.reason = 'es5-parse-error';
        }
        state.scripts.push(entry);
        if (entry.blocked) {
          state.blockedUrls.set(url, entry.reason);
          await route.abort('blockedbyclient');
        } else {
          await route.fulfill({
            status: resp.status(),
            headers: headersWithoutEncoding(resp, resp.headers()['content-type']),
            body: script,
          });
        }
        return;
      }

      await route.continue();
    } catch (e) {
      // 路由失败不应该让整个测试中止；记录后放行原请求。
      state.failures.push({ url, resourceType: type, errorText: `route-error: ${e.message}`, intentional: false });
      try {
        await route.continue();
      } catch (_e) {
        /* ignore */
      }
    }
  });

  const page = await context.newPage();
  page.on('requestfailed', (req) => {
    const url = req.url();
    const intentional = state.blockedUrls.has(url);
    state.failures.push({
      url,
      resourceType: req.resourceType(),
      errorText: req.failure() ? req.failure().errorText : 'unknown',
      intentional,
      reason: intentional ? state.blockedUrls.get(url) : null,
    });
  });
  page.on('response', (resp) => {
    if (resp.status() >= 400) {
      state.httpErrors.push({ url: resp.url(), status: resp.status(), resourceType: resp.request().resourceType() });
    }
  });
  page.on('console', (msg) => {
    if (msg.type() === 'error' || msg.type() === 'warning') {
      const loc = msg.location();
      state.console.push({
        type: msg.type(),
        text: msg.text(),
        url: loc.url || '',
        line: loc.lineNumber || 0,
      });
    }
  });
  page.on('pageerror', (err) => {
    state.pageErrors.push({ message: err.message || String(err), stack: (err.stack || '').slice(0, 800) });
  });
  page.on('dialog', async (dialog) => {
    try {
      await dialog.dismiss();
    } catch (_e) {
      /* ignore */
    }
  });

  const started = Date.now();
  try {
    await page.goto(target.url, { waitUntil: 'domcontentloaded', timeout: options.timeoutMs });
  } catch (e) {
    state.navError = e.message;
  }
  try {
    await page.waitForLoadState('load', { timeout: Math.min(options.timeoutMs, 15000) });
  } catch (_e) {
    /* load 可能因被 block 的脚本延迟；继续度量 */
  }
  if (options.waitMs > 0) {
    try {
      await page.waitForTimeout(options.waitMs);
    } catch (_e) {
      /* ignore */
    }
  }

  let metrics = null;
  let metricsError = null;
  try {
    metrics = await page.evaluate(measurePage);
  } catch (e) {
    metricsError = e.message;
    metrics = {
      url: '',
      visibleChars: 0,
      contentChars: 0,
      firstScreenChars: 0,
      firstScreenContentChars: 0,
      firstScreenContentBlocks: 0,
      bodyTextLength: 0,
      innerWidth: KPW3_VIEWPORT.width,
      innerHeight: KPW3_VIEWPORT.height,
      dpr: 1,
      scrollWidth: 0,
      scrollHeight: 0,
      horizontalOverflow: 0,
      mainCandidate: null,
      mainContentInFirstScreen: false,
      promiseType: 'unknown',
      fetchType: 'unknown',
    };
  }

  const shotBase = path.join(shotDir, `${slug}-${mode}`);
  let screenshot = null;
  let firstScreenShot = null;
  try {
    await page.screenshot({ path: `${shotBase}-full.png`, fullPage: true, animations: 'disabled', caret: 'hide' });
    screenshot = path.relative(path.dirname(shotDir), `${shotBase}-full.png`);
  } catch (e) {
    state.failures.push({ url: target.url, resourceType: 'screenshot', errorText: `fullPage: ${e.message}`, intentional: false });
  }
  try {
    await page.screenshot({ path: `${shotBase}-firstscreen.png`, fullPage: false, animations: 'disabled', caret: 'hide' });
    firstScreenShot = path.relative(path.dirname(shotDir), `${shotBase}-firstscreen.png`);
  } catch (_e) {
    /* ignore */
  }

  await context.close();

  const blockedScripts = state.scripts.filter((s) => s.blocked);
  return {
    mode,
    jsEnabled,
    startedAt: started,
    durationMs: Date.now() - started,
    navError: state.navError,
    metrics,
    metricsError,
    scripts: state.scripts,
    blockedScripts,
    css: state.css,
    styleAttrs: state.styleAttrs,
    failures: state.failures,
    httpErrors: state.httpErrors,
    console: state.console,
    pageErrors: state.pageErrors,
    moduleSrcs: [...state.moduleSrcs],
    screenshot,
    firstScreenShot,
    globals: {
      promise: metrics.promiseType,
      fetch: metrics.fetchType,
      xhr: metrics.xhrType,
      localStorage: metrics.storageType,
      cookie: metrics.cookieEnabled,
    },
    runtime: {
      ua: metrics.ua,
      innerWidth: metrics.innerWidth,
      innerHeight: metrics.innerHeight,
      dpr: metrics.dpr,
      screenWidth: metrics.screenWidth,
      screenHeight: metrics.screenHeight,
      metaViewport: metrics.metaViewport,
      hasTouch: metrics.hasTouch,
      moduleScriptCount: metrics.moduleScriptCount,
      nomoduleScriptCount: metrics.nomoduleScriptCount,
    },
  };
}

function isCssCandidate(url, type) {
  return type === 'stylesheet' || /\.css(?:[?#]|$)/i.test(url);
}

export { shortUrl };
