// 处理 HTML 文档：本地重写内联 <script> 与 <style>，让门禁/降级样式对
// 内联资源同样生效（外部脚本由 runner 的请求拦截负责）。
import { analyzeScript } from './es5.js';
import { auditCSS, degradeCSS } from './css.js';

const SCRIPT_RE = /<script\b([^>]*)>([\s\S]*?)<\/script\s*>/gi;
const STYLE_RE = /<style\b([^>]*)>([\s\S]*?)<\/style\s*>/gi;
const STYLE_ATTR_RE = /\bstyle\s*=\s*("([^"]*)"|'([^']*)')/gi;

function attr(attrs, name) {
  const re = new RegExp(`\\b${name}\\s*=\\s*("([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i');
  const m = attrs.match(re);
  if (!m) return '';
  return (m[2] ?? m[3] ?? m[4] ?? '').trim();
}

function jsType(type) {
  const t = (type || '').toLowerCase();
  if (!t) return 'classic';
  if (t === 'module') return 'module';
  if (['text/javascript', 'application/javascript', 'application/ecmascript', 'text/ecmascript'].includes(t)) return 'classic';
  return 'data';
}

/**
 * @returns {{html:string, inlineScripts:object[], inlineStyles:object[], moduleSrcs:string[], styleAttrs:object[]}}
 */
export function rewriteHTML(html, baseUrl, { degrade = true } = {}) {
  const inlineScripts = [];
  const inlineStyles = [];
  const styleAttrs = [];
  const moduleSrcs = [];
  let scriptIndex = 0;

  const rewrittenScripts = html.replace(SCRIPT_RE, (whole, attrs, body) => {
    // KPW3 的 WebKit 不认识 nomodule；在它眼里这就是普通脚本，会执行。
    // Chromium 原生会跳过，所以这里删掉该属性以贴近 KPW3 的行为。
    const cleanAttrs = attrs.replace(/\s+nomodule(?:\s*=\s*("[^"]*"|'[^']*'|[^\s>]*))?/gi, '');
    const src = attr(cleanAttrs, 'src');
    const typeKind = jsType(attr(cleanAttrs, 'type'));
    if (src) {
      if (typeKind === 'module') {
        try {
          moduleSrcs.push(new URL(src, baseUrl).href);
        } catch (_e) {
          /* ignore */
        }
      }
      return `<script${cleanAttrs}>${body}</script>`;
    }
    if (typeKind === 'data') return `<script${cleanAttrs}>${body}</script>`;
    if (body.trim() === '') return `<script${cleanAttrs}>${body}</script>`;
    scriptIndex += 1;
    const label = `${baseUrl}#inline-script-${scriptIndex}`;
    if (typeKind === 'module') {
      const entry = {
        url: label,
        kind: 'inline-module',
        bytes: Buffer.byteLength(body, 'utf8'),
        parseable: false,
        blocked: true,
        error: { message: 'type=module 在 KPW3 WebKit 中不被支持', line: 0, column: 0, position: -1, sourceUrl: label },
        features: {},
        featureSummary: '',
        reason: 'module',
      };
      inlineScripts.push(entry);
      return `<script${cleanAttrs}>/* kpw3-sim: blocked inline module script */</script>`;
    }
    const analyzed = analyzeScript(body, label, 'inline');
    const blocked = !analyzed.parseable;
    inlineScripts.push({ ...analyzed, blocked, reason: blocked ? 'es5-parse-error' : null });
    if (blocked) {
      return `<script${cleanAttrs}>/* kpw3-sim: blocked inline script (ES5 parse error) */</script>`;
    }
    return `<script${cleanAttrs}>${body}</script>`;
  });

  const rewrittenStyles = rewrittenScripts.replace(STYLE_RE, (whole, attrs, body) => {
    if (!body.trim()) return whole;
    const label = `${baseUrl}#inline-style-${inlineStyles.length + 1}`;
    const audit = auditCSS(body);
    const degraded = degrade ? degradeCSS(body) : { css: body, changes: null };
    inlineStyles.push({ url: label, bytes: Buffer.byteLength(body, 'utf8'), audit, changes: degraded.changes });
    return `<style${attrs}>${degraded.css}</style>`;
  });

  const rewrittenAttrs = rewrittenStyles.replace(STYLE_ATTR_RE, (whole, quoted, dq, sq) => {
    const value = dq ?? sq ?? '';
    const audit = auditCSS(value);
    if (audit.total > 0) {
      styleAttrs.push({ snippet: value.slice(0, 200), audit });
    }
    if (!degrade) return whole;
    const transformed = degradeCSS(value).css;
    if (dq !== undefined) return `style="${transformed}"`;
    return `style='${transformed}'`;
  });

  return { html: rewrittenAttrs, inlineScripts, inlineStyles, moduleSrcs, styleAttrs };
}

export { attr };
