//! Kindle 极简阅读端 `/k`：面向 KPW3 真机（视口 1072×1268、无 flex/grid、无现代 JS）。
//!
//! 真机硬约束（来自 KPW3 上报 JSON + 照片）：
//! - `innerWidth×innerHeight = 1072×1268`，且 `innerWidth == screen.width`：
//!   浏览器没有按 DPR 1.7964 缩放，**1 CSS px ≈ 1 物理 px**，字号必须按 px 直接给。
//! - 版心 1000px（左右各留 36px）；顶栏 73px（72 内容 + 1px 线）。
//! - **版面高 = 运行时现算**（`K_VP_JS` 写在 `<head>`，每进页面重算一次）：以
//!   `outerHeight − 1`（真机 1232 → 1231，最接近真实可用高）为主，`screen.availHeight`
//!   作上限保护，都拿不到时退到 `innerHeight`；算出的 `H` 写进 URL 的 `?vh=`（用
//!   `location.replace`，不落 Cookie / sessionStorage），服务端从 `vh` 取版面高，
//!   顶栏、底栏、正文切片全部从这一个值派生。`innerHeight/clientHeight = 1268` 只用于
//!   识别机型，绝不作版面高。
//! - **整屏分页**：文档高 = 运行时 `H`，正文区写死高度且 `overflow:hidden`，
//!   结构上没有可滚内容；**不做任何「隐藏/消除滚动条」的补偿**（若 Silk 仍自绘视口竖条，
//!   就让它留着）。翻页交互只用 `click`（左右三分热区 + 底栏按钮），不依赖滑动/touch。
//! - 只使用 table / block / inline-block + 服务端算好的像素；不用 flex/grid/vw/vh/calc/rem/CSS 变量，
//!   不用 `border-radius`；底栏与贴底元素用 `position:absolute`（相对 `body`，`body` 为
//!   `position:relative` 且高 = 运行时 `H`），不用 transform/transition。
//! - 三个内联脚本：错误兜底（`K_GUARD_JS`，注册 `window.onerror` + `__kerr` img 打点）、
//!   运行时视口标定（`K_VP_JS`，算 `H` 并补 `?vh=`）、自报探针（`K_PROBE_JS`，img 单通道）；
//!   其余翻页/切换字号全部是真实链接。
//! - 分页按字符切（`char_indices`），同一章同一档字号的页码恒定，与渲染无关。
//! - 登录：两个字段的 table 表单 + HttpOnly Cookie（`k_session`，30 天）；字号存 `k_fs` Cookie。

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    routing::get,
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
use crate::routes::bookshelf::{record_progress, SHELF_LIST_SQL};
use crate::routes::image;
use crate::routes::replace;
use crate::routes::sources::load_source;
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
/// 底栏内部的底部安全留白（12px）：底栏总高 = 64 内容 + 12 安全区，避免文字贴屏幕底。
const SAFE_BOTTOM: usize = 12;
/// **版面高不写死**（v0.3.22）：每次进页面由 `K_VP_JS` 在浏览器里现算可用高，写成
/// `?vh=<px>` 随请求带回，服务端 `layout_h()` 直接用这个运行时值排版/切片。
/// 可信度顺序：`outerHeight − 1`（真机 1232 → 1231）为主，`screen.availHeight` 做上限
/// 保护，两者都取不到才退 `innerHeight`；`clientHeight/innerHeight=1268` 只作机型识别，
/// 不当画布高。不落 Cookie、不写 sessionStorage，同机型每次重算结果一致。
/// 首屏（链接未带 `vh`）用 `Config.kindle_vh` 的默认值渲染一次，脚本在 `<head>` 里
/// 立刻 `location.replace` 到实测值，用户看不到错误版面。
///
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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/k", get(shelf))
        .route("/k/toc", get(toc))
        .route("/k/read", get(read))
        .route("/k/search", get(search))
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

// ---------------- 查询参数 ----------------

#[derive(Deserialize)]
pub struct TocQ {
    pub(super) book: String,
    pub(super) source: String,
    pub(super) tp: Option<i64>,
    pub(super) fs: Option<i64>,
    /// 当前所在章（从阅读页跳目录时带上，用于「▸ + 加粗」高亮）
    pub(super) ch: Option<i64>,
    /// 进入目录前阅读页的页码：右上角「✕」返回阅读页时原样带回，不重置到第一页
    pub(super) p: Option<i64>,
    /// 视口宽/高（CSS px）：请求参数优先，缺参回落环境默认
    pub(super) vw: Option<i64>,
    pub(super) vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct ReadQ {
    pub(super) book: String,
    pub(super) source: String,
    pub(super) ch: Option<i64>,
    pub(super) p: Option<i64>,
    pub(super) fs: Option<i64>,
    /// 视口宽/高（CSS px）：请求参数优先，缺参回落环境默认
    pub(super) vw: Option<i64>,
    pub(super) vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct SearchQ {
    pub(super) key: Option<String>,
    pub(super) fs: Option<i64>,
    pub(super) sp: Option<i64>,
    /// 已创建的后台搜索任务：有值时渲染进度/结果，不再重新发起搜索。
    pub(super) job: Option<String>,
    /// 搜索范围：`/k` 缺省 curated（首屏与预算更省）；显式 `?scope=all` 才走全量
    /// （由结果页的「用全部源搜索」链接升级）。
    pub(super) scope: Option<String>,
    /// 是否包含冷却中的源：缺省 0；验收/对拍时可显式 1。
    pub(super) include_cooled: Option<String>,
    pub(super) vw: Option<i64>,
    pub(super) vh: Option<i64>,
}

/// `/k` 的 scope 解析：只有显式 `all`/`expanded` 走全量，其余（含缺省、未知值）一律 curated。
pub(super) fn k_scope(raw: Option<&str>) -> crate::req::SearchScope {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("all" | "expanded" | "enhanced") => crate::req::SearchScope::All,
        _ => crate::req::SearchScope::Curated,
    }
}
#[derive(Deserialize)]
pub struct FsQ {
    pub(super) fs: Option<i64>,
    pub(super) sp: Option<i64>,
    pub(super) vw: Option<i64>,
    pub(super) vh: Option<i64>,
}
#[derive(Deserialize)]
pub struct LoginQ {
    pub(super) username: String,
    pub(super) password: String,
}

// ---------------- 数据辅助 ----------------

/// 解析请求视口：`vh` 参数（`K_VP_JS` 现算的可用高）优先，缺参回落环境默认。
/// **不再读 Cookie**：高度不做任何持久缓存，每次进页面重新现算。
fn vp_of(s: &AppState, _headers: &HeaderMap, vw: Option<i64>, vh: Option<i64>) -> Vp {
    Vp::resolve(vw, vh, (s.cfg.kindle_vw as usize, s.cfg.kindle_vh as usize))
}

// ---------------- 真机视觉自测页（需登录；页面本体零 JS） ----------------

pub(super) async fn selftest(_u: AuthUser) -> Response {
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

#[path = "kindle_vp.rs"]
mod kindle_vp;
use kindle_vp::*;
#[path = "kindle_pages.rs"]
mod kindle_pages;
use kindle_pages::*;
#[path = "kindle_shelf.rs"]
mod kindle_shelf;
use kindle_shelf::*;
#[path = "kindle_comic.rs"]
mod kindle_comic;
use kindle_comic::*;
#[path = "kindle_read.rs"]
mod kindle_read;
use kindle_read::*;
#[path = "kindle_comic_read.rs"]
mod kindle_comic_read;
use kindle_comic_read::*;
#[path = "kindle_search.rs"]
mod kindle_search;
use kindle_search::*;

#[cfg(test)]
#[path = "kindle_tests.rs"]
mod tests;
