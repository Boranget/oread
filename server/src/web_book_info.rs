//! web_book 的详情解析（move-only 从 web_book.rs 拆出）。

use super::*;

/// 详情页解析出的扩展元数据：写馆藏层（只填空缺），不进入用户库。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookInfoExtras {
    pub series_name: Option<String>,
    pub series_index: Option<f64>,
    pub tags: Vec<String>,
}

impl BookInfoExtras {
    /// 没有系列/标签规则时的兜底：把 `kind`（分类/标签串）拆成标签。
    pub fn from_kind(kind: Option<&str>) -> Self {
        Self {
            tags: tags_from_kind(kind.unwrap_or("")),
            ..Default::default()
        }
    }
}

// ---------------- 书籍详情 ----------------
pub async fn book_info(source: &BookSource, book: &mut Book) -> anyhow::Result<()> {
    book_info_book(&ReqFetcher, source, book, None).await?;
    Ok(())
}

/// 带 book 作用域的详情：预载同书变量（如搜索阶段写入的 `bid`），结束后回写。
pub async fn book_info_book(
    fetcher: &dyn Fetcher,
    source: &BookSource,
    book: &mut Book,
    store: Option<&BookStore>,
) -> anyhow::Result<BookInfoExtras> {
    // 无详情规则时不抓页面（与原行为一致，避免额外请求）
    if source.rule_book_info.is_none() {
        return Ok(BookInfoExtras::from_kind(book.kind.as_deref()));
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
) -> anyhow::Result<BookInfoExtras> {
    let rule: &BookInfoRule = match &source.rule_book_info {
        Some(r) => r,
        None => return Ok(BookInfoExtras::from_kind(book.kind.as_deref())),
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
    Ok(extras_from_rules(
        source,
        &ar,
        &content,
        &url,
        rule,
        book.kind.as_deref(),
    ))
}

/// 详情规则 → 系列 / 系列序号 / 标签。
///
/// 这三个不是 Legado 标准字段，书源可选配；未配时标签退化为拆分 `kind`，
/// 系列保持为空（不猜测书名里的「第 N 部」，避免污染馆藏元数据）。
fn extras_from_rules(
    source: &BookSource,
    ar: &AnalyzeRule,
    content: &str,
    url: &str,
    rule: &BookInfoRule,
    kind: Option<&str>,
) -> BookInfoExtras {
    let mut extras = BookInfoExtras::default();
    if let Some(raw) = rule
        .series_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let value = eval_field(source, "series_name", ar, content, raw, false, url);
        extras.series_name = Some(value).filter(|v| !blank_value(v));
    }
    if let Some(raw) = rule
        .series_index
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let value = eval_field(source, "series_index", ar, content, raw, false, url);
        extras.series_index = parse_series_index(&value);
    }
    if let Some(raw) = rule
        .tags
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        extras.tags = normalize_tags(ar.get_string_list_all(content, raw, url));
        if extras.tags.is_empty() {
            // 规则只给单值时列表接口可能落空，再按单值求一次并拆分分隔符。
            let value = eval_field(source, "tags", ar, content, raw, false, url);
            extras.tags = split_tags(&value);
        }
    }
    if extras.tags.is_empty() {
        extras.tags = tags_from_kind(kind.unwrap_or(""));
    }
    extras
}

/// 系列序号：接受 `3`、`3.5`、`第3卷`、`第三部`；非法值返回 `None`。
fn parse_series_index(raw: &str) -> Option<f64> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.trim_start_matches("第").trim();
    let text = text
        .trim_end_matches(|c: char| matches!(c, '卷' | '部' | '册' | '集' | '季'))
        .trim();
    let first = text.split(['/', '／']).next().unwrap_or(text).trim();
    if let Ok(v) = first.parse::<f64>() {
        return (v.is_finite() && v >= 0.0).then_some(v);
    }
    chinese_number(first).map(|v| v as f64)
}

/// 纯中文数字（到百位）：`三`、`十二`、`一百零五`。无法解析返回 `None`。
fn chinese_number(text: &str) -> Option<u32> {
    fn digit(c: char) -> Option<u32> {
        Some(match c {
            '零' | '〇' => 0,
            '一' => 1,
            '二' | '两' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    }
    let mut total = 0u32;
    let mut cur = 0u32;
    let mut seen = false;
    for c in text.chars() {
        if let Some(d) = digit(c) {
            cur = d;
            seen = true;
            continue;
        }
        let unit = match c {
            '十' => 10,
            '百' => 100,
            _ => return None,
        };
        total += if cur == 0 { 1 } else { cur } * unit;
        cur = 0;
        seen = true;
    }
    seen.then_some(total + cur)
}

/// 标签上限：与馆藏元数据接口保持一致，避免把整段分类描述塞进 OPF。
const MAX_TAGS: usize = 16;
const MAX_TAG_CHARS: usize = 24;

fn normalize_tags(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let tag = clean_tag(&value);
        if tag.is_empty() || out.iter().any(|t| t == &tag) {
            continue;
        }
        out.push(tag);
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

fn clean_tag(raw: &str) -> String {
    let tag = raw.trim();
    let tag = tag
        .strip_prefix("标签")
        .or_else(|| tag.strip_prefix("分类"))
        .unwrap_or(tag);
    let tag = tag
        .trim_start_matches(|c: char| matches!(c, ':' | '：' | '·' | '-'))
        .trim();
    tag.chars().take(MAX_TAG_CHARS).collect()
}

/// `kind` 是书源的分类/标签串（`玄幻,奇幻`、`玄幻、奇幻` 等），拆成标签。
fn tags_from_kind(kind: &str) -> Vec<String> {
    split_tags(kind)
}

fn split_tags(raw: &str) -> Vec<String> {
    normalize_tags(
        raw.split(|c: char| {
            matches!(
                c,
                ',' | '，' | '、' | '/' | '／' | '|' | '｜' | ';' | '；' | '\n' | '\r' | '\t'
            )
        })
        .map(str::to_string)
        .collect(),
    )
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
