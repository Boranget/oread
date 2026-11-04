//! 「按链接打开书」：给定书源 + 详情页 URL，抓取并解析后落库为正常书籍。
//!
//! 场景：部分源搜索需要登录，但详情页、目录与图片可匿名访问（如腾讯漫画）。
//! 导入只依赖详情页 HTML，不要求登录；重复导入同一 (source_id, url) 幂等返回已有书。

use axum::{extract::State, routing::post, Json, Router};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use url::Url;

use crate::auth::{AuthUser, UserDb};
use crate::entities::Book;
use crate::entities::BookSource;
use crate::error::{AppError, AppResult};
use crate::req;
use crate::routes::sources::load_source;
use crate::routes::AppState;
use crate::web_book;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/book/import-url", post(import_url))
}

#[derive(Deserialize)]
pub struct ImportUrlReq {
    source_id: String,
    url: String,
}

#[derive(Serialize)]
pub struct ImportUrlResp {
    book_url: String,
    source_id: String,
    name: String,
    author: Option<String>,
    cover_url: Option<String>,
    intro: Option<String>,
    toc_url: Option<String>,
    /// text / image（与前端 Reader 分流一致）
    book_type: String,
    /// false 表示命中了已有记录（幂等）
    created: bool,
}

/// 已导入的书籍（用于幂等快速返回与导入前的网络短路）。
struct ExistingBook {
    source_id: Option<String>,
    name: String,
    author: Option<String>,
    cover_url: Option<String>,
    intro: Option<String>,
    toc_url: Option<String>,
}

/// 校验粘贴的详情页 URL：
/// - 仅 http/https，禁止用户名/密码；
/// - host 必须与书源 `book_source_url` 同 host（SSRF 收敛）；
/// - 拒绝回环/私网字面量；DNS 层私网校验在抓取时逐跳执行（`req::fetch_page_for_source`）。
pub fn validate_import_url(source: &BookSource, raw: &str) -> AppResult<Url> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("请先粘贴书籍详情页链接".into()));
    }
    let mut url =
        Url::parse(trimmed).map_err(|e| AppError::BadRequest(format!("链接格式无效：{e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        s => {
            return Err(AppError::BadRequest(format!(
                "仅支持 http/https 链接，收到 {s}"
            )))
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::BadRequest("链接不能包含用户名或密码信息".into()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::BadRequest("链接缺少域名".into()))?
        .to_ascii_lowercase();
    if req::is_private_host(&host) {
        return Err(AppError::BadRequest(format!(
            "禁止导入内网/回环地址：{host}"
        )));
    }
    let base = parse_source_base(&source.book_source_url).ok_or_else(|| {
        AppError::BadRequest(format!(
            "书源地址无效，无法校验链接域名：{}",
            source.book_source_url
        ))
    })?;
    let src_host = base
        .host_str()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if src_host.is_empty() || host != src_host {
        return Err(AppError::BadRequest(format!(
            "链接域名与书源不符：{host} ≠ {src_host}（仅支持该书源的详情页链接）"
        )));
    }
    // fragment 对抓取无意义，统一剥掉后再做幂等键
    url.set_fragment(None);
    Ok(url)
}

/// 书源根地址：兼容缺少 scheme 的历史存量（如 `m.ac.qq.com`）。
fn parse_source_base(raw: &str) -> Option<Url> {
    let t = raw.trim();
    Url::parse(t)
        .ok()
        .filter(|u| u.host_str().is_some())
        .or_else(|| {
            Url::parse(&format!("https://{t}"))
                .ok()
                .filter(|u| u.host_str().is_some())
        })
}

/// 状态码 / Content-Type / 登录页三重判定；4xx 中文文案，避免静默建出空书。
pub fn check_page(status: u16, content_type: Option<&str>, body: &str) -> AppResult<()> {
    if status == 401 || status == 403 {
        return Err(AppError::BadRequest(format!(
            "详情页需要登录（HTTP {status}），该源不能匿名导入"
        )));
    }
    if status == 404 || status == 410 {
        return Err(AppError::BadRequest(format!(
            "详情页不存在（HTTP {status}），请检查链接"
        )));
    }
    if !(200..300).contains(&status) {
        return Err(AppError::BadRequest(format!(
            "详情页返回 HTTP {status}，无法导入"
        )));
    }
    if let Some(ct) = content_type {
        let low = ct.to_ascii_lowercase();
        if !(low.contains("html") || low.contains("xml")) {
            return Err(AppError::BadRequest(format!(
                "目标不是 HTML 详情页（Content-Type: {ct}），请确认粘贴的是书籍详情页链接"
            )));
        }
    }
    if body.trim().is_empty() {
        return Err(AppError::BadRequest("详情页返回空内容，无法导入".into()));
    }
    if looks_like_login_page(body) {
        return Err(AppError::BadRequest(
            "详情页跳转到了登录页，该源需要登录后才能访问".into(),
        ));
    }
    Ok(())
}

const LOGIN_MARKERS: &[&str] = &[
    "请先登录",
    "登录后查看",
    "登录后阅读",
    "请登录后",
    "扫码登录",
    "立即登录",
    "用户登录",
    "账号登录",
    "登录才能",
];

/// 登录页启发式：正文含登录提示，且 `<title>` 也含“登录”，同时没有书籍元信息。
/// 阈值刻意收窄，避免详情页里的“登录后收藏”按钮误杀。
fn looks_like_login_page(html: &str) -> bool {
    let low = html.to_lowercase();
    if low.contains("og:novel:book_name") {
        return false;
    }
    if !LOGIN_MARKERS.iter().any(|m| low.contains(m)) {
        return false;
    }
    let doc = Html::parse_document(html);
    title_text(&doc)
        .map(|t| t.contains("登录"))
        .unwrap_or(false)
}

#[derive(Debug, Default, PartialEq)]
struct MetaFallback {
    name: Option<String>,
    author: Option<String>,
    intro: Option<String>,
    cover_url: Option<String>,
}

/// `<title>` / `og:*` / `og:novel:*` 兜底：规则解析不出的字段才使用。
fn meta_from_html(html: &str) -> MetaFallback {
    let doc = Html::parse_document(html);
    let name = meta_content(
        &doc,
        &[
            "meta[property=\"og:novel:book_name\"]",
            "meta[property=\"og:title\"]",
            "meta[name=\"og:title\"]",
            "meta[name=\"twitter:title\"]",
        ],
    )
    .or_else(|| title_text(&doc).map(|t| clean_title(&t)));
    MetaFallback {
        name,
        author: meta_content(
            &doc,
            &[
                "meta[property=\"og:novel:author\"]",
                "meta[name=\"author\"]",
                "meta[property=\"article:author\"]",
            ],
        ),
        intro: meta_content(
            &doc,
            &[
                "meta[property=\"og:description\"]",
                "meta[name=\"description\"]",
            ],
        ),
        cover_url: meta_content(
            &doc,
            &[
                "meta[property=\"og:image\"]",
                "meta[name=\"twitter:image\"]",
            ],
        ),
    }
}

fn meta_content(doc: &Html, selectors: &[&str]) -> Option<String> {
    for sel in selectors {
        let Ok(s) = Selector::parse(sel) else {
            continue;
        };
        if let Some(v) = doc
            .select(&s)
            .next()
            .and_then(|e| e.value().attr("content"))
        {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn title_text(doc: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let t = doc.select(&sel).next()?.text().collect::<String>();
    let t = t.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// `<title>` 常带站点后缀（`书名_站点`、`书名 - 站点`），兜底时取第一段。
fn clean_title(raw: &str) -> String {
    let t = raw.replace('\u{3000}', " ");
    let t = t.trim();
    for sep in ["_", "|", " - ", " – ", " — "] {
        if let Some((head, _)) = t.split_once(sep) {
            let head = head.trim();
            if !head.is_empty() {
                return head.to_string();
            }
        }
    }
    t.to_string()
}

fn blank(o: &Option<String>) -> bool {
    o.as_deref().map(|s| s.trim().is_empty()).unwrap_or(true)
}

fn fill_fallback(book: &mut Book, meta: MetaFallback, base: &str) {
    if blank(&book.name) {
        book.name = meta.name;
    }
    if blank(&book.author) {
        book.author = meta.author;
    }
    if blank(&book.intro) {
        book.intro = meta.intro;
    }
    if blank(&book.cover_url) {
        book.cover_url = meta.cover_url.map(|c| join_url(base, &c));
    }
}

fn join_url(base: &str, maybe: &str) -> String {
    if maybe.starts_with("http://") || maybe.starts_with("https://") {
        return maybe.to_string();
    }
    Url::parse(base)
        .ok()
        .and_then(|b| b.join(maybe).ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| maybe.to_string())
}

async fn find_existing(pool: &SqlitePool, book_url: &str) -> AppResult<Option<ExistingBook>> {
    #[allow(clippy::type_complexity)] // 行类型与 SELECT 列一一对应，保持就地元组便于核对。
    let row: Option<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT COALESCE(bm.name, bs.title), COALESCE(bm.author, bs.author), \
         COALESCE(bm.cover_url, bs.cover), bm.intro, bm.toc_url, bs.source_id \
         FROM bookshelf bs LEFT JOIN book_meta bm ON bm.book_url = bs.book_id \
         WHERE bs.book_id = ?1",
    )
    .bind(book_url)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(name, author, cover, intro, toc, source)| {
        let name = name?;
        if name.trim().is_empty() {
            return None;
        }
        Some(ExistingBook {
            source_id: source,
            name,
            author,
            cover_url: cover,
            intro,
            toc_url: toc,
        })
    }))
}

fn resp_from_existing(book_url: &str, source: &BookSource, e: ExistingBook) -> ImportUrlResp {
    ImportUrlResp {
        book_url: book_url.to_string(),
        source_id: source.book_source_url.clone(),
        name: e.name,
        author: e.author,
        cover_url: e.cover_url,
        intro: e.intro,
        toc_url: e.toc_url,
        book_type: type_token(source).to_string(),
        created: false,
    }
}

fn type_token(source: &BookSource) -> &'static str {
    if source.book_source_type == 2 {
        "image"
    } else {
        "text"
    }
}

async fn import_url(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<ImportUrlReq>,
) -> AppResult<Json<ImportUrlResp>> {
    let source = match load_source(&s.shared, &q.source_id).await {
        Ok(src) => src,
        Err(AppError::NotFound(_)) => {
            return Err(AppError::BadRequest(format!("书源不存在：{}", q.source_id)))
        }
        Err(e) => return Err(e),
    };
    if source.unsupported_type || source.book_source_type != 0 && source.book_source_type != 2 {
        return Err(AppError::BadRequest(format!(
            "书源「{}」类型不支持阅读",
            source.book_source_name
        )));
    }
    let url = validate_import_url(&source, &q.url)?;
    let book_url = url.to_string();
    // 幂等短路：同 source + 同 URL 不再发网络请求
    if let Some(e) = find_existing(&db.0, &book_url).await? {
        let same_source = e
            .source_id
            .as_deref()
            .map(|s| s == source.book_source_url)
            .unwrap_or(true);
        if same_source {
            return Ok(Json(resp_from_existing(&book_url, &source, e)));
        }
    }

    let headers = req::source_headers(&source, &book_url);
    let page = req::fetch_page_for_source_timed(
        &book_url,
        &headers,
        &source.book_source_url,
        req::source_timeout(&source),
    )
    .await?;
    check_page(page.status, page.content_type.as_deref(), &page.body)?;

    let key = web_book::book_key(&source, &book_url);
    let store = crate::routes::books::book_store_for(&db.0, &key).await;
    let (book, created) =
        import_book_from_html(&db.0, &source, &book_url, &page.body, Some(&store)).await?;
    Ok(Json(ImportUrlResp {
        book_url: book.book_url,
        source_id: source.book_source_url.clone(),
        name: book.name.unwrap_or_default(),
        author: book.author,
        cover_url: book.cover_url,
        intro: book.intro,
        toc_url: book.toc_url,
        book_type: type_token(&source).to_string(),
        created,
    }))
}

/// 解析 + 兜底 + 落库。抽成独立函数便于单测（不发起网络请求）。
async fn import_book_from_html(
    pool: &SqlitePool,
    source: &BookSource,
    book_url: &str,
    html: &str,
    store: Option<&web_book::BookStore>,
) -> AppResult<(Book, bool)> {
    if let Some(e) = find_existing(pool, book_url).await? {
        let same_source = e
            .source_id
            .as_deref()
            .map(|s| s == source.book_source_url)
            .unwrap_or(true);
        if same_source {
            return Ok((
                Book {
                    book_url: book_url.to_string(),
                    name: Some(e.name),
                    author: e.author,
                    cover_url: e.cover_url,
                    intro: e.intro,
                    toc_url: e.toc_url,
                    origin: Some(source.book_source_url.clone()),
                    origin_name: Some(source.book_source_name.clone()),
                    book_source_type: source.book_source_type,
                    ..Default::default()
                },
                false,
            ));
        }
    }

    let mut book = Book {
        book_url: book_url.to_string(),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        book_source_type: source.book_source_type,
        can_update: true,
        ..Default::default()
    };
    // 规则出错不阻断导入：字段由下面 meta 兜底；整页解析失败也不该静默建空书
    if let Err(e) = web_book::parse_book_info_html(source, &mut book, html, store) {
        tracing::warn!(source = %source.book_source_url, error = %e, "import url: book info rule failed");
    }
    fill_fallback(&mut book, meta_from_html(html), book_url);

    if blank(&book.name) {
        return Err(AppError::BadRequest(
            "未能从详情页解析出书名，可能不是书籍详情页".into(),
        ));
    }
    let name = book.name.clone().unwrap_or_default();

    // 落库：书架（归属当前用户）+ book_meta（详情/目录链路直接可用）
    sqlx::query(
        "INSERT INTO bookshelf (book_id, title, author, cover, source_id) VALUES (?1,?2,?3,?4,?5) \
         ON CONFLICT(book_id) DO UPDATE SET title=excluded.title, \
         author=COALESCE(excluded.author, bookshelf.author), \
         cover=COALESCE(excluded.cover, bookshelf.cover), \
         source_id=excluded.source_id",
    )
    .bind(book_url)
    .bind(&name)
    .bind(&book.author)
    .bind(&book.cover_url)
    .bind(&source.book_source_url)
    .execute(pool)
    .await?;
    crate::routes::books::save_book_meta(pool, &book).await?;
    if let Some(st) = store {
        crate::routes::books::persist_book_changes(pool, st.take_changes()).await;
    }
    Ok((book, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::BookInfoRule;
    use sqlx::sqlite::SqlitePoolOptions;

    fn source_with(host: &str) -> BookSource {
        BookSource {
            book_source_url: format!("https://{host}"),
            book_source_name: "测试源".into(),
            book_source_type: 2,
            ..Default::default()
        }
    }

    async fn memory_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open memory db");
        sqlx::migrate!("./migrations/user")
            .run(&pool)
            .await
            .expect("run user migrations");
        pool
    }

    const DETAIL_HTML: &str = r#"
        <html><head>
          <meta property="og:title" content="兜底书名">
          <meta property="og:novel:book_name" content="OG 书名">
          <meta property="og:novel:author" content="OG 作者">
          <meta property="og:description" content="OG 简介">
          <meta property="og:image" content="/cover.jpg">
        </head><body>
          <h1 class="name">正式书名</h1>
          <span class="author">正式作者</span>
          <div class="intro">正式简介</div>
          <img class="cover" src="/cover-rule.jpg">
          <a class="toc" href="/toc/1">目录</a>
        </body></html>
    "#;

    fn detail_source() -> BookSource {
        let mut s = source_with("x.example");
        s.rule_book_info = Some(BookInfoRule {
            name: Some("class.name@text".into()),
            author: Some("class.author@text".into()),
            intro: Some("class.intro@text".into()),
            cover_url: Some("class.cover@src".into()),
            toc_url: Some("class.toc@href".into()),
            ..Default::default()
        });
        s
    }

    #[test]
    fn rules_parse_detail_fields_and_toc() {
        let source = detail_source();
        let mut book = Book {
            book_url: "https://x.example/book/1".into(),
            ..Default::default()
        };
        web_book::parse_book_info_html(&source, &mut book, DETAIL_HTML, None).unwrap();
        assert_eq!(book.name.as_deref(), Some("正式书名"));
        assert_eq!(book.author.as_deref(), Some("正式作者"));
        assert_eq!(book.intro.as_deref(), Some("正式简介"));
        assert_eq!(book.cover_url.as_deref(), Some("/cover-rule.jpg"));
        assert_eq!(
            book.toc_url.as_deref(),
            Some("https://x.example/toc/1"),
            "目录相对地址应归并为绝对 URL"
        );
    }

    #[test]
    fn meta_fallback_fills_missing_fields() {
        let source = source_with("x.example");
        let mut book = Book {
            book_url: "https://x.example/book/1".into(),
            ..Default::default()
        };
        // 源没有详情规则：所有字段走 og/title 兜底
        web_book::parse_book_info_html(&source, &mut book, DETAIL_HTML, None).unwrap();
        let base = book.book_url.clone();
        fill_fallback(&mut book, meta_from_html(DETAIL_HTML), &base);
        assert_eq!(book.name.as_deref(), Some("OG 书名"));
        assert_eq!(book.author.as_deref(), Some("OG 作者"));
        assert_eq!(book.intro.as_deref(), Some("OG 简介"));
        assert_eq!(
            book.cover_url.as_deref(),
            Some("https://x.example/cover.jpg"),
            "相对封面应归并为绝对 URL"
        );
    }

    #[test]
    fn title_fallback_strips_site_suffix() {
        let html = "<html><head><title>某书_某站</title></head><body></body></html>";
        let meta = meta_from_html(html);
        assert_eq!(meta.name.as_deref(), Some("某书"));
    }

    #[tokio::test]
    async fn import_is_idempotent() {
        let pool = memory_pool().await;
        let source = detail_source();
        let url = "https://x.example/book/1";
        let (b1, created1) = import_book_from_html(&pool, &source, url, DETAIL_HTML, None)
            .await
            .expect("first import");
        assert!(created1);
        assert_eq!(b1.name.as_deref(), Some("正式书名"));
        let (b2, created2) = import_book_from_html(&pool, &source, url, DETAIL_HTML, None)
            .await
            .expect("second import");
        assert!(!created2, "重复导入不得重复建书");
        assert_eq!(b2.name.as_deref(), Some("正式书名"));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookshelf WHERE book_id = ?1")
            .bind(url)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn rejects_login_and_non_html_pages() {
        let err = check_page(403, Some("text/html"), "<html></html>").unwrap_err();
        assert!(format!("{err:?}").contains("需要登录"), "{err:?}");
        let err = check_page(200, Some("application/json"), "{\"a\":1}").unwrap_err();
        assert!(format!("{err:?}").contains("不是 HTML"), "{err:?}");
        let err = check_page(200, Some("text/html"), "").unwrap_err();
        assert!(format!("{err:?}").contains("空内容"), "{err:?}");
        assert!(check_page(200, Some("text/html"), "<html><h1>书名</h1></html>").is_ok());
    }

    #[test]
    fn rejects_login_page_with_200() {
        let html = "<html><head><title>用户登录</title></head><body>请先登录后查看</body></html>";
        let err = check_page(200, Some("text/html"), html).unwrap_err();
        assert!(format!("{err:?}").contains("登录页"), "{err:?}");
    }

    #[test]
    fn ssrf_url_validation() {
        let source = source_with("m.ac.qq.com");
        assert!(validate_import_url(&source, "https://m.ac.qq.com/book/1").is_ok());
        let err = validate_import_url(&source, "https://evil.example/book/1").unwrap_err();
        assert!(format!("{err:?}").contains("域名与书源不符"), "{err:?}");
        let err = validate_import_url(&source, "ftp://m.ac.qq.com/book/1").unwrap_err();
        assert!(format!("{err:?}").contains("仅支持 http/https"), "{err:?}");
        let err = validate_import_url(&source, "http://127.0.0.1/book/1").unwrap_err();
        assert!(format!("{err:?}").contains("内网"), "{err:?}");
        let err = validate_import_url(&source, "https://user:pass@m.ac.qq.com/book/1").unwrap_err();
        assert!(format!("{err:?}").contains("用户名"), "{err:?}");
    }

    #[test]
    fn fragment_is_stripped_for_idempotency_key() {
        let source = source_with("m.ac.qq.com");
        let u = validate_import_url(&source, "https://m.ac.qq.com/book/1#top").unwrap();
        assert_eq!(u.as_str(), "https://m.ac.qq.com/book/1");
    }
}
