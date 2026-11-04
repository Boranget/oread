// 页宽 / 字体族：服务端 user_settings 为准，localStorage 只做离线兜底。
// 后端枚举：font_family 接受 serif/sans/sans-serif/kai/kaiti/song/songti/hei/heiti/mono/system；
// page_width 为 CSS px，范围 320–2000（默认 720）。
import { api, getToken, parseJwt } from './api.js';

export const PAGE_WIDTH_PX = {
  narrow: 560,
  normal: 720,
  wide: 900,
  // 满屏写 2000（后端上限），阅读器按 0 宽度（不限制）渲染
  full: 2000,
};

export const WIDTH_OPTIONS = [
  { id: 'narrow', label: '窄', px: PAGE_WIDTH_PX.narrow },
  { id: 'normal', label: '中', px: PAGE_WIDTH_PX.normal },
  { id: 'wide', label: '宽', px: PAGE_WIDTH_PX.wide },
  { id: 'full', label: '满屏', px: PAGE_WIDTH_PX.full },
];

export const FONT_STACKS = {
  serif: "Georgia, 'Songti SC', 'SimSun', serif",
  sans: "system-ui, -apple-system, 'PingFang SC', 'Microsoft YaHei', sans-serif",
  hei: "'Heiti SC', 'SimHei', 'Microsoft YaHei', sans-serif",
  mono: "ui-monospace, 'SFMono-Regular', Consolas, 'Liberation Mono', 'Courier New', monospace",
};

export const FONT_OPTIONS = [
  { id: 'serif', label: '衬线' },
  { id: 'sans', label: '无衬线' },
  { id: 'mono', label: '等宽' },
  { id: 'hei', label: '黑体' },
];

const WIDTH_KEY = 'oread_read_width';
const FONT_KEY = 'oread_font_family';
const MIGRATED_KEY = 'oread_layout_migrated:';
const DEFAULT_WIDTH = 720;

const FONT_ALIASES = {
  serif: 'serif',
  song: 'serif',
  songti: 'serif',
  kai: 'serif',
  kaiti: 'serif',
  sans: 'sans',
  'sans-serif': 'sans',
  system: 'sans',
  hei: 'hei',
  heiti: 'hei',
  mono: 'mono',
};

export function widthIdFromPx(px) {
  const n = Number(px);
  if (!Number.isFinite(n) || n <= 0) return 'normal';
  if (n >= 1600) return 'full';
  if (n >= 860) return 'wide';
  if (n >= 620) return 'normal';
  return 'narrow';
}

export function widthPxFromId(id) {
  return PAGE_WIDTH_PX[id] || DEFAULT_WIDTH;
}

// 满屏用 none，其余按预设 px 限制最大宽度
export function widthCss(id) {
  return id === 'full' ? 'none' : widthPxFromId(id) + 'px';
}

export function fontIdFromServer(v) {
  return FONT_ALIASES[String(v || '').trim()] || 'serif';
}

export function fontServerValue(id) {
  return FONT_STACKS[id] ? id : 'serif';
}

export function readLocalLayout() {
  const widthRaw = localStorage.getItem(WIDTH_KEY) || '';
  const fontRaw = localStorage.getItem(FONT_KEY) || '';
  return {
    widthRaw,
    fontRaw,
    width: WIDTH_OPTIONS.some((w) => w.id === widthRaw) ? widthRaw : 'normal',
    fontFamily: fontIdFromServer(fontRaw || 'serif'),
  };
}

export function writeLocalLayout(width, fontFamily) {
  try {
    localStorage.setItem(WIDTH_KEY, width);
    localStorage.setItem(FONT_KEY, fontFamily);
  } catch {
    /* 本地存储不可用时忽略 */
  }
}

function userScope() {
  const claims = parseJwt(getToken());
  return String(claims.username || claims.sub || 'guest');
}

// 首次读取设置时迁移旧的本机值：
// - 仅当服务端仍是默认值（page_width=800、font_family=serif）才写服务端，避免覆盖用户已改过的设置；
// - 迁移成功后写一次性标记；离线失败则保留本地值，下次再试。
export async function syncLayoutSettings(server) {
  const local = readLocalLayout();
  const scope = userScope();
  const flagKey = MIGRATED_KEY + scope;
  let width = widthIdFromPx(server && server.page_width);
  let fontFamily = fontIdFromServer(server && server.font_family);
  let migrationFailed = false;

  if (!localStorage.getItem(flagKey)) {
    const patch = {};
    if (width === 'normal' && local.widthRaw && local.width !== 'normal') {
      patch.page_width = widthPxFromId(local.width);
    }
    if (fontIdFromServer(server && server.font_family) === 'serif' && local.fontRaw && local.fontFamily !== 'serif') {
      patch.font_family = fontServerValue(local.fontFamily);
    }

    if (!Object.keys(patch).length) {
      localStorage.setItem(flagKey, '1');
    } else {
      try {
        await api.saveSettings(patch);
        localStorage.setItem(flagKey, '1');
        if (patch.page_width) width = local.width;
        if (patch.font_family) fontFamily = local.fontFamily;
      } catch {
        // 服务端不可达：本次继续用本机值，迁移标记留空以便下次重试
        migrationFailed = true;
      }
    }
  }

  // 迁移失败时不要把本机值改写成服务端默认值，否则离线兜底就丢了
  if (!migrationFailed) writeLocalLayout(width, fontFamily);
  return { width, fontFamily };
}
