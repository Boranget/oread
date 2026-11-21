// 阅读器正文空态判定（与后端 /api/book/content 的响应形态对齐）。
// 抽成纯函数便于 node --test 回归：空正文不能再以“只有标题+目录”的白屏呈现。

const CN_NUM = '0-9零〇一二三四五六七八九十百千两';

export function isVolumeTitle(title) {
  const t = String(title || '').trim();
  if (!t) return false;
  return (
    new RegExp(`^第\\s*[${CN_NUM}]+\\s*[卷部篇册]`).test(t) ||
    new RegExp(`^[卷部篇册]\\s*[${CN_NUM}]+`).test(t) ||
    /^(vol|volume)\.?\s*\d+/i.test(t)
  );
}

export function isEmptyChapter(data) {
  if (!data) return false;
  if (String(data.content_type || 'text') === 'image') {
    return !(Array.isArray(data.images) ? data.images.length : 0);
  }
  const text = String(data.content || '')
    .replace(/<[^>]*>/g, ' ')
    .replace(/&nbsp;/gi, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  return text.length === 0;
}
