//! rule_engine HTML 辅助的规则切分、替换与 HTML 实体（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// 解析 `@put:{k1:v1,k2:v2}` 的 `{…}` 内部：顶层（不在引号/括号内）按 `,` 分组，再按首个 `:`
/// 拆 key/value。对应上游 `splitPutRule`（规则语义 L516-530）里的 GSON 宽松解析。
pub(in crate::rule_engine) fn split_put_pairs(inner: &str) -> Vec<(String, String)> {
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
pub(in crate::rule_engine) fn split_outside_js<'a>(
    rule: &'a str,
    sep: &str,
) -> Option<Vec<&'a str>> {
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
pub(in crate::rule_engine) fn starts_with_ci(rule: &str, prefix: &str) -> bool {
    rule.len() >= prefix.len()
        && rule.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

pub(in crate::rule_engine) fn strip_prefix_ci<'a>(rule: &'a str, prefix: &str) -> &'a str {
    if starts_with_ci(rule, prefix) {
        &rule[prefix.len()..]
    } else {
        rule
    }
}

/// 模板展开后的整串是否就是最终值（而非还需要继续解析的规则）。
/// 依据 Legado 语义：含 `{{…}}` 的规则整体按模板求值，展开结果即字段值；
/// 只有仍带 `@text` 这类抽取后缀、`$.`/`@js:`/`<js>` 前缀或 `##`/`||` 管道时才继续解析。
pub(in crate::rule_engine) fn expanded_is_literal(rule: &str) -> bool {
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

pub(in crate::rule_engine) fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle)
}

/// 从 `from` 字节处开始找 `needle`（ASCII 大小写不敏感），返回字节下标。
pub(in crate::rule_engine) fn find_ci_from(
    haystack: &str,
    needle: &str,
    from: usize,
) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.is_empty() || from >= hb.len() || hb.len() < nb.len() {
        return None;
    }
    (from..=hb.len() - nb.len()).find(|&i| hb[i..i + nb.len()].eq_ignore_ascii_case(nb))
}

/// 从 `open`（`{` 的字节下标）开始按括号深度找配对的 `}`；跳过字符串字面量内的括号。
pub(in crate::rule_engine) fn find_matching_brace(s: &str, open: usize) -> Option<usize> {
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
pub(in crate::rule_engine) fn resolve_index(i: i64, len: usize) -> Option<usize> {
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
pub(in crate::rule_engine) fn apply_replace_regex(
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
pub(in crate::rule_engine) fn java_replacement_to_rust(replacement: &str) -> String {
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
pub(in crate::rule_engine) fn unescape_html4(s: &str) -> String {
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

pub(in crate::rule_engine) fn decode_entity(ent: &str) -> Option<char> {
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
