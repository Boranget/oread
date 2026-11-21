// 阅读器正文图片状态机：按需把 <img> 代理加载成 objectURL，最多保留 N 张。
// 抽成工厂便于 node --test 用假 load 验证并发去重、失败重试与缓存淘汰。

import { writable } from 'svelte/store';

export function createImageLoader({ load, maxCache = 40 } = {}) {
  const state = writable({});
  const cache = new Map();

  function revoke(rec) {
    if (rec && rec.objectUrl && typeof URL !== 'undefined' && typeof URL.revokeObjectURL === 'function') {
      URL.revokeObjectURL(rec.objectUrl);
    }
  }

  function set(src, rec) {
    cache.set(src, rec);
    state.update((s) => ({ ...s, [src]: rec }));
  }

  // 超出上限按插入顺序淘汰并释放 objectURL，避免长时间阅读内存上涨
  function dropOld() {
    while (cache.size > maxCache) {
      const [key, val] = cache.entries().next().value;
      cache.delete(key);
      revoke(val);
      state.update((s) => {
        const next = { ...s };
        delete next[key];
        return next;
      });
    }
  }

  async function loadOne(src, force = false) {
    if (!src) return;
    const prev = cache.get(src);
    if (!force && prev && prev.status !== 'error') return;
    revoke(prev);
    set(src, { status: 'loading' });
    try {
      const rec = await load(src);
      set(src, { status: 'ok', objectUrl: rec.objectUrl, bytes: rec.bytes });
      dropOld();
    } catch (e) {
      set(src, { status: 'error', error: (e && e.message) || '图片加载失败，请重试' });
    }
  }

  // 触发本章图片加载：已在缓存（含加载中/成功）的跳过，失败的等用户点“重试”
  function queue(segs) {
    for (const p of segs || []) {
      if (!p || p.type !== 'image') continue;
      if (!cache.get(p.src)) loadOne(p.src);
    }
  }

  // 组件卸载时释放全部 objectURL
  function dispose() {
    for (const rec of cache.values()) revoke(rec);
    cache.clear();
    state.set({});
  }

  return {
    state,
    queue,
    retry: (src) => loadOne(src, true),
    markError: (src) => set(src, { status: 'error', error: '图片解码失败，请重试' }),
    dispose,
  };
}
