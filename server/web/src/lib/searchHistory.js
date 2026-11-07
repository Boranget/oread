import { get, writable } from 'svelte/store';

// 搜索历史：本地持久化（localStorage），最多 12 条
const HISTORY_KEY = 'oread_search_history';
const HISTORY_MAX = 12;

export const history = writable([]);

function readHistory() {
  try {
    const arr = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
    if (!Array.isArray(arr)) return [];
    return arr.filter((x) => typeof x === 'string' && x.trim()).slice(0, HISTORY_MAX);
  } catch {
    return [];
  }
}

function writeHistory(list) {
  const next = list.slice(0, HISTORY_MAX);
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
  } catch {
    /* 本地存储不可用时只作用于当前会话 */
  }
  history.set(next);
}

export function loadHistory() {
  history.set(readHistory());
}

export function rememberKey(q) {
  const v = String(q == null ? '' : q).trim();
  if (!v) return;
  writeHistory([v, ...get(history).filter((x) => x !== v)]);
}

export function removeHistory(q) {
  writeHistory(get(history).filter((x) => x !== q));
}

export function clearHistory() {
  writeHistory([]);
}
