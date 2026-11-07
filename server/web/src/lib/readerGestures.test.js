import test from 'node:test';
import assert from 'node:assert/strict';
import { createGestures } from './readerGestures.js';

function makeGestures(overrides = {}) {
  const st = {
    calls: [],
    stops: 0,
    now: 1000,
    prevPos: 3,
    nextPos: 5,
    selection: '',
    paged: false,
    el: {
      scrollTop: 0,
      scrollHeight: 200,
      clientHeight: 100,
      getBoundingClientRect: () => ({ left: 0, width: 300 }),
      scrollBy: (o) => st.calls.push({ scrollBy: o }),
    },
  };
  const g = createGestures({
    getScrollEl: () => st.el,
    isPaged: () => st.paged,
    isBlocked: () => false,
    panelsOpen: () => false,
    prevPage: () => st.calls.push('prev'),
    nextPage: () => st.calls.push('next'),
    toggleControls: () => st.calls.push('controls'),
    toggleToc: () => st.calls.push('toc'),
    openChapter: (pos) => st.calls.push(`open:${pos}`),
    toggleAuto: () => st.calls.push('auto'),
    stopAuto: () => {
      st.stops += 1;
    },
    getPrevPos: () => st.prevPos,
    getNextPos: () => st.nextPos,
    isExportOpen: () => false,
    onEscape: () => st.calls.push('esc'),
    getSelection: () => st.selection,
    now: () => st.now,
    ...overrides,
  });
  return { g, st };
}

test('点击左右 1/3 翻页，中间显隐控制栏', () => {
  const { g, st } = makeGestures();
  const target = { closest: () => null };
  g.onClick({ target, clientX: 10 });
  g.onClick({ target, clientX: 150 });
  g.onClick({ target, clientX: 290 });
  assert.deepEqual(st.calls, ['prev', 'controls', 'next']);
});

test('按钮/输入控件或选中文本不触发翻页', () => {
  const { g, st } = makeGestures();
  g.onClick({ target: { closest: () => ({}) }, clientX: 10 });
  st.selection = '选中';
  g.onClick({ target: { closest: () => null }, clientX: 10 });
  assert.deepEqual(st.calls, []);
});

test('上滑到正文底部切下一章，并抑制紧随的 click', () => {
  const { g, st } = makeGestures();
  st.el.scrollTop = 100;
  g.onTouchStart({ touches: [{ clientX: 10, clientY: 120 }] });
  g.onTouchEnd({ changedTouches: [{ clientX: 12, clientY: 20 }] });
  assert.deepEqual(st.calls, ['open:5']);
  assert.equal(st.stops, 1);
  st.now = 1100;
  g.onClick({ target: { closest: () => null }, clientX: 10 });
  assert.deepEqual(st.calls, ['open:5']);
});

test('滚轮仅分页模式翻页，320ms 内节流', () => {
  const { g, st } = makeGestures({ isPaged: () => true });
  g.onWheel({ deltaY: 40, deltaX: 0 });
  st.now = 1200;
  g.onWheel({ deltaY: 40, deltaX: 0 });
  assert.deepEqual(st.calls, ['next']);
  assert.equal(st.stops, 2);
});

test('键盘：方向键翻页、空格自动滚动、输入框内忽略', () => {
  const { g, st } = makeGestures();
  let prevented = 0;
  const ev = (key, tag = 'div') => ({
    key,
    target: { tagName: tag },
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    preventDefault: () => {
      prevented += 1;
    },
  });
  g.onKeydown(ev('ArrowLeft'));
  g.onKeydown(ev(' '));
  g.onKeydown(ev('ArrowLeft', 'input'));
  g.onKeydown(ev('Escape'));
  assert.deepEqual(st.calls, ['prev', 'auto', 'esc']);
  assert.equal(prevented, 2);
});
