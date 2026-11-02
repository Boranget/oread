//! WebBook 编排（五类解析：搜索/发现/详情/目录/正文）
//! 输入 HTML/JSON + 书源规则 -> 输出结构化书籍数据（纯函数管线）。

use std::collections::{HashMap, HashSet};

use url::Url;
use serde_json::Value;

use crate::entities::{
    Book, BookChapter, BookContent, BookInfoRule, BookSource, ContentRule, ExploreRule, SearchBook,
    SearchRule, TocRule,
};
use crate::req;
use crate::rule_engine::AnalyzeRule;

/// 判断书源是否为 JSON 型（规则以 $. 开头或 url 含 json）
fn is_json_source(source: &BookSource) -> bool {
    source
        .search_url
        .as_deref()
        .map(|u| u.contains("json"))
        .unwrap_or(false)
        || source
            .rule_search
            .as_ref()
            .and_then(|r| r.book_list.as_deref())
            .map(|b| b.starts_with("$."))
            .unwrap_or(false)
}

fn make_ar(source: &BookSource) -> AnalyzeRule {
    let mut ar = AnalyzeRule::new();
    ar.js_lib = source.js_lib.clone().unwrap_or_default();
    ar.is_json = is_json_source(source);
    ar
}

/// 把相对 URL 归并为绝对 URL
fn abs(base: &str, maybe: &str) -> String {
    if maybe.starts_with("http://") || maybe.starts_with("https://") {
        return maybe.to_string();
    }
    if let Ok(b) = Url::parse(base) {
        if let Ok(u) = b.join(maybe) {
            return u.to_string();
        }
    }
    maybe.to_string()
}

/// 对可选子规则求值，空结果返回 None
fn opt(ar: &AnalyzeRule, item: &str, rule: &Option<String>, base: &str) -> Option<String> {
    rule.as_ref()
        .map(|r| ar.get_string(item, r, false, base).unwrap_or_default())
        .filter(|s| !s.is_empty())
}

// ---------------- 搜索 ----------------
/// 解析 Legado 搜索 URL：支持 `url,{"method":"POST",...}` 形式；纯 URL 则按 GET 处理。
/// 返回 (最终 url, method, 请求头, 可选 body)。
fn build_search_request(
    source: &BookSource,
    key: &str,
    page: i32,
) -> (String, String, HashMap<String, String>, Option<String>) {
    let url_tpl = source.search_url.clone().unwrap_or_default();
    let (base, config) = if let Some(idx) = url_tpl.find(",{") {
        (&url_tpl[..idx], Some(&url_tpl[idx + 1..]))
    } else {
        (url_tpl.as_str(), None)
    };
    let base = base
        .replace("{{key}}", key)
        .replace("{{page}}", &page.to_string());
    let mut headers = req::parse_headers(&source.header);
    let mut method = "GET".to_string();
    let mut body: Option<String> = None;
    if let Some(cfg_str) = config {
        if let Ok(v) = serde_json::from_str::<Value>(cfg_str) {
            if let Some(m) = v.get("method").and_then(|x| x.as_str()) {
                method = m.to_uppercase();
            }
            if let Some(b) = v.get("body").and_then(|x| x.as_str()) {
                body = Some(
                    b.replace("{{key}}", key)
                        .replace("{{page}}", &page.to_string()),
                );
            }
            if let Some(h) = v.get("headers").and_then(|x| x.as_object()) {
                for (k, val) in h {
                    if let Some(s) = val.as_str() {
                        headers.insert(k.clone(), s.to_string());
                    }
                }
            }
        }
    }
    (base, method, headers, body)
}

pub async fn search(source: &BookSource, key: &str, page: i32) -> anyhow::Result<Vec<SearchBook>> {
    let rule: &SearchRule = match &source.rule_search {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let (url, method, headers, body) = build_search_request(source, key, page);
    let html = req::fetch(&url, &method, &headers, body.as_deref()).await?;
    let ar = make_ar(source);
    let items = ar.get_string_list(&html, rule.book_list.as_deref().unwrap_or(""))?;
    let mut books = Vec::new();
    for item in items {
        let book_url = ar.get_string(&item, rule.book_url.as_deref().unwrap_or(""), true, &url)?;
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(&url, &book_url);
        let mut sb = SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            name: opt(&ar, &item, &rule.name, &url),
            author: opt(&ar, &item, &rule.author, &url),
            cover_url: opt(&ar, &item, &rule.cover_url, &url),
            intro: opt(&ar, &item, &rule.intro, &url),
            kind: opt(&ar, &item, &rule.kind, &url),
            latest_chapter_title: opt(&ar, &item, &rule.last_chapter, &url),
            ..Default::default()
        };
        books.push(sb);
    }
    Ok(books)
}

// ---------------- 发现 ----------------
pub async fn explore(source: &BookSource, url: &str) -> anyhow::Result<Vec<SearchBook>> {
    let rule: &ExploreRule = match &source.rule_explore {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let html = req::fetch(url, "GET", &req::parse_headers(&source.header), None).await?;
    let ar = make_ar(source);
    let items = ar.get_string_list(&html, rule.book_list.as_deref().unwrap_or(""))?;
    let mut books = Vec::new();
    for item in items {
        let book_url = ar.get_string(&item, rule.book_url.as_deref().unwrap_or(""), true, url)?;
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(url, &book_url);
        books.push(SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            name: opt(&ar, &item, &rule.name, url),
            author: opt(&ar, &item, &rule.author, url),
            cover_url: opt(&ar, &item, &rule.cover_url, url),
            intro: opt(&ar, &item, &rule.intro, url),
            kind: opt(&ar, &item, &rule.kind, url),
            latest_chapter_title: opt(&ar, &item, &rule.last_chapter, url),
            ..Default::default()
        });
    }
    Ok(books)
}

// ---------------- 书籍详情 ----------------
pub async fn book_info(source: &BookSource, book: &mut Book) -> anyhow::Result<()> {
    let rule: &BookInfoRule = match &source.rule_book_info {
        Some(r) => r,
        None => return Ok(()),
    };
    let url = book.book_url.clone();
    let html = req::fetch(&url, "GET", &req::parse_headers(&source.header), None).await?;
    let ar = make_ar(source);
    if let Some(init) = &rule.init {
        let _ = ar.get_string(&html, init, false, &url);
    }
    book.name = opt(&ar, &html, &rule.name, &url);
    book.author = opt(&ar, &html, &rule.author, &url);
    book.intro = opt(&ar, &html, &rule.intro, &url);
    book.cover_url = opt(&ar, &html, &rule.cover_url, &url);
    book.kind = opt(&ar, &html, &rule.kind, &url);
    book.latest_chapter_title = opt(&ar, &html, &rule.last_chapter, &url);
    let toc_url = ar.get_string(&html, rule.toc_url.as_deref().unwrap_or(""), true, &url)?;
    if !toc_url.is_empty() {
        book.toc_url = Some(abs(&url, &toc_url));
    }
    Ok(())
}

// ---------------- 目录 ----------------
pub async fn chapter_list(source: &BookSource, book: &Book) -> anyhow::Result<Vec<BookChapter>> {
    let rule: &TocRule = match &source.rule_toc {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let headers = req::parse_headers(&source.header);
    let mut url = book
        .toc_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| book.book_url.clone());
    let ar = make_ar(source);
    let mut chaps = Vec::new();
    let mut seen = HashSet::new();
    let mut guard = 0;
    loop {
        if guard >= 10 || seen.contains(&url) {
            break;
        }
        guard += 1;
        seen.insert(url.clone());
        let html = req::fetch(&url, "GET", &headers, None).await?;
        let items = ar.get_string_list(&html, rule.chapter_list.as_deref().unwrap_or(""))?;
        for item in &items {
            let raw_url =
                ar.get_string(item, rule.chapter_url.as_deref().unwrap_or(""), true, &url)?;
            let url_final = if raw_url.is_empty() {
                format!("{}{}", book.book_url, chaps.len())
            } else {
                abs(&url, &raw_url)
            };
            chaps.push(BookChapter {
                url: url_final,
                title: opt(&ar, item, &rule.chapter_name, &url),
                book_url: Some(book.book_url.clone()),
                base_url: Some(url.clone()),
                index: chaps.len() as i64,
                ..Default::default()
            });
        }
        let next = ar.get_string(&html, rule.next_toc_url.as_deref().unwrap_or(""), true, &url)?;
        if next.is_empty() {
            break;
        }
        let next_abs = abs(&url, &next);
        if next_abs == url {
            break;
        }
        url = next_abs;
    }
    Ok(chaps)
}

// ---------------- 正文 ----------------
pub async fn chapter_content(
    source: &BookSource,
    chapter: &BookChapter,
) -> anyhow::Result<BookContent> {
    let rule: &ContentRule = match &source.rule_content {
        Some(r) => r,
        None => return Ok(BookContent::default()),
    };
    let headers = req::parse_headers(&source.header);
    let mut url = chapter.url.clone();
    let mut parts: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut guard = 0;
    loop {
        if guard >= 10 || seen.contains(&url) {
            break;
        }
        guard += 1;
        seen.insert(url.clone());
        let html = req::fetch(&url, "GET", &headers, None).await?;
        let ar = make_ar(source);
        let part = ar.get_string(&html, rule.content.as_deref().unwrap_or(""), false, &url)?;
        parts.push(part);
        let next =
            ar.get_string(&html, rule.next_content_url.as_deref().unwrap_or(""), true, &url)?;
        if next.is_empty() {
            break;
        }
        let next_abs = abs(&url, &next);
        if next_abs == url {
            break;
        }
        url = next_abs;
    }
    // TODO: source_regex / replace_regex 内容净化
    Ok(BookContent {
        book_url: chapter.book_url.clone().unwrap_or_default(),
        url: chapter.url.clone(),
        title: chapter.title.clone(),
        content: Some(parts.join("\n")),
        chapter_index: chapter.index,
        ..Default::default()
    })
}

/// 多源搜索：并发请求所有启用书源，聚合结果
pub async fn search_all(
    sources: &[BookSource],
    key: &str,
    page: i32,
) -> Vec<SearchBook> {
    let mut tasks = Vec::new();
    for s in sources {
        if !s.enabled {
            continue;
        }
        if s.rule_search.is_none() || s.search_url.is_none() {
            continue;
        }
        tasks.push(search(s, key, page));
    }
    let results = futures::future::join_all(tasks).await;
    let mut out = Vec::new();
    for r in results {
        if let Ok(mut books) = r {
            out.append(&mut books);
        }
    }
    out
}

/// 多源发现：并发请求所有启用且开启发现的书源
pub async fn explore_all(sources: &[BookSource], url: &str) -> Vec<SearchBook> {
    let mut tasks = Vec::new();
    for s in sources {
        if !s.enabled || !s.enabled_explore {
            continue;
        }
        if s.rule_explore.is_none() {
            continue;
        }
        tasks.push(explore(s, url));
    }
    let results = futures::future::join_all(tasks).await;
    let mut out = Vec::new();
    for r in results {
        if let Ok(mut books) = r {
            out.append(&mut books);
        }
    }
    out
}

#[allow(dead_code)]
fn _unused(_: HashMap<String, String>) {}
