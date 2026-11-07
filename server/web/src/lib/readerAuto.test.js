import test from 'node:test';
import assert from 'node:assert/strict';
import { createAutoScroll } from './readerAuto.js';

function makeAuto(overrides = {}) {
  const state = {
    on: false,
    page: 0,
    timers: [],
    rafs: [],
    toasts: [],
    chapters: [],
    started: 0,
  };
  const ctx = {
    isPaged: () => false,
    canStart: () => true,
    getScrollEl: () => null,
    getNextPos: () => -1,
    openChapter: (pos, opts) => {
      state.chapters.push({ pos, opts });
      return Promise.resolve();
    },
    isLoading: () => false,
    hasError: () => false,
    setPage: (i) => {
      state.page = i;
    },
    getPageIndex: () => state.page,
    getPageCount: () => 1,
    getSpeed: () => 3,
    setOn: (v) => {
      state.on = v;
    },
    onStart: () => {
      state.started += 1;
    },
    toast: (m) => state.toasts.push(m),
    raf: (cb) => {
      state.rafs.push(cb);
      return state.rafs.length;
    },
    caf: (id) => {
      state.rafs[id - 1] = null;
    },
    later: (fn, ms) => {
      state.timers.push({ fn, ms });
      return state.timers.length;
    },
    cancelLater: (id) => {
      if (state.timers[id - 1]) state.timers[id - 1].cancelled = true;
    },
    ...overrides,
  };
  return { auto: createAutoScroll(ctx), state };
}

test('未加载正文时只提示，不启动', () => {
  const { auto, state } = makeAuto({ canStart: () => false });
  auto.toggle();
  assert.equal(state.on, false);
  assert.deepEqual(state.toasts, ['正文尚未加载']);
});

test('滚动模式按 tick 推进 scrollTop，到底后切下一章', async () => {
  const el = { scrollTop: 0, scrollHeight: 200, clientHeight: 100 };
  const { auto, state } = makeAuto({ getScrollEl: () => el, getNextPos: () => 2 });
  auto.toggle();
  const tick = state.rafs[0];
  tick(16);
  tick(32);
  assert.ok(Math.abs(el.scrollTop - 0.672) < 1e-9);
  el.scrollTop = 100;
  tick(48);
  await new Promise((r) => setImmediate(r));
  assert.equal(state.chapters.length, 1);
  assert.deepEqual(state.chapters[0], { pos: 2, opts: { ratio: 0 } });
});

test('滚动模式到全书末尾自动停止并提示', () => {
  const el = { scrollTop: 100, scrollHeight: 200, clientHeight: 100 };
  const { auto, state } = makeAuto({ getScrollEl: () => el, getNextPos: () => -1 });
  auto.toggle();
  state.rafs[0](0);
  assert.equal(state.on, false);
  assert.deepEqual(state.toasts, ['已到全书末尾']);
});

test('分页模式按档位调度翻页，末页切下一章', async () => {
  const { auto, state } = makeAuto({
    isPaged: () => true,
    getPageCount: () => 3,
    getNextPos: () => 4,
  });
  auto.toggle();
  assert.equal(state.on, true);
  assert.equal(state.timers.length, 1);
  // 初始 400ms 仍受“最低 900ms”下限约束
  assert.equal(state.timers[0].ms, 900);
  await state.timers[0].fn();
  assert.equal(state.page, 1);
  assert.equal(state.timers[1].ms, 8000);
  await state.timers[1].fn();
  assert.equal(state.page, 2);
  await state.timers[2].fn();
  assert.equal(state.chapters.length, 1);
  assert.equal(state.chapters[0].pos, 4);
});

test('分页模式加载中延迟重试，失败则停止', async () => {
  let loading = true;
  const { auto, state } = makeAuto({
    isPaged: () => true,
    isLoading: () => loading,
  });
  auto.toggle();
  await state.timers[0].fn();
  assert.equal(state.timers[1].ms, 900);
  loading = false;
  state.timers.length = 0;
  // 复用同一实例，模拟下一次调度失败
  const failed = makeAuto({ isPaged: () => true, hasError: () => true });
  failed.auto.toggle();
  await failed.state.timers[0].fn();
  assert.equal(failed.state.on, false);
  assert.deepEqual(failed.state.toasts, ['正文加载失败，已停止自动滚动']);
});

test('dispose 清理 rAF 与定时器', () => {
  const { auto, state } = makeAuto({ isPaged: () => true });
  auto.toggle();
  assert.equal(state.on, true);
  auto.dispose();
  assert.equal(state.on, false);
  assert.equal(state.timers[0].cancelled, true);
});
