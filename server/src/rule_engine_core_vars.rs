//! AnalyzeRule 的变量与模板展开（move-only 从 rule_engine_core.rs 拆出）。

use super::*;

impl AnalyzeRule {
    /// 变量/JSONPath 模板展开：支持 `${var}`、`{{var}}`，以及 `{{$.field}}` / `${$.field}` / `{$.field}`
    /// （JSONPath 形式按**当前规则输入** `content` 求值，用于书源里
    /// `.../read?bid={{$.id}}`、`https://api.x/topics/{$.id}` 这类“列表项字段拼进 URL/POST body”的写法）。
    pub(super) fn expand_vars(&self, rule: &str, content: &str) -> String {
        let re = Regex::new(r"\$\{([^}]+)\}|\{\{([^}]+)\}\}|\{(\$[^}]+)\}").unwrap();
        // `<js>` / `@js:` 段内的 `{{…}}` 不在这一层展开：交给 eval_js 的 `expand_js_template`
        // 按 JS 字符串上下文转义后拼接，避免引号/反斜杠破坏语法（p11 的根因）。
        let js_spans = Self::js_code_spans(rule);
        let json: Option<Value> = if rule.contains("$.") || rule.contains("$[") {
            serde_json::from_str(content).ok()
        } else {
            None
        };
        re.replace_all(rule, |caps: &regex::Captures| {
            // `${…}`：只在确实是引擎变量/JSONPath 时替换；未知名字原样保留——
            // 它通常是 JS 模板字符串（快看 `x=>`<img src="${x},${headers}">``），
            // 替换成空串会把 JS 的插值一起吃掉。
            if let Some(m) = caps.get(1) {
                let key = m.as_str().trim();
                if key.starts_with("$.") || key.starts_with("$[") {
                    if let Some(v) = json.as_ref() {
                        if let Ok(mut vals) = json_query(v, key) {
                            if let Some(x) = vals.pop() {
                                return value_to_string(x);
                            }
                        }
                    }
                    return String::new();
                }
                if let Some(v) = self.variables.borrow().get(key).cloned() {
                    return v;
                }
                return caps.get(0).unwrap().as_str().to_string();
            }
            let key = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            let key = key.trim();
            // JS 段内的 `{{…}}`（含变量名/JSONPath）一律保留原文，由 `expand_js_template`
            // 在进入 boa 前按字符串上下文展开，避免裸替换破坏 JS 语法。
            if caps.get(2).is_some() {
                let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                let start = caps.get(0).map(|m| m.start()).unwrap_or(0);
                if js_spans.iter().any(|(a, b)| start >= *a && start < *b) {
                    return whole.to_string();
                }
                // `{{规则##正则[##替换[###]]}}`：Legado 把 `{{}}` 内带规则标志的内容包成
                // SourceRule，`##` 属于规则本身，必须在求值之后应用。若把 `##…` 直接当路径/
                // 选择器的一部分，JSONPath 会解析失败，整段展开为空（源包 11 处真实规则）。
                if let Some(s) = self.template_rule_replace(content, key, json.as_ref()) {
                    return s;
                }
            }
            if key.starts_with("$.") || key.starts_with("$[") {
                if let Some(v) = json.as_ref() {
                    if let Ok(mut vals) = json_query(v, key) {
                        if let Some(x) = vals.pop() {
                            return value_to_string(x);
                        }
                    }
                }
                return String::new();
            }
            // `{{@@.ltim@text}}` / `{{@css:.x@text}}`：模板里嵌套的选择器规则
            // （爱轻写真 search.intro）。这不是 JS 表达式，先按规则管道求值再拼回文本。
            if key.starts_with('@') || key.starts_with('<') || key.starts_with("//") {
                if let Ok(v) = self.get_string_inner(content, key, false, "") {
                    return v;
                }
            }
            if let Some(v) = self.variables.borrow().get(key).cloned() {
                return v;
            }
            // `{{…}}`：Legado 对非规则内容一律交给 evalJS（`{{page*20}}`、`{{java.base64Encode(…)}}` 等），
            // 变量已在 eval_js 作用域内绑定。
            if caps.get(2).is_some() {
                let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                if let Ok(s) = self.eval_js(content, key, "") {
                    return s;
                }
                // 展开失败不静默：保留原文并记日志（便于定位坏规则）
                tracing::warn!(expr = key, "{{}} 模板展开失败，保留原文");
                return whole.to_string();
            }
            String::new()
        })
        .to_string()
    }

    /// `{{规则##…}}` 里的 `##` 替换：先按规则标志求值前半段，再对结果做正则替换。
    /// 与 Legado `AnalyzeRule.getString` 一致：`##pat` 删匹配、`##pat##repl` 全替换、
    /// 出现第 4 段（常见写法 `##pat##repl###`）时只替换第一个匹配。
    /// 非规则标志内容（纯 JS）返回 `None`，维持原有的 JS 求值路径。
    pub(super) fn template_rule_replace(
        &self,
        content: &str,
        key: &str,
        json: Option<&Value>,
    ) -> Option<String> {
        let parts = split_outside_js(key, "##")?;
        let base = parts[0].trim();
        let is_rule = base.starts_with("$.")
            || base.starts_with("$[")
            || base.starts_with('@')
            || base.starts_with('<')
            || base.starts_with("//");
        if !is_rule {
            return None;
        }
        let pattern = parts.get(1).copied().unwrap_or("");
        if pattern.is_empty() {
            return None;
        }
        let value = if base.starts_with("$.") || base.starts_with("$[") {
            json.and_then(|v| json_query(v, base).ok())
                .and_then(|mut vals| vals.pop())
                .map(value_to_string)?
        } else {
            self.get_string_inner(content, base, false, "").ok()?
        };
        Some(apply_replace_regex(
            &value,
            pattern,
            parts.get(2).copied().unwrap_or(""),
            parts.len() > 3,
        ))
    }

    /// 进入 boa 前展开 JS 源码里的 `{{表达式}}`（Legado 规则级模板语义：模板优先于脚本）。
    /// - 表达式按当前输入 `content` 求值为字符串后**拼接进源码**；
    /// - 若占位符位于字符串字面量内，按该引号转义（反斜杠/引号/换行），避免破坏语法；
    /// - 展开失败记 warn 并保留原文（不静默丢内容）。
    pub(super) fn expand_js_template(&self, code: &str, content: &str) -> String {
        if !code.contains("{{") {
            return code.to_string();
        }
        let mut out = String::with_capacity(code.len());
        let mut quote: Option<char> = None;
        let mut it = code.char_indices().peekable();
        while let Some((idx, ch)) = it.next() {
            if let Some(q) = quote {
                if ch == '\\' {
                    out.push(ch);
                    if let Some((_, n)) = it.next() {
                        out.push(n);
                    }
                    continue;
                }
                if ch == q {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' || ch == '`' {
                quote = Some(ch);
            }
            if ch == '{' && code[idx..].starts_with("{{") {
                if let Some(rel) = code[idx + 2..].find("}}") {
                    let inner = code[idx + 2..idx + 2 + rel].trim();
                    let after = idx + 2 + rel + 2;
                    // 表达式既可能是 JSONPath/选择器规则（`{{$.data}}`，p11 曼哈漫画同款），
                    // 也可能是 JS 表达式（`{{'a'+'b'}}`、`{{java.getString(...)}}`）：
                    // 规则形态按规则求值，其余先按 JS 求值；都失败才保留原文（不能把
                    // `{{1 1}}` 这类坏表达式按 CSS 选择器静默吃掉）。
                    let is_rule_expr = starts_with_ci(inner, "$")
                        || starts_with_ci(inner, "@")
                        || starts_with_ci(inner, "<");
                    let value = if is_rule_expr {
                        self.get_string_inner(content, inner, false, "").ok()
                    } else {
                        self.eval_js(content, inner, "").ok()
                    };
                    match value {
                        Some(v) => {
                            match quote {
                                Some(q) => out.push_str(&escape_js_string(&v, q)),
                                None => out.push_str(&v),
                            }
                            while let Some(&(j, _)) = it.peek() {
                                if j < after {
                                    it.next();
                                } else {
                                    break;
                                }
                            }
                            continue;
                        }
                        None => {
                            tracing::warn!(expr = inner, "JS 段内 {{}} 展开失败，保留原文");
                        }
                    }
                }
            }
            out.push(ch);
        }
        out
    }
}
