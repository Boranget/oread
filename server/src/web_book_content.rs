//! web_book 的正文抽取（move-only 从 web_book.rs 拆出）。

use super::*;

pub async fn chapter_content(
    source: &BookSource,
    chapter: &BookChapter,
) -> anyhow::Result<BookContent> {
    chapter_content_book(source, chapter, None).await
}

/// 正文规则以 `@textNodes` 结尾时，生成等价的 `@html` 回退规则。
/// `@html` 保留 <p>/<br> 等分段，前端能按段渲染；仅用于「抽取结果为空」的兜底。
pub(super) fn content_text_fallback(rule: &str) -> Option<String> {
    let trimmed = rule.trim_end();
    let head = trimmed.strip_suffix("@textNodes")?;
    if head.is_empty() {
        return None;
    }
    Some(format!("{head}@html"))
}

/// 带 book 作用域的正文（正文规则里的 `@get` 变量按同书预载）。
pub async fn chapter_content_book(
    source: &BookSource,
    chapter: &BookChapter,
    store: Option<&BookStore>,
) -> anyhow::Result<BookContent> {
    let rule: &ContentRule = match &source.rule_content {
        Some(r) => r,
        None => return Ok(BookContent::default()),
    };
    let pages = fetch_html_pages(source, chapter, rule).await?;
    let book_url = chapter.book_url.clone().unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for (page_url, html) in &pages {
        let ar = make_ar_book(source, store, &book_url);
        let content_rule = rule.content.as_deref().unwrap_or("");
        let mut part = eval_field(source, "content", &ar, html, content_rule, false, page_url);
        // `@textNodes` 只取直接子文本节点；部分站点把正文包在 <p> 里，直接文本节点只有空白。
        // 结果为空时用同一选择器的 `@html` 回退一次，避免整章空白（不改变普通规则的语义）。
        if part.trim().is_empty() {
            if let Some(fallback) = content_text_fallback(content_rule) {
                let retried = eval_field(
                    source,
                    "content_fallback",
                    &ar,
                    html,
                    &fallback,
                    false,
                    page_url,
                );
                if !retried.trim().is_empty() {
                    tracing::debug!(
                        rule = content_rule,
                        fallback = fallback.as_str(),
                        "content @textNodes empty, fell back to @html"
                    );
                    part = retried;
                }
            }
        }
        record_ar(&ar, store);
        parts.push(part);
    }
    // 书源自带的净化规则：先 sourceRegex（源清洗）再 replaceRegex（可读性替换）
    let raw = parts.join("\n");
    let cleaned_source = apply_legado_replacements(&raw, &rule.source_regex);
    let cleaned = apply_legado_replacements(&cleaned_source, &rule.replace_regex);
    Ok(BookContent {
        book_url: chapter.book_url.clone().unwrap_or_default(),
        url: chapter.url.clone(),
        title: chapter.title.clone(),
        content: Some(cleaned),
        chapter_index: chapter.index,
        content_type: "text".into(),
        ..Default::default()
    })
}
