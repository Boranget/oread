//! JSONPath 子集（move-only 从 rule_engine.rs 拆出）。
//!
//! 只被父模块的 `json_query` 调用；类型与辅助函数保持私有。

use jsonpath_rust::JsonPathQuery;
use serde_json::Value;

use super::html::unquote_arg;
use super::RuleError;

/// Legado 常用 JSONPath 子集：`$.a.b` / `$..field`（递归下降）/ `[*]` / `[n]` / `['key']`。
#[derive(Debug, Clone, PartialEq)]
enum JpSeg {
    Key(String),
    Index(i64),
    Wildcard,
    RecursiveKey(String),
    RecursiveWildcard,
    /// 过滤谓词 `[?(@.field=='x' && @.n>1)]` / `[?(@.a=='x' || @.b=='y')]`
    Filter {
        conds: Vec<JpCond>,
        any: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum JpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, PartialEq)]
struct JpCond {
    path: Vec<String>,
    op: JpOp,
    value: Value,
}

/// 找到与 `open` 处 `[` 配对的 `]`：跳过引号与内层方括号（谓词里可能有 `@['k']`）。
fn find_close_bracket(cs: &[char], open: usize) -> Option<usize> {
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
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 按顶层 `&&` 或 `||` 切分谓词；两种混用视为不支持（需要括号优先级）。
fn split_predicate(body: &str) -> Result<(Vec<String>, bool), String> {
    let cs: Vec<char> = body.chars().collect();
    let mut quote: Option<char> = None;
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut op: Option<bool> = None; // Some(true)=||, Some(false)=&&
    let mut i = 0usize;
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            cur.push(c);
            i += 1;
            continue;
        }
        if (c == '&' || c == '|') && i + 1 < cs.len() && cs[i + 1] == c {
            let is_any = c == '|';
            if op.map(|o| o != is_any).unwrap_or(false) {
                return Err("谓词不支持混用 && 与 ||".to_string());
            }
            op = Some(is_any);
            parts.push(cur.trim().to_string());
            cur.clear();
            i += 2;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    parts.push(cur.trim().to_string());
    Ok((parts, op.unwrap_or(false)))
}

/// 解析 `@.a.b` / `@['a']` 形式的字段路径。
fn parse_cond_path(raw: &str) -> Result<Vec<String>, String> {
    let s = raw.trim();
    let mut rest = s
        .strip_prefix('@')
        .ok_or_else(|| format!("谓词字段必须以 @ 开头：{s}"))?;
    let mut out = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            let key = r[..end].trim();
            if key.is_empty() {
                return Err(format!("谓词字段名为空：{s}"));
            }
            out.push(key.to_string());
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r
                .find(']')
                .ok_or_else(|| format!("谓词字段方括号未闭合：{s}"))?;
            let key = unquote_arg(&r[..end]);
            if key.is_empty() {
                return Err(format!("谓词字段名为空：{s}"));
            }
            out.push(key);
            rest = &r[end + 1..];
        } else {
            return Err(format!("谓词字段无法解析：{s}"));
        }
    }
    if out.is_empty() {
        return Err(format!("谓词字段为空：{s}"));
    }
    Ok(out)
}

/// 解析谓词比较值：字符串 / 数字 / true / false / null。
fn parse_pred_value(raw: &str) -> Result<Value, String> {
    let s = raw.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        return Ok(Value::String(unquote_arg(s)));
    }
    match s {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        "null" => return Ok(Value::Null),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Value::from(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Ok(Value::Number(n));
        }
    }
    Err(format!("不支持的谓词比较值：{s}"))
}

/// 解析单条谓词：`@.field == 'x'` / `@.price>=10`（空格可选）。
fn parse_cond(raw: &str) -> Result<JpCond, String> {
    let s = raw.trim();
    let ops = ["==", "!=", ">=", "<=", ">", "<"];
    let mut found: Option<(usize, &str)> = None;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            continue;
        }
        for op in ops {
            if s[i..].starts_with(op) {
                found = Some((i, op));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (pos, op) = found.ok_or_else(|| format!("谓词缺少比较运算符：{s}"))?;
    let path = parse_cond_path(s[..pos].trim())?;
    let rhs = &s[pos + op.len()..];
    let value = parse_pred_value(rhs)?;
    let op = match op {
        "==" => JpOp::Eq,
        "!=" => JpOp::Ne,
        ">=" => JpOp::Ge,
        "<=" => JpOp::Le,
        ">" => JpOp::Gt,
        _ => JpOp::Lt,
    };
    Ok(JpCond { path, op, value })
}

/// 解析 `?(...)` 里的全部谓词。
fn parse_filter(body: &str) -> Result<(Vec<JpCond>, bool), String> {
    let inner = body.trim();
    if !(inner.starts_with("?(") && inner.ends_with(')')) {
        return Err(format!("谓词必须以 ?(...) 包裹：{body}"));
    }
    let expr = &inner[2..inner.len() - 1];
    let (parts, any) = split_predicate(expr)?;
    let conds = parts
        .iter()
        .map(|p| parse_cond(p))
        .collect::<Result<Vec<_>, _>>()?;
    if conds.is_empty() {
        return Err("谓词表达式为空".to_string());
    }
    Ok((conds, any))
}

/// 解析 JSONPath 子集；不支持的语法返回可读中文原因（不再把 PestError 原样抛给用户）。
fn parse_json_path(path: &str) -> Result<Vec<JpSeg>, String> {
    let s = path.trim();
    let cs: Vec<char> = s.chars().collect();
    if cs.first() != Some(&'$') {
        return Err(format!("路径必须以 $ 开头：{s}"));
    }
    let read_key = |start: usize| -> (String, usize) {
        let mut i = start;
        while i < cs.len() && !matches!(cs[i], '.' | '[' | ']' | '*') {
            i += 1;
        }
        (cs[start..i].iter().collect::<String>(), i)
    };
    let mut segs = Vec::new();
    let mut i = 1usize;
    while i < cs.len() {
        match cs[i] {
            '.' => {
                if cs.get(i + 1) == Some(&'.') {
                    // 递归下降：`$..books` / `$..*`
                    i += 2;
                    if cs.get(i) == Some(&'*') {
                        segs.push(JpSeg::RecursiveWildcard);
                        i += 1;
                    } else {
                        let (k, ni) = read_key(i);
                        if k.is_empty() {
                            return Err(format!("递归下降缺少字段名：{s}"));
                        }
                        segs.push(JpSeg::RecursiveKey(k));
                        i = ni;
                    }
                } else {
                    i += 1;
                    if cs.get(i) == Some(&'*') {
                        segs.push(JpSeg::Wildcard);
                        i += 1;
                    } else if cs.get(i) == Some(&'[') {
                        // Legado 用 Jayway json-path，容忍 `$.data.[*]` 这类多余的点
                        continue;
                    } else {
                        let (k, ni) = read_key(i);
                        if k.is_empty() {
                            return Err(format!("字段名为空：{s}"));
                        }
                        segs.push(JpSeg::Key(k));
                        i = ni;
                    }
                }
            }
            '[' => {
                let end = find_close_bracket(&cs, i).ok_or_else(|| format!("方括号未闭合：{s}"))?;
                let inner: String = cs[i + 1..end].iter().collect();
                let inner = inner.trim();
                if inner == "*" {
                    segs.push(JpSeg::Wildcard);
                } else if let Ok(n) = inner.parse::<i64>() {
                    segs.push(JpSeg::Index(n));
                } else if inner.starts_with("?(") {
                    let (conds, any) = parse_filter(inner)?;
                    segs.push(JpSeg::Filter { conds, any });
                } else if (inner.starts_with('\'') && inner.ends_with('\''))
                    || (inner.starts_with('"') && inner.ends_with('"'))
                {
                    segs.push(JpSeg::Key(inner[1..inner.len() - 1].to_string()));
                } else {
                    return Err(format!("不支持的方括号表达式 [{inner}]"));
                }
                i = end + 1;
            }
            '*' => {
                segs.push(JpSeg::Wildcard);
                i += 1;
            }
            c => return Err(format!("无法识别的字符 '{c}'：{s}")),
        }
    }
    Ok(segs)
}

fn jp_recursive_key(v: &Value, key: &str, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get(key) {
                out.push(x.clone());
            }
            for x in m.values() {
                jp_recursive_key(x, key, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                jp_recursive_key(x, key, out);
            }
        }
        _ => {}
    }
}

fn jp_recursive_all(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for x in m.values() {
                out.push(x.clone());
                jp_recursive_all(x, out);
            }
        }
        Value::Array(a) => {
            for x in a {
                out.push(x.clone());
                jp_recursive_all(x, out);
            }
        }
        _ => {}
    }
}

fn jp_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn jp_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 谓词比较：两侧都能转数字时按数值比较，否则按字符串（只支持 ==/!=）。
fn jp_compare(a: &Value, b: &Value, op: &JpOp) -> bool {
    let nums = jp_num(a).zip(jp_num(b));
    match op {
        JpOp::Eq | JpOp::Ne => {
            let eq = match nums {
                Some((x, y)) => x == y,
                None => jp_text(a) == jp_text(b),
            };
            if *op == JpOp::Eq {
                eq
            } else {
                !eq
            }
        }
        _ => {
            let (x, y) = match nums {
                Some(v) => v,
                None => return false,
            };
            match op {
                JpOp::Gt => x > y,
                JpOp::Ge => x >= y,
                JpOp::Lt => x < y,
                JpOp::Le => x <= y,
                _ => false,
            }
        }
    }
}

fn jp_matches(cond: &JpCond, el: &Value) -> bool {
    let mut cur = el;
    for k in &cond.path {
        cur = match cur {
            Value::Object(m) => match m.get(k) {
                Some(v) => v,
                None => return false,
            },
            Value::Array(a) => match k.parse::<usize>().ok().and_then(|i| a.get(i)) {
                Some(v) => v,
                None => return false,
            },
            _ => return false,
        };
    }
    jp_compare(cur, &cond.value, &cond.op)
}

fn jp_filter_match(conds: &[JpCond], any: bool, el: &Value) -> bool {
    if any {
        conds.iter().any(|c| jp_matches(c, el))
    } else {
        conds.iter().all(|c| jp_matches(c, el))
    }
}

fn eval_json_path(root: &Value, segs: &[JpSeg]) -> Vec<Value> {
    let mut cur = vec![root.clone()];
    for seg in segs {
        let mut next = Vec::new();
        match seg {
            JpSeg::Key(k) => {
                for v in &cur {
                    if let Value::Object(m) = v {
                        if let Some(x) = m.get(k) {
                            next.push(x.clone());
                        }
                    }
                }
            }
            JpSeg::Index(n) => {
                for v in &cur {
                    if let Value::Array(a) = v {
                        let idx = if *n < 0 { a.len() as i64 + *n } else { *n };
                        if idx >= 0 && (idx as usize) < a.len() {
                            next.push(a[idx as usize].clone());
                        }
                    }
                }
            }
            JpSeg::Wildcard => {
                for v in &cur {
                    match v {
                        Value::Array(a) => next.extend(a.clone()),
                        Value::Object(m) => next.extend(m.values().cloned()),
                        _ => {}
                    }
                }
            }
            JpSeg::RecursiveKey(k) => {
                for v in &cur {
                    jp_recursive_key(v, k, &mut next);
                }
            }
            JpSeg::RecursiveWildcard => {
                for v in &cur {
                    jp_recursive_all(v, &mut next);
                }
            }
            JpSeg::Filter { conds, any } => {
                for v in &cur {
                    match v {
                        Value::Array(a) => {
                            for el in a {
                                if jp_filter_match(conds, *any, el) {
                                    next.push(el.clone());
                                }
                            }
                        }
                        // 对象也允许过滤（命中则保留对象本身），与部分 JSONPath 实现一致
                        Value::Object(_) if jp_filter_match(conds, *any, v) => {
                            next.push(v.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        cur = next;
    }
    cur
}

/// 对 JSON 执行 JSONPath：先走本地子集解析，含过滤表达式等复杂写法再退回 jsonpath-rust；
/// 两条路径的语法错误都统一成“JSONPath 语法不受支持：…”，不暴露 PestError。
/// 按顶层 `&&` 切分（跳过引号与 [] / () 内部，过滤表达式里的 && 不拆）。
fn split_top_level_and(rule: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut last = 0usize;
    let bytes = rule.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q && (i == 0 || bytes[i - 1] != b'\\') {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth -= 1,
                b'&' if depth == 0 && bytes.get(i + 1) == Some(&b'&') => {
                    parts.push(rule[last..i].trim());
                    last = i + 2;
                    i += 2;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    if parts.is_empty() {
        None
    } else {
        parts.push(rule[last..].trim());
        Some(parts)
    }
}

pub(super) fn json_query(v: &Value, rule: &str) -> Result<Vec<Value>, RuleError> {
    let rule = rule.trim();
    // Legado 的 JSONPath `&&` 表示多个路径结果合并（`$.a.*&&$.b.*`）
    if let Some(parts) = split_top_level_and(rule) {
        let mut out = Vec::new();
        for p in parts {
            out.extend(json_query(v, p)?);
        }
        return Ok(out);
    }
    match parse_json_path(rule) {
        Ok(segs) => Ok(eval_json_path(v, &segs)),
        Err(local_err) => {
            let complex = rule.contains("[?") || rule.contains("](") || rule.contains("&&");
            if complex {
                match v.clone().path(rule) {
                    Ok(Value::Array(arr)) => Ok(arr),
                    Ok(other) => Ok(vec![other]),
                    Err(e) => Err(RuleError::Json(format!("JSONPath 语法不受支持：{e}"))),
                }
            } else {
                Err(RuleError::Json(format!(
                    "JSONPath 语法不受支持：{local_err}"
                )))
            }
        }
    }
}
