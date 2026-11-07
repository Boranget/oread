// 后端错误 -> 用户文案。纯函数，便于 node --test 覆盖各错误形态。

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

export function shortText(s) {
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
export function errorText(status, data) {
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
