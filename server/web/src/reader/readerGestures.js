// 阅读手势：点击分区翻页/显隐控制栏、上下滑切章、滚轮与键盘。
// 从 Reader.svelte 原样下沉，页面状态通过注入回调读写，内部维护点击抑制与触摸起点。

export function createGestures({
  getScrollEl,
  isPaged,
  isBlocked,
  panelsOpen,
  prevPage,
  nextPage,
  toggleControls,
  toggleToc,
  openChapter,
  toggleAuto,
  stopAuto,
  getPrevPos,
  getNextPos,
  isExportOpen,
  onEscape,
  getSelection = defaultSelection,
  now = () => Date.now(),
} = {}) {
  let wheelAt = 0;
  let touchStart = null;
  let suppressClickUntil = 0;

  function onClick(e) {
    if (now() < suppressClickUntil) return;
    if (isBlocked()) return;
    if (e.target.closest('button,a,input,select,textarea,label')) return;
    if (getSelection()) return; // 长按选中文本时不触发翻页
    const el = getScrollEl();
    if (!el) return;
    const r = el.getBoundingClientRect();
    const x = e.clientX - r.left;
    if (x < r.width / 3) prevPage();
    else if (x > (r.width * 2) / 3) nextPage();
    else toggleControls();
  }

  function onTouchStart(e) {
    if (!e.touches || !e.touches.length) return;
    const t = e.touches[0];
    const el = getScrollEl();
    touchStart = { x: t.clientX, y: t.clientY, at: now(), top: el ? el.scrollTop : 0 };
    stopAuto();
  }

  function onTouchEnd(e) {
    const s = touchStart;
    touchStart = null;
    if (!s || !e.changedTouches || !e.changedTouches.length) return;
    const t = e.changedTouches[0];
    const dx = t.clientX - s.x;
    const dy = t.clientY - s.y;
    const dt = now() - s.at;
    // 有明显位移时抑制随后的 click，避免横滑/纵滑结束后误触翻页
    if (Math.abs(dx) > 24 || Math.abs(dy) > 24) suppressClickUntil = now() + 400;
    // 长按 / 小幅滑动 / 以横滑为主时不切章，避免误触与选择文本冲突
    if (dt > 600 || Math.abs(dy) < 100 || Math.abs(dy) <= Math.abs(dx) * 1.3) return;
    if (getSelection()) return;
    suppressClickUntil = now() + 400;
    if (isPaged()) {
      if (dy < 0 && getNextPos() >= 0) openChapter(getNextPos(), { ratio: 0 });
      else if (dy > 0 && getPrevPos() >= 0) openChapter(getPrevPos(), { ratio: 0.999 });
      return;
    }
    const el = getScrollEl();
    if (!el) return;
    const max = el.scrollHeight - el.clientHeight;
    if (dy > 0 && s.top <= 4 && getPrevPos() >= 0) openChapter(getPrevPos(), { ratio: 0.999 });
    else if (dy < 0 && s.top >= max - 4 && getNextPos() >= 0) openChapter(getNextPos(), { ratio: 0 });
  }

  function onTouchCancel() {
    touchStart = null;
  }

  // 桌面滚轮：滚动模式只负责停止自动滚动；分页模式把滚轮映射为翻页
  function onWheel(e) {
    stopAuto();
    if (!isPaged() || panelsOpen()) return;
    if (Math.abs(e.deltaY) < 6 && Math.abs(e.deltaX) < 6) return;
    const t = now();
    if (t - wheelAt < 320) return;
    wheelAt = t;
    if (e.deltaY > 0 || e.deltaX > 0) nextPage();
    else prevPage();
  }

  function onKeydown(e) {
    const tag = ((e.target && e.target.tagName) || '').toLowerCase();
    if (['input', 'textarea', 'select', 'button'].includes(tag)) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (isExportOpen() && e.key !== 'Escape') return;
    if (e.key === 'ArrowLeft') {
      e.preventDefault();
      prevPage();
    } else if (e.key === 'ArrowRight') {
      e.preventDefault();
      nextPage();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      const el = getScrollEl();
      if (!isPaged() && el) el.scrollBy({ top: -80, behavior: 'smooth' });
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      const el = getScrollEl();
      if (!isPaged() && el) el.scrollBy({ top: 80, behavior: 'smooth' });
    } else if (e.key === ' ' || e.key === 'Spacebar') {
      e.preventDefault();
      toggleAuto();
    } else if (e.key === 't' || e.key === 'T') {
      // 键盘用户无需先点中间唤出控制栏再点“目录”。
      toggleToc();
    } else if (e.key === 'Escape') {
      onEscape();
    }
  }

  return { onClick, onTouchStart, onTouchEnd, onTouchCancel, onWheel, onKeydown };
}

function defaultSelection() {
  return window.getSelection ? String(window.getSelection()).trim() : '';
}
