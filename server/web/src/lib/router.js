// 极简 hash 路由：不引入依赖，刷新/分享/后退都可用。
//
// 选 hash 而不是 history API：后端静态资源是 ServeDir，没有 SPA 回退；
// hash 不需要改服务端就能深链到 /book、/library 等页面。
import { writable } from 'svelte/store';

export function parseHash(hash) {
  const raw = String(hash || '').replace(/^#/, '') || '/';
  const [pathPart, qs] = raw.split('?');
  const query = {};
  new URLSearchParams(qs || '').forEach((value, key) => {
    query[key] = value;
  });
  return { path: pathPart || '/', query };
}

export const route = writable(parseHash(typeof location === 'undefined' ? '' : location.hash));

export function toHash(to) {
  const s = String(to || '/');
  return '#' + (s.startsWith('/') ? s : '/' + s);
}

export function navigate(to, { replace = false } = {}) {
  const target = toHash(to);
  if (typeof location === 'undefined') return;
  if (location.hash === target) {
    route.set(parseHash(target));
    return;
  }
  if (replace) {
    history.replaceState(null, '', location.pathname + location.search + target);
    route.set(parseHash(target));
  } else {
    location.hash = target;
  }
}

export function href(to) {
  return toHash(to);
}

export function startRouter() {
  if (typeof window === 'undefined') return () => {};
  const sync = () => route.set(parseHash(location.hash));
  window.addEventListener('hashchange', sync);
  sync();
  return () => window.removeEventListener('hashchange', sync);
}

// 把参数拼成 `?a=1&b=2`，自动丢掉空值，保持 URL 干净。
export function buildPath(path, params = {}) {
  const qs = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null) continue;
    const s = String(value).trim();
    if (!s) continue;
    qs.set(key, s);
  }
  const q = qs.toString();
  return q ? `${path}?${q}` : path;
}
