// CSS 能力审计 + 降级样式近似。
// 注意：Chromium 无法真正删除 flexbox/grid 实现，这里的做法是
//   1) 审计原始 CSS 中出现的不兼容特性；
//   2) 把 CSS 文本改写成 KPW3 上更接近的降级声明（flex→block 等），
//      仅用于观察“样式坍塌后”的版面，不等同于真机。
// 因此 CSS 相关结论在报告中统一标注为中/低置信度。

const COMMENT_RE = /\/\*[\s\S]*?\*\//g;

export function stripComments(css) {
  return String(css || '').replace(COMMENT_RE, ' ');
}

export function auditCSS(text) {
  const css = stripComments(text);
  const count = (re) => (css.match(re) || []).length;
  const flexAll = count(/display\s*:\s*(?:inline-)?flex\b/gi);
  const inlineFlex = count(/display\s*:\s*inline-flex\b/gi);
  const gridAll = count(/display\s*:\s*(?:inline-)?grid\b/gi);
  const inlineGrid = count(/display\s*:\s*inline-grid\b/gi);
  const fixed = count(/position\s*:\s*fixed\b/gi);
  const vw = count(/\d(?:\.\d+)?vw\b/gi);
  const vh = count(/\d(?:\.\d+)?vh\b/gi);
  const clamp = count(/clamp\s*\(/gi);
  const media = count(/@media\b/gi);
  const supports = count(/@supports\b/gi);
  const sticky = count(/position\s*:\s*sticky\b/gi);
  return {
    flex: flexAll,
    inlineFlex,
    grid: gridAll,
    inlineGrid,
    fixed,
    vw,
    vh,
    clamp,
    media,
    supports,
    sticky,
    total: flexAll + gridAll + fixed + vw + vh + clamp + media + supports + sticky,
  };
}

export function mergeAudit(a, b) {
  const out = {};
  for (const key of Object.keys(a || {})) out[key] = (a[key] || 0) + (b[key] || 0);
  return out;
}

/**
 * 把 CSS 改写成“KPW3 近似可支持”的版本。
 * - 删除 @media / @supports 整块（KPW3 不认多数特性）
 * - 删除含 vw/vh/clamp() 的声明
 * - display:flex/grid -> block；inline-* -> inline-block
 * - position:fixed -> absolute
 */
export function degradeCSS(text) {
  const changes = { mediaBlocks: 0, supportsBlocks: 0, viewportDecls: 0, flex: 0, grid: 0, fixed: 0, sticky: 0 };
  let css = stripComments(String(text || ''));
  css = stripAtRuleBlocks(css, '@media', changes, 'mediaBlocks');
  css = stripAtRuleBlocks(css, '@supports', changes, 'supportsBlocks');

  css = css.replace(
    /([a-zA-Z-]+)\s*:\s*[^;{}]*?(?:(?:\d|\.\d)(?:\.\d+)?(?:vw|vh|vmin|vmax)|clamp\s*\()[^;{}]*;?/gi,
    () => {
      changes.viewportDecls += 1;
      return '';
    },
  );

  css = css.replace(/display\s*:\s*inline-flex\b/gi, () => {
    changes.flex += 1;
    return 'display:inline-block';
  });
  css = css.replace(/display\s*:\s*flex\b/gi, () => {
    changes.flex += 1;
    return 'display:block';
  });
  css = css.replace(/display\s*:\s*inline-grid\b/gi, () => {
    changes.grid += 1;
    return 'display:inline-block';
  });
  css = css.replace(/display\s*:\s*grid\b/gi, () => {
    changes.grid += 1;
    return 'display:block';
  });
  css = css.replace(/position\s*:\s*fixed\b/gi, () => {
    changes.fixed += 1;
    return 'position:absolute';
  });
  css = css.replace(/position\s*:\s*sticky\b/gi, () => {
    changes.sticky += 1;
    return 'position:relative';
  });
  return { css, changes };
}

// 扫描并删除 @media / @supports 的整块内容（简化括号匹配，忽略字符串内的括号）。
function stripAtRuleBlocks(css, atRule, changes, key) {
  let out = '';
  let i = 0;
  while (i < css.length) {
    const idx = css.indexOf(atRule, i);
    if (idx === -1) {
      out += css.slice(i);
      break;
    }
    out += css.slice(i, idx);
    let j = idx;
    let quote = null;
    while (j < css.length) {
      const ch = css[j];
      if (quote) {
        if (ch === '\\') j += 1;
        else if (ch === quote) quote = null;
      } else if (ch === '"' || ch === "'") {
        quote = ch;
      } else if (ch === '{') {
        break;
      }
      j += 1;
    }
    if (j >= css.length) {
      out += css.slice(idx);
      break;
    }
    let depth = 0;
    let k = j;
    let q = null;
    for (; k < css.length; k += 1) {
      const ch = css[k];
      if (q) {
        if (ch === '\\') k += 1;
        else if (ch === q) q = null;
      } else if (ch === '"' || ch === "'") {
        q = ch;
      } else if (ch === '{') {
        depth += 1;
      } else if (ch === '}') {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    changes[key] += 1;
    out += `/* kpw3-sim: removed ${atRule} block */`;
    i = k + 1;
  }
  return out;
}

export function cssTotals(files) {
  const total = { flex: 0, inlineFlex: 0, grid: 0, inlineGrid: 0, fixed: 0, vw: 0, vh: 0, clamp: 0, media: 0, supports: 0, sticky: 0, total: 0 };
  for (const f of files) {
    for (const key of Object.keys(total)) total[key] += f.audit?.[key] || 0;
  }
  return total;
}
