//! rule_engine HTML 辅助的jQuery 过滤器与选择（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// jsoup `Element.text()` 的空白归一（与 `normaliseWhitespace` 一致：连续空白折叠为单空格并裁边）。
pub(in crate::rule_engine) fn normalized_text(el: &scraper::ElementRef) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// jsoup `Element.ownText()` 的空白归一（只保留元素自身直接文本节点）。
pub(in crate::rule_engine) fn normalized_own_text(el: &scraper::ElementRef) -> String {
    text_nodes(el)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// jsoup `Element.elementSiblingIndex()`：父节点下、当前元素之前有多少个元素兄弟。
/// 返回 None 表示没有元素父节点（根元素），jsoup 的 `:eq/:lt/:gt` 会排除根元素。
pub(in crate::rule_engine) fn jsoup_sibling_index(el: &scraper::ElementRef) -> Option<usize> {
    let parent = el.parent().and_then(scraper::ElementRef::wrap)?;
    Some(
        parent
            .children()
            .filter(|n| n.value().as_element().is_some())
            .take_while(|n| n.id() != el.id())
            .count(),
    )
}

/// 按出现顺序依次套用 jQuery 过滤（与 jQuery 对结果集链式过滤的语义一致）。
pub(in crate::rule_engine) fn apply_jq_filters<'a>(
    mut els: Vec<scraper::ElementRef<'a>>,
    filters: &[JqFilter],
) -> Vec<scraper::ElementRef<'a>> {
    for f in filters {
        els = match f {
            JqFilter::First => els.into_iter().take(1).collect(),
            JqFilter::Last => els.pop().into_iter().collect(),
            // jsoup 的 :eq/:lt/:gt 比较 `elementSiblingIndex()`（父节点下元素序号），
            // 不是结果集下标；没有从尾取负数的语义，负值按普通数值比较。
            JqFilter::Eq(n) => els
                .into_iter()
                .filter(|e| *n >= 0 && jsoup_sibling_index(e).is_some_and(|idx| idx as i64 == *n))
                .collect(),
            JqFilter::Lt(n) => els
                .into_iter()
                .filter(|e| jsoup_sibling_index(e).is_some_and(|idx| (idx as i64) < *n))
                .collect(),
            JqFilter::Gt(n) => els
                .into_iter()
                .filter(|e| jsoup_sibling_index(e).is_some_and(|idx| (idx as i64) > *n))
                .collect(),
            JqFilter::Contains(text) => {
                // jsoup 语义：`Element.text()` 包含匹配，**不区分大小写**
                let needle = text.to_lowercase();
                els.into_iter()
                    .filter(|e| normalized_text(e).to_lowercase().contains(&needle))
                    .collect()
            }
            JqFilter::ContainsOwn(text) => {
                let needle = text.to_lowercase();
                els.into_iter()
                    .filter(|e| own_text_contains(e, &needle))
                    .collect()
            }
            JqFilter::Matches(pattern) => {
                // 上游用 Java Pattern.find()，这里用 regex crate 做同样的“部分匹配”。
                // 非法正则上游会抛 SelectorParseException；本引擎在过滤链里降级为不匹配。
                let re = regex::Regex::new(pattern).ok();
                els.into_iter()
                    .filter(|e| re.as_ref().is_some_and(|r| r.is_match(&normalized_text(e))))
                    .collect()
            }
            JqFilter::MatchesOwn(pattern) => {
                let re = regex::Regex::new(pattern).ok();
                els.into_iter()
                    .filter(|e| {
                        re.as_ref()
                            .is_some_and(|r| r.is_match(&normalized_own_text(e)))
                    })
                    .collect()
            }
            JqFilter::Has(sel) => {
                let sel = scraper::Selector::parse(sel).ok();
                els.into_iter()
                    .filter(|e| sel.as_ref().is_some_and(|s| e.select(s).next().is_some()))
                    .collect()
            }
        };
    }
    els
}

/// 初始选择器：支持 Legado 的 `class.x` / `id.x` / `tag.x`、可选索引（取第 i 个），
/// `!下标`（Legado 语义是**排除**这些下标，如 `li!0:1:2:3:4` 去掉前 5 个 li），
/// 以及 jQuery 伪类（`:first/:last/:eq(n)/:lt(n)/:gt(n)/:contains(...)`）。
pub(in crate::rule_engine) fn select_initial<'a>(
    doc: &'a scraper::Html,
    selector: &str,
) -> Vec<scraper::ElementRef<'a>> {
    // Legado 末尾下标：class.x[3,4] / tag.li[0:2] / class.x[-1:0]（倒序）
    let (selector, index_expr) = split_bracket_indices(selector.trim());
    let selector = selector.as_str();
    let (core, excluded) = match selector.find('!') {
        Some(pos) => (&selector[..pos], parse_indices(&selector[pos + 1..])),
        None => (selector, Vec::new()),
    };
    let (core, jq) = split_jquery_filters(core.trim());
    let mut text_filter: Option<String> = None;
    let (css, idx) = if let Some(rest) = core.trim().strip_prefix("text.") {
        // Legado `text.xxx`：元素**自身**直接文本包含 xxx（jsoup
        // `getElementsContainingOwnText`）。此前被当成 CSS「标签 text + 类名 xxx」，
        // 永远匹配不到元素，正文下一页/目录翻页/完整目录入口因此全空（28 源）。
        let (name, i) = split_name_index(rest);
        text_filter = Some(name);
        ("*".to_string(), i)
    } else if let Some(rest) = core.trim().strip_prefix("class.") {
        let (name, i) = split_name_index(rest);
        (class_name_css(shorthand_segment(&name)), i)
    } else if let Some(rest) = core.trim().strip_prefix("id.") {
        let (name, i) = split_name_index(rest);
        (format!("#{}", shorthand_segment(&name)), i)
    } else if let Some(rest) = core.trim().strip_prefix("tag.") {
        // Legado 常用 `tag.a@href`：`tag.` 前缀表示标签名，不是合法 CSS 选择器
        let (name, i) = split_name_index(rest);
        (shorthand_segment(&name).to_string(), i)
    } else {
        // 裸 `tag.N` / `tag.-N`（如 `a.0`、`li.-1`）与 `.class.N`（如 `.author.0`）：
        // 上游 `findIndexSet` 先把尾部 `.N` 拆成下标，剩余部分才是选择器；
        // 否则 CSS 会把 `a.0` / `.author.0` 判为非法选择器而返回空。
        let (name, i) = split_name_index(core.trim());
        let bare_tag_index =
            i.is_some() && !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric());
        let indexed_selector =
            i.is_some() && !name.is_empty() && scraper::Selector::parse(&name).is_ok();
        if bare_tag_index || indexed_selector {
            (name, i)
        } else {
            (core.trim().to_string(), None)
        }
    };
    // 只有伪类（如 `:first`）时按全部元素处理
    let css = if css.trim().is_empty() {
        "*".to_string()
    } else {
        css
    };
    let sel = match scraper::Selector::parse(&css) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let els: Vec<scraper::ElementRef<'a>> = match &text_filter {
        Some(needle) => text_contains_from(&[doc.root_element()], needle),
        None => doc.select(&sel).collect(),
    };
    let els = filter_indices(els, &excluded, true);
    let els = match &index_expr {
        Some(expr) => apply_index_expr(els, expr),
        None => els,
    };
    let els = apply_jq_filters(els, &jq);
    if let Some(i) = idx {
        match resolve_index(i, els.len()) {
            Some(k) => vec![els[k]],
            None => vec![],
        }
    } else {
        els
    }
}

/// 取元素的直接子元素（仅元素节点）。
pub(in crate::rule_engine) fn child_elements<'a>(
    el: scraper::ElementRef<'a>,
) -> Vec<scraper::ElementRef<'a>> {
    el.children()
        .filter_map(scraper::ElementRef::wrap)
        .collect()
}

/// 从当前元素集合向下钻取到指定标签（@tag.name[.index]）。
/// Legado 的 `@tag.x` 是**后代**查询（jsoup `getElementsByTag`），不是只看直接子元素；
/// 需要直接子元素时用 `@children`。
pub(in crate::rule_engine) fn apply_tag<'a>(
    els: Vec<scraper::ElementRef<'a>>,
    spec: &str,
) -> Vec<scraper::ElementRef<'a>> {
    let (spec, index_expr) = split_bracket_indices(spec.trim());
    let (tag, idx) = split_name_index(spec.trim());
    let (tag, jq) = split_jquery_filters(tag.trim());
    let matched: Vec<scraper::ElementRef<'a>> =
        if let Some(needle) = tag.trim().strip_prefix("text.") {
            // `@text.xxx`：在当前元素及其后代中，按自身文本包含关系筛选（上游同一语义）。
            text_contains_from(&els, needle)
        } else {
            let css = if tag.trim().is_empty() {
                "*".to_string()
            } else {
                tag
            };
            let sel = match scraper::Selector::parse(&css) {
                Ok(s) => s,
                Err(_) => return vec![],
            };
            let mut m: Vec<scraper::ElementRef<'a>> = Vec::new();
            for e in &els {
                m.extend(e.select(&sel));
            }
            m
        };
    let matched = apply_jq_filters(matched, &jq);
    let matched = match &index_expr {
        Some(expr) => apply_index_expr(matched, expr),
        None => matched,
    };
    if let Some(i) = idx {
        match resolve_index(i, matched.len()) {
            Some(k) => vec![matched[k]],
            None => vec![],
        }
    } else {
        matched
    }
}

/// 取元素自身的直接文本节点（不含子元素文本），对应 @textNodes。
pub(in crate::rule_engine) fn text_nodes(el: &scraper::ElementRef) -> String {
    el.children()
        .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
        .collect()
}

/// Legado `text.xxx` 的单元素判定：自身直接文本（空白归一、忽略大小写）包含 needle。
pub(in crate::rule_engine) fn own_text_contains(
    el: &scraper::ElementRef,
    needle_lower: &str,
) -> bool {
    let own = text_nodes(el)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    own.contains(needle_lower)
}

/// 从给定根元素（含自身）按文档序收集「自身文本包含 needle」的元素，
/// 对应 jsoup `Element.getElementsContainingOwnText`。
pub(in crate::rule_engine) fn text_contains_from<'a>(
    roots: &[scraper::ElementRef<'a>],
    needle: &str,
) -> Vec<scraper::ElementRef<'a>> {
    let needle = needle.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<scraper::ElementRef<'a>> = Vec::new();
    for root in roots {
        let mut candidates = vec![*root];
        candidates.extend(root.descendants().filter_map(scraper::ElementRef::wrap));
        for c in candidates {
            if own_text_contains(&c, &needle) && seen.insert(c.id()) {
                out.push(c);
            }
        }
    }
    out
}

/// 裸抽取标记（`text` / `href` 等）：Legado 语义是对“当前节点”直接取值。
/// 解析片段，取第一个真实元素（跳过 html/head/body 容器）；没有元素时按纯文本处理。
pub(in crate::rule_engine) fn extract_first(html: &str, tok: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let sel = match scraper::Selector::parse("*") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let first = doc
        .select(&sel)
        .find(|e| !matches!(e.value().name(), "html" | "head" | "body"));
    match first {
        Some(el) => match tok {
            "textNodes" => text_nodes(&el),
            "html" => el.inner_html(),
            "all" => el.html(),
            _ => extract_token(el, tok),
        },
        None => match tok {
            "text" | "ownText" | "textNodes" | "html" | "all" => html.to_string(),
            _ => String::new(),
        },
    }
}
