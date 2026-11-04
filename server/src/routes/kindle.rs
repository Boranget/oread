//! Kindle 极简阅读端 `/k`：面向 KPW3 真机（视口 1072×1268、无 flex/grid、无现代 JS）。
//!
//! 真机硬约束（来自 KPW3 上报 JSON + 照片）：
//! - `innerWidth×innerHeight = 1072×1268`，且 `innerWidth == screen.width`：
//!   浏览器没有按 DPR 1.7964 缩放，**1 CSS px ≈ 1 物理 px**，字号必须按 px 直接给。
//! - 版心固定 1000px（左右各留 36px）；顶栏 96px、正文区 1172px（=1268−96），
//!   底部固定栏 64px 覆盖在正文区之上，正文容器留 72px 底 padding 保证不遮最后一行。
//! - 只用 table / block / inline-block + px；不用 flex/grid/vw/vh/calc/clamp/rem/CSS 变量，
//!   不用 `border-radius`（探测与照片冲突，暂缓）；底栏用 `position:fixed;bottom:0`
//!   （真机实测可用、rect 贴 (0,0)），不用 transform/transition；不引入任何 JS
//!   （翻页/切换字号/提交进度全部是真实链接或原生 form）。
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

/// 版心宽度（1072 视口 − 左右各 36px）
const PAGE_W: usize = 1000;
/// 版心左右留白
const SIDE_PAD: usize = 36;
/// 带顶栏页面（书架/目录/搜索）的正文区高度 = 1268 − 96（顶栏）= 1172
const STAGE_H: usize = 1172;
/// 阅读页无顶栏：整页 1268 全部给正文（WeRead Kindle 阅读页同样没有顶栏）
const READ_STAGE_H: usize = 1268;
/// 底部固定栏高度（触控目标 64px，保守高于 44px 下限）
const BAR_H: usize = 64;
/// 正文容器底部留白 = 栏高 + 8px，保证固定栏不遮正文最后一行
const STAGE_PAD_BOTTOM: usize = BAR_H + 8;
/// 阅读页正文可用高度 = 1268 − 72 = 1196
const READ_STAGE_CONTENT_H: usize = READ_STAGE_H - STAGE_PAD_BOTTOM;
/// 左右透明翻页热区宽度 = 视口 1072 / 3（取整），整屏高可点
const HOT_W: usize = 1072 / 3;
/// 右侧热区左边界 = 1072 − 357
const HOT_LEFT_R: usize = 1072 - HOT_W;

/// 阅读页每页目标行数：≈26 行（WeRead 整页阅读观感）
const READ_TARGET_LINES: usize = 26;

/// 阅读页每页行数预算：目标 26 行，但不超过 1196px 物理可容纳的行数
/// （32/52 最多 23 行，38/62 最多 19 行；26 行只在 28/46 与 22/38 档成立）
const fn line_budget(line: usize) -> usize {
    let fit = READ_STAGE_CONTENT_H / line;
    if fit < READ_TARGET_LINES {
        fit
    } else {
        READ_TARGET_LINES
    }
}
/// 目录每页章节数：20 行 × 52px = 1040，一屏装得下（正文区 1108）
const K_TOC_PAGE: usize = 20;
/// /k 渲染前对正文纯文本的硬截断（防御超大章节把分页临时索引撑爆内存）
const K_MAX_TEXT_CHARS: usize = 800_000;
/// 超长章节分块：每块最多 20 万字符，按块分页，避免一次性按 80 万字构建全量页表
const K_CHUNK_CHARS: usize = 200_000;
/// 字号 Cookie（与 `?fs=` 二选一，显式参数优先）
const K_FS_COOKIE: &str = "k_fs";
/// 版心固定 1072（1000 + 左右各 36），必须 1:1 不缩放；Silk 等会缩放设备也按 1072 排版
const K_VIEWPORT: &str = "width=1072,initial-scale=1,maximum-scale=1,user-scalable=0";

/// 四档字号。`lines` = 每页行数预算（min(26, 正文区能装下的行数)），
/// 每页字符数 = `行数 × 每行字数`（由测试按版心宽度换算校验）。
struct FsSpec {
    size: usize,
    line: usize,
    lines: usize,
    label: &'static str,
}

const FS_SPECS: [FsSpec; 4] = [
    // 928/28 ≈ 33 字/行，1196/46 = 26 行 → 858 字（正好是 26 行目标档）。
    FsSpec { size: 28, line: 46, lines: line_budget(46), label: "小" },
    // 928/32 = 29 字/行，1196/52 = 23 行（26 行需 1352px，物理装不下）→ 667 字。
    // 字号口径：真机实测 32px/52px，保留 32px。此处显式写死 32，且全局
    // `-webkit-text-size-adjust:none` 已关闭老引擎的字号自动放大——实测 32/52，勿低于 30。
    FsSpec { size: 32, line: 52, lines: line_budget(52), label: "标准" },
    // 928/38 ≈ 24 字/行，1196/62 = 19 行（26 行需 1612px，物理装不下）→ 475 字。
    FsSpec { size: 38, line: 62, lines: line_budget(62), label: "大" },
    // 928/20 ≈ 46 字/行，1196/36 = 33 行 → 仍取 26 行，26×36=936px 装下 → 1196 字。
    FsSpec { size: 20, line: 36, lines: line_budget(36), label: "超小" },
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

/// 统一页面骨架：无脚本、固定版心、`-webkit-text-size-adjust:none` 防老引擎自动放大。
/// viewport 固定 1072（与版心一致）；不写 initial-scale=1 时 Silk 会按 device-width 缩放。
fn page_html(title: &str, fs: usize, body: &str) -> String {
    page_html_css(title, fs, body, "")
}

/// 与 `page_html` 相同，额外拼接一段页面级 CSS（阅读页用「一屏一页」锁高，其它页不受影响）。
fn page_html_css(title: &str, fs: usize, body: &str, extra_css: &str) -> String {
    let spec = &FS_SPECS[fs - 1];
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"{viewport}\"><title>{title}</title><style>\
html{{-webkit-text-size-adjust:none}}\
html,body{{margin:0;padding:0;background:#fff;color:#000;overflow-x:hidden}}\
body{{font-family:serif;font-size:{size}px;line-height:{line}px}}\
a{{color:#000;text-decoration:none}}\
h1,h2,h3,p{{margin:0}}\
table{{border-collapse:collapse}}\
.wrap{{width:{pw}px;margin-left:{pad}px}}\
{extra}\
</style></head><body><div class=\"wrap\">{body}</div></body></html>",
        viewport = K_VIEWPORT,
        title = esc(title),
        size = spec.size,
        line = spec.line,
        pw = PAGE_W,
        pad = SIDE_PAD,
        extra = extra_css,
        body = body
    )
}

fn page(title: &str, fs: usize, body: &str) -> Response {
    page_css(title, fs, body, "")
}

fn page_css(title: &str, fs: usize, body: &str, extra_css: &str) -> Response {
    let html = page_html_css(title, fs, body, extra_css);
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .expect("html response")
}

/// 顶栏（96px）：36px 标题行 + 60px 导航行（table 等分，每格 ≥120×60 可点）
fn top_bar(title_line: &str, row: &str) -> String {
    format!(
        "<div style=\"height:96px;overflow:hidden;background:#e6e6e6\">\
         <div style=\"height:36px;line-height:36px;font-size:26px;overflow:hidden;padding:0 36px\">{title_line}</div>\
         <table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\"><tr>{row}</tr></table></div>"
    )
}

/// 顶栏导航格：整块 `<a>`（60px 高，满足真机 60×60 热区下限）
fn nav_cell(href: &str, label: &str, width: usize) -> String {
    format!(
        "<td width=\"{width}%\" height=\"60\" align=\"center\">\
         <a href=\"{href}\" style=\"display:block;height:60px;line-height:60px;font-size:26px\">{label}</a></td>"
    )
}

/// 贴底固定栏：`position:fixed`（真机实测可用、rect 贴 (0,0)）+ table 等宽单元格。
/// 不用 flex/grid/transform/transition/vw/calc；`width:100%` 取视口宽度，避免横向滚动。
/// 高度 64px，单元格整块 `<a>` 可点（触控目标 ≥44px）。
fn fixed_bar(row: &str) -> String {
    format!(
        "<div style=\"position:fixed;left:0;bottom:0;width:100%;height:{h}px;background:#e6e6e6;z-index:10\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-left:{pad}px\"><tr>{row}</tr></table></div>",
        h = BAR_H,
        pw = PAGE_W,
        pad = SIDE_PAD,
        row = row
    )
}

fn bottom_cell(href: &str, label: &str, width: usize) -> String {
    format!(
        "<td width=\"{width}%\" height=\"64\" align=\"center\">\
         <a href=\"{href}\" style=\"display:block;height:64px;line-height:64px;font-size:28px\">{label}</a></td>"
    )
}

/// 底栏格：主标签一行 + 16px 进度小字一行（阅读页页码/分块进度，零脚本可见）
fn bottom_cell_meta(href: &str, label: &str, meta: &str, width: usize) -> String {
    format!(
        "<td width=\"{width}%\" height=\"64\" align=\"center\">\
         <a href=\"{href}\" style=\"display:block;height:64px;line-height:42px;font-size:28px\">{label}\
         <span style=\"display:block;line-height:20px;font-size:16px;color:#333\">{meta}</span></a></td>"
    )
}

/// 底栏格：禁用态（灰掉、不可点），用于「最后一页且没有下一章」时的「下一页」
fn bottom_cell_off(label: &str, width: usize) -> String {
    format!(
        "<td width=\"{width}%\" height=\"64\" align=\"center\">\
         <span style=\"display:block;height:64px;line-height:64px;font-size:28px;color:#999\">{label}</span></td>"
    )
}

/// 左右透明翻页热区：各占屏宽 1/3（357px）、整屏高 1268px。
/// `position:absolute` 相对初始包含块（`.wrap` 不是定位元素，故不受其裁剪），
/// `z-index:5` 高于正文（0）低于贴底栏（10）；点击面积 357×1268 ≫ 200×200。
fn hot_zones(prev: &str, next: Option<&str>) -> String {
    let mut out = format!(
        "<a href=\"{prev}\" style=\"position:absolute;top:0;left:0;width:{w}px;height:{h}px;z-index:5;display:block\"></a>",
        prev = prev,
        w = HOT_W,
        h = READ_STAGE_H
    );
    if let Some(nx) = next {
        out.push_str(&format!(
            "<a href=\"{nx}\" style=\"position:absolute;top:0;left:{l}px;width:{w}px;height:{h}px;z-index:5;display:block\"></a>",
            nx = nx,
            l = HOT_LEFT_R,
            w = HOT_W,
            h = READ_STAGE_H
        ));
    }
    out
}

fn read_only_css() -> String {
    format!(
        "html,body{{height:{h}px;overflow:hidden}}.wrap{{height:{h}px;overflow:hidden}}",
        h = READ_STAGE_H
    )
}

/// 阅读页内部链接：始终带目标档位的 `fs`，切档后不会被当前档位拼成重复参数。
fn read_link(q: &ReadQ, fs: usize, ch: i64, p: i64) -> String {
    with_fs(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            ch,
            p
        ),
        fs,
    )
}

/// 目录链接（按 20 章/页定位到当前章所在目录页）
fn toc_link(q: &ReadQ, fs: usize, ch: i64) -> String {
    with_fs(
        &format!(
            "/k/toc?book={}&source={}&tp={}",
            qenc(&q.book),
            qenc(&q.source),
            (ch / K_TOC_PAGE as i64) + 1
        ),
        fs,
    )
}

/// 翻页导航：翻完本章接上一章/下一章；全书末页返回 `None`（底栏禁用 + 右侧热区不渲染）。
fn read_nav(
    q: &ReadQ,
    fs: usize,
    ch: i64,
    p: i64,
    n: i64,
    total: i64,
    toc: &str,
) -> ((String, &'static str), Option<(String, &'static str)>) {
    let prev = if p > 1 {
        (read_link(q, fs, ch, p - 1), "上一页")
    } else if ch > 0 {
        (read_link(q, fs, ch - 1, 1), "上一章")
    } else {
        (toc.to_string(), "目录")
    };
    let next = if p < n {
        Some((read_link(q, fs, ch, p + 1), "下一页"))
    } else if ch + 1 < total {
        Some((read_link(q, fs, ch + 1, 1), "下一章"))
    } else {
        None
    };
    (prev, next)
}

/// 底栏第 2 格：有下一页/下一章则给链接，全书末页给灰色不可点的禁用态。
fn bottom_cell_next(next: Option<&(String, &'static str)>, width: usize) -> String {
    match next {
        Some((href, label)) => bottom_cell(href, label, width),
        None => bottom_cell_off("下一页", width),
    }
}

/// 进度提交：零 JS 的原生 form（底栏第 5 格）
fn progress_form(q: &ReadQ, ch: i64, p: i64, n: i64, total: usize, fs: usize) -> String {
    format!(
        "<form method=\"post\" action=\"/k/progress\" style=\"margin:0\">\
         <input type=\"hidden\" name=\"book\" value=\"{book}\">\
         <input type=\"hidden\" name=\"source\" value=\"{source}\">\
         <input type=\"hidden\" name=\"ch\" value=\"{ch}\">\
         <input type=\"hidden\" name=\"p\" value=\"{p}\">\
         <input type=\"hidden\" name=\"n\" value=\"{n}\">\
         <input type=\"hidden\" name=\"total\" value=\"{total}\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\">\
         <input type=\"submit\" value=\"记录\" style=\"font-size:26px;height:64px;width:100%;border:0;background:transparent\"></form>",
        book = esc(&q.book),
        source = esc(&q.source),
        ch = ch,
        p = p,
        n = n,
        total = total,
        fs = fs
    )
}

fn err_page(msg: &str) -> Response {
    page(
        "错误",
        2,
        &format!(
            "<div style=\"height:96px;line-height:96px;font-size:30px;background:#e6e6e6;padding:0 36px\">出错了</div>\
             <div style=\"height:1108px;padding:36px;box-sizing:border-box\"><p style=\"font-size:30px;line-height:52px\">{}</p>\
             <p style=\"font-size:30px;line-height:52px\"><a href=\"/k\">返回书架</a></p></div>",
            esc(msg)
        ),
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
fn volume_notice(q: &ReadQ, fs: usize, explicit: bool) -> Response {
    let back = with_fs(
        &format!("/k/toc?book={}&source={}", qenc(&q.book), qenc(&q.source)),
        fs,
    );
    let body = format!(
        "<div style=\"height:96px;line-height:96px;font-size:30px;background:#e6e6e6;padding:0 36px\">分卷标题</div>\
         <div style=\"height:1108px;padding:36px;box-sizing:border-box\"><p style=\"font-size:30px;line-height:52px\">该条为分卷标题，请从目录选择章节</p>\
         <p style=\"font-size:30px;line-height:52px\"><a href=\"{back}\">返回目录</a></p></div>"
    );
    let mut res = page("分卷标题", fs, &body);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 正文纯文本化与分页 ----------------

static RE_BLOCK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)<\s*(br|/p|/div|/li|/h[1-6]|p|div|li|h[1-6])[^>]*>").unwrap()
});
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

/// 每行可用 em 数（版心宽度 ÷ 字号）
fn line_em(spec: &FsSpec) -> f64 {
    (PAGE_W - SIDE_PAD * 2) as f64 / spec.size as f64
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
/// 返回每页的字节区间（相对 `text`），页边界只落在字符边界上。
fn paginate_lines(text: &str, spec: &FsSpec) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let em = line_em(spec);
    let budget = spec.lines.max(1);
    let mut pages: Vec<(usize, usize)> = Vec::new();
    let mut page_start: Option<usize> = None;
    let mut page_lines = 0usize;
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
            if page_lines >= budget {
                if let Some(ps) = page_start.take() {
                    pages.push((ps, page_end));
                }
                page_lines = 0;
            }
            let remaining = &text[rem_start..trim_end];
            let need = para_lines(remaining, em);
            let cap = budget - page_lines;
            if need <= cap {
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                page_lines += need;
                page_end = trim_end;
                rem_start = trim_end;
            } else {
                let (head, used) = split_para(remaining, cap, em);
                debug_assert!(head > 0 && used > 0);
                if head == 0 {
                    break;
                }
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                pages.push((page_start.take().unwrap(), rem_start + head));
                page_lines = 0;
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

fn plan_pages(text: &str, spec: &FsSpec) -> PagePlan {
    let chunks = chunk_ranges(text, K_CHUNK_CHARS);
    let mut pages = Vec::new();
    for (ci, &(cs, ce)) in chunks.iter().enumerate() {
        for (s, e) in paginate_lines(&text[cs..ce], spec) {
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
}
#[derive(Deserialize)]
pub struct ReadQ {
    book: String,
    source: String,
    ch: Option<i64>,
    p: Option<i64>,
    fs: Option<i64>,
}
#[derive(Deserialize)]
pub struct SearchQ {
    key: Option<String>,
    fs: Option<i64>,
}
#[derive(Deserialize)]
pub struct FsQ {
    fs: Option<i64>,
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
}

// ---------------- 数据辅助 ----------------

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
        "<p style=\"font-size:26px;line-height:46px;color:#900\">账号或密码错误，请重试</p>"
    } else {
        ""
    };
    page(
        "登录",
        2,
        &format!(
            "<div style=\"height:96px;line-height:96px;font-size:30px;background:#e6e6e6;padding:0 36px\">登录</div>\
             <div class=\"content\" style=\"height:1108px;padding:36px;box-sizing:border-box\"><p style=\"font-size:26px;line-height:46px;color:#444\">Kindle 极简阅读 · 请用网页端账号登录</p>\
             <p style=\"font-size:24px;line-height:42px;color:#555\">没有账号请先在电脑浏览器打开主站注册；忘记密码可在主站重置。</p>{}\
             <form method=\"post\" action=\"/k/login\">\
             <table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">\
             <tr><td width=\"140\" height=\"76\" style=\"font-size:26px\">账号</td>\
             <td><input type=\"text\" name=\"username\" style=\"font-size:26px;height:52px;width:700px\"></td></tr>\
             <tr><td height=\"76\" style=\"font-size:26px\">密码</td>\
             <td><input type=\"password\" name=\"password\" style=\"font-size:26px;height:52px;width:700px\"></td></tr>\
             <tr><td height=\"88\"></td>\
             <td><input type=\"submit\" value=\"登录\" style=\"font-size:28px;height:64px;width:200px\"></td></tr>\
             </table></form></div>",
            err
        ),
    )
}

async fn login_post(State(s): State<AppState>, Form(q): Form<LoginQ>) -> Response {
    let row: Option<(i64, String, String, i64, i64)> = match sqlx::query_as(
        "SELECT id, password_hash, role, disabled, token_version FROM users WHERE username = ?",
    )
    .bind(&q.username)
    .fetch_optional(&s.shared)
    .await
    {
        Ok(r) => r,
        Err(_) => None,
    };
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
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<FsQ>,
) -> Response {
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
    let fs_url = format!("/k?fs={next_fs}");
    let nav = format!(
        "{}{}{}",
        nav_cell(&with_fs("/k/search", fs), "搜索", 33),
        nav_cell(&fs_url, FS_SPECS[next_fs - 1].label, 33),
        nav_cell("/k/logout", "退出", 34),
    );
    let top = top_bar("书架", &nav);

    let mut list = String::from("<table class=\"content\" width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">");
    if rows.is_empty() {
        list.push_str(
            "<tr><td style=\"height:1108px;padding:36px\">\
             <p style=\"font-size:32px;line-height:52px\">书架为空，请先在主站添加书籍。</p></td></tr>",
        );
    }
    for b in &rows {
        let title = b.title.clone().unwrap_or_else(|| "未命名".into());
        let author = b.author.clone().unwrap_or_default();
        let src = b.source_id.clone().unwrap_or_default();
        let read = with_fs(
            &format!(
                "/k/read?book={}&source={}&ch={}&p=1",
                qenc(&b.book_id),
                qenc(&src),
                b.chapter_index
            ),
            fs,
        );
        let toc = with_fs(
            &format!("/k/toc?book={}&source={}", qenc(&b.book_id), qenc(&src)),
            fs,
        );
        list.push_str(&format!(
            "<tr><td style=\"height:92px;border-bottom:1px solid #999;padding:0 36px\">\
             <a href=\"{read}\" style=\"font-size:30px\">{title}</a>\
             <div style=\"font-size:24px;line-height:34px\">{author} · {pct:.0}% \
             <a href=\"{toc}\">目录</a></div></td></tr>",
            read = read,
            title = esc(&title),
            author = esc(&author),
            pct = b.reading_progress,
            toc = toc
        ));
    }
    list.push_str("</table>");
    list.push_str(
        "<p style=\"font-size:24px;line-height:40px;padding:0 36px;color:#555\">点书名直接续读，点“目录”进入章节列表。</p>",
    );

    let bottom = fixed_bar(&format!(
        "{}{}{}",
        bottom_cell(&with_fs("/k/search", fs), "搜索", 33),
        bottom_cell(&fs_url, FS_SPECS[next_fs - 1].label, 33),
        bottom_cell("/k/logout", "退出", 34),
    ));
    let mut res = page(
        "书架",
        fs,
        &format!("{top}{spacer}{bottom}", spacer = format!(
            "<div id=\"k-stage\" style=\"height:{h}px;padding:0 0 {pb}px;box-sizing:border-box;overflow:auto\">{list}</div>",
            h = STAGE_H,
            pb = STAGE_PAD_BOTTOM,
            list = list
        )),
    );
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
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let (_source, chapters) = match ensure_chapters(&s, &db.0, &q.source, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_to_page(e),
    };
    // 分卷标题不是可读章节：目录不展示、不计入总数与分页。
    // 保留 `BookChapter.index`（原始下标），链接仍按原 index 生成，/k/read 才能对得上。
    let visible = visible_chapters(&chapters);
    let total = visible.len() as i64;
    let t_pages = ((total + K_TOC_PAGE as i64 - 1) / K_TOC_PAGE as i64).max(1);
    let tp = q.tp.unwrap_or(1).clamp(1, t_pages);
    let start = ((tp - 1) * K_TOC_PAGE as i64) as usize;
    let end = (start + K_TOC_PAGE).min(visible.len());
    let name = load_book_meta(&db.0, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_else(|| "目录".into());

    let next_fs = cycle_fs(fs);
    let fs_href = with_fs(
        &format!(
            "/k/toc?book={}&source={}&tp={}",
            qenc(&q.book),
            qenc(&q.source),
            tp
        ),
        next_fs,
    );
    let nav = format!(
        "{}{}",
        nav_cell(&with_fs("/k", fs), "书架", 50),
        nav_cell(&fs_href, FS_SPECS[next_fs - 1].label, 50),
    );
    let top = top_bar(
        &format!(
            "{} <span style=\"font-size:22px;color:#555\">共 {total} 章 · {tp}/{t_pages} 页</span>",
            esc(&name)
        ),
        &nav,
    );

    let mut list = String::from("<table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">");
    for (i, c) in visible[start..end].iter().enumerate() {
        let bg = if i % 2 == 0 { "#ffffff" } else { "#f2f2f2" };
        let href = with_fs(
            &format!(
                "/k/read?book={}&source={}&ch={}&p=1",
                qenc(&q.book),
                qenc(&q.source),
                c.index
            ),
            fs,
        );
        list.push_str(&format!(
            "<tr style=\"background:{bg}\"><td height=\"52\" style=\"font-size:30px;line-height:52px;padding:0 36px\">\
             <a href=\"{href}\" style=\"display:block;height:52px;line-height:52px\">{title}</a></td></tr>",
            bg = bg,
            href = href,
            title = esc(c.title.as_deref().unwrap_or("未命名章节"))
        ));
    }
    list.push_str("</table>");

    let prev = with_fs(
        &format!(
            "/k/toc?book={}&source={}&tp={}",
            qenc(&q.book),
            qenc(&q.source),
            (tp - 1).max(1)
        ),
        fs,
    );
    let next = with_fs(
        &format!(
            "/k/toc?book={}&source={}&tp={}",
            qenc(&q.book),
            qenc(&q.source),
            (tp + 1).min(t_pages)
        ),
        fs,
    );
    let bottom = fixed_bar(&format!(
        "{}{}{}",
        bottom_cell(&prev, "上一页", 33),
        bottom_cell(&with_fs("/k", fs), "书架", 34),
        bottom_cell(&next, "下一页", 33),
    ));

    let mut res = page(
        &name,
        fs,
        &format!("{top}{stage}{bottom}", stage = format!(
            "<div id=\"k-stage\" style=\"height:{h}px;padding:0 0 {pb}px;box-sizing:border-box;overflow:auto\">{list}</div>",
            h = STAGE_H,
            pb = STAGE_PAD_BOTTOM,
            list = list
        )),
    );
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

/// 由图片列表与原始宽高构建页表，保证每页显示高 ≤ `READ_STAGE_CONTENT_H`。
/// - 长条页（`slice_count_window` > 1）：一图拆 `layers` 页，非末片 1196、末片取剩余；
/// - 一屏内：整图按版心 928 宽，显示高超过 1196 时按高度等比缩小；
/// - 尺寸未知：整图一页，只给宽度。
fn comic_pages(images: &[BookImage], dims: &[Option<(u32, u32)>]) -> Vec<ComicPage> {
    let sp = image::SliceParams::default();
    let dw = sp.dw as usize;
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
        let n = image::slice_count_window(w, h, &sp);
        if n > 1 {
            // 切片的真实高度由服务端按近白行落点决定（实测 1150/667，窗口预判只有 1196/620），
            // 写死 height 会把图拉扁；这里只给宽度，页面高度由固定 1268 的 stage 兜住。
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
            if scaled > READ_STAGE_CONTENT_H {
                // 1.1 屏这类「不切但一屏放不下」的整图：按高度约束等比缩小，仍是一屏一页
                let cw = ((READ_STAGE_CONTENT_H as f64) * w as f64 / h as f64).round() as usize;
                pages.push(ComicPage {
                    img: i,
                    slice: 0,
                    layers: 1,
                    width: cw.max(1),
                    height: READ_STAGE_CONTENT_H,
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
fn comic_stage(
    images: &[BookImage],
    pg: &ComicPage,
    source_id: &str,
    book_url: &str,
    chapter_url: &str,
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
        src.push_str(&format!("&slice=1&i={}", pg.slice));
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
        "<div id=\"k-stage\" style=\"height:{h}px;width:{w}px;padding:0 {pad}px {pb}px;box-sizing:border-box;overflow:hidden\">{img}</div>",
        h = READ_STAGE_H,
        w = PAGE_W,
        pad = SIDE_PAD,
        pb = STAGE_PAD_BOTTOM,
        img = img
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
    if let Ok(Some(c)) = crate::routes::comic::load_cached_images(pool, &chapter.url, s.cfg.content_ttl_secs).await {
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
        return volume_notice(&q, fs, explicit);
    }
    // 漫画源：一屏一页（整图或切片），不走正文分页
    if source.book_source_type == 2 {
        return comic_read(&s, &db.0, &source, &chapters, ch, &q, fs, explicit).await;
    }

    let mut c: BookContent = match load_cached_content(&db.0, &chapter.url, s.cfg.content_ttl_secs).await {
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
    let (title, body_html) =
        replace::purify(&db.0, &book_name, &q.source, c.title.clone(), c.content.clone()).await;
    let mut text = html_to_text(body_html.as_deref().unwrap_or(""));
    let truncated = truncate_to_chars(&mut text, K_MAX_TEXT_CHARS);
    let spec = &FS_SPECS[fs - 1];
    let plan = plan_pages(&text, spec);
    let n = plan.pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let (block_idx, cur_start, cur_end) = plan.pages[(p - 1) as usize];
    let cur = &text[cur_start..cur_end];
    let blocks = plan.chunks.len();

    let ch_title = title.clone().unwrap_or_else(|| format!("第 {} 章", ch + 1));
    let toc_url = toc_link(&q, fs, ch);
    let (prev, next) = read_nav(&q, fs, ch, p, n, chapters.len() as i64, &toc_url);
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
    let progress_cell = progress_form(&q, ch, p, n, chapters.len(), fs);
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(&q, next_fs, ch, p);

    let mut paras = String::new();
    for para in cur.split('\n') {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        paras.push_str(&format!(
            "<p style=\"font-size:{size}px;line-height:{line}px;text-indent:2em;word-wrap:break-word\">{para}</p>",
            size = spec.size,
            line = spec.line,
            para = esc(para)
        ));
    }
    // 阅读页无顶栏：整页 1268 全给正文；底栏固定覆盖，靠 padding-bottom 托住最后一行
    let stage = format!(
        "<div id=\"k-stage\" style=\"height:{h}px;width:{w}px;padding:0 {pad}px {pb}px;box-sizing:border-box;overflow:hidden\">{paras}</div>",
        h = READ_STAGE_H,
        w = PAGE_W,
        pad = SIDE_PAD,
        pb = STAGE_PAD_BOTTOM,
        paras = paras
    );
    // 贴底固定栏 5 格：上一页 / 下一页 / 目录(页码+分块进度，略宽) / 字号 / 记录进度
    let bottom = fixed_bar(&format!(
        "{}{}{}{}<td width=\"18%\" height=\"64\" align=\"center\">{progress}</td>",
        bottom_cell(&prev.0, prev.1, 18),
        bottom_cell_next(next.as_ref(), 18),
        bottom_cell_meta(&toc_url, "目录", &meta, 28),
        bottom_cell(&fs_href, FS_SPECS[next_fs - 1].label, 18),
        progress = progress_cell
    ));

    let body = format!(
        "{stage}{hot}{bottom}",
        stage = stage,
        hot = hot_zones(&prev.0, next.as_ref().map(|x| x.0.as_str())),
        bottom = bottom
    );
    let mut res = page_css(&ch_title, fs, &body, &read_only_css());
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

/// 漫画阅读页：一屏一页（整图或切片），翻页/进度/字号与小说页同一套底栏与热区。
async fn comic_read(
    s: &AppState,
    pool: &SqlitePool,
    source: &BookSource,
    chapters: &[BookChapter],
    ch: i64,
    q: &ReadQ,
    fs: usize,
    explicit: bool,
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
    let pages = comic_pages(&images, &dims);
    if pages.is_empty() {
        return err_page("本章没有图片");
    }
    let n = pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let pg = &pages[(p - 1) as usize];

    let toc_url = toc_link(q, fs, ch);
    let (prev, next) = read_nav(q, fs, ch, p, n, chapters.len() as i64, &toc_url);
    let meta = format!("{p}/{n}");
    let progress_cell = progress_form(q, ch, p, n, chapters.len(), fs);
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(q, next_fs, ch, p);

    let stage = comic_stage(&images, pg, &q.source, &q.book, &chapter.url);
    let bottom = fixed_bar(&format!(
        "{}{}{}{}<td width=\"18%\" height=\"64\" align=\"center\">{progress}</td>",
        bottom_cell(&prev.0, prev.1, 18),
        bottom_cell_next(next.as_ref(), 18),
        bottom_cell_meta(&toc_url, "目录", &meta, 28),
        bottom_cell(&fs_href, FS_SPECS[next_fs - 1].label, 18),
        progress = progress_cell
    ));
    let title = chapter.title.clone().unwrap_or_else(|| format!("第 {} 章", ch + 1));
    let body = format!(
        "{stage}{hot}{bottom}",
        stage = stage,
        hot = hot_zones(&prev.0, next.as_ref().map(|x| x.0.as_str())),
        bottom = bottom
    );
    let mut res = page_css(&title, fs, &body, &read_only_css());
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 进度提交（零 JS form） ----------------

async fn progress(
    State(_s): State<AppState>,
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
    let loc = with_fs(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(q.source.as_deref().unwrap_or("")),
            ch,
            q.p.max(1)
        ),
        fs,
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
    let key = q.key.unwrap_or_default().trim().to_string();
    let next_fs = cycle_fs(fs);
    let fs_url = format!("/k/search?fs={next_fs}");
    let nav = format!(
        "{}{}",
        nav_cell(&with_fs("/k", fs), "书架", 50),
        nav_cell(&fs_url, FS_SPECS[next_fs - 1].label, 50),
    );
    let top = top_bar("搜索", &nav);
    let mut body = format!(
        "<div style=\"padding:8px 36px\"><form method=\"get\" action=\"/k/search\">\
         <input type=\"text\" name=\"key\" value=\"{key}\" style=\"font-size:28px;height:56px;width:600px\"> \
         <input type=\"submit\" value=\"搜索\" style=\"font-size:28px;height:56px;width:160px\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\"></form></div>",
        key = esc(&key),
        fs = fs
    );
    if !key.is_empty() {
        let sources = match load_sources(&s.shared).await {
            Ok(v) => v,
            Err(e) => return err_to_page(e),
        };
        let books = web_book::search_all(&sources, &key, 1).await;
        if books.is_empty() {
            body.push_str(
                "<p style=\"font-size:30px;line-height:52px;padding:0 36px\">没有找到结果</p>",
            );
        }
        body.push_str("<table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">");
        for b in books.iter().take(30) {
            let src = b.origin.clone().unwrap_or_default();
            let href = with_fs(
                &format!("/k/toc?book={}&source={}", qenc(&b.book_url), qenc(&src)),
                fs,
            );
            body.push_str(&format!(
                "<tr><td style=\"height:92px;border-bottom:1px solid #ccc;padding:0 36px\">\
                 <a href=\"{href}\" style=\"font-size:30px\">{name}</a>\
                 <div style=\"font-size:24px;line-height:34px\">{author} · {origin}</div></td></tr>",
                href = href,
                name = esc(b.name.as_deref().unwrap_or("未命名")),
                author = esc(b.author.as_deref().unwrap_or("")),
                origin = esc(b.origin_name.as_deref().unwrap_or(""))
            ));
        }
        body.push_str("</table>");
    }
    let mut res = page("搜索", fs, &format!("{top}{body}"));
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
        cell("-webkit-box / flex", demo_flex, "预期：三块并排（flex 不可用时会竖排）"),
        cell("vw 单位", demo_vw, "预期：20vw 不生效，宽度塌陷"),
        cell("border-radius", demo_radius, "预期无效；若照片仍是圆角，说明探测假阴"),
        cell("position:fixed", demo_fixed, "预期不可靠；角标应留在文档流里")
    );
    let body = format!(
        "<div style=\"height:96px;line-height:96px;background:#e6e6e6;padding:0 36px;font-size:30px\">\
         KPW3 视觉自测 · <a href=\"/k\">返回书架</a></div>\
         <table width=\"1000\" cellpadding=\"0\" cellspacing=\"0\">{rows}</table>"
    );
    page("KPW3 视觉自测", 2, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginate_lines_respects_budget_and_char_boundaries() {
        // 26 行预算（32px 档实际物理上限 23 行）：前 5 段占 20 行，第 6 段用余下 3 行切在行边界，余文进第 2 页。
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
        let pages = paginate_lines(&text, spec);
        assert_eq!(pages.len(), 2, "行数预算下 6 段应分 2 页");
        let p1 = &text[pages[0].0..pages[0].1];
        let p2 = &text[pages[1].0..pages[1].1];
        assert_eq!(
            p1.matches('字').count() + p2.matches('字').count(),
            600,
            "分页不应丢字"
        );
        // 第 1 页装满预算；两页都不超预算
        let em = line_em(spec);
        let l1: usize = p1
            .split('\n')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| para_lines(p, em))
            .sum();
        let l2: usize = p2
            .split('\n')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| para_lines(p, em))
            .sum();
        assert_eq!(l1, spec.lines, "第 1 页应刚好用满行数预算");
        assert!(l2 <= spec.lines);
        assert!(pages.windows(2).all(|w| w[0].1 <= w[1].0), "页区间不重叠");
        // 同一文本同一档位 → 分页结果恒定
        assert_eq!(paginate_lines(&text, spec), pages);
    }

    #[test]
    fn paginate_empty_is_one_blank_page() {
        assert_eq!(paginate_lines("", &FS_SPECS[1]), vec![(0, 0)]);
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
            let em = line_em(spec);
            let pages = paginate_lines(&text, spec);
            for &(s, e) in &pages {
                let piece = &text[s..e];
                let lines: usize = piece
                    .split('\n')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| para_lines(p, em))
                    .sum();
                assert!(
                    lines <= spec.lines,
                    "fs={} 页内 {} 行超过预算 {}",
                    spec.size,
                    lines,
                    spec.lines
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

        let plan = plan_pages(&text, &FS_SPECS[1]);
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
        assert!(truncate_to_chars(&mut text, K_MAX_TEXT_CHARS), "90 万字应触发截断");
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
        assert_eq!(page1.len(), 20);
        assert_eq!(page2.len(), 20);
        // 分页区间仍指向原 Vec 里的章节，index 单调且跳过被过滤的分卷下标。
        assert!(page1.windows(2).all(|w| w[0].index < w[1].index));
        assert!(page2.windows(2).all(|w| w[0].index < w[1].index));
        assert!(page1.iter().all(|c| !matches!(c.index, 0 | 10 | 20 | 30 | 40)));
        assert_eq!(page1.last().unwrap().index, 22);
        assert_eq!(page2.first().unwrap().index, 23);
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

    /// 阅读页整页 1268、可用 1196；四档行预算 = min(26, 物理可容纳行数)。
    #[test]
    fn font_specs_fit_read_stage() {
        assert_eq!(READ_STAGE_H, 1268);
        assert_eq!(READ_STAGE_CONTENT_H, 1196);
        let expect = [(28, 46, 26), (32, 52, 23), (38, 62, 19), (20, 36, 26)];
        assert_eq!(FS_SPECS.len(), 4, "应有四档字号");
        for (spec, (size, line, lines)) in FS_SPECS.iter().zip(expect) {
            assert_eq!((spec.size, spec.line), (size, line));
            assert_eq!(spec.lines, lines, "fs={size} 行预算错误");
            let per_line = (PAGE_W - SIDE_PAD * 2) / size;
            let chars = spec.lines * per_line;
            assert!(
                spec.lines * spec.line <= READ_STAGE_CONTENT_H,
                "fs={size} 行高总和 {} 超过正文区 {}",
                spec.lines * spec.line,
                READ_STAGE_CONTENT_H
            );
            assert!(chars >= 450, "fs={size} 每页字数 {chars} 少于 450");
        }
        let std_chars = FS_SPECS[1].lines * (PAGE_W - SIDE_PAD * 2) / FS_SPECS[1].size;
        assert_eq!(std_chars, 667);
        assert!(
            (500..=800).contains(&std_chars),
            "标准档每页字数应在 500–800"
        );
        // 小字档 26 行 × 33 字 = 858，正好落在参考目标
        let small_chars = FS_SPECS[0].lines * ((PAGE_W - SIDE_PAD * 2) / FS_SPECS[0].size);
        assert_eq!(small_chars, 858);
    }

    /// 字号轮换按物理大小 超小(4)→小(1)→标准(2)→大(3)→超小(4)，旧档位语义不变。
    #[test]
    fn font_cycle_keeps_existing_tiers() {
        assert_eq!(resolve_fs(&HeaderMap::new(), Some(4)).0, 4);
        assert_eq!(resolve_fs(&HeaderMap::new(), Some(9)).0, 2, "越界回落标准档");
        assert_eq!(cycle_fs(4), 1);
        assert_eq!(cycle_fs(1), 2);
        assert_eq!(cycle_fs(2), 3);
        assert_eq!(cycle_fs(3), 4);
    }

    /// 三个 /k 页面共用同一骨架：必须带固定 1072 viewport，且零脚本、零子资源。
    #[test]
    fn viewport_is_fixed_and_page_is_self_contained() {
        let html = page_html("测试", 2, "<div>正文</div>");
        assert!(
            html.contains("<meta name=\"viewport\" content=\"width=1072,initial-scale=1"),
            "缺少 viewport meta"
        );
        assert!(!html.contains("<script"), "不允许脚本");
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

    /// 版式硬约束：底 padding = 栏高 + 8；标准档显式 32/52；默认档每页字数仍在 500–800。
    #[test]
    fn fixed_bar_keeps_content_clear_of_bar() {
        assert_eq!(STAGE_PAD_BOTTOM, BAR_H + 8);
        let spec = &FS_SPECS[1];
        assert_eq!(spec.size, 32, "标准档必须显式 32px（实测 32/52，勿低于 30）");
        assert_eq!(spec.line, 52);
        let per_line = (PAGE_W - SIDE_PAD * 2) / spec.size;
        let chars = spec.lines * per_line;
        assert!(
            (500..=800).contains(&chars),
            "默认档每页字数应在 500–800，实际 {}",
            chars
        );
        assert!(spec.lines <= READ_STAGE_CONTENT_H / spec.line);
    }

    /// 一屏一页：`html`/`body`/`.wrap` 三层锁死 1268 并禁纵向滚动，阅读页不会产生第二屏。
    #[test]
    fn read_page_locks_one_screen_and_hides_overflow() {
        let css = read_only_css();
        assert!(css.contains("html,body{height:1268px;overflow:hidden}"), "{css}");
        assert!(css.contains(".wrap{height:1268px;overflow:hidden}"), "{css}");
        let html = page_html_css("测试", 2, "<div id=\"k-stage\"></div>", &css);
        assert!(html.contains("html,body{height:1268px;overflow:hidden}"));
        assert!(!html.contains("<script"), "阅读页仍然零脚本");
    }

    /// 左右热区：各占屏宽 1/3（357px）、整屏 1268 高，右区左边界 715；无下一页时右区不渲染。
    #[test]
    fn hot_zones_are_one_third_screen_and_clickable() {
        assert_eq!(HOT_W, 1072 / 3);
        assert_eq!(HOT_LEFT_R, 1072 - HOT_W);
        assert!(HOT_W >= 200, "热区宽必须 ≥ 200px，实际 {HOT_W}");
        assert!(READ_STAGE_H >= 200, "热区高必须 ≥ 200px");
        let z = hot_zones("/k/read?p=1", Some("/k/read?p=2"));
        assert_eq!(z.matches("<a ").count(), 2);
        assert!(z.contains("left:0;width:357px;height:1268px;z-index:5"));
        assert!(z.contains("left:715px;width:357px;height:1268px;z-index:5"));
        assert_eq!(hot_zones("/k/read?p=1", None).matches("<a ").count(), 1);
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
        let pages = comic_pages(&images, &dims);
        assert_eq!(pages.len(), 6, "三张两片长条页 = 6 页");
        for (i, pg) in pages.iter().enumerate() {
            assert_eq!(pg.layers, 2, "第 {i} 页片数");
            assert_eq!(pg.slice, i % 2, "第 {i} 页片下标");
            assert_eq!(pg.width, 928, "版心宽固定 928");
            // 切片真实高度由服务端落点决定，页表只给宽度（≠0 才写 height，避免拉扁）
            assert_eq!(pg.height, 0, "第 {i} 页不写死切片高度");
        }
        // 1.1 屏（1024×1471）：不切，一屏一页，按高度约束等比缩小
        let one = vec![BookImage {
            index: 1,
            url: "https://example.test/one.jpg".into(),
            width: Some(1024),
            height: Some(1471),
            ..Default::default()
        }];
        let one_dims = vec![Some((1024, 1471))];
        let p1 = comic_pages(&one, &one_dims);
        assert_eq!(p1.len(), 1, "1.1 屏不切");
        assert_eq!(p1[0].layers, 1);
        assert_eq!(p1[0].height, READ_STAGE_CONTENT_H);
        assert!(p1[0].width <= 928, "等比缩小后不得宽于版心");
        // 尺寸未知：整图一页，height=0（渲染退化为 max-height）
        let unknown = vec![BookImage {
            index: 1,
            url: "https://example.test/x.jpg".into(),
            ..Default::default()
        }];
        let u = comic_pages(&unknown, &[None]);
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
        let pages = comic_pages(&images, &[Some((1024, 1200))]);
        assert_eq!(pages.len(), 1);
        let html = comic_stage(&images, &pages[0], "src", "book", "https://example.test/ch");
        assert!(!html.contains("slice=1"), "整图页不得带切片参数");
        // 1024×1200 缩到 928 宽 → 高 1088（≤1196），显式宽高防抖动
        assert!(html.contains("width=\"928\" height=\"1088\""), "{html}");
        assert!(html.contains("max-width:100%"), "禁止横向溢出");
        assert!(html.contains("id=\"k-stage\"") && html.contains("height:1268px"));

        let strip = vec![BookImage {
            index: 1,
            url: "https://example.test/s.jpg".into(),
            width: Some(1200),
            height: Some(2266),
            ..Default::default()
        }];
        let sp = comic_pages(&strip, &[Some((1200, 2266))]);
        assert_eq!(sp.len(), 2);
        let s2 = comic_stage(&strip, &sp[1], "src", "book", "https://example.test/ch");
        assert!(s2.contains("slice=1&i=1"), "{s2}");
        assert!(s2.contains("width=\"928\""), "{s2}");
        assert!(!s2.contains("height=\""), "切片页不写死高度，避免与真实落点错位拉扁");
    }
}
