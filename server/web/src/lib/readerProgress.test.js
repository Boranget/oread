import test from 'node:test';
import assert from 'node:assert/strict';
import { createProgressTracker } from './readerProgress.js';

function makeTracker(overrides = {}) {
  const st = {
    el: null,
    paged: false,
    viewW: 300,
    pageCount: 1,
    pageIndex: 0,
    suppress: false,
    pos: 0,
    bookUrl: 'book-1',
    chapters: 2,
    restored: true,
    now: 0,
    calls: [],
    deferred: [],
  };
  const storage = {
    data: {},
    getItem(k) {
      return this.data[k] ?? null;
    },
    setItem(k, v) {
      this.data[k] = v;
    },
  };
  const tracker = createProgressTracker({
    setProgress: (p, keepalive) => st.calls.push({ p, keepalive }),
    key: (id) => `oread_progress:${id}`,
    getScrollEl: () => st.el,
    isPaged: () => st.paged,
    getViewW: () => st.viewW,
    getPageCount: () => st.pageCount,
    setPageCount: (n) => {
      st.pageCount = n;
    },
    getPageIndex: () => st.pageIndex,
    setPageIndex: (i) => {
      st.pageIndex = i;
    },
    getSuppress: () => st.suppress,
    setSuppress: (v) => {
      st.suppress = v;
    },
    getPos: () => st.pos,
    getBookUrl: () => st.bookUrl,
    getChapterCount: () => st.chapters,
    getRestored: () => st.restored,
    interval: 30000,
    storage: () => storage,
    now: () => st.now,
    defer: (fn) => {
      st.deferred.push(fn);
      return st.deferred.length;
    },
    ...overrides,
  });
  return { tracker, st, storage };
}

test('readLocal 读取本地进度，损坏时返回 null', () => {
  const { tracker, storage } = makeTracker();
  storage.data['oread_progress:book-1'] = JSON.stringify({ chapter_index: 2, position: 0.5 });
  assert.deepEqual(tracker.readLocal(), { chapter_index: 2, position: 0.5 });
  storage.data['oread_progress:book-1'] = '{bad';
  assert.equal(tracker.readLocal(), null);
});

test('滚动模式 currentRatio 与 applyRatio（含 240ms 校正）', () => {
  const { tracker, st } = makeTracker();
  st.el = { scrollTop: 15, scrollHeight: 130, clientHeight: 100, scrollLeft: 0 };
  assert.equal(tracker.currentRatio(), 0.5);
  tracker.applyRatio(0.25);
  assert.equal(st.suppress, true);
  assert.equal(st.el.scrollTop, 7.5);
  st.el.scrollTop = 99;
  st.deferred[0]();
  assert.equal(st.el.scrollTop, 7.5);
  assert.equal(st.suppress, false);
});

test('分页模式 applyRatio 校正页数与页码', () => {
  const { tracker, st } = makeTracker({ isPaged: () => true });
  st.el = { scrollWidth: 900, clientWidth: 300, scrollLeft: 0, scrollTo() {} };
  tracker.applyRatio(0.5);
  assert.equal(st.el.scrollLeft, 300);
  st.deferred[0]();
  assert.equal(st.pageCount, 3);
  assert.equal(st.pageIndex, 1);
  assert.equal(tracker.currentRatio(), 0.5);
});

test('recordRatio 写本地并按 30s 节流上报', () => {
  const { tracker, st, storage } = makeTracker();
  const base = 1700000000000;
  st.now = base;
  tracker.recordRatio(0.4);
  assert.equal(st.calls.length, 1);
  assert.equal(st.calls[0].p.position, 0.4);
  assert.equal(st.calls[0].p.book_id, 'book-1');
  assert.equal(st.calls[0].p.total_chapters, 2);
  assert.deepEqual(JSON.parse(storage.data['oread_progress:book-1']), {
    chapter_index: 0,
    position: 0.4,
    at: base,
  });
  st.now = base + 1000;
  tracker.recordRatio(0.5);
  assert.equal(st.calls.length, 1);
  st.now = base + 31000;
  tracker.recordRatio(0.6);
  assert.equal(st.calls.length, 2);
});

test('flushSave 仅在已恢复且进章后补发，saveNow 夹取比例', () => {
  const { tracker, st } = makeTracker();
  st.el = { scrollTop: 30, scrollHeight: 130, clientHeight: 100 };
  st.restored = false;
  tracker.flushSave(true);
  assert.equal(st.calls.length, 0);
  st.restored = true;
  tracker.flushSave(true);
  assert.equal(st.calls.length, 1);
  assert.equal(st.calls[0].keepalive, true);
  tracker.saveNow(2);
  assert.equal(st.calls[1].p.position, 1);
  st.pos = -1;
  tracker.flushSave();
  assert.equal(st.calls.length, 2);
});

test('onScroll 更新页码并延迟吸附整页', () => {
  const { tracker, st } = makeTracker({ isPaged: () => true });
  st.pageCount = 3;
  const scrolled = [];
  st.el = {
    scrollLeft: 280,
    scrollWidth: 900,
    clientWidth: 300,
    scrollTop: 0,
    scrollHeight: 0,
    clientHeight: 0,
    scrollTo: (o) => scrolled.push(o),
  };
  tracker.onScroll();
  assert.equal(st.pageIndex, 1);
  assert.equal(st.deferred.length, 1);
  st.deferred[0]();
  assert.equal(st.pageIndex, 1);
  assert.deepEqual(scrolled, [{ left: 300, behavior: 'smooth' }]);
});
