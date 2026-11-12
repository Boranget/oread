// 搜索任务共享状态 + 轮询。
//
// App 级持有：登录后开始轮询，退出时停止；Discover 只读取 store 并触发动作。
// 轮询节奏自适应：有运行中任务时 2.5s，空闲时 15s（历史任务变化很慢）。
// 使用 setTimeout 链而不是 setInterval，保证请求不重叠、失败后也不会堆积。

import { get, writable } from 'svelte/store';
import { api, describeFailure } from './api.js';

const EMPTY = {
  jobs: [],
  active: 0,
  limit: 0,
  loading: false,
  error: '',
  updatedAt: 0,
};

const state = writable({ ...EMPTY });

/** 只读订阅入口：`$searchJobs`。 */
export const searchJobs = { subscribe: state.subscribe };

const RUNNING_POLL_MS = 2500;
const IDLE_POLL_MS = 15000;

let timer = null;
let running = false;
let failures = 0;
let inflight = null; // { promise, ctrl }

function countActive(jobs) {
  return jobs.filter((j) => j && j.status === 'running').length;
}

function scheduleNext() {
  if (!running) return;
  if (timer) clearTimeout(timer);
  const delay = get(state).active > 0 ? RUNNING_POLL_MS : IDLE_POLL_MS;
  timer = setTimeout(() => {
    timer = null;
    refreshSearchJobs();
  }, delay);
}

/** 立即刷新一次任务列表；并发调用复用同一个请求。 */
export function refreshSearchJobs() {
  if (inflight) return inflight.promise;
  const ctrl = new AbortController();
  state.update((s) => ({ ...s, loading: true }));
  const promise = (async () => {
    try {
      const r = await api.searchJobs(ctrl.signal);
      const jobs = Array.isArray(r && r.jobs) ? r.jobs : [];
      const active = Number.isFinite(Number(r && r.active))
        ? Math.max(0, Number(r.active))
        : countActive(jobs);
      failures = 0;
      state.set({
        jobs,
        active,
        limit: Number(r && r.limit) || 0,
        loading: false,
        error: '',
        updatedAt: Date.now(),
      });
    } catch (e) {
      if (e && e.aborted) return;
      failures += 1;
      // 失败时保留上一份列表：网络抖动不该让面板瞬间清空。
      state.update((s) => ({
        ...s,
        loading: false,
        error: describeFailure(e, '读取搜索任务'),
      }));
    } finally {
      if (running) scheduleNext();
    }
  })().finally(() => {
    if (inflight && inflight.promise === promise) inflight = null;
  });
  inflight = { promise, ctrl };
  return promise;
}

/** 登录后调用（幂等）。 */
export function startSearchJobsPolling() {
  if (running) return;
  running = true;
  failures = 0;
  refreshSearchJobs();
}

/** 退出登录时调用：停轮询、丢弃在飞请求并清空列表。 */
export function stopSearchJobsPolling() {
  running = false;
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  if (inflight) {
    inflight.ctrl.abort();
    inflight = null;
  }
  failures = 0;
  state.set({ ...EMPTY });
}

/** 取消任务；刷新列表让进度立即反映“取消中/已取消”。 */
export async function cancelSearchJob(jobId) {
  const r = await api.searchJobCancel(jobId);
  await refreshSearchJobs();
  return r;
}

/** 移除任务（后端要求先取消）；先本地移除，再以后端列表为准。 */
export async function removeSearchJob(jobId) {
  await api.searchJobDelete(jobId);
  state.update((s) => ({
    ...s,
    jobs: s.jobs.filter((j) => j.job_id !== jobId),
    active: countActive(s.jobs.filter((j) => j.job_id !== jobId)),
  }));
  await refreshSearchJobs();
}

/** 连续失败次数（供 UI 提示“稍后自动重试”）。 */
export function searchJobsFailures() {
  return failures;
}
