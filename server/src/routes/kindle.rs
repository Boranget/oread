//! Kindle 极简阅读端 `/k`：面向 KPW3 真机（视口 1072×1268、无 flex/grid、无现代 JS）。
//!
//! 真机硬约束（来自 KPW3 上报 JSON + 照片）：
//! - `innerWidth×innerHeight = 1072×1268`，且 `innerWidth == screen.width`：
//!   浏览器没有按 DPR 1.7964 缩放，**1 CSS px ≈ 1 物理 px**，字号必须按 px 直接给。
//! - 版心 1000px（左右各留 36px）；顶栏 73px（72 内容 + 1px 线）。
//! - 文档高不再取 `100%`/`vh`：`K_FIT_JS` 在 `<head>` 里按实测
//!   `documentElement.clientHeight − SAFE_BOTTOM(24)` 写死（1268 → 1244）。
//!   这样即使老引擎在「文档高 == 视口高」时也画滚动条、或 URL 里的 `vh` 比真实
//!   布局视口大，文档也永远比视口矮 24px，不会出现第二屏；底栏
//!   `position:fixed;bottom:0`（总高 88 = 64 内容 + 24 安全区）压在这 24px 上，
//!   视觉与原来一致。正文区仍然由 `#k-stage` 的 `top/bottom` 贴视口，不读任何参数。
//! - 只使用 table / block / inline-block + 服务端算好的像素；不用 flex/grid/vw/vh/calc/rem/CSS 变量，
//!   不用 `border-radius`；底栏用 `position:fixed`，不用 transform/transition。
//! - 三个内联脚本：文档高兜底（`K_FIT_JS`，不重载）、视口补参（`K_VP_FIX_JS`，ES5、
//!   最多重载一次）、自报探针（`K_PROBE_JS`，不重载）；其余翻页/切换字号全部是真实链接或原生 form。
//! - 分页按字符切（`char_indices`），同一章同一档字号的页码恒定，与渲染无关。
//! - 登录：两个字段的 table 表单 + HttpOnly Cookie（`k_session`，30 天）；字号存 `k_fs` Cookie。

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    routing::{get, post},
    Form, Router,
};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use sqlx::SqlitePool;

/// axum 0.7 里 `http::Response` 是结构体（无默认泛型），本地起个别名省得到处写 <Body>
type Response = axum::http::Response<Body>;

use crate::auth::{self, AuthUser, UserDb};
use crate::entities::{Book, BookChapter, BookContent, BookImage, BookSource};
use crate::error::{AppError, AppResult};
use crate::models::BookshelfItem;
use crate::routes::books::{
    load_book_meta, load_cached_chapters, load_cached_content, save_book_meta, save_chapters,
    save_content_cached,
};
use crate::routes::bookshelf::SHELF_LIST_SQL;
use crate::routes::image;
use crate::routes::replace;
use crate::routes::sources::{load_source, load_sources};
use crate::routes::AppState;
use crate::web_book;

// ---------------- 真机版心常量 ----------------

/// 版心左右留白
const SIDE_PAD: usize = 36;
/// 视口宽 / 高兜底（请求未带 `vw`/`vh` 时使用；可用环境变量覆盖）
const K_W: usize = 1072;
const K_H: usize = 1268;
/// 顶栏高度：72px 内容 + 1px 下边线 = 73
const NAV_H: usize = 72;
/// 底部固定栏高度（触控目标 64px，保守高于 44px 下限）
const BAR_H: usize = 64;
/// 底部安全余量：真机浏览器底栏/滚动条会占掉几到十几像素。
/// 页面按 `vh` 精确填满会顶出纵向滚动条、把贴底栏裁掉，这里统一预留 24px。
/// 代价是 1072×1268 下小档/标准档各少 1 行（24→23、21→20），大档/超小档行数不变。
const SAFE_BOTTOM: usize = 24;
/// 阅读页顶栏高：56px。够大（≥48 触控热区，整格可点），又比列表页的 73px 顶栏少 17px，
/// 四档字号只各让出 1–2 行；顶栏固定贴视口顶，正文区从它下沿开始。
const READ_NAV_H: usize = 56;
/// 顶栏左格「← 书架」宽（160px，两字 + 箭头 + 分隔留白）
const READ_NAV_BACK_W: usize = 160;
/// 顶栏右格「目录」宽（120px）
const READ_NAV_TOC_W: usize = 120;
/// 阅读页正文顶部留白：与左右版心一致 36px（此前贴顶 10px，视觉上像被裁切）
const READ_PAD_TOP: usize = 36;
/// 正文底部页码行：28px 行高 + 上下各 14px = 56
const READ_PAGENO_H: usize = 56;

/// 请求级视口（CSS px）。几何全部由它推导，不再写死 1000/1268；
/// 优先级：请求参数 `vw`/`vh` > 环境变量默认（见 `Config.kindle_vw/vh`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Vp {
    w: usize,
    h: usize,
    /// 请求是否显式带了 `vw`。显式时 viewport 锁 `width={w}`（真机 1:1 不缩放）；
    /// 缺参时用 `width=device-width`，让补参脚本能量出真实 CSS 宽度。
    fixed: bool,
}

impl Vp {
    /// 缺参/越界时回落到默认视口，保证任何尺寸都不会算出负高度。
    fn resolve(vw: Option<i64>, vh: Option<i64>, d: (usize, usize)) -> Self {
        let ok = |v: Option<i64>| v.filter(|n| (480..=2400).contains(n)).map(|n| n as usize);
        Self {
            w: ok(vw).unwrap_or(d.0),
            h: ok(vh).unwrap_or(d.1),
            fixed: ok(vw).is_some(),
        }
    }
    /// 版心宽 = 视口宽 − 左右留白 ×2（1072 → 1000）
    fn page_w(self) -> usize {
        self.w.saturating_sub(SIDE_PAD * 2)
    }
    /// 漫画图宽 = 版心宽 − 左右留白 ×2（1072 → 928，即切片版心宽 dw）
    fn content_w(self) -> usize {
        self.w.saturating_sub(SIDE_PAD * 4)
    }
    /// 页面实际高度 = 视口高 − 底部安全余量（1268 → 1244）。
    /// 真机浏览器会留底栏/滚动条等几到十几像素，按 vh 精确填满必然顶出滚动条。
    fn layout_h(self) -> usize {
        self.h.saturating_sub(SAFE_BOTTOM)
    }
    /// 阅读页正文容器高 = 页面高 − 底栏（1268 → 1180）
    fn body_h(self) -> usize {
        self.layout_h().saturating_sub(BAR_H)
    }
    /// 带顶栏页面的正文区高 = 页面高 − 顶栏 − 1（1268 → 1171）
    fn nav_h(self) -> usize {
        self.layout_h().saturating_sub(NAV_H + 1)
    }
    /// 小说正文可用高 = 正文容器 − 顶栏 − 顶部留白 − 页码行（1268 → 1032）
    fn text_h(self) -> usize {
        self.body_h()
            .saturating_sub(READ_NAV_H + READ_PAD_TOP + READ_PAGENO_H)
    }
    /// 漫画整图/单片可用高 = 页面高 − 底栏 − 8（1268 → 1172，即切片可用高 vh）
    fn content_h(self) -> usize {
        self.layout_h().saturating_sub(BAR_H + 8)
    }
    /// 左右翻页热区宽 = 视口宽 / 3（1072 → 357，CSS 用 33% 自适应）
    #[allow(dead_code)] // 仅单测引用：渲染改用 CSS 百分比，不再需要服务端算像素
    fn hot_w(self) -> usize {
        self.w / 3
    }
    /// 书架每页本数与行高：优先保持 10 本/页，把可用高（`nav_h − 底栏`）等分给 10 行，
    /// 行高上限 112；视口极矮（行高不足 56）才减少每页本数，保证列表永不超过 stage。
    fn shelf_layout(self) -> (usize, usize) {
        let avail = self.nav_h().saturating_sub(BAR_H);
        let row_h = (avail / K_SHELF_PAGE).clamp(56, 112);
        let per_page = if avail / K_SHELF_PAGE >= 56 {
            K_SHELF_PAGE
        } else {
            (avail / 56).clamp(1, K_SHELF_PAGE)
        };
        (per_page, row_h)
    }
}

impl Default for Vp {
    fn default() -> Self {
        Self {
            w: K_W,
            h: K_H,
            fixed: true,
        }
    }
}

/// 阅读页每页行数预算 = 可用高 ÷ 行高（字号四档 28/32/38/20 对应 46/52/62/36）。
/// 动态算出，不再有 24/21/17/25 这类硬编码常量。
fn line_budget(vp: Vp, line: usize) -> usize {
    (vp.text_h() / line.max(1)).max(1)
}
/// 段间距（行）：WeRead 实测段距 0.6 行（默认档 27px）
/// 实际排版用 `para_gap_px`（整数 px）；本常量保留为换算基准，仅单测引用。
#[allow(dead_code)]
const PARA_GAP_LINES: f64 = 0.6;
/// 段间距像素（向下取整，保证渲染高度不超过行预算）
const fn para_gap_px(line: usize) -> usize {
    (line * 6) / 10
}
/// 目录每页章节数 = 每列行数 × 2；每列行数按可用高实算（72px/行，上限 15 行/列）。
fn toc_page_size(vp: Vp) -> usize {
    ((vp.nav_h().saturating_sub(BAR_H)) / 72).clamp(1, K_TOC_PAGE / K_TOC_COLS) * K_TOC_COLS
}
/// 搜索结果每页条数 = （正文区 − 表单 88 − 翻页条 64）/ 112；有回退说明/升级按钮时再少一条。
fn search_page_size(vp: Vp, extra_row: bool) -> usize {
    let base = ((vp.nav_h().saturating_sub(BAR_H + 88)) / 112).clamp(1, K_SEARCH_PAGE);
    if extra_row {
        base.saturating_sub(1).max(1)
    } else {
        base
    }
}
/// 目录每页章节数上限：两列各 15 行 × 72px = 1080（实际行数按可用高实算）
const K_TOC_PAGE: usize = 30;
/// 目录每列行数
const K_TOC_COLS: usize = 2;
/// 书架每页本数上限：10 行 × 112px = 1120（实际行高按可用高等分，见 `Vp::shelf_layout`）
const K_SHELF_PAGE: usize = 10;
/// 搜索结果每页条数上限：9 行 × 112px = 1008（实际条数按可用高实算）
const K_SEARCH_PAGE: usize = 9;
/// /k 渲染前对正文纯文本的硬截断（防御超大章节把分页临时索引撑爆内存）
const K_MAX_TEXT_CHARS: usize = 800_000;
/// 超长章节分块：每块最多 20 万字符，按块分页，避免一次性按 80 万字构建全量页表
const K_CHUNK_CHARS: usize = 200_000;
/// 字号 Cookie（与 `?fs=` 二选一，显式参数优先）
const K_FS_COOKIE: &str = "k_fs";

/// 视口补参脚本（ES5，≤420 字节）：URL 缺 `vw`/`vh` 或与真实布局视口不符时，
/// 用 `location.replace` 补参并重载一次；`kfix=1` 是防循环标记，最多重载一次。
/// 高一律取 `documentElement.clientHeight`（真正参与排版、决定滚动存不存在的布局视口高；
/// Kindle Silk 的 `innerHeight` 可能报整屏高，比布局视口大，不能作为排版依据），取不到才退回 `innerHeight`。
/// 宽仍用 `innerWidth`（含滚动条占位），避免横滚条把宽度算窄后反复补参。
/// 服务端渲染的翻页/换章链接已带参，正常翻页不会触发重载。
const K_VP_FIX_JS: &str = "(function(){var s=location.search.replace(/^\\?/,''),p=s?s.split('&'):[],o=[],vw=null,vh=null,f=0;\
for(var i=0;i<p.length;i++){var k=p[i].split('=')[0];\
if(k=='vw')vw=p[i].substring(3);else if(k=='vh')vh=p[i].substring(3);else if(k=='kfix')f=1;else if(p[i])o.push(p[i]);}\
var d=document.documentElement,H=innerHeight;\
if(d&&d.clientHeight>0)H=d.clientHeight;\
if(!f&&innerWidth>0&&H>0&&(vw!=innerWidth||vh!=H)){o.push('vw='+innerWidth,'vh='+H,'kfix=1');\
location.replace(location.pathname+'?'+o.join('&'));}})()";

/// 自报探针（ES5，零依赖，不重载页面）：`load` 后 200ms 用 `XMLHttpRequest`
/// 把「URL 参数视口 + 真实量到的视口/文档尺寸」回传 `/api/k/probe`。
/// `innerWidth/innerHeight` 与 `documentElement.clientWidth/clientHeight` 同时上报，
/// 用来分辨「设备报的窗口高」和「真正参与排版的布局视口高」；失败静默。
/// v2 追加 `screen_w/screen_h/avail_h/outer_w/outer_h/body_h/stage_h`：
/// 若 `screen_h`/`avail_h` 明显大于 `client_h`，说明 Silk 的可视视口被系统栏或工具栏
/// 遮住了一部分（此时即使文档高 = `clientHeight` 仍会露滚动条），量不到的数一律填 `-`。
const K_PROBE_JS: &str = "(function(){function p(n){var m=new RegExp('[?&]'+n+'=([^&]*)').exec(location.search);return m?m[1]:''}\
function v(x){return typeof x=='number'?x:'-'}\
function s(){try{var d=document.documentElement,dpr=window.devicePixelRatio||1,sn=window.screen||{},b=document.body,\
st=document.getElementById('k-stage');\
var q='vw_param='+p('vw')+'&vh_param='+p('vh')+'&inner_w='+innerWidth+'&inner_h='+innerHeight+\
'&client_w='+d.clientWidth+'&client_h='+d.clientHeight+'&scroll_w='+d.scrollWidth+'&scroll_h='+d.scrollHeight+\
'&dpr='+dpr+'&fs='+p('fs')+'&path='+encodeURIComponent(location.pathname)+\
'&screen_w='+v(sn.width)+'&screen_h='+v(sn.height)+'&avail_h='+v(sn.availHeight)+\
'&outer_w='+v(window.outerWidth)+'&outer_h='+v(window.outerHeight)+\
'&body_h='+v(b&&b.clientHeight)+'&stage_h='+v(st&&st.clientHeight);\
var x=new XMLHttpRequest();x.open('GET','/api/k/probe?'+q,true);x.send(null)}catch(e){}}\
if(window.addEventListener){window.addEventListener('load',function(){setTimeout(s,200)},false)}\
else if(window.attachEvent){window.attachEvent('onload',function(){setTimeout(s,200)})}})()";

/// 文档高兜底脚本（ES5，零依赖，不重载，注入 `<head>` 保证首帧生效）：
/// 把 `html` 的实测高度固定为 `documentElement.clientHeight − SAFE_BOTTOM`。
/// 真机问题是「文档高恰好等于布局视口高」时 Silk 仍绘制滚动条，以及 URL 里的 `vh`
/// 可能比真实布局视口大；这里以浏览器实测值为准、并主动让文档比视口矮 24px，
/// 于是无论参数是否准确，文档都不可能产生第二屏。取不到 `clientHeight` 时不改，
/// 退回 CSS 的 `height:100%`。
fn fit_js() -> String {
    format!(
        "(function(){{function f(){{try{{var d=document.documentElement,H=d.clientHeight||innerHeight;\
if(H>{min}){{d.style.height=(H-{safe})+'px'}}}}catch(e){{}}}}f();\
if(window.addEventListener){{window.addEventListener('load',f,false)}}}})()",
        min = 400,
        safe = SAFE_BOTTOM
    )
}

/// 四档字号。每页行数由 `line_budget(vp, line)` 按视口高动态算出。
struct FsSpec {
    size: usize,
    line: usize,
    label: &'static str,
}

const FS_SPECS: [FsSpec; 4] = [
    // 927/28 ≈ 33 字/行；行数 = 正文可用高 ÷ 46。
    FsSpec {
        size: 28,
        line: 46,
        label: "小",
    },
    // 字号口径：真机实测 32px/52px，保留 32px。此处显式写死 32，且全局
    // `-webkit-text-size-adjust:none` 已关闭老引擎的字号自动放大——实测 32/52，勿低于 30。
    FsSpec {
        size: 32,
        line: 52,
        label: "标准",
    },
    FsSpec {
        size: 38,
        line: 62,
        label: "大",
    },
    FsSpec {
        size: 20,
        line: 36,
        label: "超小",
    },
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/k", get(shelf))
        .route("/k/toc", get(toc))
        .route("/k/read", get(read))
        .route("/k/search", get(search))
        .route("/k/progress", post(progress))
        .route("/k/_selftest", get(selftest))
        .route("/k/login", get(login_form).post(login_post))
        .route("/k/logout", get(logout))
}

// ---------------- 入口门禁（Kindle UA → /k） ----------------

/// Kindle/Silk 判定只看 HTTP 头：真机 UA 是 `AppleWebKit/531.2+ ... Kindle/3.0+`，
/// 页面内 JS 的 UA 是 `534.26+` 且不含 Kindle 字样，所以不能在页面里判。
pub(crate) fn is_kindle_ua(ua: &str) -> bool {
    let ua = ua.to_ascii_lowercase();
    ua.contains("kindle") || ua.contains("silk")
}

/// 现代 SPA 的 HTML 入口：`/`、`/index.html`，以及未来可能新增的无扩展名前端路由。
/// `/api`、`/k`、健康检查、静态资源（末段带扩展名）都不在此列。
fn is_spa_entry(path: &str) -> bool {
    if path.starts_with("/api") || path.starts_with("/k") || path.starts_with("/healthz") {
        return false;
    }
    if path == "/" || path == "/index.html" {
        return true;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    !last.contains('.')
}

/// 入口门禁中间件：Kindle/Silk 访问 `/`（及 SPA 路由入口）时 303 到 `/k`。
/// 其他 UA 行为不变，`/k` 自身不再跳；端口无关（3000/3050 同一份代码）。
pub async fn entry_redirect(req: Request, next: Next) -> Response {
    let ua = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let navigation = matches!(*req.method(), Method::GET | Method::HEAD);
    if navigation && is_kindle_ua(ua) && is_spa_entry(req.uri().path()) {
        return axum::http::Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(axum::http::header::LOCATION, "/k")
            .body(Body::empty())
            .expect("redirect response");
    }
    next.run(req).await
}

// ---------------- HTML 工具 ----------------

/// HTML 转义（标题/正文等所有用户与书源数据都必须过这里）
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// query 参数百分号编码（book_id / source 都是 URL，必须编码后拼 href）
fn qenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// 给 /k 内部链接补上字号参数（Cookie 之外的第二重保险）
fn with_fs(url: &str, fs: usize) -> String {
    if url.contains('?') {
        format!("{url}&fs={fs}")
    } else {
        format!("{url}?fs={fs}")
    }
}

/// 给链接补上视口参数：链接自传播，客户端翻页零测量、零额外请求。
fn with_vp(url: &str, vp: Vp) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}vw={}&vh={}", vp.w, vp.h)
}

/// /k 内部链接的标配：字号 + 视口参数。
fn qlink(url: &str, fs: usize, vp: Vp) -> String {
    with_vp(&with_fs(url, fs), vp)
}

/// 解析字号：显式 `?fs=1..4` 优先，其次 `k_fs` Cookie，缺省标准档（2）。
/// 档位 1/2/3 为既有小/标准/大，4 为新增超小档。
/// 返回 (档位, 是否需要回写 Cookie)。
fn resolve_fs(headers: &HeaderMap, q: Option<i64>) -> (usize, bool) {
    if let Some(v) = q {
        if (1..=4).contains(&v) {
            return (v as usize, true);
        }
    }
    if let Some(cookie) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for kv in cookie.split(';') {
            if let Some(rest) = kv.trim().strip_prefix(&format!("{K_FS_COOKIE}=")) {
                if let Ok(v) = rest.trim().parse::<i64>() {
                    if (1..=4).contains(&v) {
                        return (v as usize, false);
                    }
                }
            }
        }
    }
    (2, false)
}

/// 字号轮换：按字号从小到大 4→1→2→3，避免“大→超小”以外的乱跳
fn cycle_fs(fs: usize) -> usize {
    match fs {
        4 => 1,
        1 => 2,
        2 => 3,
        _ => 4,
    }
}

/// 显式切换字号时把档位写进 30 天 Cookie
fn remember_fs(res: &mut Response, fs: usize) {
    let value = format!("{K_FS_COOKIE}={fs}; Path=/; Max-Age=2592000; SameSite=Lax");
    if let Ok(v) = HeaderValue::from_str(&value) {
        res.headers_mut().insert(SET_COOKIE, v);
    }
}

/// 统一页面骨架：固定版心、`-webkit-text-size-adjust:none` 防老引擎自动放大。
/// viewport 宽度随请求视口（`vw`），缺参时回落到默认 1072；不写 initial-scale=1
/// 时 Silk 会按 device-width 缩放。
/// 生产渲染走 `page_html_css`（需要页面级 CSS）；本函数为无额外 CSS 的便捷封装，仅单测使用。
#[allow(dead_code)]
fn page_html(title: &str, fs: usize, body: &str, vp: Vp) -> String {
    page_html_css(title, fs, body, "", vp)
}

/// 与 `page_html` 相同，额外拼接一段页面级 CSS（阅读页用「一屏一页」锁高，其它页不受影响）。
fn page_html_css(title: &str, fs: usize, body: &str, extra_css: &str, vp: Vp) -> String {
    let spec = &FS_SPECS[fs - 1];
    // 显式 vw → 锁死版心宽（1:1 不缩放）；缺参 → device-width，交给补参脚本量出真实宽度
    let viewport = if vp.fixed {
        format!(
            "width={},initial-scale=1,maximum-scale=1,user-scalable=0",
            vp.w
        )
    } else {
        "width=device-width,initial-scale=1,maximum-scale=1,user-scalable=0".to_string()
    };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"{viewport}\"><title>{title}</title><style>\
html{{-webkit-text-size-adjust:none;height:100%;overflow:hidden}}\
html,body{{margin:0;padding:0;width:100%;background:#fff;color:#000}}\
html::-webkit-scrollbar,body::-webkit-scrollbar,#k-stage::-webkit-scrollbar{{width:0;height:0;display:none}}\
body{{height:100%;overflow:hidden;font-family:serif;font-size:{size}px;line-height:{line}px}}\
*{{box-sizing:border-box}}\
a{{color:#000;text-decoration:none}}\
h1,h2,h3,p{{margin:0}}\
table{{border-collapse:collapse}}\
.wrap{{width:{pw}px;margin-left:{pad}px}}\
{extra}\
</style><script>{fit}</script></head><body><div class=\"wrap\">{body}</div><script>{js}</script><script>{probe}</script></body></html>",
        viewport = viewport,
        title = esc(title),
        size = spec.size,
        line = spec.line,
        pw = vp.page_w(),
        pad = SIDE_PAD,
        extra = extra_css,
        body = body,
        fit = fit_js(),
        js = K_VP_FIX_JS,
        probe = K_PROBE_JS
    )
}

fn page(title: &str, fs: usize, body: &str, vp: Vp) -> Response {
    page_css(title, fs, body, "", vp)
}

fn page_css(title: &str, fs: usize, body: &str, extra_css: &str, vp: Vp) -> Response {
    let html = page_html_css(title, fs, body, extra_css, vp);
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .expect("html response")
}

/// 顶栏（73px = 72 内容 + 1px 下边线，白底，只有一条）：
/// 左：标题 28px 加粗（可带一段 20px 弱灰副标题）；右：动作链接 20px，间距 32px。
fn top_bar(title: &str, sub: Option<&str>, actions: &str, vp: Vp) -> String {
    let sub = match sub {
        Some(s) => format!(
            " <span style=\"font-size:20px;color:#555;font-weight:400\">{}</span>",
            esc(s)
        ),
        None => String::new(),
    };
    format!(
        "<div id=\"k-nav\" style=\"height:73px;border-bottom:1px solid #ccc;background:#fff;white-space:nowrap;overflow:hidden\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td height=\"72\" align=\"left\" style=\"font-size:28px;line-height:72px;font-weight:700;padding-left:36px;overflow:hidden\">{title}{sub}</td>\
         <td height=\"72\" align=\"right\" style=\"font-size:20px;line-height:72px;padding-right:36px\">{actions}</td>\
         </tr></table></div>",
        pw = vp.page_w(),
        title = esc(title),
        sub = sub,
        actions = actions
    )
}

/// 顶栏动作链接：20px，间距 32px（用内边距，不依赖 flex/grid）
fn nav_link(href: &str, label: &str) -> String {
    format!(
        "<a href=\"{href}\" style=\"padding-left:32px;font-size:20px\">{label}</a>",
        href = href,
        label = esc(label)
    )
}

/// 贴视口底部的固定栏（仅阅读页）：`left:0;right:0;bottom:0`，总高 88 =
/// 上 64 内容（5 格各 200px）+ 下 24 安全区（`padding-bottom`，避免被系统栏/滚动条遮住）。
/// 不再用 `bottom:24px` + 视口宽像素：页面高由 100% 决定，底栏必须贴住真实视口底。
fn fixed_bar(row: &str, vp: Vp) -> String {
    format!(
        "<div id=\"k-bar\" style=\"position:fixed;left:0;right:0;bottom:0;height:{h}px;padding-bottom:{safe}px;background:#fff;z-index:10\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-left:{pad}px;table-layout:fixed\"><tr>{row}</tr></table></div>",
        h = BAR_H + SAFE_BOTTOM,
        safe = SAFE_BOTTOM,
        pw = vp.page_w(),
        pad = SIDE_PAD,
        row = row
    )
}

/// 底栏格内容：粗体字符（28px/700，无边框）+ 标签（20px）
fn bar_inner(icon: &str, label: &str) -> String {
    format!(
        "<div style=\"width:28px;height:28px;margin:0 auto;font-size:28px;line-height:28px;font-weight:700;text-align:center\">{icon}</div>\
         <div style=\"font-size:20px;line-height:20px;margin-top:4px\">{label}</div>",
        icon = esc(icon),
        label = esc(label)
    )
}

fn bar_cell(href: &str, icon: &str, label: &str) -> String {
    format!(
        "<td width=\"20%\" height=\"64\" align=\"center\" valign=\"middle\" style=\"height:63px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
         <a href=\"{href}\" style=\"display:block;height:63px;padding-top:6px\">{inner}</a></td>",
        href = href,
        inner = bar_inner(icon, label)
    )
}

/// 底栏格：禁用态（弱灰、不可点），用于全书末页的「下一页」
fn bar_cell_off(icon: &str, label: &str) -> String {
    format!(
        "<td width=\"20%\" height=\"64\" align=\"center\" valign=\"middle\" style=\"height:63px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
         <span style=\"display:block;height:63px;padding-top:6px;color:#555\">{inner}</span></td>",
        inner = bar_inner(icon, label)
    )
}

/// 列表页翻页条（书架/搜索/目录）：64px 高、两格各占一半，贴 stage 底部（左右跟随 stage）。
/// 禁用态用 `#555` 弱灰且不可点。
fn pager_bar(
    prev: Option<(String, &'static str)>,
    next: Option<(String, &'static str)>,
    vp: Vp,
) -> String {
    let cell = |item: &Option<(String, &'static str)>, fallback: &str| {
        match item {
        Some((href, label)) => format!(
            "<td width=\"50%\" height=\"64\" align=\"center\" valign=\"middle\" style=\"height:63px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
             <a href=\"{href}\" style=\"display:block;height:63px;line-height:63px;font-size:24px\">{label}</a></td>",
            href = href,
            label = esc(label)
        ),
        None => format!(
            "<td width=\"50%\" height=\"64\" align=\"center\" valign=\"middle\" style=\"height:63px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
             <span style=\"display:block;height:63px;line-height:63px;font-size:24px;color:#555\">{label}</span></td>",
            label = fallback
        ),
    }
    };
    format!(
        "<div id=\"k-pager\" style=\"position:absolute;left:0;right:0;bottom:0;height:{h}px;background:#fff;z-index:8\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"table-layout:fixed\"><tr>{p}{n}</tr></table></div>",
        pw = vp.page_w(),
        h = BAR_H,
        p = cell(&prev, "上一页"),
        n = cell(&next, "下一页")
    )
}

/// 左右透明翻页热区：各占屏宽 1/3，`top:{READ_NAV_H}`（从顶栏下沿开始）到底。
/// `position:absolute` 相对初始包含块（`.wrap` 不是定位元素，故不受其裁剪），
/// `z-index:5` 高于正文（0）低于底栏（10）与顶栏（12）；顶栏整格可点，
/// 热区又整整让开 56px，所以点书名/「书架」不会误翻页。不再写死 `height:vh−24`，
/// 否则视口比参数矮时热区自己会顶出第二屏。
fn hot_zones(prev: Option<&str>, next: Option<&str>) -> String {
    let css = format!(
        "position:absolute;top:{nav}px;bottom:0;width:33%;z-index:5;display:block",
        nav = READ_NAV_H
    );
    let mut out = String::new();
    if let Some(pv) = prev {
        out.push_str(&format!(
            "<a href=\"{prev}\" style=\"{css};left:0\"></a>",
            prev = pv,
            css = css
        ));
    }
    if let Some(nx) = next {
        out.push_str(&format!(
            "<a href=\"{nx}\" style=\"{css};right:0\"></a>",
            nx = nx,
            css = css
        ));
    }
    out
}

/// 按像素宽度截断文本：CJK/全角按 `size` 像素计，ASCII 按 `half` 计；超宽用 `…` 收尾。
/// 墨水屏不能用 `text-overflow`（老 Silk 不稳定），所以服务端直接把书名截到能放下的长度，
/// 再配 `white-space:nowrap` + `overflow:hidden` 双保险，保证顶栏高度不被撑开。
fn fit_px(s: &str, max_px: usize, size: usize, half: usize) -> String {
    let mut w = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = if (ch as u32) < 0x2E80 { half } else { size };
        if w + cw > max_px {
            out.push('…');
            return out;
        }
        w += cw;
        out.push(ch);
    }
    out
}

/// 阅读页顶栏（小说页/漫画页共用）：左「← 书架」、中书名（按像素截断）、右「目录」。
/// 高度 `READ_NAV_H`(56)，`position:fixed` 贴视口顶，白底 + 1px 黑分隔线，`z-index:12`
/// 高于翻页热区（5）与底栏（10）；三格全部 `display:block` 整格可点，
/// 书架/目录链接都带当前 `fs/vw/vh`，点一次就能带着同一档字号与视口回去。
fn read_nav_html(title: &str, back_href: &str, toc_href: &str, vp: Vp) -> String {
    let title_px = vp
        .page_w()
        .saturating_sub(READ_NAV_BACK_W + READ_NAV_TOC_W)
        .saturating_sub(24);
    let title = fit_px(title, title_px, 20, 10);
    format!(
        "<div id=\"k-read-nav\"><table cellpadding=\"0\" cellspacing=\"0\"><tr>\
<td width=\"{bw}\" align=\"center\" style=\"border-right:1px solid #ccc\"><a href=\"{back}\">&larr; 书架</a></td>\
<td align=\"center\">{title}</td>\
<td width=\"{tw}\" align=\"center\" style=\"border-left:1px solid #ccc\"><a href=\"{toc}\">目录</a></td>\
</tr></table></div>",
        bw = READ_NAV_BACK_W,
        tw = READ_NAV_TOC_W,
        back = back_href,
        toc = toc_href,
        title = esc(&title)
    )
}

fn read_only_css() -> String {
    format!(
        "html,body{{height:100%;overflow:hidden}}\
         #k-read-nav{{position:fixed;top:0;left:0;right:0;height:{nav}px;padding:0 {pad}px;background:#fff;border-bottom:1px solid #000;z-index:12}}\
         #k-read-nav table{{width:100%;table-layout:fixed;border-collapse:collapse}}\
         #k-read-nav td{{height:{tdh}px;padding:0;font-size:20px;line-height:28px;vertical-align:middle;overflow:hidden;white-space:nowrap;color:#000}}\
         #k-read-nav td a{{display:block;height:{tdh}px;line-height:{tdh}px;color:#000;text-decoration:none}}\
         #k-stage{{position:absolute;top:{nav}px;left:{pad}px;right:{pad}px;bottom:{safe}px;padding:{pt}px {pad}px 0;overflow:hidden}}\
         #k-page-no{{position:absolute;right:{pad}px;bottom:8px;width:200px;font-size:20px;line-height:28px;color:#555;text-align:right}}",
        pad = SIDE_PAD,
        nav = READ_NAV_H,
        tdh = READ_NAV_H - 1,
        safe = BAR_H + SAFE_BOTTOM,
        pt = READ_PAD_TOP
    )
}

/// 阅读页内部链接：始终带目标档位的 `fs`，切档后不会被当前档位拼成重复参数。
fn read_link(q: &ReadQ, fs: usize, ch: i64, p: i64, vp: Vp) -> String {
    qlink(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            ch,
            p
        ),
        fs,
        vp,
    )
}

/// 目录链接（按 30 章/页定位到当前章所在目录页，并带上当前章用于高亮）
fn toc_link(q: &ReadQ, fs: usize, ch: i64, vp: Vp) -> String {
    qlink(
        &format!(
            "/k/toc?book={}&source={}&tp={}&ch={}",
            qenc(&q.book),
            qenc(&q.source),
            (ch / K_TOC_PAGE as i64) + 1,
            ch
        ),
        fs,
        vp,
    )
}

/// 翻页导航：翻完本章接上一章/下一章；全书首页/末页对应项为 `None`（底栏禁用 + 热区不渲染）。
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // 导航参数均为标量；返回类型是 (上一项, 下一项) 二元组，本身语义清晰。
fn read_nav(
    q: &ReadQ,
    fs: usize,
    ch: i64,
    p: i64,
    n: i64,
    total: i64,
    _toc: &str,
    vp: Vp,
) -> (
    Option<(String, &'static str)>,
    Option<(String, &'static str)>,
) {
    let prev = if p > 1 {
        Some((read_link(q, fs, ch, p - 1, vp), "上一页"))
    } else if ch > 0 {
        Some((read_link(q, fs, ch - 1, 1, vp), "上一章"))
    } else {
        None
    };
    let next = if p < n {
        Some((read_link(q, fs, ch, p + 1, vp), "下一页"))
    } else if ch + 1 < total {
        Some((read_link(q, fs, ch + 1, 1, vp), "下一章"))
    } else {
        None
    };
    (prev, next)
}

/// 底栏「上一页」格：章首变「上一章」，全书首页为禁用态
fn bar_cell_prev(prev: Option<&(String, &'static str)>) -> String {
    match prev {
        Some((href, label)) => bar_cell(href, "前", label),
        None => bar_cell_off("前", "上一页"),
    }
}

/// 底栏「下一页」格：有下一页/下一章则给链接，全书末页给弱灰不可点的禁用态。
fn bar_cell_next(next: Option<&(String, &'static str)>) -> String {
    match next {
        Some((href, label)) => bar_cell(href, "后", label),
        None => bar_cell_off("后", "下一页"),
    }
}

/// 进度提交：零 JS 的原生 form（底栏第 5 格），整格是 `<button>`，与其它格同构
fn progress_form(q: &ReadQ, ch: i64, p: i64, n: i64, total: usize, fs: usize, vp: Vp) -> String {
    format!(
        "<td width=\"20%\" height=\"64\" align=\"center\" valign=\"middle\" style=\"height:63px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
         <form method=\"post\" action=\"/k/progress\" style=\"margin:0\">\
         <input type=\"hidden\" name=\"book\" value=\"{book}\">\
         <input type=\"hidden\" name=\"source\" value=\"{source}\">\
         <input type=\"hidden\" name=\"ch\" value=\"{ch}\">\
         <input type=\"hidden\" name=\"p\" value=\"{p}\">\
         <input type=\"hidden\" name=\"n\" value=\"{n}\">\
         <input type=\"hidden\" name=\"total\" value=\"{total}\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\">\
         <input type=\"hidden\" name=\"vw\" value=\"{vw}\">\
         <input type=\"hidden\" name=\"vh\" value=\"{vh}\">\
         <button type=\"submit\" style=\"width:100%;height:63px;border:0;padding:0;background:#fff;color:#000;font-family:serif;-webkit-appearance:none;appearance:none\">{inner}</button></form></td>",
        book = esc(&q.book),
        source = esc(&q.source),
        ch = ch,
        p = p,
        n = n,
        total = total,
        fs = fs,
        vw = vp.w,
        vh = vp.h,
        inner = bar_inner("记", "记录")
    )
}

/// 描边按钮（只描边不填充、圆角 0、无阴影/渐变/过渡），240×72
fn outline_button(href: &str, label: &str) -> String {
    format!(
        "<a href=\"{href}\" style=\"display:block;width:240px;height:72px;line-height:70px;border:1px solid #000;text-align:center;font-size:24px\">{label}</a>",
        href = href,
        label = esc(label)
    )
}

/// 空书架：2px 描边块，整块在可用区内垂直居中（`table` 100% + `valign="middle"`），
/// 含标题 26 加粗 + 说明 20 `#555` + 240×72 描边按钮。
fn empty_shelf(fs: usize, vp: Vp) -> String {
    format!(
        "<table width=\"{pw}\" height=\"{h}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td align=\"center\" valign=\"middle\">\
         <table width=\"600\" cellpadding=\"0\" cellspacing=\"0\" style=\"border:1px solid #000\"><tr>\
         <td align=\"left\" style=\"padding:32px\">\
         <div style=\"font-size:26px;line-height:40px;font-weight:700\">书架还是空的</div>\
         <div style=\"font-size:20px;line-height:32px;color:#555;margin-top:12px\">在电脑浏览器打开主站，把想看的书加进书架。</div>\
         <div style=\"margin-top:24px\">{btn}</div></td></tr></table></td></tr></table>",
        pw = vp.page_w(),
        h = vp.nav_h(),
        btn = outline_button(&qlink("/k/search", fs, vp), "去搜索一本书")
    )
}

/// 带顶栏页面的标题行（页内标题，用于错误/分卷等没有顶栏动作的页面）
fn page_head(title: &str) -> String {
    format!(
        "<div style=\"height:73px;border-bottom:1px solid #ccc;padding:0 36px;font-size:28px;line-height:72px;font-weight:700\">{title}</div>",
        title = esc(title)
    )
}

/// 提示页（错误 / 分卷）：页内标题 + 说明 + 描边按钮，单屏内
fn notice_page(title: &str, msg: &str, href: &str, label: &str) -> String {
    format!(
        "{head}<div style=\"padding:36px\"><p style=\"font-size:24px;line-height:36px;color:#333\">{msg}</p>\
         <p style=\"margin-top:24px\">{btn}</p></div>",
        head = page_head(title),
        msg = esc(msg),
        btn = outline_button(href, label)
    )
}

fn err_page(msg: &str) -> Response {
    page(
        "错误",
        2,
        &notice_page("出错了", msg, "/k", "返回书架"),
        Vp::default(),
    )
}

fn err_to_page(e: AppError) -> Response {
    err_page(&format!("{e}"))
}

/// 分卷行：`is_volume` 标记，或抓取层合成的 `volume:<book_url>#<index>` URL。
fn volume_entry(c: &BookChapter) -> bool {
    c.is_volume || c.url.starts_with("volume:")
}

/// /k 目录可见章节（过滤分卷标题），保序且保留原始 `index`。
fn visible_chapters(chapters: &[BookChapter]) -> Vec<&BookChapter> {
    chapters.iter().filter(|c| !volume_entry(c)).collect()
}

/// 直接访问分卷行时返回的友好提示页（沿用现有版心，不新增版式）。
fn volume_notice(q: &ReadQ, fs: usize, explicit: bool, vp: Vp) -> Response {
    let back = qlink(
        &format!("/k/toc?book={}&source={}", qenc(&q.book), qenc(&q.source)),
        fs,
        vp,
    );
    let body = notice_page(
        "分卷标题",
        "该条为分卷标题，请从目录选择章节",
        &back,
        "返回目录",
    );
    let mut res = page("分卷标题", fs, &body, vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 正文纯文本化与分页 ----------------

static RE_BLOCK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)<\s*(br|/p|/div|/li|/h[1-6]|p|div|li|h[1-6])[^>]*>").unwrap());
static RE_TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

/// 把章节 HTML 转成纯文本：块级标签换行化 → 去标签 → 解码常用实体 → 合并空行
fn html_to_text(html: &str) -> String {
    let s = RE_BLOCK.replace_all(html, "\n");
    let s = RE_TAG.replace_all(&s, "");
    let s = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let s = RE_WS.replace_all(&s, "\n\n");
    s.trim().to_string()
}

/// 单字宽度（em）：ASCII/数字/半角标点 0.5，其余（汉字、全角标点）1.0
fn char_em(c: char) -> f64 {
    if c.is_ascii() {
        0.5
    } else {
        1.0
    }
}

/// 每行可用 em 数（版心宽度 ÷ 字号）；版心宽度随请求视口变化
fn line_em(spec: &FsSpec, page_w: usize) -> f64 {
    (page_w - SIDE_PAD * 2) as f64 / spec.size as f64
}

/// 段落占用行数：首行缩进 2em，续行不缩进（与渲染的 text-indent:2em 一致）
fn para_lines(s: &str, em: f64) -> usize {
    let mut lines = 1usize;
    let mut used = 2.0f64;
    for c in s.chars() {
        let w = char_em(c);
        if used + w > em {
            lines += 1;
            used = w;
        } else {
            used += w;
        }
    }
    lines
}

/// 从段落头部取不超过 cap 行的一段，返回 (该段字节长度, 实际行数)
fn split_para(s: &str, cap: usize, em: f64) -> (usize, usize) {
    let mut lines = 1usize;
    let mut used = 2.0f64;
    for (i, c) in s.char_indices() {
        let w = char_em(c);
        if used + w > em {
            if lines == cap {
                return (i, lines);
            }
            lines += 1;
            used = w;
        } else {
            used += w;
        }
    }
    (s.len(), lines)
}

/// 按行数预算分页：优先在段落边界断页，单段超预算时按行边界切分。
/// 行预算按「行当量」计：段与段之间另计 0.6 行段距（与渲染的 `margin-top` 一致），
/// 保证页面实际高度 ≤ `lines × line`，不把最后一行挤出可视区。
/// 返回每页的字节区间（相对 `text`），页边界只落在字符边界上。
fn paginate_lines(text: &str, spec: &FsSpec, budget: usize, page_w: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let em = line_em(spec, page_w);
    let budget = budget.max(1) as f64;
    let gap = para_gap_px(spec.line) as f64 / spec.line as f64;
    let mut pages: Vec<(usize, usize)> = Vec::new();
    let mut page_start: Option<usize> = None;
    let mut used = 0.0f64;
    let mut has_para = false;
    let mut page_end = 0usize;
    let mut offset = 0usize;
    for raw in text.split('\n') {
        let p_start = offset;
        let p_end = offset + raw.len();
        offset = p_end + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let trim_start = p_start + (raw.len() - raw.trim_start().len());
        let trim_end = trim_start + trimmed.len();
        let mut rem_start = trim_start;
        while rem_start < trim_end {
            let mut g = if has_para { gap } else { 0.0 };
            if budget - used - g < 1.0 {
                if let Some(ps) = page_start.take() {
                    pages.push((ps, page_end));
                }
                used = 0.0;
                has_para = false;
                g = 0.0;
            }
            let cap = (budget - used - g).floor() as usize;
            let remaining = &text[rem_start..trim_end];
            let need = para_lines(remaining, em);
            if need <= cap {
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                used += g + need as f64;
                has_para = true;
                page_end = trim_end;
                rem_start = trim_end;
            } else {
                let (head, _) = split_para(remaining, cap, em);
                debug_assert!(head > 0);
                if head == 0 {
                    break;
                }
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                pages.push((page_start.take().unwrap(), rem_start + head));
                used = 0.0;
                has_para = false;
                page_end = rem_start + head;
                rem_start += head;
            }
        }
    }
    if let Some(ps) = page_start.take() {
        pages.push((ps, page_end));
    }
    if pages.is_empty() {
        pages.push((0, 0));
    }
    pages
}

/// 按字符数切块：返回每个块的字节区间，每块 ≤ per 个字符
fn chunk_ranges(text: &str, per: usize) -> Vec<(usize, usize)> {
    if text.is_empty() || per == 0 {
        return vec![(0, 0)];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut n = 0usize;
    for (i, _) in text.char_indices() {
        if n == per {
            out.push((start, i));
            start = i;
            n = 0;
        }
        n += 1;
    }
    out.push((start, text.len()));
    out
}

/// 分页计划：正文按 20 万字符分块，逐块分页；不额外复制正文，页表只存字节区间。
struct PagePlan {
    /// 每块的全局字节区间
    chunks: Vec<(usize, usize)>,
    /// 每页 (块下标, 起, 止)，起止为全局字节偏移
    pages: Vec<(usize, usize, usize)>,
}

fn plan_pages(text: &str, spec: &FsSpec, budget: usize, page_w: usize) -> PagePlan {
    let chunks = chunk_ranges(text, K_CHUNK_CHARS);
    let mut pages = Vec::new();
    for (ci, &(cs, ce)) in chunks.iter().enumerate() {
        for (s, e) in paginate_lines(&text[cs..ce], spec, budget, page_w) {
            pages.push((ci, cs + s, cs + e));
        }
    }
    if pages.is_empty() {
        pages.push((0, 0, 0));
    }
    PagePlan { chunks, pages }
}

/// 超过 `max` 字符时按字符边界截断（不额外复制整段文本），返回是否发生截断
fn truncate_to_chars(text: &mut String, max: usize) -> bool {
    if text.chars().count() <= max {
        return false;
    }
    let cut = text
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.truncate(cut);
    true
}

// ---------------- 查询参数 ----------------

#[derive(Deserialize)]
pub struct TocQ {
    book: String,
    source: String,
    tp: Option<i64>,
    fs: Option<i64>,
    /// 当前所在章（从阅读页跳目录时带上，用于「▸ + 加粗」高亮）
    ch: Option<i64>,
    /// 视口宽/高（CSS px）：请求参数优先，缺参回落环境默认
    vw: Option<i64>,
    vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct ReadQ {
    book: String,
    source: String,
    ch: Option<i64>,
    p: Option<i64>,
    fs: Option<i64>,
    /// 视口宽/高（CSS px）：请求参数优先，缺参回落环境默认
    vw: Option<i64>,
    vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct SearchQ {
    key: Option<String>,
    fs: Option<i64>,
    sp: Option<i64>,
    /// 搜索范围：`/k` 缺省 curated（服务端渲染必须等整批，KPW3 首屏要快）；
    /// 显式 `?scope=all` 才走全量（由页面上的「用全部源搜索」链接升级）。
    scope: Option<String>,
    /// 是否包含冷却中的源：缺省 0；验收/对拍时可显式 1。
    include_cooled: Option<String>,
    vw: Option<i64>,
    vh: Option<i64>,
}

/// `/k` 的 scope 解析：只有显式 `all`/`expanded` 走全量，其余（含缺省、未知值）一律 curated。
fn k_scope(raw: Option<&str>) -> crate::req::SearchScope {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("all" | "expanded" | "enhanced") => crate::req::SearchScope::All,
        _ => crate::req::SearchScope::Curated,
    }
}
#[derive(Deserialize)]
pub struct FsQ {
    fs: Option<i64>,
    sp: Option<i64>,
    vw: Option<i64>,
    vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct LoginQ {
    username: String,
    password: String,
}
#[derive(Deserialize)]
pub struct ProgressQ {
    book: String,
    source: Option<String>,
    ch: i64,
    p: i64,
    n: i64,
    fs: Option<i64>,
    total: Option<i64>,
    vw: Option<i64>,
    vh: Option<i64>,
}

// ---------------- 数据辅助 ----------------

/// 解析请求视口：`vw`/`vh` 参数优先，缺参/越界回落到环境默认（默认 1072×1268）。
fn vp_of(s: &AppState, vw: Option<i64>, vh: Option<i64>) -> Vp {
    Vp::resolve(vw, vh, (s.cfg.kindle_vw as usize, s.cfg.kindle_vh as usize))
}

/// 目录（带缓存）：先读 book_chapters，miss 则 book_info 补 toc_url 后抓目录并落库
async fn ensure_chapters(
    s: &AppState,
    db: &SqlitePool,
    source_url: &str,
    book_url: &str,
) -> AppResult<(BookSource, Vec<BookChapter>)> {
    let source = load_source(&s.shared, source_url).await?;
    if let Some(chaps) = load_cached_chapters(db, book_url, s.cfg.toc_ttl_secs).await? {
        return Ok((source, chaps));
    }
    let mut book = Book {
        book_url: book_url.to_string(),
        ..Default::default()
    };
    if let Some(m) = load_book_meta(db, book_url).await? {
        book.toc_url = m.toc_url;
    }
    // 补详情拿 tocUrl（字面量 URL 的情况也要先走 book_info）
    web_book::book_info(&source, &mut book).await?;
    save_book_meta(db, &book).await?;
    let chapters = web_book::chapter_list(&source, &book).await?;
    save_chapters(db, book_url, &chapters).await?;
    Ok((source, chapters))
}

// ---------------- 登录 / 注销 ----------------

async fn login_form(Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let err = if q.get("err").map(|v| v == "1").unwrap_or(false) {
        "<p style=\"font-size:24px;line-height:36px;color:#333;margin-top:16px\">账号或密码错误，请重试</p>"
    } else {
        ""
    };
    page(
        "登录",
        2,
        &format!(
            "{head}\
             <div style=\"padding:36px\">\
             <p style=\"font-size:20px;line-height:32px;color:#555\">Kindle 极简阅读 · 请用网页端账号登录</p>\
             <p style=\"font-size:20px;line-height:32px;color:#555\">没有账号请先在电脑浏览器打开主站注册。</p>{err}\
             <form method=\"post\" action=\"/k/login\">\
             <table width=\"928\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-top:24px\">\
             <tr><td width=\"120\" height=\"76\" style=\"font-size:24px\">账号</td>\
             <td><input type=\"text\" name=\"username\" style=\"font-size:24px;height:56px;width:640px;border:1px solid #000;padding:0 12px;font-family:serif\"></td></tr>\
             <tr><td height=\"76\" style=\"font-size:24px\">密码</td>\
             <td><input type=\"password\" name=\"password\" style=\"font-size:24px;height:56px;width:640px;border:1px solid #000;padding:0 12px;font-family:serif\"></td></tr>\
             <tr><td height=\"88\"></td>\
             <td><input type=\"submit\" value=\"登录\" style=\"font-size:24px;height:56px;width:240px;border:1px solid #000;background:#fff;color:#000;font-family:serif\"></td></tr>\
             </table></form></div>",
            head = page_head("登录"),
            err = err
        ),
        Vp::default(),
    )
}

async fn login_post(State(s): State<AppState>, Form(q): Form<LoginQ>) -> Response {
    let row: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, password_hash, role, disabled, token_version FROM users WHERE username = ?",
    )
    .bind(&q.username)
    .fetch_optional(&s.shared)
    .await
    .unwrap_or_default();
    // 禁用账号与普通账号一样登录失败（不泄漏具体原因）
    let ok = match row {
        Some((_, ref hash, _, disabled, _)) => {
            disabled == 0 && bcrypt::verify(&q.password, hash).unwrap_or(false)
        }
        None => false,
    };
    if !ok {
        let mut res = Response::new(axum::body::Body::empty());
        *res.status_mut() = StatusCode::SEE_OTHER;
        res.headers_mut().insert(
            axum::http::header::LOCATION,
            HeaderValue::from_static("/k/login?err=1"),
        );
        return res;
    }
    let (id, _, role, _, ver) = row.expect("checked above");
    let token = match auth::create_token_days(id, &q.username, &role, ver, &s.cfg.jwt_secret, 30) {
        Ok(t) => t,
        Err(e) => return err_to_page(e),
    };
    let cookie = format!(
        "k_session={}; Path=/; HttpOnly; Max-Age={}; SameSite=Lax",
        token,
        60 * 60 * 24 * 30
    );
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    res.headers_mut()
        .insert(axum::http::header::LOCATION, HeaderValue::from_static("/k"));
    res.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).unwrap_or(HeaderValue::from_static("")),
    );
    res
}

async fn logout() -> Response {
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    res.headers_mut().insert(
        axum::http::header::LOCATION,
        HeaderValue::from_static("/k/login"),
    );
    res.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static("k_session=; Path=/; HttpOnly; Max-Age=0"),
    );
    res
}

// ---------------- 书架 ----------------

async fn shelf(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<FsQ>,
) -> Response {
    let vp = vp_of(&s, q.vw, q.vh);
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let rows: Vec<BookshelfItem> = match sqlx::query_as::<_, BookshelfItem>(SHELF_LIST_SQL)
        .bind(None::<String>)
        .fetch_all(&db.0)
        .await
    {
        Ok(r) => r,
        Err(e) => return err_to_page(AppError::Sqlx(e)),
    };

    let next_fs = cycle_fs(fs);
    let total_books = rows.len();
    // 每页本数与行高按「页面高 − 顶栏 − 底部分页条 − 安全余量」实算（1268 → 10 本 × 110px）
    let (per_page, row_h) = vp.shelf_layout();
    let t_pages = total_books.div_ceil(per_page).max(1);
    let sp = q.sp.unwrap_or(1).clamp(1, t_pages as i64) as usize;
    let start = (sp - 1) * per_page;
    let end = (start + per_page).min(total_books);
    let fs_url = format!("/k?fs={}&sp={}", next_fs, sp.min(t_pages.max(1)));
    // 导航只出现一次：书架/搜索/目录用顶部导航，底栏只留给阅读页
    let nav = format!(
        "{}{}{}",
        nav_link(&qlink("/k/search", fs, vp), "搜索"),
        nav_link(&with_vp(&fs_url, vp), FS_SPECS[next_fs - 1].label),
        nav_link("/k/logout", "退出"),
    );
    let top = top_bar("书架", None, &nav, vp);

    let mut list = String::new();
    if rows.is_empty() {
        list.push_str(&empty_shelf(fs, vp));
    } else {
        list.push_str(&format!(
            "<table width=\"{}\" cellpadding=\"0\" cellspacing=\"0\">",
            vp.page_w()
        ));
        for b in &rows[start..end] {
            let title = b.title.clone().unwrap_or_else(|| "未命名".into());
            let author = b.author.clone().unwrap_or_default();
            let src = b.source_id.clone().unwrap_or_default();
            let read = qlink(
                &format!(
                    "/k/read?book={}&source={}&ch={}&p=1",
                    qenc(&b.book_id),
                    qenc(&src),
                    b.chapter_index
                ),
                fs,
                vp,
            );
            let first = title.chars().next().unwrap_or('书');
            list.push_str(&format!(
                "<tr><td style=\"padding:0\">\
                 <a href=\"{read}\" style=\"display:block;height:{rh}px;border-bottom:1px solid #ccc\">\
                 <table width=\"{iw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                 <td width=\"56\" valign=\"middle\"><div style=\"width:56px;height:80px;border:1px solid #000;font-size:24px;line-height:78px;text-align:center\">{first}</div></td>\
                 <td style=\"padding-left:24px\" valign=\"middle\">\
                 <div style=\"font-size:32px;line-height:48px;font-weight:700\">{title}</div>\
                 <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-top:8px\"><tr>\
                 <td align=\"left\" style=\"font-size:20px;line-height:32px;color:#555\">{author}</td>\
                 <td align=\"right\" style=\"font-size:20px;line-height:32px;color:#555\">{pct:.0}%</td>\
                 </tr></table></td></tr></table></a></td></tr>",
                read = read,
                rh = row_h,
                iw = vp.page_w(),
                first = esc(&first.to_string()),
                title = esc(&title),
                author = esc(if author.is_empty() { "未知作者" } else { author.as_str() }),
                pct = b.reading_progress
            ));
        }
        list.push_str("</table>");
    }

    let pager = if t_pages > 1 {
        let prev = if sp > 1 {
            Some((
                with_vp(&format!("/k?fs={}&sp={}", fs, sp - 1), vp),
                "上一页",
            ))
        } else {
            None
        };
        let next = if sp < t_pages {
            Some((
                with_vp(&format!("/k?fs={}&sp={}", fs, sp + 1), vp),
                "下一页",
            ))
        } else {
            None
        };
        pager_bar(prev, next, vp)
    } else {
        String::new()
    };
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        list = list,
        pager = pager
    );
    let mut res = page("书架", fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 目录 ----------------

async fn toc(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<TocQ>,
) -> Response {
    let vp = vp_of(&s, q.vw, q.vh);
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let (_source, chapters) = match ensure_chapters(&s, &db.0, &q.source, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_to_page(e),
    };
    // 分卷标题不是可读章节：目录不展示、不计入总数与分页。
    // 保留 `BookChapter.index`（原始下标），链接仍按原 index 生成，/k/read 才能对得上。
    let visible = visible_chapters(&chapters);
    let total = visible.len() as i64;
    // 每列行数按「页面高 − 顶栏 − 底部分页条 − 安全余量」实算，30 只是上限
    let toc_page = toc_page_size(vp);
    let rows_per_col = toc_page / K_TOC_COLS;
    let t_pages = ((total + toc_page as i64 - 1) / toc_page as i64).max(1);
    let tp = q.tp.unwrap_or(1).clamp(1, t_pages);
    let start = ((tp - 1) * toc_page as i64) as usize;
    let end = (start + toc_page).min(visible.len());
    let name = load_book_meta(&db.0, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_else(|| "目录".into());

    let next_fs = cycle_fs(fs);
    let fs_href = with_vp(
        &with_fs(
            &format!(
                "/k/toc?book={}&source={}&tp={}&ch={}",
                qenc(&q.book),
                qenc(&q.source),
                tp,
                q.ch.unwrap_or(-1)
            ),
            next_fs,
        ),
        vp,
    );
    // 导航只出现一次：标题=书名，右侧=书架 / 字号
    let nav = format!(
        "{}{}",
        nav_link(&qlink("/k", fs, vp), "书架"),
        nav_link(&fs_href, FS_SPECS[next_fs - 1].label),
    );
    let top = top_bar(&name, Some(&format!("共 {total} 章")), &nav, vp);

    // 两列各 rows_per_col 行（1268 → 15 行/列）；列宽随版心自适应（1072 → 452，1024 → 428）
    let col_w = vp.content_w().saturating_sub(24) / K_TOC_COLS;
    let mut cols = [String::new(), String::new()];
    for (i, c) in visible[start..end].iter().enumerate() {
        let col = (i / rows_per_col).min(K_TOC_COLS - 1);
        let href = qlink(
            &format!(
                "/k/read?book={}&source={}&ch={}&p=1",
                qenc(&q.book),
                qenc(&q.source),
                c.index
            ),
            fs,
            vp,
        );
        let title = c.title.as_deref().unwrap_or("未命名章节");
        let cur = q.ch.map(|x| x == c.index).unwrap_or(false);
        let name = if cur {
            format!("<b>▸ {}</b>", esc(title))
        } else {
            esc(title)
        };
        cols[col].push_str(&format!(
            "<tr><td height=\"72\" style=\"border-bottom:1px solid #ccc\">\
             <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
             <td width=\"80\" align=\"right\" valign=\"middle\" style=\"font-size:24px;line-height:46px;color:#555\">{no}</td>\
             <td valign=\"middle\" style=\"padding-left:16px;font-size:28px;line-height:46px\"><a href=\"{href}\" style=\"display:block\">{name}</a></td>\
             </tr></table></td></tr>",
            no = start + i + 1,
            href = href,
            name = name
        ));
    }
    for c in cols.iter_mut() {
        if c.is_empty() {
            c.push_str("<tr><td height=\"72\"></td></tr>");
        }
        *c = format!(
            "<table width=\"{col_w}\" cellpadding=\"0\" cellspacing=\"0\">{}</table>",
            c
        );
    }
    let list = format!(
        "<table width=\"{tw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-left:{pad}\"><tr>\
         <td width=\"{col_w}\" valign=\"top\">{left}</td><td width=\"24\"></td><td width=\"{col_w}\" valign=\"top\">{right}</td>\
         </tr></table>",
        tw = vp.content_w(),
        pad = SIDE_PAD,
        col_w = col_w,
        left = cols[0],
        right = cols[1]
    );

    let prev = if tp > 1 {
        Some((
            qlink(
                &format!(
                    "/k/toc?book={}&source={}&tp={}&ch={}",
                    qenc(&q.book),
                    qenc(&q.source),
                    tp - 1,
                    q.ch.unwrap_or(-1)
                ),
                fs,
                vp,
            ),
            "上一页",
        ))
    } else {
        None
    };
    let next = if tp < t_pages {
        Some((
            qlink(
                &format!(
                    "/k/toc?book={}&source={}&tp={}&ch={}",
                    qenc(&q.book),
                    qenc(&q.source),
                    tp + 1,
                    q.ch.unwrap_or(-1)
                ),
                fs,
                vp,
            ),
            "下一页",
        ))
    } else {
        None
    };
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        list = list,
        pager = pager_bar(prev, next, vp)
    );

    let mut res = page(&name, fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 漫画（一屏一页） ----------------

/// 漫画页：某张图的整图，或该图的第 `slice` 片（0 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComicPage {
    /// `images` 里的下标
    img: usize,
    slice: usize,
    /// 该图总片数（1 = 整图）
    layers: usize,
    /// 显示宽（CSS px）
    width: usize,
    /// 显示高（CSS px）；0 = 尺寸未知，渲染时退化到 `max-height`
    height: usize,
}

/// 由图片列表与原始宽高构建页表，保证每页显示高 ≤ 漫画可用高（`vp.content_h()`，1268 → 1172）。
/// - 长条页（切片开关开启且 `slice_count_window` > 1）：一图拆 `layers` 页，
///   非末片按可用高、末片取剩余；
/// - 一屏内：整图按版心 928 宽，显示高超过可用高时按高度等比缩小；
/// - 尺寸未知：整图一页，只给宽度。
fn comic_pages(
    images: &[BookImage],
    dims: &[Option<(u32, u32)>],
    cfg: &crate::config::Config,
    sp: &image::SliceParams,
) -> Vec<ComicPage> {
    let dw = sp.dw as usize;
    let content_h = sp.vh as usize;
    let mut pages = Vec::new();
    for i in 0..images.len() {
        let known = dims
            .get(i)
            .copied()
            .flatten()
            .filter(|(w, h)| *w > 0 && *h > 0);
        let Some((w, h)) = known else {
            pages.push(ComicPage {
                img: i,
                slice: 0,
                layers: 1,
                width: 0,
                height: 0,
            });
            continue;
        };
        // 开关关闭时恒按整图一页，与切片上线前的行为一致。
        let n = if cfg.comic_slice {
            image::slice_count_window(w, h, sp)
        } else {
            1
        };
        if n > 1 {
            // 切片的真实高度由服务端按近白行落点决定（实测 1150/667，窗口预判只有 1172/620），
            // 写死 height 会把图拉扁；这里只给宽度，页面高度由 `vh−24` 的 stage 兜住
            // （漫画页保留像素高：切片高度按 `sp.vh` 生成，改成拉伸填满会丢图）。
            for k in 0..n {
                pages.push(ComicPage {
                    img: i,
                    slice: k,
                    layers: n,
                    width: dw,
                    height: 0,
                });
            }
        } else {
            let scaled = ((h as f64) * dw as f64 / w as f64).round() as usize;
            if scaled > content_h {
                // 1.1 屏这类「不切但一屏放不下」的整图：按高度约束等比缩小，仍是一屏一页
                let cw = ((content_h as f64) * w as f64 / h as f64).round() as usize;
                pages.push(ComicPage {
                    img: i,
                    slice: 0,
                    layers: 1,
                    width: cw.max(1),
                    height: content_h,
                });
            } else {
                pages.push(ComicPage {
                    img: i,
                    slice: 0,
                    layers: 1,
                    width: dw,
                    height: scaled.max(1),
                });
            }
        }
    }
    pages
}

/// 渲染漫画页：`#k-stage` 与小说页同高同宽，图片给显式宽高（不用 `height:auto`，避免抖动）。
/// 只有长条页的切片才带 `slice=1&i=k`，整图页维持旧 URL（桌面端与 `slice=0` 路径完全不受影响）。
#[allow(clippy::too_many_arguments)] // 漫画渲染上下文（图集/页码/源/书/章/meta/视口），拆分会把同一屏的数据分散到多处。
fn comic_stage(
    images: &[BookImage],
    pg: &ComicPage,
    source_id: &str,
    book_url: &str,
    chapter_url: &str,
    meta: &str,
    vp: Vp,
    sp: &image::SliceParams,
) -> String {
    let im = &images[pg.img];
    let mut src = format!(
        "/api/book/image?source={}&book_url={}&chapter_url={}&url={}&eink=1",
        qenc(source_id),
        qenc(book_url),
        qenc(chapter_url),
        qenc(&im.url)
    );
    if pg.layers > 1 {
        // 切片 URL 带 dw/vh：缓存 key 天然按尺寸隔离，换视口不会命中别的尺寸的旧片
        src.push_str(&format!(
            "&slice=1&i={}&dw={}&vh={}",
            pg.slice, sp.dw, sp.vh
        ));
    }
    let img = if pg.height > 0 {
        format!(
            "<img src=\"{src}\" width=\"{w}\" height=\"{h}\" style=\"display:block;max-width:100%\">",
            src = src,
            w = pg.width,
            h = pg.height
        )
    } else if pg.width > 0 {
        // 切片：真实高度由服务端按落点决定，这里只给宽度，避免把图拉扁
        format!(
            "<img src=\"{src}\" width=\"{w}\" style=\"display:block;max-width:100%\">",
            src = src,
            w = pg.width
        )
    } else {
        format!(
            "<img src=\"{src}\" style=\"display:block;max-width:100%\">",
            src = src
        )
    };
    format!(
        "<div id=\"k-stage\" style=\"position:relative;top:0;left:0;right:auto;bottom:auto;margin-top:{nav}px;height:{h}px;width:{w}px;padding:0 {pad}px {pb}px;overflow:hidden\">{img}<div id=\"k-page-no\">{meta}</div></div>",
        nav = READ_NAV_H,
        h = vp.layout_h().saturating_sub(READ_NAV_H),
        w = vp.page_w(),
        pad = SIDE_PAD,
        pb = BAR_H + 8,
        img = img,
        meta = esc(meta)
    )
}

/// 取本章图片：先读 `book_content.images` 缓存，未命中回源并按 `/api/comic/chapter` 同规则回写。
async fn load_comic_images(
    s: &AppState,
    pool: &SqlitePool,
    source: &BookSource,
    chapter: &BookChapter,
    book_url: &str,
) -> Result<Vec<BookImage>, String> {
    if let Ok(Some(c)) =
        crate::routes::comic::load_cached_images(pool, &chapter.url, s.cfg.content_ttl_secs).await
    {
        if c.content_type == "image" {
            let (v, _) = crate::routes::comic::decode_images(c.images_json.as_deref());
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    let parsed = web_book::chapter_images(source, chapter)
        .await
        .map_err(|e| format!("本章图片抓取失败: {e}"))?;
    let images = parsed.images;
    if images.is_empty() {
        return Ok(images);
    }
    let payload = serde_json::to_string(&web_book::ChapterImages {
        images: images.clone(),
        truncated: parsed.truncated,
    })
    .unwrap_or_default();
    let c = BookContent {
        url: chapter.url.clone(),
        book_url: book_url.to_string(),
        title: chapter.title.clone(),
        content: Some(String::new()),
        chapter_index: chapter.index,
        content_type: "image".into(),
        images_json: Some(payload),
        ..Default::default()
    };
    let limit = match replace::user_cache_limit_mb(pool).await {
        Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
        None => s.cfg.cache_limit_bytes(),
    };
    if let Err(e) = save_content_cached(pool, &c, limit).await {
        tracing::warn!(url = %chapter.url, error = %e, "comic content cache write failed");
    }
    Ok(images)
}

// ---------------- 正文 ----------------

async fn read(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<ReadQ>,
) -> Response {
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let vp = vp_of(&s, q.vw, q.vh);
    let (source, chapters) = match ensure_chapters(&s, &db.0, &q.source, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_to_page(e),
    };
    if chapters.is_empty() {
        return err_page("目录为空");
    }
    let ch = q.ch.unwrap_or(0).clamp(0, chapters.len() as i64 - 1);
    let chapter: &BookChapter = &chapters[ch as usize];
    // 目录已不展示分卷行；兜底拦截直接访问（合成 URL `volume:`）时给出友好提示，不抛错误页。
    if volume_entry(chapter) {
        return volume_notice(&q, fs, explicit, vp);
    }
    // 漫画源：一屏一页（整图或切片），不走正文分页
    if source.book_source_type == 2 {
        return comic_read(&s, &db.0, &source, &chapters, ch, &q, fs, explicit, vp).await;
    }

    let mut c: BookContent =
        match load_cached_content(&db.0, &chapter.url, s.cfg.content_ttl_secs).await {
            Ok(Some(c)) => c,
            _ => {
                let fetched = match web_book::chapter_content(&source, chapter).await {
                    Ok(c) => c,
                    Err(e) => return err_page(&format!("正文抓取失败: {e}")),
                };
                let limit = match replace::user_cache_limit_mb(&db.0).await {
                    Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
                    None => s.cfg.cache_limit_bytes(),
                };
                let _ = save_content_cached(&db.0, &fetched, limit).await;
                fetched
            }
        };
    if c.book_url.trim().is_empty() {
        c.book_url = q.book.clone();
    }
    if c.title.is_none() {
        c.title = chapter.title.clone();
    }
    // 与主站一致：正文输出前应用用户替换规则
    let book_name = load_book_meta(&db.0, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_default();
    let (title, body_html) = replace::purify(
        &db.0,
        &book_name,
        &q.source,
        c.title.clone(),
        c.content.clone(),
    )
    .await;
    let mut text = html_to_text(body_html.as_deref().unwrap_or(""));
    let truncated = truncate_to_chars(&mut text, K_MAX_TEXT_CHARS);
    let spec = &FS_SPECS[fs - 1];
    // 每页行数由「正文可用高 ÷ 行高」动态算出（字号四档不再有 24/21/17/25 硬编码）
    let budget = line_budget(vp, spec.line);
    let plan = plan_pages(&text, spec, budget, vp.page_w());
    let n = plan.pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let (block_idx, cur_start, cur_end) = plan.pages[(p - 1) as usize];
    let cur = &text[cur_start..cur_end];
    let blocks = plan.chunks.len();

    let ch_title = title.clone().unwrap_or_else(|| format!("第 {} 章", ch + 1));
    let toc_url = toc_link(&q, fs, ch, vp);
    let (prev, next) = read_nav(&q, fs, ch, p, n, chapters.len() as i64, &toc_url, vp);
    let mut meta = if blocks > 1 {
        // 超长章节按 20 万字符分块：底栏“目录”格下方同时给出页内进度、页码与分块号
        let pct = ((p as f64 * 100.0 / n as f64).round() as usize).clamp(1, 100);
        format!("加载中{pct}%·{p}/{n}·{}/{}块", block_idx + 1, blocks)
    } else {
        format!("{p}/{n}")
    };
    if truncated {
        meta.push_str("·已截断");
    }
    // 进度提交：零 JS 的原生 form，放在底栏第 5 格（阅读页无顶栏）
    let progress_cell = progress_form(&q, ch, p, n, chapters.len(), fs, vp);
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(&q, next_fs, ch, p, vp);

    let mut paras = String::new();
    let gap = para_gap_px(spec.line);
    let mut first_para = true;
    for para in cur.split('\n') {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        let mt = if first_para { 0 } else { gap };
        first_para = false;
        paras.push_str(&format!(
            "<p style=\"font-size:{size}px;line-height:{line}px;text-indent:2em;margin-top:{mt}px;word-wrap:break-word\">{para}</p>",
            size = spec.size,
            line = spec.line,
            mt = mt,
            para = esc(para)
        ));
    }
    // 阅读页无顶栏：正文区由 `#k-stage` 绝对定位填满（视口高 − 88 底栏/安全区）
    let stage = format!(
        "<div id=\"k-stage\">{paras}<div id=\"k-page-no\">{meta}</div></div>",
        paras = paras,
        meta = esc(&meta)
    );
    // 贴底固定栏 5 格（各 200px）：上一页 / 下一页 / 目录 / 字号 / 记录
    let bottom = fixed_bar(
        &format!(
            "{prev}{next}{toc}{fs}{progress}",
            prev = bar_cell_prev(prev.as_ref()),
            next = bar_cell_next(next.as_ref()),
            toc = bar_cell(&toc_url, "目", "目录"),
            fs = bar_cell(&fs_href, "字", "字号"),
            progress = progress_cell
        ),
        vp,
    );

    // 顶栏：书名（无书名时回落到章节名）+「← 书架」+「目录」，两者都带当前 fs/vw/vh
    let nav_title = if book_name.is_empty() {
        ch_title.as_str()
    } else {
        book_name.as_str()
    };
    let nav = read_nav_html(nav_title, &qlink("/k", fs, vp), &toc_url, vp);

    let body = format!(
        "{nav}{stage}{hot}{bottom}",
        nav = nav,
        stage = stage,
        hot = hot_zones(
            prev.as_ref().map(|x| x.0.as_str()),
            next.as_ref().map(|x| x.0.as_str()),
        ),
        bottom = bottom
    );
    let mut res = page_css(&ch_title, fs, &body, &read_only_css(), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

/// 漫画阅读页：一屏一页（整图或切片），翻页/进度/字号与小说页同一套底栏与热区。
#[allow(clippy::too_many_arguments)] // 阅读页渲染上下文（状态/池/源/目录/章/查询/字号），与小说页保持同一组参数。
async fn comic_read(
    s: &AppState,
    pool: &SqlitePool,
    source: &BookSource,
    chapters: &[BookChapter],
    ch: i64,
    q: &ReadQ,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let chapter = &chapters[ch as usize];
    let images = match load_comic_images(s, pool, source, chapter, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_page(&e),
    };
    if images.is_empty() {
        return err_page("本章没有图片");
    }
    // 尺寸：源页面已带宽高的直接用，缺失的批量探测（进程内缓存，同章重复打开不再回源）
    let mut dims: Vec<Option<(u32, u32)>> = images
        .iter()
        .map(|im| match (im.width, im.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        })
        .collect();
    let missing: Vec<String> = images
        .iter()
        .zip(dims.iter())
        .filter(|(_, d)| d.is_none())
        .map(|(im, _)| im.url.clone())
        .collect();
    if !missing.is_empty() {
        let probed = image::probe_dims(
            source,
            Some(&chapter.url),
            Some(&q.book),
            &missing,
            s.cfg.image_max_body_bytes.max(1024),
        )
        .await;
        let mut it = probed.into_iter();
        for d in dims.iter_mut() {
            if d.is_none() {
                *d = it.next().flatten();
            }
        }
    }
    // 切片参数按请求视口推导：版心宽 = vw − 144（1072 → 928），
    // 可用高 = 页面高 − 底栏 − 8 − 顶栏（1268 → 1172 − 56 = 1116），
    // 少切这一刀，整图/切片才不会被新顶栏压掉最后 56px
    let sp = image::SliceParams::from_config(
        &s.cfg,
        Some(vp.content_w() as u32),
        Some(vp.content_h().saturating_sub(READ_NAV_H) as u32),
        None,
    );
    let pages = comic_pages(&images, &dims, &s.cfg, &sp);
    if pages.is_empty() {
        return err_page("本章没有图片");
    }
    let n = pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let pg = &pages[(p - 1) as usize];

    let toc_url = toc_link(q, fs, ch, vp);
    let (prev, next) = read_nav(q, fs, ch, p, n, chapters.len() as i64, &toc_url, vp);
    let meta = format!("{p}/{n}");
    let progress_cell = progress_form(q, ch, p, n, chapters.len(), fs, vp);
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(q, next_fs, ch, p, vp);

    let stage = comic_stage(
        &images,
        pg,
        &q.source,
        &q.book,
        &chapter.url,
        &meta,
        vp,
        &sp,
    );
    let bottom = fixed_bar(
        &format!(
            "{prev}{next}{toc}{fs}{progress}",
            prev = bar_cell_prev(prev.as_ref()),
            next = bar_cell_next(next.as_ref()),
            toc = bar_cell(&toc_url, "目", "目录"),
            fs = bar_cell(&fs_href, "字", "字号"),
            progress = progress_cell
        ),
        vp,
    );
    let title = chapter
        .title
        .clone()
        .unwrap_or_else(|| format!("第 {} 章", ch + 1));
    // 与小说页同一套顶栏：书名优先，取不到回落章节名
    let book_name = load_book_meta(pool, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_default();
    let nav_title = if book_name.is_empty() {
        title.as_str()
    } else {
        book_name.as_str()
    };
    let nav = read_nav_html(nav_title, &qlink("/k", fs, vp), &toc_url, vp);
    let body = format!(
        "{nav}{stage}{hot}{bottom}",
        nav = nav,
        stage = stage,
        hot = hot_zones(
            prev.as_ref().map(|x| x.0.as_str()),
            next.as_ref().map(|x| x.0.as_str()),
        ),
        bottom = bottom
    );
    let mut res = page_css(&title, fs, &body, &read_only_css(), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 进度提交（零 JS form） ----------------

async fn progress(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Form(q): Form<ProgressQ>,
) -> Response {
    let ch = q.ch.max(0);
    let position = if q.n > 0 {
        (q.p.max(1) as f64 / q.n as f64).min(1.0)
    } else {
        0.0
    };
    let _ = sqlx::query(
        "INSERT INTO reading_progress (book_id, chapter_index, position) \
         VALUES (?1,?2,?3) ON CONFLICT(book_id) DO UPDATE SET \
         chapter_index=excluded.chapter_index, position=excluded.position, updated_at=datetime('now')",
    )
    .bind(&q.book)
    .bind(ch)
    .bind(position)
    .execute(&db.0)
    .await;

    let total = match q.total {
        Some(t) if t > 0 => t,
        _ => sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(total_chapter_num,0) FROM book_meta WHERE book_url = ?",
        )
        .bind(&q.book)
        .fetch_optional(&db.0)
        .await
        .ok()
        .flatten()
        .unwrap_or(0),
    };
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM book_meta WHERE book_url = ?")
        .bind(&q.book)
        .fetch_optional(&db.0)
        .await
        .ok()
        .flatten();
    let _ = sqlx::query(
        "INSERT INTO book_stats (book_id, name, total_chapters, read_chapters, last_read_at) \
         VALUES (?1,?2,?3,?4, datetime('now')) ON CONFLICT(book_id) DO UPDATE SET \
         name=COALESCE(excluded.name, book_stats.name), \
         total_chapters=CASE WHEN excluded.total_chapters>0 THEN excluded.total_chapters ELSE book_stats.total_chapters END, \
         read_chapters=MAX(book_stats.read_chapters, excluded.read_chapters), \
         last_read_at=datetime('now')",
    )
    .bind(&q.book)
    .bind(&name)
    .bind(total)
    .bind(ch + 1)
    .execute(&db.0)
    .await;

    let fs = match q.fs {
        Some(v) if (1..=4).contains(&v) => v as usize,
        _ => 2,
    };
    let loc = qlink(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(q.source.as_deref().unwrap_or("")),
            ch,
            q.p.max(1)
        ),
        fs,
        vp_of(&s, q.vw, q.vh),
    );
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(v) = HeaderValue::from_str(&loc) {
        res.headers_mut().insert(axum::http::header::LOCATION, v);
    }
    res
}

// ---------------- 搜索 ----------------

async fn search(
    State(s): State<AppState>,
    _u: AuthUser,
    _db: UserDb,
    headers: HeaderMap,
    Query(q): Query<SearchQ>,
) -> Response {
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let vp = vp_of(&s, q.vw, q.vh);
    let key = q.key.unwrap_or_default().trim().to_string();
    let next_fs = cycle_fs(fs);
    let fs_url = format!("/k/search?fs={}&key={}", next_fs, qenc(&key));
    // 导航只出现一次：标题=搜索，右侧=书架 / 字号
    let nav = format!(
        "{}{}",
        nav_link(&qlink("/k", fs, vp), "书架"),
        nav_link(&with_vp(&fs_url, vp), FS_SPECS[next_fs - 1].label),
    );
    let top = top_bar("搜索", None, &nav, vp);
    let form = format!(
        "<div style=\"padding:20px 36px 12px\"><form method=\"get\" action=\"/k/search\">\
         <input type=\"text\" name=\"key\" value=\"{key}\" style=\"font-size:24px;height:56px;width:600px;border:1px solid #000;padding:0 12px;font-family:serif\">\
         <input type=\"submit\" value=\"搜索\" style=\"font-size:24px;height:56px;width:160px;border:1px solid #000;background:#fff;color:#000;font-family:serif;margin-left:12px\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\"></form></div>",
        key = esc(&key),
        fs = fs
    );
    let mut list = String::new();
    let mut pager = String::new();
    if !key.is_empty() {
        let mut sources = match load_sources(&s.shared).await {
            Ok(v) => v,
            Err(e) => return err_to_page(e),
        };
        crate::routes::sources::sort_sources(&mut sources);
        // 与 legacy `/api/search` 共用 `aggregated`：同一源集合、同一预算/并发池、
        // 同一 dedupe_and_sort + relevance 档位排序，并共享 60s 结果缓存；
        // 同一实例上 /k 首屏与 `/api/search` 前 8 条顺序一致。
        let include_cooled = matches!(
            q.include_cooled.as_deref().map(str::trim),
            Some("1" | "true" | "yes" | "on")
        );
        let requested_scope = k_scope(q.scope.as_deref());
        let plan = crate::routes::books::resolve_search_sources(
            &s.shared,
            sources,
            &[],
            requested_scope,
            include_cooled,
            // `/k` 精选集合缺失时不退全量：只跑 preferred 层，保住 KPW3 首屏耗时。
            crate::routes::books::CuratedMissingFallback::Preferred,
        )
        .await;
        let (books, _errors) =
            crate::routes::books::aggregated(&plan.active, &key, include_cooled, plan.scope).await;
        let strong_total = books
            .iter()
            .filter(|b| b.relevance.as_deref() == Some("strong"))
            .count();
        // 三层退化链的页面说明（复用现有次级文字样式，不加新视觉 token）：
        // curated_fallback = 精选不可用已退常用源；all_fallback = 常用源也为空、已退全部可搜索源。
        let fallback_note = if plan.source_total == 0 {
            None
        } else {
            match plan.source_mode {
                "curated_fallback" => Some(if plan.curated_total == 0 {
                    format!(
                        "尚未生成精选集，已改用常用源（{n} 个源）。可在书源设置里跑一次体检生成精选。",
                        n = plan.source_total
                    )
                } else {
                    format!(
                        "精选集暂无可用条目，已改用常用源（{n} 个源）。",
                        n = plan.source_total
                    )
                }),
                "all_fallback" => Some(if plan.curated_total == 0 {
                    format!(
                        "尚未生成精选集，已改用全部 {n} 个源。可在书源设置里跑一次体检生成精选。",
                        n = plan.source_total
                    )
                } else {
                    format!(
                        "精选集与常用源暂无可用条目，已改用全部 {n} 个源。",
                        n = plan.source_total
                    )
                }),
                _ => None,
            }
        };
        if let Some(note) = &fallback_note {
            list.push_str(&format!(
                "<p style=\"font-size:20px;line-height:32px;color:#555;padding:8px 36px 0\">{note}</p>"
            ));
        }
        // 结果不足（0 条或 strong < 5）时给「用全部源搜索」的主动升级入口，不是默认路径。
        let upgrade_link = plan.scope == crate::req::SearchScope::Curated
            && (books.is_empty() || strong_total < crate::search_rank::STRONG_MIN_RESULTS);
        // 回退说明 / 升级按钮要占纵向空间：多一行时少放一条结果，避免被贴底栏盖住。
        let per_page = search_page_size(vp, fallback_note.is_some() || upgrade_link);
        if books.is_empty() {
            if plan.source_total == 0 {
                list.push_str(
                    "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">\
                     当前没有可搜索的源，请先在书源设置中导入并启用。</p>",
                );
            } else {
                list.push_str(
                    "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">没有找到结果</p>",
                );
            }
        } else {
            let t_pages = books.len().div_ceil(per_page).max(1);
            let sp = q.sp.unwrap_or(1).clamp(1, t_pages as i64) as usize;
            let start = (sp - 1) * per_page;
            let end = (start + per_page).min(books.len());
            list.push_str(&format!(
                "<table width=\"{iw}\" cellpadding=\"0\" cellspacing=\"0\">",
                iw = vp.page_w()
            ));
            for b in &books[start..end] {
                let src = b.origin.clone().unwrap_or_default();
                let href = qlink(
                    &format!("/k/toc?book={}&source={}", qenc(&b.book_url), qenc(&src)),
                    fs,
                    vp,
                );
                let name = b.name.as_deref().unwrap_or("未命名");
                let first = name.chars().next().unwrap_or('书');
                // 沿用现有副标题位置（字号/颜色不变）：多源共识显示「N 个源」，
                // partial 档追加「可能相关」，不与正名结果混在一起。
                let mut sub = format!(
                    "{} · {}",
                    b.author.as_deref().unwrap_or("未知作者"),
                    b.origin_name.as_deref().unwrap_or("未知来源")
                );
                if b.origins.len() > 1 {
                    sub.push_str(&format!(" · {} 个源", b.origins.len()));
                }
                if b.relevance.as_deref() == Some("partial") {
                    sub.push_str(" · 可能相关");
                }
                list.push_str(&format!(
                    "<tr><td style=\"padding:0\">\
                     <a href=\"{href}\" style=\"display:block;height:112px;border-bottom:1px solid #ccc\">\
                     <table width=\"{rw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                     <td width=\"56\" valign=\"middle\"><div style=\"width:56px;height:80px;border:1px solid #000;font-size:24px;line-height:78px;text-align:center\">{first}</div></td>\
                     <td style=\"padding-left:24px\" valign=\"middle\">\
                     <div style=\"font-size:32px;line-height:48px\">{name}</div>\
                     <div style=\"font-size:20px;line-height:32px;color:#555;margin-top:8px\">{sub}</div></td>\
                     </tr></table></a></td></tr>",
                    href = href,
                    rw = vp.page_w(),
                    first = esc(&first.to_string()),
                    name = esc(name),
                    sub = esc(&sub)
                ));
            }
            list.push_str("</table>");
            if t_pages > 1 {
                // 翻页必须带上实际生效的 scope，否则第二页会退回缺省的 curated。
                let base = format!(
                    "/k/search?key={}&fs={}&scope={}",
                    qenc(&key),
                    fs,
                    plan.scope.as_str()
                );
                let prev = if sp > 1 {
                    Some((with_vp(&format!("{base}&sp={}", sp - 1), vp), "上一页"))
                } else {
                    None
                };
                let next = if sp < t_pages {
                    Some((with_vp(&format!("{base}&sp={}", sp + 1), vp), "下一页"))
                } else {
                    None
                };
                pager = pager_bar(prev, next, vp);
            }
        }
        if upgrade_link {
            let href = with_fs(&format!("/k/search?key={}&scope=all", qenc(&key)), fs);
            list.push_str(&format!(
                "<div style=\"padding:12px 36px\">\
                 <a href=\"{href}\" style=\"display:inline-block;height:56px;line-height:56px;\
                 padding:0 24px;border:1px solid #000;font-size:24px;color:#000\">用全部源搜索</a></div>",
                href = esc(&href)
            ));
        }
    }
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{form}{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        form = form,
        list = list,
        pager = pager
    );
    let mut res = page("搜索", fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 真机视觉自测页（需登录；页面本体零 JS） ----------------

async fn selftest(_u: AuthUser) -> Response {
    let cell = |title: &str, demo: &str, expect: &str| {
        format!(
            "<tr><td width=\"250\" style=\"border:1px solid #000;padding:12px;vertical-align:top\">\
             <div style=\"font-size:26px;line-height:38px\">{t}</div>\
             <div style=\"font-size:22px;line-height:32px;color:#555\">{e}</div></td>\
             <td style=\"border:1px solid #000;padding:12px\">{d}</td></tr>",
            t = esc(title),
            e = esc(expect),
            d = demo
        )
    };
    let demo_table = "<table width=\"300\" cellpadding=\"0\" cellspacing=\"0\" style=\"border:1px solid #000\">\
        <tr><td height=\"60\" align=\"center\" style=\"background:#ccc;font-size:24px\">table 单元格</td>\
        <td height=\"60\" align=\"center\" style=\"background:#eee;font-size:24px\">第二格</td></tr></table>";
    let demo_float = "<div style=\"width:300px;height:120px;border:1px solid #000\">\
        <div style=\"float:left;width:140px;height:80px;background:#ccc;font-size:24px\">float 左</div>\
        <div style=\"float:right;width:140px;height:80px;background:#eee;font-size:24px\">float 右</div>\
        <div style=\"clear:both;font-size:22px\">清浮动行</div></div>";
    let demo_flex = "<div style=\"display:-webkit-box;display:box;display:flex;width:420px;height:80px;border:1px solid #000\">\
        <div style=\"-webkit-box-flex:1;width:140px;height:80px;background:#ccc;font-size:24px\">flex 1</div>\
        <div style=\"-webkit-box-flex:1;width:140px;height:80px;background:#eee;font-size:24px\">flex 2</div>\
        <div style=\"-webkit-box-flex:1;width:140px;height:80px;background:#ccc;font-size:24px\">flex 3</div></div>";
    let demo_vw =
        "<div style=\"width:20vw;height:80px;background:#ccc;font-size:24px\">20vw 宽（若无效则塌陷）</div>";
    let demo_radius = "<table cellpadding=\"0\" cellspacing=\"0\"><tr>\
        <td width=\"180\" height=\"90\" align=\"center\" style=\"background:#ccc;font-size:24px\">直角</td>\
        <td width=\"20\"></td>\
        <td width=\"180\" height=\"90\" align=\"center\" style=\"background:#ccc;border-radius:40px;font-size:24px\">圆角</td>\
        </tr></table>";
    let demo_fixed = "<div style=\"position:fixed;right:10px;bottom:10px;width:160px;height:80px;background:#000;color:#fff;font-size:24px;line-height:80px;text-align:center\">fixed 角标</div>\
        <div style=\"height:80px;font-size:24px\">若照片里角标贴屏，则 fixed 可用；若它留在文档流中，则不可用。</div>";

    let rows = format!(
        "{}{}{}{}{}{}",
        cell("table 布局", demo_table, "应显示：并排两格"),
        cell("float", demo_float, "应显示：左右各一块 + 清浮动行"),
        cell(
            "-webkit-box / flex",
            demo_flex,
            "预期：三块并排（flex 不可用时会竖排）"
        ),
        cell("vw 单位", demo_vw, "预期：20vw 不生效，宽度塌陷"),
        cell(
            "border-radius",
            demo_radius,
            "预期无效；若照片仍是圆角，说明探测假阴"
        ),
        cell(
            "position:fixed",
            demo_fixed,
            "预期不可靠；角标应留在文档流里"
        )
    );
    let body = format!(
        "<div style=\"height:96px;line-height:96px;background:#e6e6e6;padding:0 36px;font-size:30px\">\
         KPW3 视觉自测 · <a href=\"/k\">返回书架</a></div>\
         <table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">{rows}</table>"
    );
    page("KPW3 视觉自测", 2, &body, Vp::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/k` 默认 curated（首屏必须快）；只有显式 all/expanded 才走全量。
    #[test]
    fn k_scope_defaults_to_curated() {
        assert_eq!(k_scope(None), crate::req::SearchScope::Curated);
        assert_eq!(k_scope(Some("")), crate::req::SearchScope::Curated);
        assert_eq!(k_scope(Some("unknown")), crate::req::SearchScope::Curated);
        assert_eq!(k_scope(Some("curated")), crate::req::SearchScope::Curated);
        assert_eq!(k_scope(Some(" all ")), crate::req::SearchScope::All);
        assert_eq!(k_scope(Some("ENHANCED")), crate::req::SearchScope::All);
    }

    /// 默认 KPW3 视口（1072×1268）：所有历史几何断言都以它为基准。
    fn vp() -> Vp {
        Vp::default()
    }
    /// 指定尺寸的「已带参」视口（fixed = true，meta viewport 锁宽）。
    fn vps(w: usize, h: usize) -> Vp {
        Vp { w, h, fixed: true }
    }
    /// 某档字号在默认视口下的每页行预算。
    fn budget(spec: &FsSpec) -> usize {
        line_budget(vp(), spec.line)
    }
    /// 默认视口下的切片参数（与 `comic_read` 推出来的完全一致）。
    fn slice_p() -> image::SliceParams {
        image::SliceParams::from_config(
            &test_cfg(true),
            Some(vp().content_w() as u32),
            Some(vp().content_h() as u32),
            None,
        )
    }

    #[test]
    fn paginate_lines_respects_budget_and_char_boundaries() {
        // 行预算由视口高动态算出（标准档 1088/52 = 20 行）：前 4 段占 16 行，
        // 第 5 段再放会超预算，余文进第 2 页。
        let text = "字".repeat(100)
            + "\n"
            + &"字".repeat(100)
            + "\n"
            + &"字".repeat(100)
            + "\n"
            + &"字".repeat(100)
            + "\n"
            + &"字".repeat(100)
            + "\n"
            + &"字".repeat(100);
        let spec = &FS_SPECS[1];
        let lines = budget(spec);
        let pages = paginate_lines(&text, spec, lines, vp().page_w());
        assert_eq!(pages.len(), 2, "行数预算下 6 段应分 2 页");
        let p1 = &text[pages[0].0..pages[0].1];
        let p2 = &text[pages[1].0..pages[1].1];
        assert_eq!(
            p1.matches('字').count() + p2.matches('字').count(),
            600,
            "分页不应丢字"
        );
        // 行当量 = 文本行 + 段间 0.6 行，任何一页都不得超预算；第 1 页也不该大量留白
        let em = line_em(spec, vp().page_w());
        let gap = para_gap_px(spec.line) as f64 / spec.line as f64;
        let cost = |p: &str| -> f64 {
            let paras: Vec<&str> = p
                .split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let lines: usize = paras.iter().map(|s| para_lines(s, em)).sum();
            lines as f64 + gap * (paras.len().saturating_sub(1)) as f64
        };
        let c1 = cost(p1);
        let c2 = cost(p2);
        assert!(c1 <= lines as f64, "第 1 页行当量 {c1} 超预算");
        assert!(c2 <= lines as f64, "第 2 页行当量 {c2} 超预算");
        assert!(
            c1 >= lines as f64 - 5.0,
            "第 1 页填充率过低：{c1} / {lines}"
        );
        assert!(pages.windows(2).all(|w| w[0].1 <= w[1].0), "页区间不重叠");
        // 同一文本同一档位 → 分页结果恒定
        assert_eq!(paginate_lines(&text, spec, lines, vp().page_w()), pages);
    }

    #[test]
    fn paginate_empty_is_one_blank_page() {
        assert_eq!(
            paginate_lines("", &FS_SPECS[1], budget(&FS_SPECS[1]), vp().page_w()),
            vec![(0, 0)]
        );
    }

    /// 任意正文（含 ASCII/长英文词/全角标点）分页后，每页估算行数不得超过该档预算。
    #[test]
    fn no_page_exceeds_line_budget() {
        let mut text = String::new();
        for i in 0..80 {
            if i % 7 == 0 {
                text.push_str(&"A".repeat(120)); // 无空格长串，按 0.5em 折行
            } else if i % 5 == 0 {
                text.push_str(&format!("第{i}段，短句。"));
            } else {
                text.push_str(&"正文句子。".repeat(12));
            }
            text.push('\n');
        }
        for spec in &FS_SPECS {
            let em = line_em(spec, vp().page_w());
            let budget = budget(spec);
            let pages = paginate_lines(&text, spec, budget, vp().page_w());
            for &(s, e) in &pages {
                let piece = &text[s..e];
                let lines: usize = piece
                    .split('\n')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| para_lines(p, em))
                    .sum();
                assert!(
                    lines <= budget,
                    "fs={} 页内 {} 行超过预算 {}",
                    spec.size,
                    lines,
                    budget
                );
                assert!(piece.chars().count() > 0, "不应产生空页");
                assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
            }
            // 页首尾相接（去掉间隔换行），不丢字
            let joined: String = pages
                .iter()
                .map(|&(s, e)| text[s..e].replace('\n', ""))
                .collect();
            assert_eq!(joined, text.replace('\n', ""), "fs={} 分页丢字", spec.size);
        }
    }

    #[test]
    fn chunk_ranges_split_at_200k_chars() {
        let text = "字".repeat(300_000);
        let chunks = chunk_ranges(&text, K_CHUNK_CHARS);
        assert_eq!(chunks.len(), 2, "30 万字应切成 2 块");
        assert_eq!(&text[chunks[0].0..chunks[0].1].chars().count(), &200_000);
        assert_eq!(&text[chunks[1].0..chunks[1].1].chars().count(), &100_000);

        let plan = plan_pages(&text, &FS_SPECS[1], budget(&FS_SPECS[1]), vp().page_w());
        assert_eq!(plan.chunks.len(), 2);
        assert!(plan.pages.len() > 2);
        assert_eq!(plan.pages[0].0, 0);
        assert_eq!(plan.pages.last().unwrap().0, 1, "末页应属于第 2 块");
        assert!(plan
            .pages
            .iter()
            .all(|&(ci, s, e)| ci < 2 && s <= e && e <= text.len()));
    }

    #[test]
    fn truncate_to_chars_is_in_place_and_char_safe() {
        let mut text = "字".repeat(900_000);
        assert!(
            truncate_to_chars(&mut text, K_MAX_TEXT_CHARS),
            "90 万字应触发截断"
        );
        assert_eq!(text.chars().count(), K_MAX_TEXT_CHARS);
        assert!(text.is_char_boundary(text.len()));

        let mut short = "短".repeat(100);
        assert!(!truncate_to_chars(&mut short, K_MAX_TEXT_CHARS));
        assert_eq!(short.chars().count(), 100);
    }

    /// 目录含分卷行：总数只算非分卷章节，分页按过滤后的列表，链接 index 保持原始值。
    #[test]
    fn toc_skips_volumes_and_keeps_original_index() {
        let mut chapters = Vec::new();
        for i in 0..45_i64 {
            let is_volume = matches!(i, 0 | 10 | 20 | 30 | 40);
            chapters.push(BookChapter {
                url: if is_volume {
                    format!("volume:book#{i}")
                } else {
                    format!("https://example.test/ch/{i}")
                },
                title: Some(format!("第 {i} 条")),
                is_volume,
                index: i,
                ..Default::default()
            });
        }

        let visible = visible_chapters(&chapters);
        assert_eq!(visible.len(), 40);
        assert!(visible.iter().all(|c| !volume_entry(c)));
        assert_eq!(visible[0].index, 1);
        assert_eq!(visible[39].index, 44);

        let total = visible.len() as i64;
        let pages = (total + K_TOC_PAGE as i64 - 1) / K_TOC_PAGE as i64;
        assert_eq!(pages, 2, "40 条非分卷章节应分 2 页");
        let page1 = &visible[0..K_TOC_PAGE];
        let page2 = &visible[K_TOC_PAGE..visible.len()];
        assert_eq!(page1.len(), 30, "每页两列各 15 行");
        assert_eq!(page2.len(), 10);
        // 分页区间仍指向原 Vec 里的章节，index 单调且跳过被过滤的分卷下标。
        assert!(page1.windows(2).all(|w| w[0].index < w[1].index));
        assert!(page2.windows(2).all(|w| w[0].index < w[1].index));
        assert!(page1
            .iter()
            .all(|c| !matches!(c.index, 0 | 10 | 20 | 30 | 40)));
        assert_eq!(page1.last().unwrap().index, 33);
        assert_eq!(page2.first().unwrap().index, 34);
    }

    #[test]
    fn volume_entry_detects_flag_and_synthetic_url() {
        assert!(volume_entry(&BookChapter {
            is_volume: true,
            url: "https://example.test/ch/1".into(),
            ..Default::default()
        }));
        assert!(volume_entry(&BookChapter {
            is_volume: false,
            url: "volume:book#3".into(),
            ..Default::default()
        }));
        assert!(!volume_entry(&BookChapter {
            is_volume: false,
            url: "https://example.test/ch/3".into(),
            ..Default::default()
        }));
    }

    #[test]
    fn html_to_text_strips_tags() {
        let html = "<p>第一段&nbsp;内容</p><p>第二段<br>换行</p><div class=\"x\">尾</div>";
        let t = html_to_text(html);
        assert!(t.contains("第一段 内容"));
        assert!(t.contains("第二段\n换行"));
        assert!(t.contains("尾"));
        assert!(!t.contains('<'));
    }

    /// 阅读页页面高 1244（=1268−24 安全余量）、可用 1172；四档行预算按
    /// 「正文可用高 − 56 顶栏 ÷ 行高」动态算出。
    #[test]
    fn font_specs_fit_read_stage() {
        assert_eq!(vp().h, 1268);
        assert_eq!(vp().layout_h(), 1244);
        assert_eq!(vp().content_h(), 1172);
        // 1072×1268 下表：1032/46=22、1032/52=19、1032/62=16、1032/36=28（已扣 56px 顶栏）
        let expect = [(28, 46, 22), (32, 52, 19), (38, 62, 16), (20, 36, 28)];
        assert_eq!(FS_SPECS.len(), 4, "应有四档字号");
        for (spec, (size, line, lines)) in FS_SPECS.iter().zip(expect) {
            assert_eq!((spec.size, spec.line), (size, line));
            assert_eq!(budget(spec), lines, "fs={size} 行预算错误");
            let per_line = (vp().page_w() - SIDE_PAD * 2) / size;
            let chars = lines * per_line;
            assert!(
                lines * spec.line <= vp().text_h(),
                "fs={size} 行高总和 {} 超过正文区 {}",
                lines * spec.line,
                vp().text_h()
            );
            assert!(
                (350..=1500).contains(&chars),
                "fs={size} 每页字数 {chars} 超出合理区间（顶栏占 56px，最大档少 1 行）"
            );
        }
        // 标准档 32px：20 行 × 29 字/行 = 580 字，落在 500–800 的参考区间
        let std_chars = budget(&FS_SPECS[1]) * (vp().page_w() - SIDE_PAD * 2) / FS_SPECS[1].size;
        assert!(
            (500..=800).contains(&std_chars),
            "标准档每页字数应在 500–800，实际 {std_chars}"
        );
    }

    /// 字号轮换按物理大小 超小(4)→小(1)→标准(2)→大(3)→超小(4)，旧档位语义不变。
    #[test]
    fn font_cycle_keeps_existing_tiers() {
        assert_eq!(resolve_fs(&HeaderMap::new(), Some(4)).0, 4);
        assert_eq!(
            resolve_fs(&HeaderMap::new(), Some(9)).0,
            2,
            "越界回落标准档"
        );
        assert_eq!(cycle_fs(4), 1);
        assert_eq!(cycle_fs(1), 2);
        assert_eq!(cycle_fs(2), 3);
        assert_eq!(cycle_fs(3), 4);
    }

    /// /k 页面骨架：viewport 宽度随请求视口，唯一的内联脚本只做「缺参补参一次」。
    #[test]
    fn viewport_is_fixed_and_page_is_self_contained() {
        let html = page_html("测试", 2, "<div>正文</div>", vp());
        assert!(
            html.contains("<meta name=\"viewport\" content=\"width=1072,initial-scale=1"),
            "缺少 viewport meta"
        );
        // 视图宽跟着请求参数走：同一骨架换 1024 视口，viewport 也要跟着变
        let html_small = page_html("测试", 2, "<div>正文</div>", vps(1024, 1200));
        assert!(
            html_small.contains("content=\"width=1024,initial-scale=1"),
            "{html_small}"
        );
        // 缺参首屏：meta 必须放开给设备真实宽度（否则 innerWidth 恒等 vw，补参永远量不出真尺寸）
        let html_naked = page_html(
            "测试",
            2,
            "<div>正文</div>",
            Vp::resolve(None, None, (K_W, K_H)),
        );
        assert!(
            html_naked.contains("content=\"width=device-width,initial-scale=1"),
            "{html_naked}"
        );
        // 三个脚本：文档高兜底（head 内、按实测 clientHeight）+ 视口补参（带 kfix
        // 防循环标记）+ 自报探针（只上报不重载）
        assert_eq!(
            html.matches("<script").count(),
            3,
            "只允许文档高兜底、补参脚本与自报探针"
        );
        assert!(
            html.contains("d.style.height=(H-24)+'px'"),
            "文档高必须按实测 clientHeight 减 24px：{html}"
        );
        assert!(
            html.contains("</style><script>(function(){"),
            "文档高兜底脚本必须进 head，首帧就生效"
        );
        assert!(html.contains("kfix=1"), "补参脚本必须带防循环标记");
        assert!(
            html.contains("/api/k/probe?"),
            "探针必须上报到 /api/k/probe"
        );
        for f in [
            "screen_w=",
            "screen_h=",
            "avail_h=",
            "outer_w=",
            "outer_h=",
            "body_h=",
            "stage_h=",
        ] {
            assert!(K_PROBE_JS.contains(f), "探针 v2 字段缺失：{f}");
        }
        assert!(
            K_PROBE_JS.contains("getElementById('k-stage')"),
            "stage_h 必须取 #k-stage 的 clientHeight"
        );
        assert!(
            !K_PROBE_JS.contains("Promise") && !K_PROBE_JS.contains("fetch("),
            "探针必须是 ES5（XHR，不用 Promise/fetch）"
        );
        assert!(
            !K_PROBE_JS.contains("location.replace") && !K_PROBE_JS.contains("location.href"),
            "探针不得改写 location（不能引起额外重载）"
        );
        assert!(
            html.contains("location.replace"),
            "补参用 replace，不产生历史记录"
        );
        assert!(!html.contains("<link"), "不允许子资源");
        assert!(!html.contains("<img"), "不允许子资源");
    }

    /// 入口门禁只看 HTTP UA：真机 Kindle/3.0+ 命中；页面内 JS 的 534.26+ 不命中。
    #[test]
    fn kindle_ua_gate_only_matches_kindle_and_silk() {
        assert!(is_kindle_ua(
            "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+"
        ));
        assert!(is_kindle_ua(
            "Mozilla/5.0 (Linux; Android 4.4; KFAPWI Build/KTU84P) AppleWebKit/537.36 Silk/3.68 like Chrome/34.0"
        ));
        assert!(!is_kindle_ua(
            "AppleWebKit/534.26+ (KHTML, like Gecko) Version/5.0 Safari/534.26+"
        ));
        assert!(!is_kindle_ua(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15"
        ));
        assert!(!is_kindle_ua(""));
    }

    /// 跳转范围：`/` 与 SPA 入口跳；`/k` 自身、API、健康检查、带扩展名的静态资源不跳。
    #[test]
    fn spa_entry_gate_excludes_k_page_and_assets() {
        assert!(is_spa_entry("/"));
        assert!(is_spa_entry("/index.html"));
        assert!(is_spa_entry("/reader"));
        assert!(!is_spa_entry("/k"));
        assert!(!is_spa_entry("/k/read"));
        assert!(!is_spa_entry("/api/sources"));
        assert!(!is_spa_entry("/healthz"));
        assert!(!is_spa_entry("/assets/index-CyUAYOCA.js"));
        assert!(!is_spa_entry("/favicon.ico"));
    }

    /// 版式硬约束：底栏 64 不压正文、正文区 = 页面高（1268−24）− 56（顶栏）− 64 − 36（顶部留白）
    /// − 56（页码行）；标准档显式 32/52。
    #[test]
    fn fixed_bar_keeps_content_clear_of_bar() {
        assert_eq!(vp().body_h(), vp().h - SAFE_BOTTOM - BAR_H);
        assert_eq!(
            vp().text_h(),
            vp().body_h() - READ_NAV_H - READ_PAD_TOP - READ_PAGENO_H
        );
        assert!(vp().text_h() <= vp().content_h(), "正文不得越过一屏可用高");
        let spec = &FS_SPECS[1];
        assert_eq!(
            spec.size, 32,
            "标准档必须显式 32px（实测 32/52，勿低于 30）"
        );
        assert_eq!(spec.line, 52);
        let per_line = (vp().page_w() - SIDE_PAD * 2) / spec.size;
        let chars = budget(spec) * per_line;
        assert!(
            (500..=800).contains(&chars),
            "默认档每页字数应在 500–800，实际 {}",
            chars
        );
        assert!(budget(spec) <= vp().text_h() / spec.line);
    }

    /// 四档行预算全部由视口高动态算出（已扣 56px 顶栏）：
    /// 1072×1268 → 22/19/16/28，1024×1200 同档位按更低可用高再算一遍（20/18/15/26），
    /// 且总高始终 ≤ 可用高。
    #[test]
    fn read_line_budget_matches_reference() {
        assert_eq!(
            FS_SPECS
                .iter()
                .map(|s| line_budget(vp(), s.line))
                .collect::<Vec<_>>(),
            vec![22, 19, 16, 28]
        );
        for s in FS_SPECS.iter() {
            assert!(budget(s) * s.line <= vp().text_h(), "{}px 行高溢出", s.line);
            assert!(budget(s) * s.line <= vp().content_h());
        }
        // 换尺寸：行预算随可用高变化，不会沿用 1072×1268 的常量
        let small = vps(1024, 1200);
        let small_lines: Vec<usize> = FS_SPECS
            .iter()
            .map(|s| line_budget(small, s.line))
            .collect();
        assert_eq!(
            small_lines,
            vec![20, 18, 15, 26],
            "1024×1200 行预算应按可用高重算"
        );
        for s in FS_SPECS.iter() {
            assert!(line_budget(small, s.line) * s.line <= small.text_h());
        }
    }

    /// 列表页（书架/目录/搜索）每页条数按「页面高 − 顶栏 − 分页条 − 安全余量」实算：
    /// 1072×1268 与 1024×1200 两档都必须装得下，也不允许多留一整行。
    #[test]
    fn list_pages_fit_inside_stage_at_reference_viewports() {
        // 1268：正文区 1171；书架 10×110+64=1164、目录 15×72+64=1144、搜索 9×112+152=1160
        let v = vp();
        assert_eq!((v.nav_h(), v.shelf_layout()), (1171, (10, 110)));
        assert_eq!(toc_page_size(v), 30);
        assert_eq!(search_page_size(v, false), 9);
        let s = vps(1024, 1200);
        assert_eq!((s.nav_h(), s.shelf_layout()), (1103, (10, 103)));
        assert_eq!(toc_page_size(s), 28);
        assert_eq!(search_page_size(s, false), 8);
        for vp in [v, s] {
            let (pp, rh) = vp.shelf_layout();
            assert!(pp * rh + BAR_H <= vp.nav_h(), "书架列表溢出正文区");
            assert!(
                (pp + 1) * rh + BAR_H > vp.nav_h(),
                "书架应贴近填满，别多留一整行"
            );
            let t = toc_page_size(vp);
            assert!(t / K_TOC_COLS * 72 + BAR_H <= vp.nav_h(), "目录溢出");
            let q = search_page_size(vp, false);
            assert!(q * 112 + BAR_H + 88 <= vp.nav_h(), "搜索结果溢出");
        }
        // 极矮视口：行高触底后减少每页本数，绝不溢出
        let tiny = vps(1072, 600);
        let (pp, rh) = tiny.shelf_layout();
        assert!(pp * rh + BAR_H <= tiny.nav_h(), "极矮视口书架溢出");
    }

    /// 一屏一页（不依赖参数视口是否准确）：`#k-stage` 绝对定位填满「视口 − 88」，
    /// 底栏贴 `bottom:0` 且内置 24px 安全区；文档高由 `fit_js()` 按实测
    /// `clientHeight − 24` 写死，永远比视口矮，不会出现第二屏/滚动条。
    #[test]
    fn read_page_locks_one_screen_and_hides_overflow() {
        let css = read_only_css();
        assert!(
            css.contains("html,body{height:100%;overflow:hidden}"),
            "{css}"
        );
        // 老引擎可能在「文档高 == 视口高」时仍画滚动条：根元素与正文区都要显式关掉滚动条绘制
        let html_css = page_html_css("测试", 2, "", "", vp());
        assert!(
            html_css.contains("#k-stage::-webkit-scrollbar{width:0;height:0;display:none}"),
            "必须显式关闭滚动条绘制：{html_css}"
        );
        // 兜底脚本以实测 clientHeight 为准，且不依赖 URL 参数、不重载
        let fit = fit_js();
        assert!(fit.contains("d.clientHeight||innerHeight"), "{fit}");
        assert!(fit.contains("(H-24)+'px'"), "{fit}");
        assert!(
            !fit.contains("location") && !fit.contains("replace"),
            "文档高兜底不得改写 location：{fit}"
        );
        assert!(
            css.contains("#k-stage{position:absolute;top:56px;left:36px;right:36px;bottom:88px;padding:36px 36px 0;overflow:hidden}"),
            "正文区必须从 56px 顶栏下沿开始：{css}"
        );
        assert!(
            css.contains("#k-read-nav{position:fixed;top:0;left:0;right:0;height:56px;padding:0 36px;background:#fff;border-bottom:1px solid #000;z-index:12}"),
            "顶栏必须固定贴顶、白底 1px 分隔线，且 z-index 高于热区(5)：{css}"
        );
        assert!(
            !css.contains("height:1180px"),
            "正文容器不得再写死像素高：{css}"
        );
        let html = page_html_css("测试", 2, "<div id=\"k-stage\"></div>", &css, vp());
        assert!(html.contains("html,body{height:100%;overflow:hidden}"));
        assert!(
            html.contains("html{-webkit-text-size-adjust:none;height:100%;overflow:hidden}"),
            "{html}"
        );
        assert!(
            html.contains("body{height:100%;overflow:hidden;"),
            "body 也必须撑满视口：{html}"
        );
        let bar = fixed_bar("", vp());
        assert!(
            bar.contains("position:fixed;left:0;right:0;bottom:0;height:88px;padding-bottom:24px"),
            "底栏必须贴视口底并内置 {SAFE_BOTTOM}px 安全区：{bar}"
        );
        assert!(!bar.contains("width:1072px"), "底栏宽度不得写死像素：{bar}");
    }

    /// 阅读页顶栏：左「← 书架」回 `/k`、右「目录」回目录页，两者都透传当前 fs/vw/vh；
    /// 书名按像素截断成单行（超长加 `…`），z-index 12 高于翻页热区 5。
    #[test]
    fn read_page_has_nav_with_shelf_entry_and_params() {
        let v = vp();
        let back = qlink("/k", 2, v);
        let toc = qlink("/k/toc", 2, v);
        assert!(
            back.contains("fs=2") && back.contains("vw=1072") && back.contains("vh=1268"),
            "书架入口必须透传参数：{back}"
        );
        let long = "一个特别特别长的书名".repeat(6);
        let nav = read_nav_html(&long, &back, &toc, v);
        assert!(nav.contains("id=\"k-read-nav\""), "{nav}");
        assert!(nav.contains("&larr; 书架"), "必须有返回书架入口：{nav}");
        assert!(nav.contains("目录"), "必须有目录入口：{nav}");
        assert!(nav.contains(&format!("href=\"{}\"", back)), "{nav}");
        assert!(nav.contains(&format!("href=\"{}\"", toc)), "{nav}");
        assert!(
            nav.contains('…'),
            "超长书名必须按像素截断并用省略号收尾：{nav}"
        );
        let short = read_nav_html("三体", &back, &toc, v);
        assert!(!short.contains('…'), "短书名不得加省略号：{short}");
        assert!(short.contains(">三体<"), "{short}");
        // 顶栏整格可点（链接 display:block 撑满 55px），热区又让开 56px
        assert!(
            read_only_css().contains("z-index:12"),
            "顶栏须高于热区/底栏"
        );
    }

    /// 顶栏只有一条、白底 + 1px #ccc，不再有整条灰底；书架/目录/搜索页不再渲染阅读页底栏。
    #[test]
    fn top_nav_is_single_and_has_no_gray_fill() {
        let nav = top_bar("书架", None, &nav_link("/k/search", "搜索"), vp());
        assert!(nav.contains("height:73px;border-bottom:1px solid #ccc;background:#fff"));
        assert!(nav.contains("font-size:28px;line-height:72px;font-weight:700"));
        assert!(nav.contains("font-size:20px"), "动作链接 20px");
        assert!(nav.contains("padding-left:32px"), "动作间距 32px");
        assert!(!nav.contains("e6e6e6"), "不得再用灰底");
        assert_eq!(nav.matches("id=\"k-nav\"").count(), 1, "顶栏只能有一条");
    }

    /// 空书架：1px 描边块 + 满区居中（table 100% + valign=middle）+ 240×72 描边按钮。
    #[test]
    fn empty_shelf_is_outlined_and_centered() {
        let html = empty_shelf(2, vp());
        assert!(html.contains("border:1px solid #000"), "{html}");
        assert!(html.contains("width=\"1000\" height=\"1171\""), "{html}");
        assert!(
            html.contains("align=\"center\" valign=\"middle\""),
            "{html}"
        );
        assert!(html.contains("书架还是空的"), "{html}");
        assert!(html.contains("width:240px;height:72px;line-height:70px;border:1px solid #000"));
    }

    /// 底栏 5 格等宽（各 200 = 20%）、64 高、图标 28 + 文字 20，禁用态用 #555 且不可点。
    #[test]
    fn bottom_bar_has_five_equal_cells() {
        let row = format!(
            "{}{}{}{}{}",
            bar_cell("/k?a", "前", "上一页"),
            bar_cell_off("后", "下一页"),
            bar_cell("/k?b", "目", "目录"),
            bar_cell("/k?c", "字", "字号"),
            progress_form(
                &ReadQ {
                    book: "b".into(),
                    source: "s".into(),
                    ch: None,
                    p: None,
                    fs: None,
                    vw: None,
                    vh: None,
                },
                0,
                1,
                1,
                1,
                2,
                vp()
            )
        );
        let bar = fixed_bar(&row, vp());
        assert_eq!(bar.matches("<td ").count(), 5, "{bar}");
        assert_eq!(bar.matches("width=\"20%\"").count(), 5, "{bar}");
        assert_eq!(bar.matches("height=\"64\"").count(), 5, "{bar}");
        assert!(bar.contains("width:28px;height:28px"));
        assert!(
            bar.contains("font-size:28px;line-height:28px;font-weight:700"),
            "{bar}"
        );
        assert!(!bar.contains("border:1px solid #000"), "字符不得再套方框");
        assert!(bar.contains("<a href=\"/k?a\""), "上一页必须是链接");
        assert_eq!(
            bar.matches("<a ").count(),
            3,
            "上/下/目录/字号里只有 3 个 <a>，记录是 form"
        );
        assert!(bar.contains("color:#555"), "禁用态用弱灰 #555");
        assert!(!bar.contains("background:#e6e6e6"));
    }

    /// 左右热区：CSS `top:56px;bottom:0;width:33%`（从顶栏下沿开始）跟随真实视口
    /// （1072 → 353），无下一页时右区不渲染。
    #[test]
    fn hot_zones_are_one_third_screen_and_clickable() {
        assert_eq!(vp().hot_w(), 1072 / 3);
        assert_eq!(vp().w - vp().hot_w(), 1072 - vp().hot_w());
        assert!(vp().hot_w() >= 200, "热区宽必须 ≥ 200px");
        assert!(vp().h >= 200, "热区高必须 ≥ 200px");
        let z = hot_zones(Some("/k/read?p=1"), Some("/k/read?p=2"));
        assert_eq!(z.matches("<a ").count(), 2);
        assert!(z.contains("left:0"), "{z}");
        assert!(z.contains("right:0"), "{z}");
        assert!(
            z.contains("top:56px;bottom:0;width:33%") && !z.contains("height:"),
            "热区必须从顶栏下沿跟随真实视口（top/bottom + 33%），不得写死像素：{z}"
        );
        // 顶栏整格可点，热区 z-index 5 < 顶栏 12：点书名/「书架」不会落到翻页区
        assert!(z.contains("z-index:5"), "{z}");
        assert_eq!(
            hot_zones(Some("/k/read?p=1"), None).matches("<a ").count(),
            1
        );
        assert_eq!(hot_zones(None, None).matches("<a ").count(), 0);
    }

    /// `vw`/`vh` 参数优先，缺参回落默认 1072×1268，越界值直接忽略。
    #[test]
    fn viewport_params_override_defaults_and_clamp() {
        assert_eq!(
            Vp::resolve(Some(1024), Some(1200), (K_W, K_H)),
            vps(1024, 1200)
        );
        // 缺参：几何回默认，但 fixed = false（meta 放开 device-width 才能量出真实尺寸）
        let naked = Vp::resolve(None, None, (K_W, K_H));
        assert_eq!((naked.w, naked.h), (K_W, K_H));
        assert!(!naked.fixed, "缺参首屏不得锁宽");
        // 越界/非法尺寸不许把版心算成负数
        let bad = Vp::resolve(Some(100), Some(5000), (K_W, K_H));
        assert_eq!((bad.w, bad.h), (K_W, K_H));
        for (w, h) in [(1024usize, 1200usize), (800, 600), (1072, 1268)] {
            let v = vps(w, h);
            assert!(v.content_w() >= 320, "版心宽不能小于 320");
            assert!(v.content_h() >= 320, "可用高不能小于 320");
            assert_eq!(v.content_w(), w - 144);
            assert_eq!(v.content_h(), h - SAFE_BOTTOM - 72);
            assert_eq!(
                v.text_h(),
                h - SAFE_BOTTOM - READ_NAV_H - 64 - 36 - 56,
                "正文可用高必须扣掉 56px 顶栏"
            );
        }
    }

    /// 翻页/换章链接必须把 vw/vh 原样带下去（客户端零测量）。
    #[test]
    fn pagination_links_carry_viewport_params() {
        let q = ReadQ {
            book: "b".into(),
            source: "s".into(),
            ch: Some(3),
            p: Some(2),
            fs: Some(2),
            vw: None,
            vh: None,
        };
        let next = read_link(&q, 2, 3, 3, vp());
        assert!(
            next.contains("vw=1072") && next.contains("vh=1268"),
            "{next}"
        );
        let toc = toc_link(&q, 2, 3, vp());
        assert!(toc.contains("vw=1072") && toc.contains("vh=1268"), "{toc}");
        // 换尺寸后链接里的参数跟着变
        let small = read_link(&q, 2, 3, 3, vps(1024, 1200));
        assert!(
            small.contains("vw=1024") && small.contains("vh=1200"),
            "{small}"
        );
        assert!(!small.contains("vw=1072"), "不得残留默认尺寸");
    }

    /// 测试用配置：切片参数取默认值，开关按用例需要打开/关闭。
    fn test_cfg(slice: bool) -> crate::config::Config {
        let mut cfg = crate::config::Config::from_env();
        cfg.comic_slice = slice;
        cfg.comic_slice_dw = 928;
        cfg.comic_slice_vh = 1196;
        cfg.comic_slice_ov = 64;
        cfg.comic_slice_mu = 200;
        cfg.comic_slice_sd = 30;
        cfg.comic_slice_r = 96;
        cfg
    }

    /// 漫画页表：长条页按窗口拆片（非末片 1196、末片取剩余），一屏内整图 1 页且高 ≤ 1196。
    #[test]
    fn comic_pages_slice_long_strip_and_keep_one_screen() {
        let images: Vec<BookImage> = (0..3)
            .map(|i| BookImage {
                index: i + 1,
                url: format!("https://example.test/{i}.jpg"),
                width: Some(1200),
                height: Some(2266),
                ..Default::default()
            })
            .collect();
        let dims: Vec<Option<(u32, u32)>> = vec![Some((1200, 2266)); 3];
        let cfg = test_cfg(true);
        let sp = slice_p();
        let pages = comic_pages(&images, &dims, &cfg, &sp);
        assert_eq!(pages.len(), 6, "三张两片长条页 = 6 页");
        for (i, pg) in pages.iter().enumerate() {
            assert_eq!(pg.layers, 2, "第 {i} 页片数");
            assert_eq!(pg.slice, i % 2, "第 {i} 页片下标");
            assert_eq!(pg.width, 928, "版心宽固定 928");
            // 切片真实高度由服务端落点决定，页表只给宽度（≠0 才写 height，避免拉扁）
            assert_eq!(pg.height, 0, "第 {i} 页不写死切片高度");
        }
        // 开关关闭：同一批长条页退回「一图一页」，且不得带切片参数
        let off = comic_pages(&images, &dims, &test_cfg(false), &sp);
        assert_eq!(off.len(), 3, "关闭时三张图各一页");
        assert!(off.iter().all(|p| p.layers == 1 && p.slice == 0));
        assert_eq!(off[0].height, vp().content_h(), "整图按高度约束缩到一屏");
        // 1.1 屏（1024×1471）：不切，一屏一页，按高度约束等比缩小
        let one = vec![BookImage {
            index: 1,
            url: "https://example.test/one.jpg".into(),
            width: Some(1024),
            height: Some(1471),
            ..Default::default()
        }];
        let one_dims = vec![Some((1024, 1471))];
        let p1 = comic_pages(&one, &one_dims, &cfg, &sp);
        assert_eq!(p1.len(), 1, "1.1 屏不切");
        assert_eq!(p1[0].layers, 1);
        assert_eq!(p1[0].height, vp().content_h());
        assert!(p1[0].width <= 928, "等比缩小后不得宽于版心");
        // 尺寸未知：整图一页，height=0（渲染退化为 max-height）
        let unknown = vec![BookImage {
            index: 1,
            url: "https://example.test/x.jpg".into(),
            ..Default::default()
        }];
        let u = comic_pages(&unknown, &[None], &cfg, &sp);
        assert_eq!(u.len(), 1);
        assert_eq!((u[0].layers, u[0].height), (1, 0));
    }

    /// 整图页 URL 不带切片参数（桌面端/`slice=0` 路径字节不变）；长条页才带 `slice=1&i=k`。
    #[test]
    fn comic_stage_uses_explicit_height_and_slice_only_for_long_pages() {
        let images = vec![BookImage {
            index: 1,
            url: "https://example.test/a.jpg".into(),
            width: Some(1024),
            height: Some(1200),
            ..Default::default()
        }];
        let sp0 = slice_p();
        let pages = comic_pages(&images, &[Some((1024, 1200))], &test_cfg(true), &sp0);
        assert_eq!(pages.len(), 1);
        let html = comic_stage(
            &images,
            &pages[0],
            "src",
            "book",
            "https://example.test/ch",
            "1/1",
            vp(),
            &sp0,
        );
        assert!(!html.contains("slice=1"), "整图页不得带切片参数");
        // 1024×1200 缩到 928 宽 → 高 1088（≤1172 漫画可用高），显式宽高防抖动
        assert!(html.contains("width=\"928\" height=\"1088\""), "{html}");
        assert!(html.contains("max-width:100%"), "禁止横向溢出");
        // 漫画页保留像素高（1244 − 56 顶栏 = 1188），并整体下移到顶栏下沿
        assert!(html.contains("id=\"k-stage\"") && html.contains("height:1188px"));
        // 漫画页同样清零 read_only_css 的绝对定位偏移，否则整页会被 right 挤向右
        assert!(
            html.contains(
                "position:relative;top:0;left:0;right:auto;bottom:auto;margin-top:56px;height:1188px"
            ),
            "{html}"
        );
        // 短图在开关开/关两种模式下 HTML 必须逐字节一致（回归要求）
        let off_html = {
            let off_pages = comic_pages(&images, &[Some((1024, 1200))], &test_cfg(false), &sp0);
            comic_stage(
                &images,
                &off_pages[0],
                "src",
                "book",
                "https://example.test/ch",
                "1/1",
                vp(),
                &sp0,
            )
        };
        assert_eq!(html, off_html, "短图在切片开关两种模式下必须字节一致");

        let strip = vec![BookImage {
            index: 1,
            url: "https://example.test/s.jpg".into(),
            width: Some(1200),
            height: Some(2266),
            ..Default::default()
        }];
        let sp1 = comic_pages(&strip, &[Some((1200, 2266))], &test_cfg(true), &sp0);
        assert_eq!(sp1.len(), 2);
        let s2 = comic_stage(
            &strip,
            &sp1[1],
            "src",
            "book",
            "https://example.test/ch",
            "2/2",
            vp(),
            &sp0,
        );
        assert!(s2.contains("slice=1&i=1"), "{s2}");
        // 切片 URL 必须带 dw/vh：缓存 key 按尺寸天然隔离（vh 取漫画可用高 1172）
        assert!(s2.contains("dw=928") && s2.contains("vh=1172"), "{s2}");
        assert!(s2.contains("width=\"928\""), "{s2}");
        assert!(
            !s2.contains("height=\""),
            "切片页不写死高度，避免与真实落点错位拉扁"
        );
    }
}
