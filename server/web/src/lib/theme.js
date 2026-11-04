// 显示主题：light / dark / sepia / eink。
// 渲染器（/k 还是 SPA）只由服务端按 UA 决定；主题是用户级偏好，唯一真相源是账号设置
// （GET/POST /api/settings 的 theme 字段），不另存 Cookie。
// ?theme=eink 仅作直达别名：进入时写入偏好，之后刷新不再依赖 query。
// 同时把视口宽度写到 html[data-vp] / data-viewport，供 layout.css 的响应式断点使用。

import { api, getToken } from './api.js';

export const THEMES = ['light', 'dark', 'sepia', 'eink'];
export const THEME_LABELS = { light: '浅色', dark: '深色', sepia: '护眼', eink: '墨水屏' };

let current = 'light';

function docWidth() {
  return document.documentElement.clientWidth || window.innerWidth || 1024;
}

function viewportClass(w) {
  if (w <= 680) return 'sm';
  if (w <= 1100) return 'md';
  return 'lg';
}

function paintViewport() {
  const width = docWidth();
  const root = document.documentElement;
  root.setAttribute('data-vp', viewportClass(width));
  root.setAttribute('data-viewport', String(width));
}

export function currentTheme() {
  return current;
}

export function applyTheme(theme) {
  const t = THEMES.includes(theme) ? theme : 'light';
  current = t;
  document.documentElement.setAttribute('data-theme', t);
  paintViewport();
  return t;
}

function queryTheme() {
  try {
    const v = new URLSearchParams(location.search).get('theme') || '';
    return THEMES.includes(v) ? v : '';
  } catch {
    return '';
  }
}

// 启动：先把 ?theme 别名即时生效（未登录时也能预览），再补视口信息。
export function initTheme() {
  applyTheme(queryTheme() || 'light');
  window.addEventListener('resize', () => paintViewport());
  window.addEventListener('orientationchange', () => setTimeout(paintViewport, 300));
}

// 登录后/进入应用后调用：从账号设置读取主题；若带 ?theme 则写回偏好，之后只认账号设置。
export async function syncTheme() {
  let saved = 'light';
  const alias = queryTheme();
  if (getToken()) {
    try {
      const s = await api.settings();
      if (s && THEMES.includes(s.theme)) saved = s.theme;
    } catch {
      /* 设置接口不可用时保持默认浅色 */
    }
    if (alias && alias !== saved) {
      try {
        await api.saveSettings({ theme: alias });
        saved = alias;
      } catch {
        /* 写入失败仍按别名先展示，下次进入以账号设置为准 */
        saved = alias;
      }
    }
  } else if (alias) {
    saved = alias;
  }
  applyTheme(saved);
  // 别名用过一次后从地址栏移除，避免地址与偏好长期双写
  if (alias) {
    try {
      const u = new URL(location.href);
      u.searchParams.delete('theme');
      history.replaceState(null, '', u.toString());
    } catch {
      /* 忽略 */
    }
  }
  return saved;
}

// 设置页/阅读器切换主题：立即生效并持久化到账号设置。
export async function setTheme(theme) {
  const t = applyTheme(theme);
  if (!getToken()) return t;
  try {
    await api.saveSettings({ theme: t });
  } catch {
    /* 离线时保留本机展示，后续保存设置会再次同步 */
  }
  return t;
}
