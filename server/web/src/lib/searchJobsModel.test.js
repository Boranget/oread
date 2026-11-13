import test from 'node:test';
import assert from 'node:assert/strict';

import {
  canCancel,
  canRemove,
  canView,
  countActive,
  formatRelativeTime,
  isRunning,
  jobProgress,
  jobProgressText,
  jobStatusLabel,
  jobStatusTone,
  jobTimeText,
} from './searchJobsModel.js';

const running = {
  job_id: 'j1',
  status: 'running',
  source_total: 100,
  finished: 30,
  cancelled: 0,
  failed: 2,
  skipped: 1,
  created_at: 1_000_000,
};

const done = {
  ...running,
  status: 'done',
  finished: 100,
  skipped: 3,
  finished_at: 1_060_000,
};

test('isRunning only true for running status', () => {
  assert.equal(isRunning(running), true);
  assert.equal(isRunning(done), false);
  assert.equal(isRunning(null), false);
  assert.equal(isRunning({ status: 'cancelled' }), false);
});

test('status labels and tones cover all four backend states', () => {
  assert.equal(jobStatusLabel('running'), '运行中');
  assert.equal(jobStatusLabel('done'), '已完成');
  assert.equal(jobStatusLabel('cancelled'), '已取消');
  assert.equal(jobStatusLabel('interrupted'), '已中断');
  assert.equal(jobStatusLabel('weird'), '未知');

  assert.equal(jobStatusTone('running'), 'running');
  assert.equal(jobStatusTone('done'), 'ok');
  assert.equal(jobStatusTone('cancelled'), 'warn');
  assert.equal(jobStatusTone('interrupted'), 'muted');
});

test('progress counts cancelled sources as processed', () => {
  assert.deepEqual(jobProgress(running), {
    total: 100,
    finished: 30,
    cancelled: 0,
    processed: 30,
    percent: 30,
  });
  // 取消后：finished 保留已跑完的，cancelled 是剩余未跑完的，两者相加 = 全部处理。
  const cancelled = { ...running, status: 'cancelled', finished: 30, cancelled: 70 };
  assert.deepEqual(jobProgress(cancelled), {
    total: 100,
    finished: 30,
    cancelled: 70,
    processed: 100,
    percent: 100,
  });
});

test('progress never exceeds 100% or goes negative', () => {
  const weird = { status: 'running', source_total: 10, finished: 99, cancelled: 99 };
  const p = jobProgress(weird);
  assert.equal(p.percent, 100);
  assert.equal(p.processed, 10);
  assert.equal(jobProgress({ status: 'running', source_total: 0 }).percent, 0);
  assert.equal(jobProgress({ status: 'done', source_total: 0 }).percent, 100);
});

test('progress text reports failures and skipped sources', () => {
  assert.equal(jobProgressText(running), '30/100 源 · 失败 2 · 未执行 1');
  assert.equal(jobProgressText(done), '100/100 源 · 失败 2 · 未执行 3');
  const cancelled = { ...running, status: 'cancelled', finished: 30, cancelled: 70, failed: 0, skipped: 0 };
  assert.equal(jobProgressText(cancelled), '30/100 源 · 取消 70');
  assert.equal(jobProgressText({ status: 'done', source_total: 0 }), '没有可搜索的书源');
});

test('relative time uses coarse buckets', () => {
  const now = 10 * 86_400_000;
  assert.equal(formatRelativeTime(now - 10_000, now), '刚刚');
  assert.equal(formatRelativeTime(now - 5 * 60_000, now), '5 分钟前');
  assert.equal(formatRelativeTime(now - 3 * 3_600_000, now), '3 小时前');
  assert.equal(formatRelativeTime(0, now), '');
  assert.equal(formatRelativeTime(undefined, now), '');
  // 超过一天回退到本地日期（只校验形状，避免依赖时区）
  assert.match(formatRelativeTime(now - 2 * 86_400_000, now), /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
});

test('job time text switches between started and finished', () => {
  const now = 1_000_000 + 5 * 60_000;
  assert.equal(jobTimeText(running, now), '开始于 5 分钟前');
  assert.equal(jobTimeText(done, now), '结束于 4 分钟前');
  assert.equal(jobTimeText(null, now), '');
});

test('actions: running can be cancelled/viewed, finished can be removed', () => {
  assert.equal(canCancel(running), true);
  assert.equal(canRemove(running), false, '运行中必须先取消再移除');
  assert.equal(canView(running), true);

  assert.equal(canCancel(done), false);
  assert.equal(canRemove(done), true);
  assert.equal(canView(done), true);
  assert.equal(canRemove(null), false);
});

test('countActive ignores finished jobs and bad input', () => {
  assert.equal(countActive([running, done, { status: 'cancelled' }]), 1);
  assert.equal(countActive([]), 0);
  assert.equal(countActive(null), 0);
});
