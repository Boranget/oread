//! AnalyzeRule 的规则切分与 `@put`/`@get`（move-only 从 rule_engine_core.rs 拆出）。

use super::*;

impl AnalyzeRule {
    pub(super) fn detect_mode(rule: &str, is_json: bool, content: &str) -> Mode {
        // 上游规则的 `@CSS:`/`@XPath:`/`@Json:` 前缀全部大小写不敏感。
        if starts_with_ci(rule, "@xpath:") {
            Mode::XPath
        } else if starts_with_ci(rule, "@json:") {
            Mode::Json
        } else if starts_with_ci(rule, "@css:") || rule.starts_with("@@") {
            Mode::Css
        } else if rule.starts_with("$.") || rule.starts_with("$[") {
            Mode::Json
        } else if is_json && !content.trim_start().starts_with('<') {
            // 源级 JSON 提示只对 JSON 内容生效：HTML 页面上的 `img@data-src`
            // 不能被强行当成 JSONPath（快看图片规则就栽在这里）。
            Mode::Json
        } else if rule.starts_with('/') {
            // `/.../`（含替换形式 `/pat/repl/`）判定为正则；普通 XPath 不以 / 结尾
            if rule.len() > 1 && rule.ends_with('/') {
                Mode::Regex
            } else {
                Mode::XPath
            }
        } else if rule.starts_with(':') {
            Mode::Regex
        } else {
            Mode::Css
        }
    }

    /// `<js>…</js>` / `@js:…` 段在整条规则里的字节区间（供模板展开区分“JS 内/外”）。
    pub(super) fn js_code_spans(rule: &str) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        let bytes = rule.as_bytes();
        let mut i = 0usize;
        while i < rule.len() {
            if bytes[i] < 0x80 {
                let s = &rule[i..];
                if s.starts_with("<js>") {
                    let code_start = i + 4;
                    match find_js_tag_end(rule, code_start) {
                        Some(end) => {
                            spans.push((code_start, end));
                            i = end + 5;
                            continue;
                        }
                        None => {
                            spans.push((code_start, rule.len()));
                            break;
                        }
                    }
                }
                if s.get(..4)
                    .map(|x| x.eq_ignore_ascii_case("@js:"))
                    .unwrap_or(false)
                {
                    spans.push((i + 4, rule.len()));
                    break;
                }
            }
            i += 1;
        }
        spans
    }

    /// 整串层切分：抽出 `<js>…</js>` 与 `@js:…` 段作为 JS，其余作为默认模式
    pub(in crate::rule_engine) fn split_js(rule: &str) -> Vec<(Mode, String)> {
        let mut parts = Vec::new();
        let mut last = 0usize;
        let mut i = 0usize;
        let n = rule.len();
        while i < n {
            // 只在 ASCII 字节处做标记匹配，避免多字节 UTF-8 内部切片越界
            if rule.as_bytes()[i] < 0x80 {
                let s = &rule[i..];
                if s.starts_with("<js>") {
                    if i > last {
                        parts.push((Mode::Default, rule[last..i].to_string()));
                    }
                    let code_start = i + 4;
                    match find_js_tag_end(rule, code_start) {
                        Some(end) => {
                            parts.push((Mode::Js, rule[code_start..end].to_string()));
                            i = end + 5;
                            last = i;
                            continue;
                        }
                        None => {
                            // 未闭合 `<js>`：与旧行为一致，剩余内容整体按 JS 处理
                            parts.push((Mode::Js, rule[code_start..].to_string()));
                            return parts;
                        }
                    }
                }
                if starts_with_ci(s, "@js:") {
                    if i > last {
                        parts.push((Mode::Default, rule[last..i].to_string()));
                    }
                    parts.push((Mode::Js, rule[i + 4..].to_string()));
                    return parts;
                }
            }
            i += 1;
        }
        if last < n {
            parts.push((Mode::Default, rule[last..].to_string()));
        }
        parts
    }

    /// 大小写不敏感的前缀剥离（上游规则 `startsWith(..., ignoreCase = true)`）。
    pub(super) fn strip_prefix<'a>(rule: &'a str, p: &str) -> &'a str {
        if starts_with_ci(rule, p) {
            &rule[p.len()..]
        } else {
            rule
        }
    }

    /// 上游规则 `@put:{key:rule}` / `@get:{key}`：
    /// - 上游 `AnalyzeRule.splitPutRule`（L516，正则 L1027 `@put:(\{[^}]+?\})`，大小写不敏感）
    ///   先把 `@put:{…}` 从规则正文里摘掉；求值时 `putRule`（L507）把每个 value 当**完整规则**
    ///   在当前 content 上求值后 `put(key, value)`（L856；chapter→book→ruleData→source 依次落库）。
    /// - `@get:{key}` 由 `makeUpRule`（L698/L718）替换成变量值，未命中返回空串（`get` L875）。
    /// - key 允许下划线；`value` 走完整求值链（含 `@js:`、`$.path`）；put 失败不影响主结果。
    ///
    /// 本服务没有 Book/Chapter 实体，作用域退化为一次 `AnalyzeRule`（一次请求内新建）的
    /// 变量表，不写任何全局状态，因此不同源/不同请求天然隔离。
    pub(super) fn apply_put_get(
        &self,
        rule: &str,
        content: &str,
        is_url: bool,
        base_url: &str,
    ) -> String {
        if !contains_ci(rule, "@put:") && !contains_ci(rule, "@get:") {
            return rule.to_string();
        }
        let mut text = rule.to_string();
        // @put：按**括号深度**匹配 `{…}`，允许值本身再嵌套 `@get:{…}`（旧正则会被内层 `}` 截断）。
        let mut from = 0usize;
        while let Some(pos) = find_ci_from(&text, "@put:", from) {
            let after = pos + 5;
            let open = match text[after..].find(|c: char| !c.is_whitespace()) {
                Some(o) => after + o,
                None => break,
            };
            if text.as_bytes().get(open) != Some(&b'{') {
                from = after;
                continue;
            }
            let Some(end) = find_matching_brace(&text, open) else {
                break;
            };
            let inner = text[open + 1..end].to_string();
            // 先摘掉操作符本身（与上游 splitPutRule 一致），失败也不影响剩余规则
            text.replace_range(pos..=end, "");
            from = pos;
            for (key, value_rule) in split_put_pairs(&inner) {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let value = value_rule.trim().to_string();
                if let Ok(v) = self.get_string_inner(content, &value, is_url, base_url) {
                    self.variables
                        .borrow_mut()
                        .insert(key.to_string(), v.clone());
                    // book 层持久化：即使本轮没有 book_key（如搜索阶段），也留在
                    // book_vars 里等待编排层落库，不跨请求丢失。
                    self.book_vars.borrow_mut().insert(key.to_string(), v);
                }
            }
        }
        // @get：先取 `@get:{key}`，再取 `@get:key`（两者都落在当前求值上下文，缺键读作空串）。
        let mut from = 0usize;
        while let Some(pos) = find_ci_from(&text, "@get:", from) {
            let after = pos + 5;
            let open = match text[after..].find(|c: char| !c.is_whitespace()) {
                Some(o) => after + o,
                None => {
                    from = after;
                    continue;
                }
            };
            if text.as_bytes().get(open) == Some(&b'{') {
                if let Some(end) = find_matching_brace(&text, open) {
                    let key = text[open + 1..end].trim().to_string();
                    let val = self
                        .variables
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or_default();
                    text.replace_range(pos..=end, &val);
                    from = pos + val.len();
                    continue;
                }
            }
            let rest = &text[after..];
            let klen = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                .count();
            if klen > 0 {
                let key = rest[..klen].to_string();
                let val = self
                    .variables
                    .borrow()
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                text.replace_range(pos..after + klen, &val);
                from = pos + val.len();
            } else {
                from = after;
            }
        }
        text
    }
}
