import test from 'node:test';
import assert from 'node:assert/strict';
import { createReadingStats } from './readerStats.js';

const flush = () => new Promise((resolve) => setImmediate(resolve));

function makeStats(overrides = {}) {
  const calls = [];
  const stats = createReadingStats({
    recordReading: async (payload, keepalive) => {
      calls.push({ payload, keepalive });
      return {};
    },
    getBookId: () => 'book-1',
    getName: () => '测试书',
    getChapterIndex: () => 3,
    getTotalChapters: () => 10,
    ...overrides,
  });
  return { stats, calls };
}

test('前台每秒累计，满 60 秒心跳上报', async () => {
  let visible = true;
  const { stats, calls } = makeStats({ isVisible: () => visible });
  for (let i = 0; i < 59; i += 1) stats.tick();
  assert.equal(calls.length, 0);
  visible = false;
  stats.tick();
  assert.equal(calls.length, 0);
  visible = true;
  stats.tick();
  await flush();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].payload.seconds, 60);
  assert.equal(calls[0].payload.book_id, 'book-1');
  assert.equal(calls[0].payload.name, '测试书');
  assert.equal(calls[0].payload.chapter_index, 3);
  assert.equal(calls[0].payload.total_chapters, 10);
  // 成功后扣减，再次满 60 秒才会再发
  stats.tick();
  await flush();
  assert.equal(calls.length, 1);
});

test('同一章只计一次字数', () => {
  const { stats, calls } = makeStats();
  stats.addChapter(1, 100);
  stats.addChapter(1, 100);
  stats.addChapter(2, 50);
  return stats.flush().then((ok) => {
    assert.equal(ok, true);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].payload.chars, 150);
    assert.equal(calls[0].payload.seconds, 0);
  });
});

test('上报失败保留计数，下次可重试', async () => {
  let fail = true;
  const calls = [];
  const stats = createReadingStats({
    recordReading: async (payload) => {
      calls.push(payload);
      if (fail) throw new Error('network');
      return {};
    },
    getBookId: () => 'book-1',
    getChapterIndex: () => 0,
  });
  stats.addChapter(0, 20);
  assert.equal(await stats.flush(), false);
  fail = false;
  assert.equal(await stats.flush(), true);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].chars, 20);
  // 成功后计数清零
  assert.equal(await stats.flush(), false);
  assert.equal(calls.length, 2);
});

test('无书或未进章节时不请求', async () => {
  const calls = [];
  const stats = createReadingStats({
    recordReading: async (payload) => calls.push(payload),
    getBookId: () => '',
    getChapterIndex: () => -1,
  });
  stats.addChapter(0, 10);
  assert.equal(await stats.flush(), false);
  assert.equal(calls.length, 0);
});
