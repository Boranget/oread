//! OPDS 1.2 目录：给 KOReader / Moon+ Reader 等阅读器一份标准的 Atom 书单。
//!
//! 范围（Phase 4）：
//! - `GET /opds`：导航 feed（全部 / 文字书 / 漫画 / 最近入藏）；
//! - `GET /opds/books`：馆藏 acquisition feed，keyset 分页；
//! - `GET /opds/download/{epub,cbz}`：复用导出管线，逐章/逐页写临时文件后流式回传；
//! - `GET /opds/cover`：复用图片代理，把馆藏封面按书源 Referer/UA 取回。
//!
//! 认证只用 HTTP Basic（见 `auth::auth_middleware` 的 `/opds` 分支）：OPDS 阅读器
//! 普遍不支持 Bearer。Basic 通过后仍注入 `AuthUser` / `UserDb` / `LibraryDb`，
//! 因此下载与封面走的是与网页端完全相同的权限与临时文件目录。
//!
//! 只暴露 `tier=collection` 的永久馆藏；临时缓存不进入目录，避免阅读器把
//! 随时会被 TTL 回收的书当成馆藏。

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::auth::AuthUser;
use crate::ebook::format::txt::normalize_author;
use crate::ebook::xml::xml_escape;
use crate::error::{AppError, AppResult};
use crate::library;
use crate::routes::export::{cbz_download, epub_download};
use crate::routes::image::{proxy_image, ImageQ};
use crate::routes::AppState;

const NAV_TYPE: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";
const ACQ_TYPE: &str = "application/atom+xml;profile=opds-catalog;kind=acquisition";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/opds", get(navigation))
        .route("/opds/", get(navigation))
        .route("/opds/books", get(books))
        .route("/opds/download/epub", get(download_epub))
        .route("/opds/download/cbz", get(download_cbz))
        .route("/opds/cover", get(cover))
}

#[derive(Deserialize)]
pub struct BookQ {
    pub book_url: String,
}

// ---------------------------------------------------------------------------
// 导航 feed
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    All,
    Text,
    Comic,
}

impl Kind {
    fn parse(raw: Option<&str>) -> AppResult<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("all") => Ok(Kind::All),
            Some("text") => Ok(Kind::Text),
            Some("comic") => Ok(Kind::Comic),
            Some(_) => Err(AppError::BadRequest("kind 只能是 all/text/comic".into())),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::All => "all",
            Kind::Text => "text",
            Kind::Comic => "comic",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Kind::All => "全部馆藏",
            Kind::Text => "文字书",
            Kind::Comic => "漫画",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Kind::All => "馆藏中全部已入馆的书籍（文字与漫画）",
            Kind::Text => "馆藏中的文字书（可下载 EPUB）",
            Kind::Comic => "馆藏中的漫画（可下载 CBZ）",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Accessed,
    Collected,
}

impl Sort {
    fn parse(raw: Option<&str>) -> AppResult<Self> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("accessed") | Some("recent") => Ok(Sort::Accessed),
            Some("collected") => Ok(Sort::Collected),
            Some(_) => Err(AppError::BadRequest(
                "sort 只能是 accessed/collected".into(),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Sort::Accessed => "accessed",
            Sort::Collected => "collected",
        }
    }

    fn column(self) -> &'static str {
        match self {
            Sort::Accessed => "b.last_accessed_at",
            Sort::Collected => "COALESCE(b.collected_at, 0)",
        }
    }
}

async fn navigation(
    State(_s): State<AppState>,
    _u: AuthUser,
    headers: HeaderMap,
) -> AppResult<Response> {
    Ok(atom_response(
        render_navigation(&request_base(&headers)),
        NAV_TYPE,
    ))
}

fn render_navigation(base: &str) -> String {
    let now = atom_time(library::now_ms());
    let mut x = String::with_capacity(4096);
    x.push_str(XML_DECL);
    writeln!(
        x,
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:dc="http://purl.org/dc/terms/">
<id>urn:oread:opds</id>
<title>oread 图书馆</title>
<updated>{now}</updated>
<author><name>oread</name></author>
<link rel="self" href="{base}/opds" type="{NAV_TYPE}"/>
<link rel="start" href="{base}/opds" type="{NAV_TYPE}"/>"#
    )
    .expect("write to String");

    for (kind, sort, desc) in [
        (Kind::All, Sort::Accessed, "按最近阅读排序"),
        (Kind::Comic, Sort::Accessed, "按最近阅读排序"),
        (Kind::Text, Sort::Accessed, "按最近阅读排序"),
        (Kind::All, Sort::Collected, "按入馆时间排序"),
    ] {
        let href = books_url(base, kind, sort, 50, None);
        let id = format!("urn:oread:opds:books:{}:{}", kind.as_str(), sort.as_str());
        writeln!(
            x,
            r#"<entry>
<id>{id}</id>
<title>{}</title>
<updated>{now}</updated>
<content type="text">{}（{desc}）</content>
<link rel="subsection" href="{}" type="{ACQ_TYPE}"/>
</entry>"#,
            kind.title(),
            kind.describe(),
            xml_escape(&href)
        )
        .expect("write to String");
    }

    x.push_str("</feed>\n");
    x
}

// ---------------------------------------------------------------------------
// acquisition feed
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct BooksQ {
    pub kind: Option<String>,
    pub sort: Option<String>,
    /// keyset 游标：`<sort_ts>:<book_url>`
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct OpdsRow {
    book_url: String,
    name: Option<String>,
    author: Option<String>,
    intro: Option<String>,
    cover_url: Option<String>,
    origin: Option<String>,
    language: Option<String>,
    publisher: Option<String>,
    pubdate: Option<String>,
    series_name: Option<String>,
    series_index: Option<f64>,
    last_accessed_at: i64,
    collected_at: Option<i64>,
    /// 已缓存且有正文的文本章节数（决定是否提供 EPUB）
    text_cached: i64,
    /// 已落盘的漫画图片数（决定是否提供 CBZ）
    image_cached: i64,
}

fn parse_cursor(raw: Option<&str>) -> AppResult<(i64, String)> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok((i64::MAX, String::new()));
    };
    let (ts, url) = raw
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest("cursor 格式应为 <sort_ts>:<book_url>".into()))?;
    let ts = ts
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("cursor 的时间戳不是整数".into()))?;
    if url.is_empty() {
        return Err(AppError::BadRequest("cursor 缺少 book_url".into()));
    }
    Ok((ts, url.to_string()))
}

async fn books(
    State(s): State<AppState>,
    _u: AuthUser,
    headers: HeaderMap,
    Query(q): Query<BooksQ>,
) -> AppResult<Response> {
    let kind = Kind::parse(q.kind.as_deref())?;
    let sort = Sort::parse(q.sort.as_deref())?;
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let (ts, after_url) = parse_cursor(q.cursor.as_deref())?;

    let sql = format!(
        "SELECT b.book_url, b.name, b.author, b.intro, b.cover_url, b.origin, \
                b.language, b.publisher, b.pubdate, \
                b.series_name, b.series_index, b.last_accessed_at, b.collected_at, \
                CASE WHEN b.book_source_type = 2 THEN 0 ELSE b.cached_chapters END AS text_cached, \
                CASE WHEN b.book_source_type = 2 THEN b.cached_chapters ELSE 0 END AS image_cached \
         FROM library_books b \
         WHERE b.tier = 'collection' \
           AND (?1 = 'all' \
                OR (?1 = 'text' AND b.book_source_type <> 2) \
                OR (?1 = 'comic' AND b.book_source_type = 2)) \
           AND ({col} < ?2 OR ({col} = ?2 AND b.book_url > ?3)) \
         ORDER BY {col} DESC, b.book_url ASC LIMIT ?4",
        col = sort.column()
    );
    let rows: Vec<OpdsRow> = sqlx::query_as(&sql)
        .bind(kind.as_str())
        .bind(ts)
        .bind(&after_url)
        .bind(limit + 1)
        .fetch_all(&s.library)
        .await?;
    let has_more = rows.len() as i64 > limit;
    let page: Vec<&OpdsRow> = rows.iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|b| {
            let key = match sort {
                Sort::Collected => b.collected_at.unwrap_or(0),
                Sort::Accessed => b.last_accessed_at,
            };
            format!("{key}:{}", b.book_url)
        })
    } else {
        None
    };

    let urls: Vec<String> = page.iter().map(|b| b.book_url.clone()).collect();
    let tags = load_tags(&s.library, &urls).await?;
    let base = request_base(&headers);
    let self_url = books_url(&base, kind, sort, limit, q.cursor.as_deref());
    let next_url = next_cursor
        .as_deref()
        .map(|c| books_url(&base, kind, sort, limit, Some(c)));
    let xml = render_books_feed(
        &base,
        kind,
        sort,
        &self_url,
        &page,
        &tags,
        next_url.as_deref(),
    );
    Ok(atom_response(xml, ACQ_TYPE))
}

fn render_books_feed(
    base: &str,
    kind: Kind,
    sort: Sort,
    self_url: &str,
    rows: &[&OpdsRow],
    tags: &HashMap<String, Vec<String>>,
    next_url: Option<&str>,
) -> String {
    let now = atom_time(library::now_ms());
    let mut x = String::with_capacity(4096 + rows.len() * 1024);
    x.push_str(XML_DECL);
    writeln!(
        x,
        r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:dc="http://purl.org/dc/terms/">
<id>urn:oread:opds:books:{}:{}</id>
<title>{}</title>
<updated>{now}</updated>
<author><name>oread</name></author>
<link rel="self" href="{}" type="{ACQ_TYPE}"/>
<link rel="start" href="{base}/opds" type="{NAV_TYPE}"/>
<link rel="up" href="{base}/opds" type="{NAV_TYPE}"/>"#,
        kind.as_str(),
        sort.as_str(),
        kind.title(),
        xml_escape(self_url),
    )
    .expect("write to String");

    if let Some(next) = next_url {
        writeln!(
            x,
            r#"<link rel="next" href="{}" type="{ACQ_TYPE}"/>"#,
            xml_escape(next)
        )
        .expect("write to String");
    }

    for b in rows {
        write_entry(&mut x, base, b, tags.get(&b.book_url));
    }
    x.push_str("</feed>\n");
    x
}

fn write_entry(x: &mut String, base: &str, b: &OpdsRow, tags: Option<&Vec<String>>) {
    let name = b
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&b.book_url);
    let author = normalize_author(b.author.as_deref().unwrap_or(""));
    let updated = atom_time(b.last_accessed_at.max(0));
    let entry_id = format!(
        "urn:oread:book:{}",
        crate::source_ledger::sha256_hex(&b.book_url)
    );
    let encoded = query_encode(&b.book_url);

    writeln!(
        x,
        "<entry>\n<id>{}</id>\n<title>{}</title>\n<updated>{}</updated>",
        xml_escape(&entry_id),
        xml_escape(name),
        updated
    )
    .expect("write to String");
    if !author.is_empty() {
        writeln!(x, "<author><name>{}</name></author>", xml_escape(&author))
            .expect("write to String");
    }
    if let Some(intro) = b.intro.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        writeln!(
            x,
            r#"<summary type="text">{}</summary>"#,
            xml_escape(&truncate_chars(intro, 500))
        )
        .expect("write to String");
    }
    if let Some(lang) = b
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        writeln!(x, "<dc:language>{}</dc:language>", xml_escape(lang)).expect("write to String");
    }
    if let Some(publisher) = b
        .publisher
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        writeln!(x, "<dc:publisher>{}</dc:publisher>", xml_escape(publisher))
            .expect("write to String");
    }
    if let Some(pubdate) = b
        .pubdate
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        writeln!(x, "<dc:issued>{}</dc:issued>", xml_escape(pubdate)).expect("write to String");
    }
    if let Some(series) = b
        .series_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let label = match b.series_index {
            Some(idx) => format!("{series} #{idx}"),
            None => series.to_string(),
        };
        writeln!(
            x,
            r#"<category term="series:{}" label="{}"/>"#,
            xml_escape(series),
            xml_escape(&label)
        )
        .expect("write to String");
    }
    if let Some(tags) = tags {
        for tag in tags {
            writeln!(
                x,
                r#"<category term="{}" label="{}"/>"#,
                xml_escape(tag),
                xml_escape(tag)
            )
            .expect("write to String");
        }
    }

    if let Some(cover) = b
        .cover_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let href = if b.origin.as_deref().unwrap_or("").trim().is_empty() {
            // 没有书源就带不上 Referer/UA，直接把源站封面地址交给阅读器。
            cover.to_string()
        } else {
            format!("{base}/opds/cover?book_url={encoded}")
        };
        writeln!(
            x,
            r#"<link rel="http://opds-spec.org/image" href="{}"/>
<link rel="http://opds-spec.org/image/thumbnail" href="{}"/>"#,
            xml_escape(&href),
            xml_escape(&href)
        )
        .expect("write to String");
    }

    if b.text_cached > 0 {
        writeln!(
            x,
            r#"<link rel="http://opds-spec.org/acquisition/open-access" href="{base}/opds/download/epub?book_url={encoded}" type="application/epub+zip"/>"#
        )
        .expect("write to String");
    }
    if b.image_cached > 0 {
        writeln!(
            x,
            r#"<link rel="http://opds-spec.org/acquisition/open-access" href="{base}/opds/download/cbz?book_url={encoded}" type="application/vnd.comicbook+zip"/>"#
        )
        .expect("write to String");
    }
    x.push_str("</entry>\n");
}

async fn load_tags(pool: &SqlitePool, urls: &[String]) -> AppResult<HashMap<String, Vec<String>>> {
    if urls.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = (1..=urls.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT book_url, tag FROM library_tags WHERE book_url IN ({marks}) ORDER BY tag");
    let mut query = sqlx::query_as::<_, (String, String)>(&sql);
    for url in urls {
        query = query.bind(url);
    }
    let rows = query.fetch_all(pool).await?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (book_url, tag) in rows {
        out.entry(book_url).or_default().push(tag);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// acquisition 下载：复用浏览器导出的同一套实现
// ---------------------------------------------------------------------------

async fn download_epub(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BookQ>,
) -> AppResult<Response> {
    require_book_url(&q.book_url)?;
    epub_download(&s, u.id, q.book_url.trim()).await
}

async fn download_cbz(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BookQ>,
) -> AppResult<Response> {
    require_book_url(&q.book_url)?;
    cbz_download(&s, u.id, q.book_url.trim()).await
}

// ---------------------------------------------------------------------------
// 封面：复用图片代理（书源 Referer/UA、墨水屏转码、本地 BLOB 都一致）
// ---------------------------------------------------------------------------

async fn cover(
    State(s): State<AppState>,
    u: AuthUser,
    headers: HeaderMap,
    Query(q): Query<BookQ>,
) -> Response {
    if let Err(e) = require_book_url(&q.book_url) {
        return e.into_response();
    }
    let book = match library::load_book(&s.library, q.book_url.trim()).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "馆藏里没有这本书\n").into_response();
        }
        Err(e) => return AppError::from(e).into_response(),
    };
    let Some(url) = book
        .cover_url
        .as_deref()
        .and_then(|c| resolve_cover(&book.book_url, c))
    else {
        return (StatusCode::NOT_FOUND, "这本书没有封面\n").into_response();
    };
    let source = book.origin.as_deref().map(str::trim).unwrap_or("");
    if source.is_empty() {
        // 无书源信息时无法带 Referer/UA，退回 302 让客户端直接取源站封面。
        return axum::response::Redirect::temporary(&url).into_response();
    }
    let iq = ImageQ {
        url,
        source: Some(source.to_string()),
        book_url: None,
        chapter_url: None,
        eink: None,
        slice: None,
        i: None,
        dw: None,
        vh: None,
        ov: None,
    };
    proxy_image(State(s), u, headers, Query(iq)).await
}

fn resolve_cover(book_url: &str, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Some(raw.to_string());
    }
    let base = url::Url::parse(book_url).ok()?;
    base.join(raw).ok().map(|u| u.to_string())
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";

fn require_book_url(raw: &str) -> AppResult<()> {
    if raw.trim().is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    Ok(())
}

fn atom_response(body: String, content_type: &'static str) -> Response {
    let mut res = Response::new(Body::from(body));
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

/// 从请求头推导对外 base URL。没有 `Host`（例如反向代理剥离）时退回相对链接；
/// `X-Forwarded-Proto` 只接受 http/https，避免把任意头值写进 Atom。
fn request_base(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(host) = host else {
        return String::new();
    };
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| *s == "http" || *s == "https")
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

fn books_url(base: &str, kind: Kind, sort: Sort, limit: i64, cursor: Option<&str>) -> String {
    let mut url = format!(
        "{base}/opds/books?kind={}&sort={}&limit={limit}",
        kind.as_str(),
        sort.as_str()
    );
    if let Some(cursor) = cursor {
        let _ = write!(url, "&cursor={}", query_encode(cursor));
    }
    url
}

fn query_encode(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}

/// epoch 毫秒 → Atom 要求的 RFC 3339（UTC，秒精度）。
fn atom_time(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms.max(0))
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn truncate_chars(raw: &str, max: usize) -> String {
    if raw.chars().count() <= max {
        return raw.to_string();
    }
    let mut out: String = raw.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> OpdsRow {
        OpdsRow {
            book_url: "https://example.test/book?a=1&b=2".into(),
            name: Some("A & B <书>".into()),
            author: Some("作者：张三".into()),
            intro: Some("简介".into()),
            cover_url: Some("https://img.test/c.jpg".into()),
            origin: Some("https://src.test".into()),
            language: Some("zh".into()),
            publisher: Some("出版社".into()),
            pubdate: Some("2024-01-02".into()),
            series_name: Some("系列 & 一".into()),
            series_index: Some(2.0),
            last_accessed_at: 1_700_000_000_000,
            collected_at: Some(1),
            text_cached: 1,
            image_cached: 0,
        }
    }

    #[test]
    fn time_is_rfc3339() {
        assert_eq!(atom_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(atom_time(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn cursor_roundtrip_rules() {
        assert_eq!(parse_cursor(None).unwrap(), (i64::MAX, String::new()));
        assert_eq!(
            parse_cursor(Some("12:https://a.test/b")).unwrap(),
            (12, "https://a.test/b".into())
        );
        assert!(parse_cursor(Some("x:u")).is_err());
        assert!(parse_cursor(Some("1:")).is_err());
    }

    #[test]
    fn kind_and_sort_reject_unknown_values() {
        assert_eq!(Kind::parse(None).unwrap().as_str(), "all");
        assert_eq!(Kind::parse(Some("comic")).unwrap().as_str(), "comic");
        assert!(Kind::parse(Some("audio")).is_err());
        assert_eq!(Sort::parse(Some("recent")).unwrap().as_str(), "accessed");
        assert!(Sort::parse(Some("title")).is_err());
    }

    #[test]
    fn feed_escapes_text_and_builds_links() {
        let rows = [row()];
        let page: Vec<&OpdsRow> = rows.iter().collect();
        let xml = render_books_feed(
            "http://host",
            Kind::All,
            Sort::Accessed,
            "http://host/opds/books?kind=all&sort=accessed&limit=50",
            &page,
            &HashMap::new(),
            Some("http://host/opds/books?kind=all&cursor=1%3Ax"),
        );
        assert!(xml.contains("<title>A &amp; B &lt;书&gt;</title>"));
        assert!(xml.contains("<author><name>张三</name></author>"));
        assert!(xml.contains(r#"<category term="series:系列 &amp; 一" label="系列 &amp; 一 #2"/>"#));
        assert!(xml.contains(r#"rel="next""#));
        assert!(xml.contains(
            "/opds/download/epub?book_url=https%3A%2F%2Fexample.test%2Fbook%3Fa%3D1%26b%3D2"
        ));
        assert!(!xml.contains("/opds/download/cbz"), "没有图片就不提供 CBZ");
        assert!(xml.contains("/opds/cover?book_url="));
    }

    #[test]
    fn relative_cover_resolves_against_book_url() {
        assert_eq!(
            resolve_cover("https://a.test/x/y", "../img/c.jpg").unwrap(),
            "https://a.test/img/c.jpg"
        );
        assert_eq!(
            resolve_cover("https://a.test/x", "https://cdn.test/c.jpg").unwrap(),
            "https://cdn.test/c.jpg"
        );
        assert!(resolve_cover("https://a.test/x", "   ").is_none());
    }

    #[test]
    fn summary_is_truncated_on_char_boundary() {
        let s = "中".repeat(10);
        assert_eq!(truncate_chars(&s, 3), "中中中…");
        assert_eq!(truncate_chars("短", 3), "短");
    }

    #[test]
    fn navigation_and_books_feeds_are_well_formed_xml() {
        let rows = [row()];
        let page: Vec<&OpdsRow> = rows.iter().collect();
        let nav = render_navigation("http://host");
        let acq = render_books_feed(
            "http://host",
            Kind::All,
            Sort::Accessed,
            "http://host/opds/books?kind=all&sort=accessed&limit=50",
            &page,
            &HashMap::new(),
            None,
        );
        for xml in [&nav, &acq] {
            assert!(
                sxd_document::parser::parse(xml).is_ok(),
                "OPDS 必须是合法 XML:\n{xml}"
            );
            assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
            assert!(xml.contains("xmlns=\"http://www.w3.org/2005/Atom\""));
        }
        assert!(nav.contains("rel=\"subsection\""));
    }
}
