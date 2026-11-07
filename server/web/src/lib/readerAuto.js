// 自动滚动 / 自动翻页：状态与节奏逻辑从 Reader.svelte 原样下沉。
// 通过注入的回调读写页面状态（滚动容器、分页位置、章节切换），便于单测替身。

export function createAutoScroll({
  isPaged,
  canStart,
  getScrollEl,
  getNextPos,
  openChapter,
  isLoading,
  hasError,
  setPage,
  getPageIndex,
  getPageCount,
  getSpeed,
  setOn,
  onStart,
  toast,
  raf = (cb) => requestAnimationFrame(cb),
  caf = (id) => cancelAnimationFrame(id),
  later = (fn, ms) => setTimeout(fn, ms),
  cancelLater = (id) => clearTimeout(id),
} = {}) {
  let on = false;
  let rafId = 0;
  let timer = null;
  let last = 0;
  let advancing = false;

  const isOn = () => on;

  function toggle() {
    if (on) {
      stop();
      return;
    }
    if (!canStart()) {
      toast('正文尚未加载');
      return;
    }
    onStart();
    on = true;
    setOn(true);
    last = 0;
    if (isPaged()) schedule(400);
    else rafId = raf(tick);
  }

  function stop(msg) {
    on = false;
    setOn(false);
    caf(rafId);
    rafId = 0;
    cancelLater(timer);
    timer = null;
    if (msg) toast(msg);
  }

  function tick(ts) {
    if (!on || isPaged()) return;
    if (!last) last = ts;
    const dt = Math.min(80, ts - last);
    last = ts;
    if (isLoading()) {
      rafId = raf(tick);
      return;
    }
    if (hasError()) {
      stop('正文加载失败，已停止自动滚动');
      return;
    }
    const el = getScrollEl();
    if (!el) {
      rafId = raf(tick);
      return;
    }
    const max = el.scrollHeight - el.clientHeight;
    if (el.scrollTop >= max - 1) {
      const next = getNextPos();
      if (next >= 0) {
        if (!advancing) {
          advancing = true;
          Promise.resolve(openChapter(next, { ratio: 0 })).finally(() => {
            advancing = false;
          });
        }
      } else {
        stop('已到全书末尾');
        return;
      }
    } else {
      el.scrollTop += (getSpeed() * 14) * dt / 1000;
    }
    rafId = raf(tick);
  }

  function schedule(delay) {
    cancelLater(timer);
    if (!on || !isPaged()) return;
    // 自动翻页节奏按档位线性变化，最低 900ms
    const wait = delay != null ? delay : (11 - getSpeed()) * 1000;
    timer = later(pageStep, Math.max(900, wait));
  }

  async function pageStep() {
    if (!on || !isPaged()) return;
    if (isLoading()) {
      schedule(900);
      return;
    }
    if (hasError()) {
      stop('正文加载失败，已停止自动滚动');
      return;
    }
    if (getPageIndex() < getPageCount() - 1) {
      setPage(getPageIndex() + 1);
      schedule();
    } else if (getNextPos() >= 0) {
      await openChapter(getNextPos(), { ratio: 0 });
      schedule();
    } else {
      stop('已到全书末尾');
    }
  }

  function dispose() {
    on = false;
    setOn(false);
    caf(rafId);
    rafId = 0;
    cancelLater(timer);
    timer = null;
  }

  return { toggle, stop, schedule, isOn, dispose };
}
