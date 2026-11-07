//! rule_engine HTML 辅助的CSS 规则切分与索引表达式（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// 去掉 @css:/@CSS:/@@ 前缀，并按 '@' 切分为 (选择器, 抽取/遍历标记)。
pub(in crate::rule_engine) fn split_css_rule(rule: &str) -> (String, Vec<&str>) {
    let rule = strip_prefix_ci(rule, "@css:")
        .trim_start_matches("@@")
        .trim();
    let parts: Vec<&str> = rule.split('@').collect();
    let selector = parts[0].trim().to_string();
    let tokens: Vec<&str> = parts[1..].iter().map(|s| s.trim()).collect();
    (selector, tokens)
}

/// 拆出上游规则末尾的 `[n]` / `[n,m]` / `[a:b]` 下标表达式。
pub(in crate::rule_engine) fn split_bracket_indices(selector: &str) -> (String, Option<String>) {
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

/// 对已选元素应用上游规则下标表达式：`[3,4]` 取第 3/4 个，`[0:2]` 切片，`[-1:0]` 倒序。
pub(in crate::rule_engine) fn apply_index_expr<'a>(
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
pub(in crate::rule_engine) fn split_name_index(s: &str) -> (String, Option<i64>) {
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

/// 上游规则的 `class.a b` 表示「同时具备 a 和 b 两个类」（AND），
/// 但 CSS 里空格是后代选择器，必须归一化成 `.a.b`。
pub(in crate::rule_engine) fn class_name_css(rest: &str) -> String {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(".{}", parts.join("."))
}

/// `class.x.y` / `id.x.y` / `tag.x.y` 只取第一个点分段：上游规则引擎
/// 对 `beforeRule.split(".")` 的结果只用 `rules[1]`，其余分段被忽略
/// （`class.list.ul` 等价 `class.list`）。
pub(in crate::rule_engine) fn shorthand_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// jQuery 扩展伪类（scraper/CSS 不认，需要先剥离再对结果集做过滤）。
#[derive(Debug, Clone, PartialEq)]
pub(in crate::rule_engine) enum JqFilter {
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

pub(in crate::rule_engine) fn starts_at(cs: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    cs.len() >= i + p.len() && cs[i..i + p.len()] == p[..]
}

/// 找到与 `open` 处 `(` 配对的 `)`；跳过引号内的括号。
pub(in crate::rule_engine) fn find_close_paren(cs: &[char], open: usize) -> Option<usize> {
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

pub(in crate::rule_engine) fn unquote_arg(raw: &str) -> String {
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
pub(in crate::rule_engine) fn split_jquery_filters(selector: &str) -> (String, Vec<JqFilter>) {
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
