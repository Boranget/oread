//! HTML/CSS 选择器与 Legado 取词辅助（move-only 从 rule_engine.rs 拆出）。
//!
//! 这些函数同时服务 AnalyzeRule 的 CSS/CSS 增强语法与 JS 的 org.jsoup 桥；
//! 对外只暴露给父模块（pub(super)），不扩展 crate 公共 API。

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

pub(super) fn is_jsoup_block_tag(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "frameset"
            | "script"
            | "noscript"
            | "style"
            | "meta"
            | "link"
            | "title"
            | "frame"
            | "noframes"
            | "section"
            | "nav"
            | "aside"
            | "hgroup"
            | "header"
            | "footer"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "div"
            | "blockquote"
            | "hr"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "ins"
            | "del"
            | "dl"
            | "dt"
            | "dd"
            | "li"
            | "table"
            | "caption"
            | "thead"
            | "tfoot"
            | "tbody"
            | "colgroup"
            | "col"
            | "tr"
            | "th"
            | "td"
            | "video"
            | "audio"
            | "canvas"
            | "details"
            | "menu"
            | "plaintext"
            | "template"
            | "article"
            | "main"
            | "svg"
            | "math"
            | "center"
            | "dir"
            | "applet"
            | "marquee"
            | "listing"
    )
}

/// jsoup `Tag.preserveWhitespaceTags`（1.16.2）。
pub(super) fn is_jsoup_preserve_ws_tag(name: &str) -> bool {
    matches!(name, "pre" | "plaintext" | "title" | "textarea")
}

/// jsoup `Tag.formatAsInlineTags`（1.16.2）：块级标签但按行内方式排版，
/// `Element.text()` 的 tail 阶段不会在这些元素前补空格。
pub(super) fn is_jsoup_format_inline_tag(name: &str) -> bool {
    matches!(
        name,
        "title"
            | "a"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "address"
            | "li"
            | "th"
            | "td"
            | "script"
            | "style"
            | "ins"
            | "del"
            | "s"
    )
}

/// jsoup 的 `isActuallyWhitespace`（只认这几个 ASCII 空白，不把 NBSP/全角空格算进去）。
pub(super) fn is_jsoup_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

pub(super) fn jsoup_last_char_is_ws(accum: &str) -> bool {
    accum.chars().last().map(is_jsoup_ws).unwrap_or(false)
}

/// jsoup `TextNode.appendNormalisedWhitespace`：连续空白折叠成一个空格，
/// `accum` 已以空白结尾时跳过前导空白。
pub(super) fn append_jsoup_normalised(accum: &mut String, text: &str) {
    let strip_leading = jsoup_last_char_is_ws(accum);
    let mut last_white = false;
    let mut reached_non_white = false;
    for c in text.chars() {
        if is_jsoup_ws(c) {
            if strip_leading && !reached_non_white {
                continue;
            }
            if last_white {
                continue;
            }
            accum.push(' ');
            last_white = true;
        } else {
            accum.push(c);
            reached_non_white = true;
            last_white = false;
        }
    }
}

/// jsoup `Element.text()`（1.16.2）的等价实现：块级元素/`br` 前后补空格、
/// 空白按 `appendNormalisedWhitespace` 折叠、`pre/plaintext/title/textarea` 保留原样。
pub(super) fn jsoup_text(el: scraper::ElementRef<'_>) -> String {
    let mut accum = String::new();
    // (节点, 是否出栈, 是否处于 preserveWhitespace 子树)
    let mut stack = vec![(*el, false, is_jsoup_preserve_ws_tag(el.value().name()))];
    while let Some((node, exiting, preserve)) = stack.pop() {
        if exiting {
            // jsoup tail：块级元素结束且下一个兄弟是文本或行内元素时补空格
            if let Some(e) = node.value().as_element() {
                if is_jsoup_block_tag(e.name())
                    && !accum.is_empty()
                    && !jsoup_last_char_is_ws(&accum)
                {
                    let need_space = match node.next_sibling() {
                        Some(n) => match n.value() {
                            scraper::node::Node::Text(_) => true,
                            scraper::node::Node::Element(ne) => {
                                !is_jsoup_format_inline_tag(ne.name())
                            }
                            _ => false,
                        },
                        None => false,
                    };
                    if need_space {
                        accum.push(' ');
                    }
                }
            }
            continue;
        }
        match node.value() {
            scraper::node::Node::Text(t) => {
                let s = t.text.to_string();
                if preserve {
                    accum.push_str(&s);
                } else {
                    append_jsoup_normalised(&mut accum, &s);
                }
            }
            scraper::node::Node::Element(e) => {
                let name = e.name();
                // jsoup head：进入块级元素（或 br）时，若已有内容且不以空白结尾则补空格
                if !accum.is_empty()
                    && (is_jsoup_block_tag(name) || name == "br")
                    && !jsoup_last_char_is_ws(&accum)
                {
                    accum.push(' ');
                }
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                let child_preserve = preserve || is_jsoup_preserve_ws_tag(name);
                for child in kids.into_iter().rev() {
                    stack.push((child, false, child_preserve));
                }
            }
            _ => {
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                for child in kids.into_iter().rev() {
                    stack.push((child, false, preserve));
                }
            }
        }
    }
    accum.trim().to_string()
}

/// 对单个元素按抽取标记取值（@text/@href/@content/@src/@html/@ownText/@all 等）。
pub(super) fn extract_token(el: scraper::ElementRef, tok: &str) -> String {
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
pub(super) fn parse_css_document(html: &str) -> scraper::Html {
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

/// 单值抽取的集合级实现：对应上游 `JSoup 规则.getResultLast`。
/// - text / ownText：逐元素取值，空值跳过；
/// - textNodes：逐元素把直接文本节点 trim 后按 `\n` 合并，空元素跳过；
/// - html：先删 script/style，再取整组元素的 outerHtml（合并为一个值）；
/// - all：整组元素 outerHtml（合并为一个值）；
/// - 其余按属性名逐个取值，空值与重复值跳过。
pub(super) fn terminal_values(els: &[scraper::ElementRef], tok: &str) -> Vec<String> {
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
pub(super) fn outer_html_without_script_style(el: scraper::ElementRef) -> String {
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
pub(super) enum Token {
    Children,
    Tag {
        name: String,
        index: Option<i64>,
        indices: Vec<usize>,
        /// Legado 的 `!` 是“排除这些下标”，而不是倒序索引（`li!0:1` = 去掉前两个）。
        exclude: bool,
    },
    Terminal(String),
}

/// 已知的末端抽取关键字（@text/@href/@html/@all 等）。
pub(super) fn is_extract_keyword(tok: &str) -> bool {
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
pub(super) fn parse_token(tok: &str) -> Token {
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
        // `@text.xxx`：Legado 的「自身文本包含」遍历步骤，保留 text. 前缀交给 apply_tag 处理。
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
    // Legado 的 `class.xxx` / `id.xxx` 是类/ID 选择器简写，不是末端抽取标记
    let name = if let Some(rest) = raw_name.strip_prefix("class.") {
        class_name_css(shorthand_segment(rest))
    } else if let Some(rest) = raw_name.strip_prefix("id.") {
        format!("#{}", shorthand_segment(rest))
    } else {
        raw_name
    };
    // `@.book.0` / `@#id` 是 Legado 默认规则里的 class/id 简写；此前被当成末端抽取
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
pub(super) fn build_spec(name: &str, index: Option<i64>) -> String {
    match index {
        Some(i) => format!("{name}.{i}"),
        None => name.to_string(),
    }
}

/// 解析 `0:1:2` 形式的下标列表。
pub(super) fn parse_indices(s: &str) -> Vec<usize> {
    s.split(':')
        .filter_map(|x| x.trim().parse::<usize>().ok())
        .collect()
}

/// 按 `!` 语义过滤下标：`exclude=true` 时剔除这些下标，否则只保留这些下标。
pub(super) fn filter_indices<'a>(
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
pub(super) fn elements_html(html: &str, rule: &str) -> Vec<String> {
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

/// 去掉 @css:/@CSS:/@@ 前缀，并按 '@' 切分为 (选择器, 抽取/遍历标记)。
pub(super) fn split_css_rule(rule: &str) -> (String, Vec<&str>) {
    let rule = strip_prefix_ci(rule, "@css:")
        .trim_start_matches("@@")
        .trim();
    let parts: Vec<&str> = rule.split('@').collect();
    let selector = parts[0].trim().to_string();
    let tokens: Vec<&str> = parts[1..].iter().map(|s| s.trim()).collect();
    (selector, tokens)
}

/// 拆出 Legado 末尾的 `[n]` / `[n,m]` / `[a:b]` 下标表达式。
pub(super) fn split_bracket_indices(selector: &str) -> (String, Option<String>) {
    let s = selector.trim();
    if !s.ends_with(']') {
        return (s.to_string(), None);
    }
    if let Some(open) = s.rfind('[') {
        let inner = &s[open + 1..s.len() - 1];
        if inner
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | ':' | '-' | ' '))
        {
            return (s[..open].trim().to_string(), Some(inner.to_string()));
        }
    }
    (s.to_string(), None)
}

/// 对已选元素应用 Legado 下标表达式：`[3,4]` 取第 3/4 个，`[0:2]` 切片，`[-1:0]` 倒序。
pub(super) fn apply_index_expr<'a>(
    els: Vec<scraper::ElementRef<'a>>,
    expr: &str,
) -> Vec<scraper::ElementRef<'a>> {
    let len = els.len() as i64;
    if len == 0 {
        return els;
    }
    let mut selected: Vec<usize> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    {
        let mut push = |i: i64, selected: &mut Vec<usize>| {
            if i >= 0 && i < len {
                let u = i as usize;
                if seen.insert(u) {
                    selected.push(u);
                }
            }
        };
        for part in expr.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((start_s, rest)) = part.split_once(':') {
                let (end_s, step_s) = match rest.split_once(':') {
                    Some((e, st)) => (e, Some(st)),
                    None => (rest, None),
                };
                let parse_opt = |s: &str| -> Option<i64> {
                    let t = s.trim();
                    if t.is_empty() {
                        None
                    } else {
                        t.parse::<i64>().ok()
                    }
                };
                let start_x = parse_opt(start_s);
                let end_x = parse_opt(end_s);
                let step_x = parse_opt(step_s.unwrap_or("")).unwrap_or(1);
                let mut start = start_x.unwrap_or(0);
                if start < 0 {
                    start += len;
                }
                let mut end = match end_x {
                    Some(e) if e < 0 => e + len,
                    Some(e) => e,
                    None => len - 1,
                };
                // 两端都在同一侧越界：整段无效（上游直接跳过）
                if (start < 0 && end < 0) || (start >= len && end >= len) {
                    continue;
                }
                start = start.clamp(0, len - 1);
                end = end.clamp(0, len - 1);
                if start == end || step_x >= len {
                    push(start, &mut selected);
                    continue;
                }
                let step = if step_x > 0 {
                    step_x
                } else if -step_x < len {
                    step_x + len
                } else {
                    1
                };
                if end > start {
                    let mut i = start;
                    while i <= end {
                        push(i, &mut selected);
                        i += step;
                    }
                } else {
                    let mut i = start;
                    while i >= end {
                        push(i, &mut selected);
                        i -= step;
                    }
                }
            } else if let Ok(it) = part.parse::<i64>() {
                let idx = if it < 0 { it + len } else { it };
                push(idx, &mut selected);
            }
        }
    }
    selected.into_iter().map(|i| els[i]).collect()
}

/// 解析 `name` 或 `name.N`（N 可为负数）形式的规格，返回 (名称, 可选索引)。
pub(super) fn split_name_index(s: &str) -> (String, Option<i64>) {
    if let Some(dot) = s.rfind('.') {
        let (name, idxs) = s.split_at(dot);
        let raw = &idxs[1..];
        if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit() || c == '-') {
            if let Ok(i) = raw.parse::<i64>() {
                return (name.to_string(), Some(i));
            }
        }
    }
    (s.to_string(), None)
}

/// Legado 的 `class.a b` 表示「同时具备 a 和 b 两个类」（AND），
/// 但 CSS 里空格是后代选择器，必须归一化成 `.a.b`。
pub(super) fn class_name_css(rest: &str) -> String {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(".{}", parts.join("."))
}

/// `class.x.y` / `id.x.y` / `tag.x.y` 只取第一个点分段：上游 `JSoup 规则`
/// 对 `beforeRule.split(".")` 的结果只用 `rules[1]`，其余分段被忽略
/// （`class.list.ul` 等价 `class.list`）。
pub(super) fn shorthand_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// jQuery 扩展伪类（scraper/CSS 不认，需要先剥离再对结果集做过滤）。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum JqFilter {
    First,
    Last,
    /// jsoup `:eq(n)`：比较元素在父节点下的**元素序号**（`elementSiblingIndex()`），
    /// 不是结果集下标，也不支持负数从尾取。
    Eq(i64),
    /// jsoup `:lt(n)`：元素序号 < n（排除根元素，负数无匹配）。
    Lt(i64),
    /// jsoup `:gt(n)`：元素序号 > n（排除根元素，负数匹配全部非根元素）。
    Gt(i64),
    Contains(String),
    /// jsoup `:containsOwn(text)`：只看元素自身直接文本（不含子元素文本）。
    ContainsOwn(String),
    /// jsoup `:matches(regex)`：`Element.text()`（空白归一）按 Java 正则 `find`。
    Matches(String),
    /// jsoup `:matchesOwn(regex)`：`ownText()` 按 Java 正则 `find`。
    MatchesOwn(String),
    /// jsoup `:has(selector)`：存在匹配的后代元素。
    Has(String),
}

pub(super) fn starts_at(cs: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    cs.len() >= i + p.len() && cs[i..i + p.len()] == p[..]
}

/// 找到与 `open` 处 `(` 配对的 `)`；跳过引号内的括号。
pub(super) fn find_close_paren(cs: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, &c) in cs.iter().enumerate().skip(open) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn unquote_arg(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// 把 jQuery 伪类从选择器里剥离，返回 (基础 CSS 选择器, 过滤链)。
/// 只识别 BookSource 常用的 `:first/:last/:eq(n)/:lt(n)/:gt(n)/:contains(文本)`；
/// `:first-child`、`:nth-child(...)` 等原生 CSS 伪类原样留给 scraper。
pub(super) fn split_jquery_filters(selector: &str) -> (String, Vec<JqFilter>) {
    let cs: Vec<char> = selector.chars().collect();
    let mut base = String::new();
    let mut filters = Vec::new();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut bracket = 0usize;
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = quote {
            base.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                base.push(c);
                i += 1;
            }
            '[' => {
                bracket += 1;
                base.push(c);
                i += 1;
            }
            ']' => {
                bracket = bracket.saturating_sub(1);
                base.push(c);
                i += 1;
            }
            ':' if bracket == 0 => {
                let boundary = |n: usize| {
                    cs.get(i + n)
                        .map(|ch| !(ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_'))
                        .unwrap_or(true)
                };
                if starts_at(&cs, i, ":first") && boundary(6) {
                    filters.push(JqFilter::First);
                    i += 6;
                } else if starts_at(&cs, i, ":last") && boundary(5) {
                    filters.push(JqFilter::Last);
                    i += 5;
                } else if starts_at(&cs, i, ":eq(")
                    || starts_at(&cs, i, ":lt(")
                    || starts_at(&cs, i, ":gt(")
                {
                    let open = i + 3;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        if let Ok(n) = unquote_arg(&arg).parse::<i64>() {
                            let f = if starts_at(&cs, i, ":eq(") {
                                JqFilter::Eq(n)
                            } else if starts_at(&cs, i, ":lt(") {
                                JqFilter::Lt(n)
                            } else {
                                JqFilter::Gt(n)
                            };
                            filters.push(f);
                            i = close + 1;
                            continue;
                        }
                    }
                    base.push(c);
                    i += 1;
                } else if starts_at(&cs, i, ":containsOwn(") {
                    let open = i + 12;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::ContainsOwn(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else if starts_at(&cs, i, ":contains(") {
                    let open = i + 9;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::Contains(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else if starts_at(&cs, i, ":matchesOwn(") {
                    let open = i + 11;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::MatchesOwn(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else if starts_at(&cs, i, ":matches(") {
                    let open = i + 8;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::Matches(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else if starts_at(&cs, i, ":has(") {
                    let open = i + 4;
                    if let Some(close) = find_close_paren(&cs, open) {
                        let arg: String = cs[open + 1..close].iter().collect();
                        filters.push(JqFilter::Has(unquote_arg(&arg)));
                        i = close + 1;
                    } else {
                        base.push(c);
                        i += 1;
                    }
                } else {
                    base.push(c);
                    i += 1;
                }
            }
            _ => {
                base.push(c);
                i += 1;
            }
        }
    }
    (base.trim().to_string(), filters)
}

/// jsoup `Element.text()` 的空白归一（与 `normaliseWhitespace` 一致：连续空白折叠为单空格并裁边）。
pub(super) fn normalized_text(el: &scraper::ElementRef) -> String {
    el.text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// jsoup `Element.ownText()` 的空白归一（只保留元素自身直接文本节点）。
pub(super) fn normalized_own_text(el: &scraper::ElementRef) -> String {
    text_nodes(el)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// jsoup `Element.elementSiblingIndex()`：父节点下、当前元素之前有多少个元素兄弟。
/// 返回 None 表示没有元素父节点（根元素），jsoup 的 `:eq/:lt/:gt` 会排除根元素。
pub(super) fn jsoup_sibling_index(el: &scraper::ElementRef) -> Option<usize> {
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
pub(super) fn apply_jq_filters<'a>(
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
pub(super) fn select_initial<'a>(
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
pub(super) fn child_elements<'a>(el: scraper::ElementRef<'a>) -> Vec<scraper::ElementRef<'a>> {
    el.children()
        .filter_map(scraper::ElementRef::wrap)
        .collect()
}

/// 从当前元素集合向下钻取到指定标签（@tag.name[.index]）。
/// Legado 的 `@tag.x` 是**后代**查询（jsoup `getElementsByTag`），不是只看直接子元素；
/// 需要直接子元素时用 `@children`。
pub(super) fn apply_tag<'a>(
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
pub(super) fn text_nodes(el: &scraper::ElementRef) -> String {
    el.children()
        .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
        .collect()
}

/// Legado `text.xxx` 的单元素判定：自身直接文本（空白归一、忽略大小写）包含 needle。
pub(super) fn own_text_contains(el: &scraper::ElementRef, needle_lower: &str) -> bool {
    let own = text_nodes(el)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    own.contains(needle_lower)
}

/// 从给定根元素（含自身）按文档序收集「自身文本包含 needle」的元素，
/// 对应 jsoup `Element.getElementsContainingOwnText`。
pub(super) fn text_contains_from<'a>(
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
pub(super) fn extract_first(html: &str, tok: &str) -> String {
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

/// `/` 在此处是否构成正则字面量的开始（而非除号）。
pub(super) fn js_regex_allowed(rule: &str, at: usize) -> bool {
    let b = rule.as_bytes();
    let mut j = at;
    while j > 0 && (b[j - 1] as char).is_ascii_whitespace() {
        j -= 1;
    }
    if j == 0 {
        return true;
    }
    let p = b[j - 1];
    if matches!(
        p,
        b'(' | b','
            | b'='
            | b':'
            | b'['
            | b'!'
            | b'&'
            | b'|'
            | b'?'
            | b'{'
            | b'}'
            | b';'
            | b'+'
            | b'-'
            | b'*'
            | b'%'
            | b'^'
            | b'~'
            | b'<'
            | b'>'
    ) {
        return true;
    }
    // `return /re/` 这类关键字后的 `/` 也是正则，而不是除法
    let mut k = j;
    while k > 0 && ((b[k - 1] as char).is_ascii_alphanumeric() || matches!(b[k - 1], b'_' | b'$')) {
        k -= 1;
    }
    matches!(
        &rule[k..j],
        "return" | "typeof" | "case" | "in" | "of" | "delete" | "void" | "instanceof" | "yield"
    )
}

/// 从 `<js>` 之后开始，找到**顶层** `</js>` 的字节位置：跳过字符串、模板串、
/// 正则字面量与注释，因此正则里的引号/斜杠、字符串里的 `</js>` 都不会截断 JS。
pub(super) fn find_js_tag_end(rule: &str, start: usize) -> Option<usize> {
    enum St {
        Normal,
        Str(u8),
        Tpl,
        Regex,
        Line,
        Block,
    }
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut st = St::Normal;
    let mut in_class = false;
    let mut i = start;
    while i < n {
        let c = bytes[i];
        match st {
            St::Normal => {
                if c == b'<' && rule[i..].starts_with("</js>") {
                    return Some(i);
                }
                if c == b'"' || c == b'\'' {
                    st = St::Str(c);
                    i += 1;
                    continue;
                }
                if c == b'`' {
                    st = St::Tpl;
                    i += 1;
                    continue;
                }
                if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
                    st = St::Line;
                    i += 2;
                    continue;
                }
                if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    st = St::Block;
                    i += 2;
                    continue;
                }
                if c == b'/' && js_regex_allowed(rule, i) {
                    st = St::Regex;
                    in_class = false;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            St::Str(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Tpl => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'`' {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Regex => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == b'[' {
                    in_class = true;
                } else if c == b']' {
                    in_class = false;
                } else if c == b'/' && !in_class {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Line => {
                if c == b'\n' {
                    st = St::Normal;
                }
                i += 1;
            }
            St::Block => {
                if c == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    st = St::Normal;
                    i += 2;
                    continue;
                }
                i += 1;
            }
        }
    }
    None
}

/// 裸字段名判定：只接受 `text`/`href`/`bid` 这类单标识符，避免劫持 JSONPath/CSS 规则。
pub(super) fn is_simple_json_key(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// JSON 对象列表项取字段：`rule` 是裸字段名（`text`/`href`/`name`/`title`…）时按键取值。
/// 别名与 Legado 章节项约定一致：标题取 `text/name/title`，链接取 `href/url/src`。
/// 返回 `None` 表示 content 不是 JSON 对象（交给原有 CSS/正则链路）；是对象但字段缺失返回空串。
pub(super) fn json_item_field(content: &str, key: &str) -> Option<String> {
    if !is_simple_json_key(key) {
        return None;
    }
    let t = content.trim_start();
    if !t.starts_with('{') {
        return None;
    }
    let val: Value = serde_json::from_str(t).ok()?;
    let map = val.as_object()?;
    let key = key.trim();
    let aliases: &[&str] = match key {
        "text" => &["text", "name", "title"],
        "name" => &["name", "title", "text"],
        "title" => &["title", "name", "text"],
        "href" | "url" | "src" => &["href", "url", "src"],
        "volume" | "isVolume" | "is_volume" => &["volume", "isVolume", "is_volume"],
        _ => &[],
    };
    for k in aliases.iter().copied().chain(std::iter::once(key)) {
        if let Some(v) = map.get(k) {
            return Some(json_scalar_text(v));
        }
    }
    Some(String::new())
}

/// 把展开值拼进 JS 字符串字面量前的转义（反斜杠/换行/闭合引号；模板串额外转义 `${`）。
pub(super) fn escape_js_string(s: &str, quote: char) -> String {
    let mut out = s
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    if quote == '`' {
        out = out.replace("${", "\\${");
    }
    out.replace(quote, &format!("\\{quote}"))
}

/// 标量转字符串；对象/数组按 JSON 串返回，null 为空串。
pub(super) fn json_scalar_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

/// JS 返回的 JSON 数组展开为列表项（Legado `getStringList` 的 List 语义）。
///
/// `keep_objects=true`（目录）：对象保留为紧凑 JSON，供 `chapterName/chapterUrl` 逐项取字段；
/// `keep_objects=false`（图片等纯值）：对象取 `src/url/href`，缺则取文本字段。
///
/// 兼容 `[[title, url]]` 二元数组、嵌套数组、`[{header, list:[…]}]` 分卷结构；
/// 空项/缺字段项跳过并记一条 warn（不报错）。
/// 返回 `None` 表示不是 JSON 数组（保持原调用方的换行拆分逻辑）。
pub(super) fn expand_json_list_items(s: &str, keep_objects: bool) -> Option<Vec<String>> {
    let t = s.trim_start();
    if !t.starts_with('[') {
        return None;
    }
    let val: Value = serde_json::from_str(t).ok()?;
    let mut out = Vec::new();
    let mut dropped = 0usize;
    flatten_json_items(val, keep_objects, &mut out, &mut dropped, true);
    if dropped > 0 {
        tracing::warn!(dropped, keep_objects, "JSON 列表展开跳过空项/缺字段项");
    }
    Some(out)
}

pub(super) fn flatten_json_items(
    v: Value,
    keep: bool,
    out: &mut Vec<String>,
    dropped: &mut usize,
    top_level: bool,
) {
    match v {
        Value::Null => *dropped += 1,
        Value::String(s) => {
            if s.trim().is_empty() {
                *dropped += 1;
            } else {
                out.push(s);
            }
        }
        Value::Bool(b) => out.push(b.to_string()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Array(arr) => {
            // `[[title, url]]` 里的二元数组：规范化为带别名的对象，后续字段规则可直接取值。
            // 顶层数组不按二元处理（`java.getStringList` 返回的 `['甲','乙']` 是两个列表项）。
            if !top_level
                && arr.len() == 2
                && !arr[0].is_array()
                && !arr[0].is_object()
                && !arr[1].is_array()
                && !arr[1].is_object()
            {
                let a = json_scalar_text(&arr[0]).trim().to_string();
                let b = json_scalar_text(&arr[1]).trim().to_string();
                if a.is_empty() && b.is_empty() {
                    *dropped += 1;
                } else if keep {
                    out.push(
                        serde_json::json!({
                            "text": a, "name": a, "title": a,
                            "href": b, "url": b
                        })
                        .to_string(),
                    );
                } else {
                    out.push(if b.is_empty() { a } else { b });
                }
            } else {
                for x in arr {
                    flatten_json_items(x, keep, out, dropped, false);
                }
            }
        }
        Value::Object(mut map) => {
            // 分卷结构 `{header, list:[…]}`：头项作分卷标记，子项递归展开
            if let Some(group) = take_group_list(&mut map) {
                let header = group_header(&map);
                if keep && !header.is_empty() {
                    out.push(
                        serde_json::json!({
                            "text": header, "name": header, "title": header,
                            "href": "", "url": "", "volume": true
                        })
                        .to_string(),
                    );
                }
                for x in group {
                    flatten_json_items(x, keep, out, dropped, false);
                }
            } else if keep {
                if map_has_chapter_field(&map) {
                    out.push(Value::Object(map).to_string());
                } else {
                    *dropped += 1;
                }
            } else {
                let pick = map_pick_url(&map);
                if pick.is_empty() {
                    *dropped += 1;
                } else {
                    out.push(pick);
                }
            }
        }
    }
}

/// 识别分卷：对象里第一个数组型分组字段（`list`/`chapters`/`items`…）。
pub(super) fn take_group_list(map: &mut serde_json::Map<String, Value>) -> Option<Vec<Value>> {
    for k in ["list", "chapters", "items", "chapterList", "chapter_list"] {
        if map.get(k).map(|v| v.is_array()).unwrap_or(false) {
            return match map.remove(k) {
                Some(Value::Array(a)) => Some(a),
                _ => None,
            };
        }
    }
    None
}

pub(super) fn group_header(map: &serde_json::Map<String, Value>) -> String {
    for k in ["header", "name", "title", "text", "chapterName"] {
        if let Some(v) = map.get(k) {
            let s = json_scalar_text(v);
            if !s.trim().is_empty() {
                return s;
            }
        }
    }
    String::new()
}

pub(super) fn map_has_chapter_field(map: &serde_json::Map<String, Value>) -> bool {
    // 字段名不做白名单：书源常用短名（`{n,u}`/`{t,h}`）或自定义字段，由后续字段规则取值；
    // 只要对象里至少有一个非空标量就保留，整项全空才按“缺字段”丢弃。
    map.values().any(|v| !json_scalar_text(v).trim().is_empty())
}

pub(super) fn map_pick_url(map: &serde_json::Map<String, Value>) -> String {
    for k in ["src", "url", "href", "link", "text", "name", "title"] {
        if let Some(v) = map.get(k) {
            let s = json_scalar_text(v);
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
    }
    String::new()
}

/// 解析 `@put:{k1:v1,k2:v2}` 的 `{…}` 内部：顶层（不在引号/括号内）按 `,` 分组，再按首个 `:`
/// 拆 key/value。对应上游 `splitPutRule`（规则语义 L516-530）里的 GSON 宽松解析。
pub(super) fn split_put_pairs(inner: &str) -> Vec<(String, String)> {
    let mut groups: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    for ch in inner.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            cur.push(ch);
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '{' | '[' | '(' => {
                depth += 1;
                cur.push(ch);
            }
            '}' | ']' | ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                groups.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        groups.push(cur.trim().to_string());
    }
    let mut out = Vec::new();
    for group in groups {
        let mut quote: Option<char> = None;
        let mut depth = 0i32;
        let mut split_at = None;
        for (idx, ch) in group.char_indices() {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
                continue;
            }
            match ch {
                '"' | '\'' => quote = Some(ch),
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => depth -= 1,
                ':' if depth == 0 => {
                    split_at = Some(idx);
                    break;
                }
                _ => {}
            }
        }
        if let Some(idx) = split_at {
            let key = group[..idx]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            let value = group[idx + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim();
            if !key.is_empty() {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// 在规则串顶层按分隔符拆分，但忽略落在 `<js>…</js>` 或 `@js:` 段内的分隔符。
/// 仅当当前字节为 ASCII 时才尝试 `starts_with`（分隔符/标记均为 ASCII），避免多字节
/// UTF-8（如中文）字符内部切片越界。
pub(super) fn split_outside_js<'a>(rule: &'a str, sep: &str) -> Option<Vec<&'a str>> {
    let mut parts: Vec<&str> = Vec::new();
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut start = 0usize;
    let mut i = 0usize;
    // 方括号/圆括号/引号内的分隔符属于表达式内部（如 JSONPath 谓词的 `||/&&`），不能切分
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    while i < n {
        if bytes[i] < 0x80 {
            if let Some(q) = quote {
                if bytes[i] == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match bytes[i] {
                b'"' | b'\'' => {
                    quote = Some(bytes[i]);
                    i += 1;
                    continue;
                }
                b'[' | b'(' => {
                    depth += 1;
                    i += 1;
                    continue;
                }
                b']' | b')' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let s = &rule[i..];
            if s.starts_with("<js>") {
                if let Some(pos) = find_js_tag_end(rule, i + 4) {
                    let end = pos + 5;
                    i = end;
                    start = end;
                    continue;
                }
            }
            if starts_with_ci(s, "@js:") {
                // `@js:` 一直延伸到规则末尾：其内部的分隔符不能被当成分割点。
                // 这里只结束扫描（尾巴留在最后一个 part 里），而不是把 `@js:` 也拆成一个 part——
                // 否则 `##` 分支会把 `@js:...` 当成“替换正则”，把带 JS 尾段的规则静默吞掉。
                break;
            }
            if depth == 0 && s.starts_with(sep) {
                parts.push(&rule[start..i]);
                i += sep.len();
                start = i;
                continue;
            }
        }
        i += 1;
    }
    if start < n {
        parts.push(&rule[start..]);
    }
    if parts.len() > 1 {
        Some(parts)
    } else {
        None
    }
}

/// ASCII 大小写不敏感前缀判断（Legado `startsWith(..., ignoreCase = true)`）。
pub(super) fn starts_with_ci(rule: &str, prefix: &str) -> bool {
    rule.len() >= prefix.len()
        && rule.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

pub(super) fn strip_prefix_ci<'a>(rule: &'a str, prefix: &str) -> &'a str {
    if starts_with_ci(rule, prefix) {
        &rule[prefix.len()..]
    } else {
        rule
    }
}

/// 模板展开后的整串是否就是最终值（而非还需要继续解析的规则）。
/// 依据 Legado 语义：含 `{{…}}` 的规则整体按模板求值，展开结果即字段值；
/// 只有仍带 `@text` 这类抽取后缀、`$.`/`@js:`/`<js>` 前缀或 `##`/`||` 管道时才继续解析。
pub(super) fn expanded_is_literal(rule: &str) -> bool {
    let t = rule.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('$') || t.starts_with('@') || starts_with_ci(t, "<js>") {
        return false;
    }
    // 模板没能展开（变量/JS 失败后保留原文）：沿用旧行为继续解析，最终得到空值。
    if t.contains("{{") || t.contains("{$") {
        return false;
    }
    if t.starts_with('/') || t.starts_with(':') {
        return false;
    }
    if t.contains("##") || t.contains("||") || t.contains("&&") || t.contains("%%") {
        return false;
    }
    // `a@text` / `…@href`：模板拼出的选择器 + 抽取标记，仍需走规则管道。
    // 值本身含 `@`（如邮箱）时尾段不是纯标识符，按字面量处理。
    match t.rsplit_once('@') {
        Some((pre, tail)) => {
            !pre.is_empty()
                && !tail
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
        }
        None => true,
    }
}

pub(super) fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

/// 从 `from` 字节处开始找 `needle`（ASCII 大小写不敏感），返回字节下标。
pub(super) fn find_ci_from(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.is_empty() || from >= hb.len() || hb.len() < nb.len() {
        return None;
    }
    (from..=hb.len() - nb.len()).find(|&i| hb[i..i + nb.len()].eq_ignore_ascii_case(nb))
}

/// 从 `open`（`{` 的字节下标）开始按括号深度找配对的 `}`；跳过字符串字面量内的括号。
pub(super) fn find_matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (off, ch) in s[open..].char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + off);
                }
            }
            _ => {}
        }
    }
    None
}

/// 把负下标解析为绝对下标；越界返回 None。
pub(super) fn resolve_index(i: i64, len: usize) -> Option<usize> {
    let n = len as i64;
    let k = if i < 0 { n + i } else { i };
    if k >= 0 && k < n {
        Some(k as usize)
    } else {
        None
    }
}

/// `##` 替换的三种形态（Legado `AnalyzeRule.replaceRegex`）：
/// - `##pat`：删除所有匹配（replacement 为空）；
/// - `##pat##repl`：替换所有匹配；
/// - `##pat##repl###`：只替换第一个匹配（无匹配时原样返回）。
pub(super) fn apply_replace_regex(
    value: &str,
    pattern: &str,
    replacement: &str,
    replace_first: bool,
) -> String {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return value.replace(pattern, replacement),
    };
    // Java Matcher 的 `$1s` = 第 1 组 + 字面量 s（数字序列到非数字即结束），
    // regex crate 会把 `$1s` 当作命名组，需规范成 `${1}s`。
    let replacement = java_replacement_to_rust(replacement);
    if replace_first {
        return re.replacen(value, 1, &replacement).to_string();
    }
    re.replace_all(value, &replacement).to_string()
}

/// 把 Java 风格的 `$<数字>` 分组引用规范化为 regex crate 的 `${<数字>}`。
pub(super) fn java_replacement_to_rust(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len() + 2);
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
            let mut digits = String::new();
            while let Some(&d) = chars.peek() {
                if !d.is_ascii_digit() {
                    break;
                }
                digits.push(d);
                chars.next();
            }
            out.push_str("${");
            out.push_str(&digits);
            out.push('}');
            continue;
        }
        out.push(c);
    }
    out
}

/// HTML4 实体反转义（只在最终输出做一次，避免重复反转义）。
pub(super) fn unescape_html4(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            if ent.len() <= 32 {
                if let Some(c) = decode_entity(ent) {
                    out.push(c);
                    rest = &tail[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

pub(super) fn decode_entity(ent: &str) -> Option<char> {
    match ent {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            let num = ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()));
            num.and_then(char::from_u32)
        }
    }
}
