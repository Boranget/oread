//! web_book 的搜索与发现入口（move-only 从 web_book.rs 拆出）。

use super::*;

pub async fn search(source: &BookSource, key: &str, page: i32) -> anyhow::Result<Vec<SearchBook>> {
    search_book(source, key, page, None).await
}

/// 带 book 作用域的搜索：每条结果的 `@put`（如爱看的 `bid`）按 book_key 落库。
pub async fn search_book(
    source: &BookSource,
    key: &str,
    page: i32,
    store: Option<&BookStore>,
) -> anyhow::Result<Vec<SearchBook>> {
    let rule: &SearchRule = match &source.rule_search {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let (url, method, headers, body) = build_search_request(source, key, page);
    let html = req::fetch_for_source_limited(
        &url,
        &method,
        &headers,
        body.as_deref(),
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await?;
    let ar = make_ar(source);
    let items = eval_list_field(
        source,
        "book_list",
        &ar,
        &html,
        rule.book_list.as_deref().unwrap_or(""),
    );
    let mut books = Vec::new();
    for item in items {
        let book_url = eval_field(
            source,
            "book_url",
            &ar,
            &item,
            rule.book_url.as_deref().unwrap_or(""),
            true,
            &url,
        );
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(&url, &book_url);
        // 每条结果独立作用域：先把上一条的 @put 落库，再按本书 book_key 预载。
        if let Some(st) = store {
            let kb = book_key(source, &book_url);
            ar.begin_book_scope(kb.clone(), st.vars(&kb), st.cache(&kb));
        }
        let sb = SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            book_type: Some(type_token(source.book_source_type).to_string()),
            name: opt(source, "name", &ar, &item, &rule.name, &url),
            author: opt(source, "author", &ar, &item, &rule.author, &url),
            cover_url: opt(source, "cover_url", &ar, &item, &rule.cover_url, &url),
            intro: opt(source, "intro", &ar, &item, &rule.intro, &url),
            kind: opt(source, "kind", &ar, &item, &rule.kind, &url),
            latest_chapter_title: opt(source, "last_chapter", &ar, &item, &rule.last_chapter, &url),
            ..Default::default()
        };
        record_ar(&ar, store);
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
    let headers = req::source_headers(source, &source.book_source_url);
    let html = req::fetch_for_source_limited(
        url,
        "GET",
        &headers,
        None,
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await?;
    let ar = make_ar(source);
    let items = eval_list_field(
        source,
        "book_list",
        &ar,
        &html,
        rule.book_list.as_deref().unwrap_or(""),
    );
    let mut books = Vec::new();
    for item in items {
        let book_url = eval_field(
            source,
            "book_url",
            &ar,
            &item,
            rule.book_url.as_deref().unwrap_or(""),
            true,
            url,
        );
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(url, &book_url);
        books.push(SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            book_type: Some(type_token(source.book_source_type).to_string()),
            name: opt(source, "name", &ar, &item, &rule.name, url),
            author: opt(source, "author", &ar, &item, &rule.author, url),
            cover_url: opt(source, "cover_url", &ar, &item, &rule.cover_url, url),
            intro: opt(source, "intro", &ar, &item, &rule.intro, url),
            kind: opt(source, "kind", &ar, &item, &rule.kind, url),
            latest_chapter_title: opt(source, "last_chapter", &ar, &item, &rule.last_chapter, url),
            ..Default::default()
        });
    }
    Ok(books)
}
