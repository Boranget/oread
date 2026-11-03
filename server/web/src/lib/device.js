// 设备显示模式：自动 / 桌面 / 手机 / 墨水屏
// 结果写到 <html data-device / data-device-mode / data-vp / data-viewport>，
// 断点与三端样式集中在 src/lib/layout.css，组件里不再散落 @media。
import { writable } from 'svelte/store';

export const DEVICE_MODES = ['auto', 'desktop', 'mobile', 'eink'];
export const deviceLabels = {
  auto: '自动',
  desktop: '桌面',
  mobile: '手机',
  eink: '墨水屏',
};

const KEY = 'oread_device_mode';

function readPref() {
  try {
    const v = localStorage.getItem(KEY);
    return DEVICE_MODES.indexOf(v) >= 0 ? v : 'auto';
  } catch {
    return 'auto';
  }
}

export const devicePref = writable(readPref());
export const deviceKind = writable('desktop');

function docWidth() {
  return document.documentElement.clientWidth || window.innerWidth || 1024;
}

function mq(q) {
  if (!window.matchMedia) return false;
  try {
    const m = window.matchMedia(q);
    return !!(m && m.matches);
  } catch {
    return false;
  }
}

// 自动判断：单色/慢刷新屏幕优先识别为墨水屏，其次按视口宽度分手机/桌面
function resolveKind(pref) {
  if (pref !== 'auto') return pref;
  if (mq('(monochrome)') || mq('(update: slow)')) return 'eink';
  return docWidth() <= 680 ? 'mobile' : 'desktop';
}

function viewportClass(w) {
  if (w <= 680) return 'sm';
  if (w <= 1100) return 'md';
  return 'lg';
}

export function applyDevice(pref) {
  const p = DEVICE_MODES.indexOf(pref) >= 0 ? pref : readPref();
  const root = document.documentElement;
  const kind = resolveKind(p);
  const width = docWidth();
  root.setAttribute('data-device', kind);
  root.setAttribute('data-device-mode', p);
  root.setAttribute('data-vp', viewportClass(width));
  root.setAttribute('data-viewport', String(width));
  deviceKind.set(kind);
  return kind;
}

export function setDeviceMode(mode) {
  if (DEVICE_MODES.indexOf(mode) < 0) return;
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    /* 本地存储不可用时只作用于当前会话 */
  }
  devicePref.set(mode);
  applyDevice(mode);
}

let started = false;

export function initDevice() {
  applyDevice();
  if (started) return;
  started = true;
  window.addEventListener('resize', () => applyDevice());
  window.addEventListener('orientationchange', () => {
    setTimeout(() => applyDevice(), 300);
  });
  if (window.matchMedia) {
    try {
      const q = window.matchMedia('(min-width: 681px)');
      const onChange = () => applyDevice();
      q.addEventListener('change', onChange);
    } catch {
      /* matchMedia 不可用时只靠 resize */
    }
  }
}
