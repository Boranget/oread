// 阅读器正文解析：从后端返回的 HTML 片段里保留 <img>，切成文本段/图片段。
// 抽成纯函数便于 node --test 回归（相对地址、懒加载属性与 HTML 实体）。

const IMG_TAG_RE = /<img\b[^>]*>/gi;

export function plainChars(s) {
  return String(s || '')
    .replace(/<[^>]*>/g, '')
    .replace(/\s/g, '').length;
}

// 读取 <img> 属性；兼容带引号/不带引号两种写法
export function imgAttr(tag, name) {
  const re = new RegExp(`\\b${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i');
  const m = re.exec(String(tag || ''));
  return m ? (m[1] ?? m[2] ?? m[3] ?? '') : '';
}

// 懒加载属性优先（src 常是 1px 占位图），顺序与后端 pick_img_src 保持一致
export function pickImgSrc(tag) {
  for (const k of ['data-src', 'data-original', 'data-lazy-src', 'data-original-src', 'data-echo', 'data-url']) {
    const v = imgAttr(tag, k).trim();
    if (v) return v;
  }
  const src = imgAttr(tag, 'src').trim();
  if (src) return src;
  for (const k of ['srcset', 'data-srcset']) {
    const first = imgAttr(tag, k).split(',')[0].trim().split(/\s+/)[0];
    if (first) return first;
  }
  return '';
}

// 相对地址按章节页归并为绝对地址（图片代理只接 http/https）
export function absImg(raw, base) {
  const s = String(raw || '').trim();
  if (!s || /^data:/i.test(s) || /^javascript:/i.test(s)) return '';
  try {
    return new URL(s, base || (typeof location !== 'undefined' ? location.href : undefined)).href;
  } catch {
    return '';
  }
}

export function textToParas(html) {
  const text = String(html || '')
    .replace(/<\s*(br|\/p|\/div|\/h[1-6]|\/li|\/tr|\/blockquote)\s*\/?\s*>/gi, '\n')
    .replace(/<[^>]*>/g, '')
    .replace(/&nbsp;/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&');
  return text
    .replace(/[ \t\u00a0]{2,}/g, ' ')
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);
}

// 正文解析：保留 <img>，按页面顺序切成文本段 / 图片段，图片随后走代理加载
export function htmlToParas(html, base) {
  const src = String(html || '');
  const out = [];
  let last = 0;
  IMG_TAG_RE.lastIndex = 0;
  let m;
  while ((m = IMG_TAG_RE.exec(src))) {
    for (const t of textToParas(src.slice(last, m.index))) out.push({ type: 'text', text: t });
    const url = absImg(pickImgSrc(m[0]), base);
    if (url) out.push({ type: 'image', src: url, alt: imgAttr(m[0], 'alt') });
    last = m.index + m[0].length;
  }
  for (const t of textToParas(src.slice(last))) out.push({ type: 'text', text: t });
  return out;
}
