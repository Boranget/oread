// 阅读页通用数值工具（从 Reader.svelte 原样下沉）
export const clamp = (v, min, max) => Math.min(max, Math.max(min, v));
export const clamp01 = (v) => clamp(Number(v) || 0, 0, 1);
export const num = (v, def) =>
  v === null || v === undefined || v === '' || !Number.isFinite(Number(v)) ? def : Number(v);

export function clampPos(i, len) {
  return Math.min(Math.max(0, i), Math.max(0, len - 1));
}

export function isReadable(i, chapters) {
  return i >= 0 && i < chapters.length && !chapters[i].is_volume;
}

// 跳过卷标题，找上一/下一可读章节
export function sibling(pos, dir, chapters) {
  let i = pos + dir;
  while (i >= 0 && i < chapters.length) {
    if (!chapters[i].is_volume) return i;
    i += dir;
  }
  return -1;
}
