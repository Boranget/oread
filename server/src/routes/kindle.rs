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
use crate::entities::{Book, BookChapter, BookContent, BookSource};
use crate::error::{AppError, AppResult};
use crate::models::BookshelfItem;
use crate::routes::books::{
    load_book_meta, load_cached_chapters, load_cached_content, save_book_meta, save_chapters,
    save_content_cached,
};
use crate::routes::bookshelf::SHELF_LIST_SQL;
use crate::routes::replace;
use crate::routes::sources::{load_source, load_sources};
use crate::routes::AppState;
use crate::web_book;

// ---------------- 真机版心常量 ----------------

/// 版心宽度（1072 视口 − 左右各 36px）
const PAGE_W: usize = 1000;
/// 版心左右留白
const SIDE_PAD: usize = 36;
/// 正文区高度 = 1268 − 96（顶栏）= 1172；底部固定栏覆盖在正文区之上，
/// 靠正文容器的底部 padding 把最后一行托到栏上方（不再用文档流底栏占位）。
const STAGE_H: usize = 1172;
/// 底部固定栏高度（触控目标 64px，保守高于 44px 下限）
const BAR_H: usize = 64;
/// 正文容器底部留白 = 栏高 + 8px，保证固定栏不遮正文最后一行
const STAGE_PAD_BOTTOM: usize = BAR_H + 8;
/// 正文可用高度 = 1172 − 72 = 1100（分页字符数按这个高度核算）
const STAGE_CONTENT_H: usize = STAGE_H - STAGE_PAD_BOTTOM;
/// 目录每页章节数：20 行 × 52px = 1040，一屏装得下（正文区 1108）
const K_TOC_PAGE: usize = 20;
/// /k 渲染前对正文纯文本的硬截断（防御超大章节把分页临时索引撑爆内存）
const K_MAX_TEXT_CHARS: usize = 200_000;
/// 字号 Cookie（与 `?fs=` 二选一，显式参数优先）
const K_FS_COOKIE: &str = "k_fs";

/// 三档字号。`chars` = 每页字符数，由“正文区 1108px ÷ 行高 = 行数”和
/// “版心 1000 − 左右 36 = 928px ÷ 字号 = 每行字数”推得，并用 kindle-sim 实测校准。
struct FsSpec {
    size: usize,
    line: usize,
    chars: usize,
    label: &'static str,
}

const FS_SPECS: [FsSpec; 3] = [
    // 928/28 ≈ 33 字/行，1100/46 = 23 行 → 759 字；留一行余量取 750。
    FsSpec { size: 28, line: 46, chars: 750, label: "小" },
    // 928/32 = 29 字/行，1100/52 = 21 行 → 609 字；实测 580 字占 20 行，取 600。
    // 字号口径：真机实测 32px/52px，保留 32px。此处显式写死 32，且全局
    // `-webkit-text-size-adjust:none` 已关闭老引擎的字号自动放大——实测 32/52，勿低于 30。
    FsSpec { size: 32, line: 52, chars: 600, label: "标准" },
    // 928/38 ≈ 24 字/行，1100/62 = 17 行 → 408 字；实测 390 字占 16 行，取 400。
    FsSpec { size: 38, line: 62, chars: 400, label: "大" },
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
fn is_kindle_ua(ua: &str) -> bool {
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

/// 解析字号：显式 `?fs=1..3` 优先，其次 `k_fs` Cookie，缺省标准档（2）。
/// 返回 (档位, 是否需要回写 Cookie)。
fn resolve_fs(headers: &HeaderMap, q: Option<i64>) -> (usize, bool) {
    if let Some(v) = q {
        if (1..=3).contains(&v) {
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
                    if (1..=3).contains(&v) {
                        return (v as usize, false);
                    }
                }
            }
        }
    }
    (2, false)
}

/// 显式切换字号时把档位写进 30 天 Cookie
fn remember_fs(res: &mut Response, fs: usize) {
    let value = format!("{K_FS_COOKIE}={fs}; Path=/; Max-Age=2592000; SameSite=Lax");
    if let Ok(v) = HeaderValue::from_str(&value) {
        res.headers_mut().insert(SET_COOKIE, v);
    }
}

/// 统一页面骨架：无脚本、固定版心、`-webkit-text-size-adjust:none` 防老引擎自动放大
fn page(title: &str, fs: usize, body: &str) -> Response {
    let spec = &FS_SPECS[fs - 1];
    let html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{title}</title><style>\
html{{-webkit-text-size-adjust:none}}\
html,body{{margin:0;padding:0;background:#fff;color:#000;overflow-x:hidden}}\
body{{font-family:serif;font-size:{size}px;line-height:{line}px}}\
a{{color:#000;text-decoration:none}}\
h1,h2,h3,p{{margin:0}}\
table{{border-collapse:collapse}}\
.wrap{{width:{pw}px;margin-left:{pad}px}}\
</style></head><body><div class=\"wrap\">{body}</div></body></html>",
        title = esc(title),
        size = spec.size,
        line = spec.line,
        pw = PAGE_W,
        pad = SIDE_PAD,
        body = body
    );
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

/// 正文/列表末尾的占位，保证固定栏不遮住最后一行内容（= 栏高 + 8px）
fn bar_spacer() -> String {
    format!("<div style=\"height:{STAGE_PAD_BOTTOM}px\"></div>")
}

fn bottom_cell(href: &str, label: &str, width: usize) -> String {
    format!(
        "<td width=\"{width}%\" height=\"64\" align=\"center\">\
         <a href=\"{href}\" style=\"display:block;height:64px;line-height:64px;font-size:28px\">{label}</a></td>"
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

/// 按字符数等分：返回每页的 (起, 止) 字节区间，页边界落在字符边界上，结果稳定可复现
fn paginate(text: &str, per: usize) -> Vec<&str> {
    let mut bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    bounds.push(text.len());
    let n_chars = bounds.len() - 1;
    let n_pages = ((n_chars + per - 1) / per).max(1); // div_ceil 需 Rust 1.73+，手写兼容旧工具链
    let mut pages = Vec::with_capacity(n_pages);
    for p in 0..n_pages {
        let s = bounds[p * per];
        let e = bounds[((p + 1) * per).min(n_chars)];
        pages.push(&text[s..e]);
    }
    pages
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

    let next_fs = if fs < 3 { fs + 1 } else { 1 };
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
        &format!("{top}{list}{spacer}{bottom}", spacer = bar_spacer()),
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

    let next_fs = if fs < 3 { fs + 1 } else { 1 };
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
        &format!("{top}{list}{spacer}{bottom}", spacer = bar_spacer()),
    );
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
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
    if text.chars().count() > K_MAX_TEXT_CHARS {
        text = text.chars().take(K_MAX_TEXT_CHARS).collect();
        text.push_str("\n\n[章节过长，已截断]");
    }
    let spec = &FS_SPECS[fs - 1];
    let pages = paginate(&text, spec.chars);
    let n = pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let cur = pages[(p - 1) as usize];

    let ch_title = title.clone().unwrap_or_else(|| format!("第 {} 章", ch + 1));
    // 不带 fs 的原始链接：切档时要换成目标档位，不能被当前档位拼成重复参数
    let raw_link = |ch: i64, p: i64| {
        format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            ch,
            p
        )
    };
    let base_link = |ch: i64, p: i64| with_fs(&raw_link(ch, p), fs);

    let toc_url = with_fs(
        &format!(
            "/k/toc?book={}&source={}&tp={}",
            qenc(&q.book),
            qenc(&q.source),
            (ch / K_TOC_PAGE as i64) + 1
        ),
        fs,
    );

    let (prev, prev_label) = if p > 1 {
        (base_link(ch, p - 1), "上一页")
    } else if ch > 0 {
        (base_link(ch - 1, 1), "上一章")
    } else {
        (toc_url.clone(), "目录")
    };
    let (next, next_label) = if p < n {
        (base_link(ch, p + 1), "下一页")
    } else if ch + 1 < chapters.len() as i64 {
        (base_link(ch + 1, 1), "下一章")
    } else {
        (toc_url.clone(), "目录")
    };
    let next_fs = if fs < 3 { fs + 1 } else { 1 };
    let fs_href = with_fs(&raw_link(ch, p), next_fs);

    let title_line = format!(
        "{} <span style=\"font-size:22px;color:#555\">第 {p}/{n} 页</span>",
        esc(&ch_title)
    );
    // 进度提交：零 JS 的原生 form（位于顶栏第 4 格，章末同样可见）
    let progress_form = format!(
        "<form method=\"post\" action=\"/k/progress\" style=\"margin:0\">\
         <input type=\"hidden\" name=\"book\" value=\"{book}\">\
         <input type=\"hidden\" name=\"source\" value=\"{source}\">\
         <input type=\"hidden\" name=\"ch\" value=\"{ch}\">\
         <input type=\"hidden\" name=\"p\" value=\"{p}\">\
         <input type=\"hidden\" name=\"n\" value=\"{n}\">\
         <input type=\"hidden\" name=\"total\" value=\"{total}\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\">\
         <input type=\"submit\" value=\"记录进度\" style=\"font-size:24px;height:60px;width:200px\"></form>",
        book = esc(&q.book),
        source = esc(&q.source),
        ch = ch,
        p = p,
        n = n,
        total = chapters.len(),
        fs = fs
    );
    let nav = format!(
        "{}{}{}<td width=\"25%\" height=\"58\" align=\"center\">{progress_form}</td>",
        nav_cell(&toc_url, "目录", 25),
        nav_cell(&with_fs("/k", fs), "书架", 25),
        nav_cell(&fs_href, FS_SPECS[next_fs - 1].label, 25),
        progress_form = progress_form
    );
    let top = top_bar(&title_line, &nav);

    let mut paras = String::new();
    for para in cur.split('\n') {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        paras.push_str(&format!(
            "<p style=\"font-size:{size}px;line-height:{line}px;text-indent:2em\">{para}</p>",
            size = spec.size,
            line = spec.line,
            para = esc(para)
        ));
    }
    let stage = format!(
        "<div id=\"k-stage\" style=\"height:{h}px;width:{w}px;padding:0 {pad}px {pb}px;box-sizing:border-box;overflow:hidden\">{paras}</div>",
        h = STAGE_H,
        w = PAGE_W,
        pad = SIDE_PAD,
        pb = STAGE_PAD_BOTTOM,
        paras = paras
    );
    // 贴底固定栏：上一页 / 下一页 / 目录 / 书架（等宽 25% 单元格，整块可点）
    let bottom = fixed_bar(&format!(
        "{}{}{}{}",
        bottom_cell(&prev, prev_label, 25),
        bottom_cell(&next, next_label, 25),
        bottom_cell(&toc_url, "目录", 25),
        bottom_cell(&with_fs("/k", fs), "书架", 25),
    ));

    let mut res = page(&ch_title, fs, &format!("{top}{stage}{bottom}"));
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
        Some(v) if (1..=3).contains(&v) => v as usize,
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
    let next_fs = if fs < 3 { fs + 1 } else { 1 };
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
    fn paginate_respects_char_boundaries() {
        let text: String = "字".repeat(1500);
        let pages = paginate(&text, 580);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].chars().count(), 580);
        assert_eq!(pages[2].chars().count(), 340);
        // 同一文本同一页长 → 分页结果恒定
        assert_eq!(paginate(&text, 580), pages);
    }

    #[test]
    fn paginate_empty_is_one_blank_page() {
        assert_eq!(paginate("", 580), vec![""]);
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

    /// 三档字号的每页字符数必须严格递减，且「行数 × 每行字数」不超过正文可用高度
    /// （STAGE_CONTENT_H = 1172 − 72，已扣除贴底固定栏的底部留白）。
    #[test]
    fn font_specs_fit_stage() {
        for spec in &FS_SPECS {
            let lines = STAGE_CONTENT_H / spec.line;
            let per_line = (PAGE_W - SIDE_PAD * 2) / spec.size;
            assert!(
                spec.chars <= lines * per_line,
                "fs={} 的 {} 字超过 {} 行 × {} 字",
                spec.size,
                spec.chars,
                lines,
                per_line
            );
        }
        assert!(FS_SPECS[0].chars > FS_SPECS[1].chars);
        assert!(FS_SPECS[1].chars > FS_SPECS[2].chars);
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
        assert!(
            (500..=800).contains(&spec.chars),
            "默认档每页字数应在 500–800，实际 {}",
            spec.chars
        );
        let per_line = (PAGE_W - SIDE_PAD * 2) / spec.size;
        let lines = STAGE_CONTENT_H / spec.line;
        assert!(spec.chars <= lines * per_line);
    }
}
