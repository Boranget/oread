// 阅读进度：本地 0..1 章内比例、滚动/分页还原、30s 节流上报。
// 从 Reader.svelte 原样下沉，页面状态通过 getter/setter 注入，便于单测。

import { clamp, clamp01 } from './readerMath.js';

export function createProgressTracker({
  setProgress,
  key,
  getScrollEl,
  isPaged,
  getViewW,
  getPageCount,
  setPageCount,
  getPageIndex,
  setPageIndex,
  getSuppress,
  setSuppress,
  getPos,
  getBookUrl,
  getChapterCount,
  getRestored,
  interval = 30000,
  storage = () => localStorage,
  now = () => Date.now(),
  defer = (fn, ms) => setTimeout(fn, ms),
} = {}) {
  let lastSaveAt = 0;
  let snapTimer = null;

  function readLocal() {
    try {
      return JSON.parse(storage().getItem(key(getBookUrl())) || 'null');
    } catch {
      return null;
    }
  }

  // 把 0..1 的阅读比例还原到当前模式（滚动用 scrollTop，分页用 scrollLeft）
  function applyRatio(r) {
    const el = getScrollEl();
    if (!el) return;
    const ratio = clamp01(r);
    setSuppress(true);
    if (isPaged()) {
      const max = Math.max(0, el.scrollWidth - el.clientWidth);
      el.scrollLeft = ratio * max;
    } else {
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      el.scrollTop = ratio * max;
    }
    // 图片等异步布局可能改变高度/列数，稍后再校正一次
    defer(() => {
      const later = getScrollEl();
      if (!later) return;
      if (isPaged()) {
        const w = Math.max(1, getViewW());
        setPageCount(Math.max(1, Math.round(later.scrollWidth / w)));
        const idx = clamp(Math.round(later.scrollLeft / w), 0, getPageCount() - 1);
        later.scrollLeft = idx * w;
        setPageIndex(idx);
      } else {
        const max = Math.max(0, later.scrollHeight - later.clientHeight);
        later.scrollTop = ratio * max;
      }
      setSuppress(false);
    }, 240);
  }

  function currentRatio() {
    const el = getScrollEl();
    if (!el) return 0;
    if (isPaged()) {
      const max = el.scrollWidth - el.clientWidth;
      return max > 0 ? clamp01(el.scrollLeft / max) : 0;
    }
    const max = el.scrollHeight - el.clientHeight;
    return max > 0 ? clamp01(el.scrollTop / max) : 0;
  }

  function persist(pos, ratio, keepalive = false) {
    if (pos < 0) return;
    lastSaveAt = now();
    Promise.resolve(
      setProgress(
        {
          book_id: getBookUrl(),
          chapter_index: pos,
          position: clamp01(ratio),
          total_chapters: getChapterCount(),
        },
        keepalive
      )
    ).catch(() => {});
  }

  function recordRatio(ratio) {
    if (getPos() < 0) return;
    try {
      storage().setItem(
        key(getBookUrl()),
        JSON.stringify({ chapter_index: getPos(), position: ratio, at: now() })
      );
    } catch {
      /* 本地存储不可用时忽略 */
    }
    // 滚动过程中只更新本地与内存；满 30s 才写一次后端，避免每页一个 POST
    if (now() - lastSaveAt >= interval) persist(getPos(), ratio);
  }

  function onScroll() {
    if (getPos() < 0) return;
    const el = getScrollEl();
    if (isPaged() && getViewW() > 0) {
      setPageIndex(clamp(Math.round(el.scrollLeft / getViewW()), 0, getPageCount() - 1));
    }
    if (getSuppress()) return;
    recordRatio(currentRatio());
    if (isPaged()) {
      clearTimeout(snapTimer);
      snapTimer = defer(snapToPage, 160);
    }
  }

  // 触摸横滑后吸附到整页
  function snapToPage() {
    if (!isPaged() || getSuppress()) return;
    const el = getScrollEl();
    if (!el) return;
    const w = Math.max(1, getViewW());
    const target = clamp(Math.round(el.scrollLeft / w), 0, getPageCount() - 1);
    const left = target * w;
    setPageIndex(target);
    if (Math.abs(el.scrollLeft - left) > 1) {
      el.scrollTo({ left, behavior: 'smooth' });
    }
  }

  function cancelSnap() {
    clearTimeout(snapTimer);
  }

  function flushSave(keepalive = false) {
    if (getPos() >= 0 && getRestored()) persist(getPos(), currentRatio(), keepalive);
  }

  function saveNow(ratio) {
    persist(getPos(), ratio);
  }

  function dispose() {
    clearTimeout(snapTimer);
  }

  return {
    readLocal,
    applyRatio,
    currentRatio,
    recordRatio,
    onScroll,
    snapToPage,
    cancelSnap,
    flushSave,
    saveNow,
    dispose,
  };
}
