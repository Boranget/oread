//! rule_engine HTML 辅助的JS 片段扫描与 JSON 标量（move-only 从 rule_engine_html.rs 拆出）。

use super::*;

/// `/` 在此处是否构成正则字面量的开始（而非除号）。
pub(in crate::rule_engine) fn js_regex_allowed(rule: &str, at: usize) -> bool {
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
pub(in crate::rule_engine) fn find_js_tag_end(rule: &str, start: usize) -> Option<usize> {
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
pub(in crate::rule_engine) fn is_simple_json_key(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// JSON 对象列表项取字段：`rule` 是裸字段名（`text`/`href`/`name`/`title`…）时按键取值。
/// 别名与 Legado 章节项约定一致：标题取 `text/name/title`，链接取 `href/url/src`。
/// 返回 `None` 表示 content 不是 JSON 对象（交给原有 CSS/正则链路）；是对象但字段缺失返回空串。
pub(in crate::rule_engine) fn json_item_field(content: &str, key: &str) -> Option<String> {
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
pub(in crate::rule_engine) fn escape_js_string(s: &str, quote: char) -> String {
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
pub(in crate::rule_engine) fn json_scalar_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}
