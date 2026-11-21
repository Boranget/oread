//! rule_engine HTML 辅助的元素取值与文档解析（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// 对单个元素按抽取标记取值（@text/@href/@content/@src/@html/@ownText/@all 等）。
pub(in crate::rule_engine) fn extract_token(el: scraper::ElementRef, tok: &str) -> String {
    match tok {
        "text" => jsoup_text(el),
        "ownText" => el.text().collect::<Vec<_>>().join(""),
        "html" => el.inner_html(),
        "all" => el.html(),
        "href" | "src" | "alt" | "content" | "id" | "value" | "class" | "style" | "title"
        | "name" => el.value().attr(tok).unwrap_or("").to_string(),
        // 任意属性（@data-id / @data-src 等）：属性不存在返回空串，不回退成整节点文本
        _ => el.value().attr(tok).unwrap_or("").to_string(),
    }
}

/// CSS 单值解析用的文档构造：表格行/单元格片段要带回 `<table>` 上下文，
/// 否则 html5ever 会按 “in body” 规则丢弃 tr/td，`p:nth-of-type` 等结构选择器会失真。
pub(in crate::rule_engine) fn parse_css_document(html: &str) -> scraper::Html {
    let head: String = html
        .trim_start()
        .chars()
        .take(9)
        .collect::<String>()
        .to_ascii_lowercase();
    let table_fragment = [
        "<tr", "<td", "<th", "<tbody", "<thead", "<tfoot", "<caption",
    ]
    .iter()
    .any(|p| head.starts_with(p));
    if table_fragment {
        scraper::Html::parse_document(&format!("<table>{}</table>", html.trim_start()))
    } else {
        scraper::Html::parse_document(html)
    }
}

/// 单值抽取的集合级实现：保留最后一个匹配结果。
/// - text / ownText：逐元素取值，空值跳过；
/// - textNodes：逐元素把直接文本节点 trim 后按 `\n` 合并，空元素跳过；
/// - html：先删 script/style，再取整组元素的 outerHtml（合并为一个值）；
/// - all：整组元素 outerHtml（合并为一个值）；
/// - 其余按属性名逐个取值，空值与重复值跳过。
pub(in crate::rule_engine) fn terminal_values(
    els: &[scraper::ElementRef],
    tok: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match tok {
        "text" => {
            for el in els {
                let v = extract_token(*el, "text");
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
        "ownText" => {
            for el in els {
                let v: String = el
                    .children()
                    .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
                    .collect();
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
        "textNodes" => {
            for el in els {
                let mut tn: Vec<String> = Vec::new();
                for n in el.children() {
                    if let Some(t) = n.value().as_text() {
                        let s = t.trim();
                        if !s.is_empty() {
                            tn.push(s.to_string());
                        }
                    }
                }
                if !tn.is_empty() {
                    out.push(tn.join("\n"));
                }
            }
        }
        "html" => {
            let joined: String = els
                .iter()
                .map(|e| outer_html_without_script_style(*e))
                .collect();
            if !joined.is_empty() {
                out.push(joined);
            }
        }
        "all" => {
            let joined: String = els.iter().map(|e| e.html()).collect();
            out.push(joined);
        }
        _ => {
            for el in els {
                let v = el.value().attr(tok).unwrap_or("").to_string();
                if v.trim().is_empty() || out.contains(&v) {
                    continue;
                }
                out.push(v);
            }
        }
    }
    out
}

/// 上游 `getResultLast("html")` 的取法：剔除元素内的 script/style 后取 outerHtml。
pub(in crate::rule_engine) fn outer_html_without_script_style(el: scraper::ElementRef) -> String {
    if matches!(el.value().name(), "script" | "style") {
        return String::new();
    }
    static STRIP: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (script, style) = STRIP.get_or_init(|| {
        (
            Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap(),
            Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap(),
        )
    });
    let outer = el.html();
    let html = script.replace_all(&outer, "");
    style.replace_all(&html, "").to_string()
}

/// 抽取/遍历 token 解析结果。
pub(in crate::rule_engine) enum Token {
    Children,
    Tag {
        name: String,
        index: Option<i64>,
        indices: Vec<usize>,
        /// 上游规则的 `!` 是“排除这些下标”，而不是倒序索引（`li!0:1` = 去掉前两个）。
        exclude: bool,
    },
    Terminal(String),
}

/// 已知的末端抽取关键字（@text/@href/@html/@all 等）。
pub(in crate::rule_engine) fn is_extract_keyword(tok: &str) -> bool {
    matches!(
        tok,
        "text"
            | "ownText"
            | "textNodes"
            | "html"
            | "all"
            | "href"
            | "src"
            | "content"
            | "alt"
            | "id"
            | "value"
            | "class"
            | "style"
            | "title"
            | "name"
    )
}

/// 解析一个 `@` 之后的 traversal/terminal token（支持裸标签、`tag.`、`!indices`）。
pub(in crate::rule_engine) fn parse_token(tok: &str) -> Token {
    if tok == "children" || tok == "child" {
        return Token::Children;
    }
    // 下标筛选：dd!0:1:2 或 tag.a!0:1（`!` 表示排除）
    if let Some(pos) = tok.find('!') {
        let (name, idxs) = tok.split_at(pos);
        let name = name.strip_prefix("tag.").unwrap_or(name);
        let indices: Vec<usize> = idxs[1..]
            .split(':')
            .filter_map(|x| x.parse::<usize>().ok())
            .collect();
        return Token::Tag {
            name: name.to_string(),
            index: None,
            indices,
            exclude: true,
        };
    }
    if let Some(rest) = tok.strip_prefix("tag.") {
        let (name, index) = split_name_index(rest);
        return Token::Tag {
            name: shorthand_segment(&name).to_string(),
            index,
            indices: vec![],
            exclude: false,
        };
    }
    if let Some(rest) = tok.strip_prefix("text.") {
        // `@text.xxx`：上游规则的「自身文本包含」遍历步骤，保留 text. 前缀交给 apply_tag 处理。
        let (name, index) = split_name_index(rest);
        return Token::Tag {
            name: format!("text.{name}"),
            index,
            indices: vec![],
            exclude: false,
        };
    }
    if is_extract_keyword(tok) {
        return Token::Terminal(tok.to_string());
    }
    let (raw_name, index) = split_name_index(tok);
    // 上游规则的 `class.xxx` / `id.xxx` 是类/ID 选择器简写，不是末端抽取标记
    let name = if let Some(rest) = raw_name.strip_prefix("class.") {
        class_name_css(shorthand_segment(rest))
    } else if let Some(rest) = raw_name.strip_prefix("id.") {
        format!("#{}", shorthand_segment(rest))
    } else {
        raw_name
    };
    // `@.book.0` / `@#id` 是上游规则默认规则里的 class/id 简写；此前被当成末端抽取
    // 标记，选择器结果直接丢弃（错层小说网详情 cover/kind 全 null 的原因）。
    let css_shorthand = (name.starts_with('.') || name.starts_with('#')) && name.len() > 1;
    // jQuery 伪类 / 属性选择器同样属于选择器（`@li:eq(0)`、`@a[href]`），不能当抽取标记
    let pseudo_selector = name.contains(':') || name.contains('[');
    if css_shorthand
        || pseudo_selector
        || (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic()))
    {
        return Token::Tag {
            name,
            index,
            indices: vec![],
            exclude: false,
        };
    }
    Token::Terminal(tok.to_string())
}

/// 拼装 apply_tag 用的 spec（name 或 name.index）。
pub(in crate::rule_engine) fn build_spec(name: &str, index: Option<i64>) -> String {
    match index {
        Some(i) => format!("{name}.{i}"),
        None => name.to_string(),
    }
}

/// 解析 `0:1:2` 形式的下标列表。
pub(in crate::rule_engine) fn parse_indices(s: &str) -> Vec<usize> {
    s.split(':')
        .filter_map(|x| x.trim().parse::<usize>().ok())
        .collect()
}

/// 按 `!` 语义过滤下标：`exclude=true` 时剔除这些下标，否则只保留这些下标。
pub(in crate::rule_engine) fn filter_indices<'a>(
    els: Vec<scraper::ElementRef<'a>>,
    indices: &[usize],
    exclude: bool,
) -> Vec<scraper::ElementRef<'a>> {
    if indices.is_empty() {
        return els;
    }
    els.into_iter()
        .enumerate()
        .filter(|(i, _)| indices.contains(i) != exclude)
        .map(|(_, e)| e)
        .collect()
}

/// 按规则选中元素并返回其 outer HTML 列表（供 bookList 嵌套解析与 java.getElements 复用）。
pub(in crate::rule_engine) fn elements_html(html: &str, rule: &str) -> Vec<String> {
    let (selector, tokens) = split_css_rule(rule);
    let doc = scraper::Html::parse_document(html);
    let mut els = select_initial(&doc, &selector);
    for tok in &tokens {
        let tok = *tok;
        if tok.is_empty() {
            continue;
        }
        match parse_token(tok) {
            Token::Children => els = els.into_iter().flat_map(child_elements).collect(),
            Token::Tag {
                name,
                index,
                indices,
                exclude,
            } => {
                let mut m = apply_tag(els, &build_spec(&name, index));
                m = filter_indices(m, &indices, exclude);
                els = m;
            }
            Token::Terminal(_) => break, // 列表模式遇到抽取标记即停止遍历
        }
    }
    els.iter().map(|e| e.html()).collect()
}
