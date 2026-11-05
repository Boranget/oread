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

// 后端返回的固定文案 -> 界面文案
const KNOWN_ERRORS = {
  'source not found': '书源不存在',
  'url or urls required': '至少需要一个书源 URL',
  'book_id or book_ids required': '至少需要选择一本书',
  'group name required': '分组名不能为空',
  missing_book_url: '缺少书籍地址，请从书架重新打开',
  unauthorized: '登录已失效，请重新登录',
  forbidden: '没有权限执行该操作（需要管理员）',
  'internal:': '书源抓取失败',
};

function apiError(message, status, data) {
  const err = new Error(message);
  err.status = status;
  err.data = data;
  return err;
}

function shortText(s) {
  const t = String(s || '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
  return t.length > 160 ? t.slice(0, 160) + '…' : t;
}

// 后端聚合搜索里的 errors[].error 是上游抓取错误原文；这里翻成人话。
// 真实书源的报错形态：builder error（请求头/URL 规则非法）、error sending request（连不上）、
// timeout（超时）、internal: ...（解析失败）。
export function describeSourceError(raw) {
  const t = String(raw || '').trim();
  if (!t) return '未知错误';
  if (/builder error/i.test(t)) return '书源配置有误，请求无法构造（请求头或 URL 规则非法）';
  if (/timeout|timed ?out|deadline/i.test(t)) return '书源站点响应超时';
  if (/error sending request|connection (?:refused|reset)|dns|connect/i.test(t)) return '无法连接书源站点（网络不可达或被拦截）';
  if (/^internal:/i.test(t)) return '书源解析失败：' + shortText(t.slice(9));
  const m = /(?:\bHTTP\b|status(?:_code)?[=: ]+)(\d{3})/i.exec(t);
  if (m) return '书源站点返回 HTTP ' + m[1];
  return shortText(t);
}

// 统一失败提示：先说“发生了什么”，再说“能做什么”。
// scene 是动作名（搜索/发现/读取目录/加载正文/换源），拼成 “搜索失败：…。请…”。
export function describeFailure(e, scene = '操作') {
  const status = e && e.status;
  const msg = shortText(e && e.message) || '未知错误';
  if (status === 401) return scene + '失败：登录已失效。请重新登录后再试。';
  if (status === 403) return scene + '失败：当前账号没有权限。请联系管理员，或换用有权限的账号。';
  if (status === 0) {
    if (/超时/.test(msg)) {
      return scene + '失败：请求超时，服务器或书源响应太慢。请稍后重试；搜索可先换个关键词或减少书源。';
    }
    return scene + '失败：连不上服务器。请确认后端服务已启动、网络正常，然后重试。';
  }
  // 504 是后端主动的超时预算（如书内搜索 5s），优先按超时文案处理
  if (status === 504) {
    return scene + '失败：请求超时（服务端或书源响应太慢）。请稍后重试；书内搜索可缩短关键词。';
  }
  if (status >= 500) {
    return scene + '失败：服务端出错（HTTP ' + status + '）。请稍后重试；持续出现可到「设置」页查看服务状态。';
  }
  if (/missing_book_url|缺少书籍地址/.test(msg)) {
    return scene + '失败：缺少书籍地址。请从书架重新打开这本书，或点「换源」换一个可用书源。';
  }
  if (/目录为空/.test(msg)) {
    return scene + '失败：书源没有返回章节，目录规则可能失效。请点「换源」改用其他书源。';
  }
  if (status === 400 || status === 422) {
    return scene + '失败：' + msg + '。请按提示修改后重试。';
  }
  return scene + '失败：' + msg + '。请稍后重试；若一直失败，可到「设置」页查看服务状态。';
}

// 把 400/422 的纯文本、JSON error 统一成用户能看懂的中文
function errorText(status, data) {
  // 新错误体：{ error: <稳定错误码>, message: <面向用户的文案>, request_id }
  const raw = data && typeof data === 'object'
    ? String(data.message ?? data.error ?? '')
    : typeof data === 'string' ? data : '';
  const text = shortText(raw);
  if (KNOWN_ERRORS[text]) return KNOWN_ERRORS[text];
  if (text.startsWith('internal:')) {
    return KNOWN_ERRORS['internal:'] + '（' + shortText(text.slice(9)) + '）';
  }
  if (status === 400 || status === 422) {
    // 后端 400 常带 {"error":"具体原因"}（如设置越界），按原意直接展示
    const isJsonError = data && typeof data === 'object' && (data.message || data.error);
    if (isJsonError && text) return text;
    const missing = /missing field `([^`]+)`/.exec(text);
    if (missing) return '缺少参数：' + missing[1];
    if (/invalid type|Failed to deserialize/.test(text)) return '请求参数格式不正确';
    if (/Expected request with|Content-Type: application\/json/i.test(text)) return '请求体不是合法的 JSON';
    return text ? '请求参数有误：' + text : '请求参数有误';
  }
  if (status === 403) return KNOWN_ERRORS.forbidden;
  if (status === 404) return '资源不存在';
  if (status >= 500) return '服务端错误（HTTP ' + status + '）' + (text ? '：' + text : '');
  return text || '请求失败（HTTP ' + status + '）';
}

// 统一处理响应：401 清会话，非 2xx 抛错，空响应返回 null
// opts.noAuthReload=true 时不刷新页面（导出弹窗需要把 401 就地展示给用户）
function finish(status, text, path, opts = {}) {
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (status >= 200 && status < 300) return data;

  if (status === 401) {
    const isLogin = path === '/auth/login' || path === '/auth/register';
    if (isLogin) throw apiError('用户名或密码不正确', status, data);
    if (opts.noAuthReload) throw apiError('登录已失效，请重新登录', status, data);
    clearToken();
    location.reload();
    throw apiError('登录已失效，请重新登录', status, data);
  }
  throw apiError(errorText(status, data), status, data);
}

async function request(method, path, body, opts = {}) {
  const headers = { 'Content-Type': 'application/json' };
  const token = getToken();
  if (token) headers.Authorization = 'Bearer ' + token;
  const timeout = opts.timeout || 45000;
  const ctrl = typeof AbortController !== 'undefined' ? new AbortController() : null;
  const timer = ctrl ? setTimeout(() => ctrl.abort(), timeout) : null;
  // 外部 signal（搜索轮询换 job/换页时取消旧请求）；取消时标记 aborted，调用方据此静默丢弃
  const external = opts.signal;
  let onAbort = null;
  if (ctrl && external) {
    if (external.aborted) ctrl.abort();
    else {
      onAbort = () => ctrl.abort();
      external.addEventListener('abort', onAbort, { once: true });
    }
  }
  let res;
  try {
    res = await fetch('/api' + path, {
      method,
      headers,
      body: body ? JSON.stringify(body) : undefined,
      signal: ctrl ? ctrl.signal : undefined,
      keepalive: !!opts.keepalive,
    });
  } catch (e) {
    if (e && e.name === 'AbortError') {
      if (external && external.aborted) {
        const err = apiError('请求已取消', 0, e);
        err.aborted = true;
        throw err;
      }
      throw apiError(`请求超时（${Math.round(timeout / 1000)}s），请稍后重试`, 0, e);
    }
    throw apiError('网络连接失败，请确认后端服务已启动', 0, e);
  } finally {
    if (timer) clearTimeout(timer);
    if (onAbort && external) external.removeEventListener('abort', onAbort);
  }
  return finish(res.status, await res.text(), path, opts);
}

// 解析 Content-Disposition：优先 RFC 5987 的 filename*（中文书名），回退 ASCII 名
function filenameFromDisposition(v) {
  const s = String(v || '');
  const m = /filename\*=UTF-8''([^;]+)/i.exec(s);
  if (m) {
    try {
      return decodeURIComponent(m[1]);
    } catch {
      /* 编码损坏时走下面的 ASCII 兜底 */
    }
  }
  const q = /filename="?([^";]+)"?/i.exec(s);
  return q ? q[1] : '';
}

// 附件下载：不能走 <a download> 直链（带不上 Bearer），必须 fetch + Blob。
// 返回 { blob, bytes, headers }，文件名由调用方按「书名.txt」决定，
// objectURL 的创建与释放也在调用方完成，避免这里泄漏。
async function downloadTxt(bookUrl, opts = {}) {
  const path = `/book/export/txt?${query({ book_url: bookUrl })}`;
  const headers = {};
  const token = getToken();
  if (token) headers.Authorization = 'Bearer ' + token;

  // 整本是逐章流式写盘，章数多时会跑几十秒，单独放宽超时
  const timeout = opts.timeout || 180000;
  const ctrl = typeof AbortController !== 'undefined' ? new AbortController() : null;
  const timer = ctrl ? setTimeout(() => ctrl.abort(), timeout) : null;

  let res;
  try {
    res = await fetch('/api' + path, {
      method: 'GET',
      headers,
      signal: ctrl ? ctrl.signal : undefined,
      cache: 'no-store',
    });
  } catch (e) {
    if (e && e.name === 'AbortError') {
      throw apiError(`导出超时（${Math.round(timeout / 1000)}s），章节较多时请稍后重试`, 0, e);
    }
    throw apiError('网络中断，导出请求没有完成', 0, e);
  } finally {
    if (timer) clearTimeout(timer);
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    let data = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = text;
      }
    }
    if (res.status === 401) throw apiError('登录已失效，请重新登录后再导出', 401, data);
    throw apiError(errorText(res.status, data), res.status, data);
  }

  let blob;
  try {
    blob = await res.blob();
  } catch (e) {
    throw apiError('导出中断，文件数据只下载到一部分', 0, e);
  }
  const h = (name) => Number(res.headers.get(name)) || 0;
  const fileBytes = h('x-file-bytes');
  return {
    blob,
    // 与响应头 x-file-bytes 对账；头部缺失时退回 Blob 实际大小
    bytes: fileBytes || blob.size,
    headers: {
      total: h('x-total-chapters'),
      cached: h('x-cached-chapters'),
      missing: h('x-missing-chapters'),
      written: h('x-written-chapters'),
      fileBytes,
    },
    filename: filenameFromDisposition(res.headers.get('content-disposition')),
  };
}

// 漫画单图：<img src> 带不上 Bearer，必须 fetch + Blob + objectURL。
// 后端 /api/book/image 已做 SSRF、Referer、大小/超时限制与内存 LRU 缓存。
async function fetchComicImage(source, url, chapterUrl = '', bookUrl = '') {
  const path = `/book/image?${query({ source, book_url: bookUrl, url, chapter_url: chapterUrl })}`;
  const headers = {};
  const token = getToken();
  if (token) headers.Authorization = 'Bearer ' + token;

  const ctrl = typeof AbortController !== 'undefined' ? new AbortController() : null;
  const timer = ctrl ? setTimeout(() => ctrl.abort(), 30000) : null;
  let res;
  try {
    res = await fetch('/api' + path, {
      method: 'GET',
      headers,
      cache: 'no-store',
      signal: ctrl ? ctrl.signal : undefined,
    });
  } catch (e) {
    if (e && e.name === 'AbortError') throw apiError('图片加载超时，请重试这张图', 0, e);
    throw apiError('图片请求失败，请检查网络后重试这张图', 0, e);
  } finally {
    if (timer) clearTimeout(timer);
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    let data = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = text;
      }
    }
    if (res.status === 401) throw apiError('登录已失效，请重新登录', 401, data);
    // 后端图片错误自带中文 message（404/非图片/超大/防盗链/WebP）
    const msg =
      data && typeof data === 'object' && (data.message || data.error)
        ? String(data.message || data.error)
        : errorText(res.status, data);
    const err = apiError(msg, res.status, data);
    err.code = data && typeof data === 'object' ? data.error : '';
    throw err;
  }

  const ct = String(res.headers.get('content-type') || '');
  if (!ct.toLowerCase().startsWith('image/')) {
    throw apiError('源站返回的不是图片（已拦截 HTML 错误页）', 415, { content_type: ct });
  }
  let blob;
  try {
    blob = await res.blob();
  } catch (e) {
    throw apiError('图片只下载到一部分，请重试这张图', 0, e);
  }
  const h = (name) => Number(res.headers.get(name)) || 0;
  return {
    objectUrl: URL.createObjectURL(blob),
    bytes: h('x-image-bytes') || blob.size,
    cache: res.headers.get('x-image-cache') || '',
    contentType: ct.split(';')[0],
    width: h('x-image-width'),
    height: h('x-image-height'),
  };
}

// 空值不写进查询串，保持 /search?key=&page=1 这类请求干净
function query(obj) {
  const qs = new URLSearchParams();
  Object.entries(obj).forEach(([k, v]) => {
    if (v !== undefined && v !== null && v !== '') qs.set(k, String(v));
  });
  return qs.toString();
}

export const api = {
  // ---- auth ----
  login: (username, password) => request('POST', '/auth/login', { username, password }),
  register: (username, password) => request('POST', '/auth/register', { username, password }),
  // 自助改密：成功即递增 token_version，当前 token 立即失效，需要重新登录
  changePassword: (old_password, new_password) =>
    request('POST', '/auth/password', { old_password, new_password }),
  // ---- 用户管理（仅管理员）----
  adminUsers: () => request('GET', '/admin/users'),
  adminSetUserStatus: (userId, disabled) =>
    request('POST', '/admin/users/status', { user_id: userId, disabled: !!disabled }),

  // ---- sources (admin) ----
  sources: () => request('GET', '/sources'),
  sourceDetail: (url) => request('GET', `/sources/detail?url=${encodeURIComponent(url)}`),
  saveSource: (s) => request('POST', '/sources', s),
  importSources: (list) => request('POST', '/sources/import', list),
  exportSources: (tier = 'all') => request('GET', `/sources/export?tier=${encodeURIComponent(tier)}`),
  deleteSource: (url) => request('DELETE', `/sources/delete?url=${encodeURIComponent(url)}`),
  setSourcesEnabled: (urls, enabled) =>
    request('POST', '/sources/enabled', {
      urls: Array.isArray(urls) ? urls : [urls],
      enabled: !!enabled,
    }),
  validateSource: (source, checkUrl = false) =>
    request('POST', '/sources/validate', { source, check_url: !!checkUrl }),
  // 只读比对：入参为 Legado 书源数组（或 {sources:[]}），返回 updated/new/local_newer/same
  checkSourceUpdates: (list) =>
    request('POST', '/sources/check-updates', list, { timeout: 60000 }),

  // ---- 体检语料 / 精选书源 ----
  benchCorpus: () => request('GET', '/sources/bench/corpus'),
  addBenchCorpus: (item) =>
    request('POST', '/sources/bench/corpus', {
      title: item.title,
      expected_author: item.expectedAuthor || null,
      category: item.category,
      type: item.type,
      enabled: item.enabled !== false,
    }),
  updateBenchCorpus: (id, item) =>
    request('POST', '/sources/bench/corpus/update', {
      id,
      title: item.title,
      expected_author: item.expectedAuthor || null,
      category: item.category,
      type: item.type,
      enabled: item.enabled !== false,
    }),
  toggleBenchCorpus: (id, enabled) =>
    request('POST', '/sources/bench/corpus/toggle', { id, enabled: !!enabled }),
  deleteBenchCorpus: (id) =>
    request('DELETE', `/sources/bench/corpus?id=${encodeURIComponent(id)}`),
  // 体检改为异步：POST 立即返回 {run_id}，用 benchRunStatus 轮询进度
  runBench: (sourceIds = 'all', limit = 5) =>
    request('POST', '/sources/bench/run', { source_ids: sourceIds, limit }),
  benchRunStatus: (runId) =>
    request('GET', `/sources/bench/run/${encodeURIComponent(runId)}`),
  curatedSources: () => request('GET', '/sources/bench/curated'),

  // ---- discovery / reading ----
  search: (key, page = 1, pageSize = 30) =>
    request('GET', `/search?${query({ key, page, page_size: pageSize })}`),
  // 两段式搜索：job 轮询，books 先到先渲染
  // result 轮询已完成源；后端只读 key（不是 q），翻页复用同一 job_id
  // sourceIds 可传 book_source_url 或 /api/sources 排序后的 1-based 序号（逗号分隔）
  // scope 必须显式传：all（默认）/ curated；调用方不能依赖后端默认值。
  searchStart: (key, sourceIds = [], scope = 'all') =>
    request(
      'GET',
      `/search/start?${query({
        key,
        source_ids: Array.isArray(sourceIds) && sourceIds.length ? sourceIds.join(',') : undefined,
        scope,
      })}`,
      undefined,
      { timeout: 20000 }
    ),
  searchResult: (jobId, page = 1, pageSize = 30, signal) =>
    request(
      'GET',
      `/search/result?${query({ job_id: jobId, page, page_size: pageSize })}`,
      undefined,
      { timeout: 15000, signal }
    ),
  // 发现分类：source 传书源 ID（book_source_url）
  exploreCategories: (source, page = 1) =>
    request('GET', `/explore/categories?${query({ source, page })}`),
  // 发现列表：url 必须是 exploreCategories 返回的分类 URL
  explore: (url, page = 1) => request('GET', `/explore?${query({ url, page })}`),
  // 详情/目录都接受搜索结果的 seed：详情规则为空时由后端沿用，避免“错层”源全空
  bookInfo: (source, bookUrl, seed = {}) =>
    request(
      'GET',
      `/book/info?${query({
        source,
        book_url: bookUrl,
        name: seed.name,
        author: seed.author,
        intro: seed.intro,
        cover_url: seed.coverUrl,
      })}`
    ),
  // 按链接打开：粘贴书源可匿名访问的详情页 URL，导入后返回 book_url 可直接进阅读页
  importByUrl: (sourceId, url) =>
    request('POST', '/book/import-url', { source_id: sourceId, url }, { timeout: 45000 }),
  // 目录标题同样支持替换规则开关（缺省沿用账号设置）
  toc: (source, bookUrl, seed = {}, useReplace = null) =>
    request(
      'GET',
      `/book/toc?${query({
        source,
        book_url: bookUrl,
        name: seed.name,
        author: seed.author,
        intro: seed.intro,
        cover_url: seed.coverUrl,
        use_replace: useReplace === null || useReplace === undefined ? undefined : useReplace ? '1' : '0',
      })}`
    ),
  // 正文：正常总带 book_url（书名圈定的替换规则才生效）；后端按 缓存 → 参数 → book_chapters 三级回填，
  // 拿不到才回 400 {"error":"missing_book_url"}
  // useReplace: true/false 显式覆盖账号的替换规则开关；null/undefined 沿用账号设置
  content: (source, chapterUrl, bookUrl = '', title = '', useReplace = null) =>
    request(
      'GET',
      `/book/content?${query({
        source,
        chapter_url: chapterUrl,
        book_url: bookUrl,
        title,
        use_replace: useReplace === null || useReplace === undefined ? undefined : useReplace ? '1' : '0',
      })}`
    ),
  // ---- 漫画 ----
  // 轻量探测书源类型：comic=true 时进入漫画阅读器
  comicSource: (source, bookUrl = '') =>
    request('GET', `/comic/source?${query({ source, book_url: bookUrl })}`),
  // 章节图片列表：chapterUrl 优先；index 为 1-based 目录序号（与 /book/toc 数组一致）
  comicChapter: (source, bookUrl, chapterUrl = '', index) =>
    request(
      'GET',
      `/comic/chapter?${query({ source, book_url: bookUrl, chapter_url: chapterUrl, index })}`,
      undefined,
      { timeout: 60000 }
    ),
  // 单图代理（Bearer + Blob），返回 objectUrl/bytes/cache(width,height)
  comicImage: (source, url, chapterUrl = '', bookUrl = '') =>
    fetchComicImage(source, url, chapterUrl, bookUrl),
  switchSource: (name, author = '', excludeSource = '') =>
    request('POST', '/book/switch-source', {
      name,
      author: author || undefined,
      exclude_source: excludeSource || undefined,
    }),
  // 书签：position 与阅读进度同一套 0..1 章内比例
  bookmarks: (bookId) => request('GET', `/bookmarks?${query({ book_id: bookId })}`),
  addBookmark: (b) => request('POST', '/bookmarks', b),
  deleteBookmark: (id) => request('DELETE', `/bookmarks/delete?id=${encodeURIComponent(id)}`),
  // 书内搜索：后端只检索已缓存正文，5s 预算，超时返回 504
  bookSearch: (bookId, q, limit = 20) =>
    request('GET', `/book/search?${query({ book_id: bookId, q, limit })}`, undefined, { timeout: 30000 }),
  // 整本缓存可能跑几十秒，单独放宽超时；limited=true 表示还有章节未处理
  cacheAll: (bookUrl, limit = 200, source = '') =>
    request(
      'POST',
      '/book/cache-all',
      { book_url: bookUrl, ...(source ? { source } : {}), limit },
      { timeout: 180000 }
    ),
  // 传空表示清理该用户全部正文缓存
  cacheClear: (bookId = '') =>
    request('POST', '/book/cache/clear', bookId ? { book_id: bookId } : {}),

  // ---- 导出 TXT（只读缓存，不发起抓取）----
  // 导出前计划：total/cached/missing/cached_bytes/complete/export_url
  exportInfo: (bookUrl) =>
    request(
      'GET',
      `/book/export/info?${query({ book_url: bookUrl })}`,
      undefined,
      { timeout: 20000, noAuthReload: true }
    ),
  // 导出正文：fetch + Bearer + Blob，返回响应头供对账（见 downloadTxt）
  exportTxt: (bookUrl) => downloadTxt(bookUrl),

  // ---- bookshelf ----
  bookshelf: (group = '') => request('GET', `/bookshelf?${query({ group })}`),
  addToShelf: (b) => request('POST', '/bookshelf', b),
  removeFromShelf: (bookId) => request('DELETE', `/bookshelf?book_id=${encodeURIComponent(bookId)}`),
  moveToGroup: (bookIds, group) =>
    request('POST', '/bookshelf/group', {
      ...(Array.isArray(bookIds) ? { book_ids: bookIds } : { book_id: bookIds }),
      group: group || '',
    }),
  // 批量操作：action=remove/set_group/remove_group/set_update；返回逐项结果与 updated/failed
  shelfBatch: (action, bookIds, opts = {}) =>
    request('POST', '/bookshelf/batch', {
      action,
      book_ids: Array.isArray(bookIds) ? bookIds : [bookIds],
      ...(opts.group !== undefined ? { group: opts.group || '' } : {}),
      ...(opts.enabled !== undefined ? { enabled: !!opts.enabled } : {}),
    }),
  // 打开章节后清除新章节角标
  markShelfSeen: (bookId) => request('POST', '/bookshelf/seen', { book_id: bookId }),
  // 批量刷新目录：返回每本的 before/after/added/removed 与逐本状态
  tocBatch: (bookUrls, source = '') =>
    request(
      'POST',
      '/book/toc/batch',
      { book_urls: Array.isArray(bookUrls) ? bookUrls : [bookUrls], ...(source ? { source } : {}) },
      { timeout: 180000 }
    ),
  shelfStats: () => request('GET', '/bookshelf/stats'),
  progress: (bookId) => request('GET', `/bookshelf/progress?book_id=${encodeURIComponent(bookId)}`),
  // keepalive=true 用于 pagehide / visibilitychange 时的兜底上报
  setProgress: (p, keepalive = false) => request('POST', '/bookshelf/progress', p, { keepalive }),
  groups: () => request('GET', '/bookshelf/groups'),
  saveGroup: (g) => request('POST', '/bookshelf/groups', g),
  addGroup: (nameOrGroup) =>
    request('POST', '/bookshelf/groups', typeof nameOrGroup === 'string' ? { name: nameOrGroup } : nameOrGroup),
  deleteGroup: (id) => request('DELETE', `/bookshelf/groups?id=${encodeURIComponent(id)}`),

  // ---- replace rules + settings ----
  replaceRules: () => request('GET', '/replace-rules'),
  saveReplaceRule: (r) => request('POST', '/replace-rules', r),
  deleteReplaceRule: (id) => request('DELETE', `/replace-rules/delete?id=${id}`),
  // Legado 规则包导入（裸数组 / {name,rules} / 单条对象），返回 {ok,inserted,skipped}
  importReplaceRules: (body) => request('POST', '/replace-rules/import', body),
  // 导出 Legado 原生 camelCase：缺省全部（数组），id 指定时单条（对象）
  exportReplaceRules: (id) =>
    request('GET', `/replace-rules/export${id ? `?id=${id}` : ''}`),
  // 试跑预览：返回 {final, steps[{index,hits,applied,skipped_reason,output}]}
  previewReplaceRules: (body) =>
    request('POST', '/replace-rules/preview', body, { timeout: 30000 }),
  settings: () => request('GET', '/settings'),
  saveSettings: (s) => request('POST', '/settings', s),

  // ---- stats ----
  recordReading: (p, keepalive = false) => request('POST', '/stats/reading', p, { keepalive }),
  statsOverview: (days = 7) => request('GET', `/stats/overview?${query({ days })}`),
  // /api/healthz：进程指标 + 当前用户正文缓存用量（只读）
  healthz: () => request('GET', '/healthz'),
};
