// 轻量 API 封装：所有请求走同源 /api/*（由 Rust 后端提供）
const TOKEN_KEY = 'legado_token';
const ROLE_KEY = 'legado_role';

export function getToken() {
  return localStorage.getItem(TOKEN_KEY) || '';
}
export function getRole() {
  return localStorage.getItem(ROLE_KEY) || 'user';
}
export function setSession(token, role) {
  localStorage.setItem(TOKEN_KEY, token);
  if (role) localStorage.setItem(ROLE_KEY, role);
}
export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(ROLE_KEY);
}

// 解码 JWT payload（非加密，仅签名），用于读取 role 等声明
export function parseJwt(token) {
  try {
    const part = token.split('.')[1];
    const json = decodeURIComponent(
      atob(part.replace(/-/g, '+').replace(/_/g, '/'))
        .split('')
        .map((c) => '%' + ('00' + c.charCodeAt(0).toString(16)).slice(-2))
        .join('')
    );
    return JSON.parse(json);
  } catch {
    return {};
  }
}

async function request(method, path, body) {
  const headers = { 'Content-Type': 'application/json' };
  const token = getToken();
  if (token) headers['Authorization'] = `Bearer ${token}`;
  const res = await fetch(`/api${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (res.status === 401) {
    clearToken();
    location.reload();
    throw new Error('unauthorized');
  }
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    throw new Error((data && (data.error || JSON.stringify(data))) || `HTTP ${res.status}`);
  }
  return data;
}

export const api = {
  // ---- auth ----
  login: (username, password) => request('POST', '/auth/login', { username, password }),
  register: (username, password) => request('POST', '/auth/register', { username, password }),

  // ---- sources (admin) ----
  sources: () => request('GET', '/sources'),
  sourceDetail: (url) => request('GET', `/sources/detail?url=${encodeURIComponent(url)}`),
  saveSource: (s) => request('POST', '/sources', s),
  importSources: (list) => request('POST', '/sources/import', list),
  exportSources: () => request('GET', '/sources/export'),
  deleteSource: (url) => request('DELETE', `/sources/delete?url=${encodeURIComponent(url)}`),

  // ---- discovery / reading ----
  search: (key, page = 1) => request('GET', `/search?key=${encodeURIComponent(key)}&page=${page}`),
  explore: (url) => request('GET', `/explore?url=${encodeURIComponent(url)}`),
  bookInfo: (source, bookUrl) =>
    request('GET', `/book/info?source=${encodeURIComponent(source)}&book_url=${encodeURIComponent(bookUrl)}`),
  toc: (source, bookUrl) =>
    request('GET', `/book/toc?source=${encodeURIComponent(source)}&book_url=${encodeURIComponent(bookUrl)}`),
  content: (source, chapterUrl) =>
    request('GET', `/book/content?source=${encodeURIComponent(source)}&chapter_url=${encodeURIComponent(chapterUrl)}`),

  // ---- bookshelf ----
  bookshelf: () => request('GET', '/bookshelf'),
  addToShelf: (b) => request('POST', '/bookshelf', b),
  removeFromShelf: (bookId) => request('DELETE', `/bookshelf?book_id=${encodeURIComponent(bookId)}`),
  progress: (bookId) => request('GET', `/bookshelf/progress?book_id=${encodeURIComponent(bookId)}`),
  setProgress: (p) => request('POST', '/bookshelf/progress', p),
  groups: () => request('GET', '/bookshelf/groups'),
  addGroup: (name) => request('POST', '/bookshelf/groups', { name }),

  // ---- replace rules + settings ----
  replaceRules: () => request('GET', '/replace-rules'),
  saveReplaceRule: (r) => request('POST', '/replace-rules', r),
  deleteReplaceRule: (id) => request('DELETE', `/replace-rules/delete?id=${id}`),
  settings: () => request('GET', '/settings'),
  saveSettings: (s) => request('POST', '/settings', s),
};
