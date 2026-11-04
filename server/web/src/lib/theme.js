// 显示主题：default / eink。
// 渲染器（/k 还是 SPA）只由服务端按 UA 决定，前端不再有“设备”概念与用户开关。
// 墨水屏主题只是配色后门：?theme=eink（可写 Cookie boranlib_theme 便于刷新保持）。
// 同时把视口宽度写到 html[data-vp] / data-viewport，供 layout.css 的响应式断点使用。

const COOKIE = 'boranlib_theme';
const COOKIE_DAYS = 30;

function readCookie(name) {
  const parts = String(document.cookie || '').split(';');
  for (const p of parts) {
    const s = p.trim();
    const i = s.indexOf('=');
    if (i < 0) continue;
    if (s.slice(0, i) === name) return decodeURIComponent(s.slice(i + 1));
  }
  return '';
}

function writeCookie(name, value, days) {
  document.cookie =
    name + '=' + encodeURIComponent(value) + '; Path=/; Max-Age=' + days * 86400 + '; SameSite=Lax';
}

function clearCookie(name) {
  document.cookie = name + '=; Path=/; Max-Age=0; SameSite=Lax';
}

function docWidth() {
  return document.documentElement.clientWidth || window.innerWidth || 1024;
}

function viewportClass(w) {
  if (w <= 680) return 'sm';
  if (w <= 1100) return 'md';
  return 'lg';
}

// 优先级：?theme=eink/default > Cookie boranlib_theme > 默认
function resolveTheme() {
  let q = '';
  try {
    q = new URLSearchParams(location.search).get('theme') || '';
  } catch {
    q = '';
  }
  if (q === 'eink') {
    writeCookie(COOKIE, 'eink', COOKIE_DAYS);
    return 'eink';
  }
  if (q === 'default') {
    clearCookie(COOKIE);
    return 'default';
  }
  return readCookie(COOKIE) === 'eink' ? 'eink' : 'default';
}

export function applyTheme() {
  const theme = resolveTheme();
  const root = document.documentElement;
  const width = docWidth();
  root.setAttribute('data-theme', theme);
  root.setAttribute('data-vp', viewportClass(width));
  root.setAttribute('data-viewport', String(width));
  return theme;
}

let started = false;

export function initTheme() {
  applyTheme();
  if (started) return;
  started = true;
  window.addEventListener('resize', () => applyTheme());
  window.addEventListener('orientationchange', () => {
    setTimeout(() => applyTheme(), 300);
  });
  if (window.matchMedia) {
    try {
      const q = window.matchMedia('(min-width: 681px)');
      q.addEventListener('change', () => applyTheme());
    } catch {
      /* matchMedia 不可用时只靠 resize */
    }
  }
}
