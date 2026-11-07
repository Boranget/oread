//! web_book 的详情解析（move-only 从 web_book.rs 拆出）。

use super::*;

// ---------------- 书籍详情 ----------------
pub async fn book_info(source: &BookSource, book: &mut Book) -> anyhow::Result<()> {
    book_info_book(&ReqFetcher, source, book, None).await
}

/// 带 book 作用域的详情：预载同书变量（如搜索阶段写入的 `bid`），结束后回写。
pub async fn book_info_book(
    fetcher: &dyn Fetcher,
    source: &BookSource,
    book: &mut Book,
    store: Option<&BookStore>,
) -> anyhow::Result<()> {
    // 无详情规则时不抓页面（与原行为一致，避免额外请求）
    if source.rule_book_info.is_none() {
        return Ok(());
    }
    let url = book.book_url.clone();
    let headers = req::source_headers(source, &url);
    let html = fetcher
        .fetch(
            &url,
            "GET",
            &headers,
            None,
            &source.book_source_url,
            req::source_timeout(source),
            req::source_limit(source),
        )
        .await?;
    parse_book_info_html(source, book, &html, store)
}

/// 对已抓取的详情页 HTML 应用详情规则（不发请求）。
/// 「按链接导入」复用它，避免为了兜底再抓一次同一页面。
pub fn parse_book_info_html(
    source: &BookSource,
    book: &mut Book,
    html: &str,
    store: Option<&BookStore>,
) -> anyhow::Result<()> {
    let rule: &BookInfoRule = match &source.rule_book_info {
        Some(r) => r,
        None => return Ok(()),
    };
    let url = book.book_url.clone();
    let ar = make_ar_book(source, store, &url);
    inject_book_meta(&ar, book);
    // `init` 的返回值在 Legado 里就是后续详情规则的输入内容（快看用它把
    // `__NUXT__` JSON 设为当前文档，name 等规则再在 JSON 上求值）。
    let mut content = html.to_string();
    if let Some(init) = &rule.init {
        match ar.get_string(html, init, false, &url) {
            Ok(init_out) => {
                let t = init_out.trim();
                if !t.is_empty() && t != "null" && t != "undefined" {
                    content = init_out;
                }
            }
            Err(e) => note_field_error(source, "init", &e),
        }
    }
    // Legado 语义：详情规则为空或未命中时**保留调用方带入的值**（搜索/书架已有
    // name/author），不能写成 null。错层小说网的 name/author/intro 规则就是空串。
    assign_opt(
        &mut book.name,
        source,
        "name",
        &ar,
        &content,
        &rule.name,
        &url,
    );
    assign_opt(
        &mut book.author,
        source,
        "author",
        &ar,
        &content,
        &rule.author,
        &url,
    );
    assign_opt(
        &mut book.intro,
        source,
        "intro",
        &ar,
        &content,
        &rule.intro,
        &url,
    );
    assign_opt(
        &mut book.cover_url,
        source,
        "cover_url",
        &ar,
        &content,
        &rule.cover_url,
        &url,
    );
    assign_opt(
        &mut book.kind,
        source,
        "kind",
        &ar,
        &content,
        &rule.kind,
        &url,
    );
    assign_opt(
        &mut book.latest_chapter_title,
        source,
        "last_chapter",
        &ar,
        &content,
        &rule.last_chapter,
        &url,
    );
    let toc_url = eval_field(
        source,
        "toc_url",
        &ar,
        &content,
        rule.toc_url.as_deref().unwrap_or(""),
        true,
        &url,
    );
    record_ar(&ar, store);
    // Legado 的 tocUrl 常直接写死绝对 URL（不是选择器），此时规则引擎会落空，需原样使用
    let literal = rule.toc_url.as_deref().map(str::trim).unwrap_or("");
    let toc_url = if toc_url.trim().is_empty()
        && (literal.starts_with("http://") || literal.starts_with("https://"))
    {
        literal.to_string()
    } else {
        toc_url
    };
    if !toc_url.trim().is_empty() {
        book.toc_url = Some(abs(&url, &toc_url));
    }
    Ok(())
}

/// 计算 `nextTocUrl` 的下页候选：
/// - 字面 URL（含 `{{page}}`/`${page}` 模板时按**下一页**页码代入）；
/// - 纯 CSS 规则（`@href`/`@value` 等）返回**所有**匹配值（多页目录一次性给出全部页码链接）；
/// - 其他复合规则（`&&`/`||`/`##`/`@js:`）退回单值求值。
pub(super) fn next_toc_urls(
    ar: &AnalyzeRule,
    html: &str,
    rule: &str,
    base: &str,
    page: i32,
) -> Vec<String> {
    let raw = rule.trim();
    if raw.is_empty() {
        return vec![];
    }
    let next_page = (page + 1).to_string();
    let tpl = raw
        .replace("{{page}}", &next_page)
        .replace("${page}", &next_page);
    if tpl != raw
        && (tpl.starts_with("http://") || tpl.starts_with("https://") || tpl.starts_with('/'))
    {
        return vec![abs(base, &tpl)];
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return vec![raw.to_string()];
    }
    let mut vals = ar.get_string_list_all(html, raw, base);
    if vals.is_empty() {
        if let Ok(s) = ar.get_string(html, raw, true, base) {
            vals.push(s);
        }
    }
    vals.into_iter()
        .filter(|s| !blank_value(s))
        .map(|s| abs(base, &s))
        .collect()
}
