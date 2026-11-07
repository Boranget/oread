//! AnalyzeRule 的单值取词与选择器动作（move-only 从 rule_engine_core.rs 拆出）。

use super::*;

impl AnalyzeRule {
    /// 单值入口：对应 Legado `getString(..., unescape = true)`。
    /// HTML4 实体只在**最外层**反转义一次；内部递归走 `get_string_inner`，避免重复反转义。
    pub fn get_string(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        self.current_content.replace(content.to_string());
        let raw = self.get_string_inner(content, rule, is_url, base_url)?;
        Ok(unescape_html4(&raw))
    }

    /// 单值管道（不做 HTML 反转义）：对 content 应用一条规则串，逐段执行，上一步结果作为下一步输入
    pub(super) fn get_string_inner(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        // 整条规则只有 `@get:{key}` / `@get:key`：直接返回变量值（否则替换后的字面量会被当成 CSS 选择器）。
        let raw = rule.trim();
        let only_get_rest = raw.get(5..).unwrap_or("").trim();
        let only_get = starts_with_ci(raw, "@get:")
            && !raw[5..].contains('@')
            && (!only_get_rest.is_empty())
            && ((only_get_rest.starts_with('{') && only_get_rest.ends_with('}'))
                || only_get_rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')));
        let rule = self.apply_put_get(rule, content, is_url, base_url);
        if only_get {
            return Ok(rule.trim().to_string());
        }
        // 整条规则就是一个 `{{…}}` 模板：Legado 直接返回模板求值结果，不能把结果再当
        // 规则/选择器解析。此前 `{{book.name}}`、`{{java.timeFormat(...)}}` 展开后会被
        // 当作 CSS 选择器，解析失败返回空串（库内 5 条规则 / 3 源）。
        let whole_template = {
            let t = rule.trim();
            t.starts_with("{{") && t.ends_with("}}") && !t[2..t.len() - 2].contains("}}")
        };
        // 局部模板（`{{$.a}}\n{{$.b}}`、`第{$.n}章`、`https://…/{{$.cover}}`）展开后是**字面量值**；
        // Legado 里含模板的规则整条按模板求值，不会再把展开结果当 JSONPath/选择器。
        // 此前 JSON 源被强制走 JSONPath，把值当路径报“必须以 $ 开头”，kind / cover_url /
        // last_chapter 因此全空（pass4 真实日志 187 条）。
        let had_template = rule.contains("{{") || rule.contains("{$");
        let rule = self.expand_vars(&rule, content);
        let rule = rule.trim().to_string();
        if whole_template && !rule.starts_with("{{") {
            return Ok(rule);
        }
        // 模板展开后的字面量：Legado 里含 `{{…}}` 的规则整条按模板求值，结果直接返回，
        // 不能当 JSONPath/选择器。仅当展开后仍带规则语法（`$.`、`<js>`、抽取后缀
        // `@text`、`##` 等）时才继续走规则管道（如 `{{'a'}}@text`）。
        if had_template && expanded_is_literal(&rule) {
            return Ok(rule);
        }
        if rule.is_empty() {
            return Ok(String::new());
        }
        let rule = rule.as_str();
        // 已是完整 URL 的模板（如 `https://api.x/read?bid={{$.id}}`）：展开后直接返回，
        // 不再当 CSS 选择器解析（否则 JSON 源里“字面量 URL”形态的 bookUrl 会得到空串）。
        if is_url
            && (rule.starts_with("http://")
                || rule.starts_with("https://")
                || rule.starts_with("//"))
        {
            return Ok(rule.to_string());
        }
        // 字面量 JSON 请求体模板（`{"bid":"{{$.id}}"}`）：展开后原样作为 body 返回，
        // 不能当 CSS 选择器解析。
        if rule.starts_with('{') && rule.ends_with('}') && rule.contains(':') {
            return Ok(rule.to_string());
        }
        // @get:{key} / @put:{key:value} 已在 `apply_put_get` 里按 Legado 的
        // `splitPutRule` + `makeUpRule` 语义处理（可出现在规则任意位置，不只整串）。
        // 顶层操作符（不在 <js>…</js> / @js: 段内）。
        // 顺序与 Legado 一致：先在 makeUpRule 里拆 `##`，再交给解析器处理 `||`/`&&`/`%%`。
        if let Some(parts) = split_outside_js(rule, "##") {
            // `##` 前半段可能只是模板展开后的字面量（如 `📂 简介：{{$.introduction}}##…`）：
            // Legado 里整条规则按模板求值，前半段不再当选择器解析，否则会得到空串。
            let head = parts[0].trim();
            let sub = if had_template && expanded_is_literal(head) {
                head.to_string()
            } else {
                self.get_string_inner(content, head, is_url, base_url)?
            };
            let pattern = parts.get(1).copied().unwrap_or("");
            if pattern.is_empty() {
                return Ok(sub);
            }
            let replacement = parts.get(2).copied().unwrap_or("");
            let replace_first = parts.len() > 3;
            return Ok(apply_replace_regex(
                &sub,
                pattern,
                replacement,
                replace_first,
            ));
        }
        if let Some(parts) = split_outside_js(rule, "||") {
            for p in &parts {
                let mut v = self.eval_list(content, p, base_url);
                if !v.is_empty() {
                    return Ok(if is_url { v.remove(0) } else { v.join("\n") });
                }
            }
            return Ok(String::new());
        }
        if let Some(parts) = split_outside_js(rule, "&&") {
            // Legado 的 `&&` 是「列表合并」：多值用 `\n` 连接，而不是空串粘连。
            let mut items: Vec<String> = Vec::new();
            for p in &parts {
                items.extend(self.eval_list(content, p, base_url));
            }
            if is_url && !items.is_empty() {
                return Ok(items.remove(0));
            }
            return Ok(items.join("\n"));
        }
        if let Some(parts) = split_outside_js(rule, "%%") {
            let lists: Vec<Vec<String>> = parts
                .iter()
                .map(|p| self.eval_list(content, p, base_url))
                .collect();
            let mut items: Vec<String> = Vec::new();
            if let Some(first) = lists.first() {
                for i in 0..first.len() {
                    for l in &lists {
                        if let Some(x) = l.get(i) {
                            items.push(x.clone());
                        }
                    }
                }
            }
            if is_url && !items.is_empty() {
                return Ok(items.remove(0));
            }
            return Ok(items.join("\n"));
        }
        // JS 对象数组展开后的列表项（`{"text":"第一章","href":"/1.html"}`）：
        // 裸字段名按键取值，别名见 `json_item_field`（Legado 章节项 text/name/title + href/url）。
        // 不按源级 `is_json` 开关跳过：JSON 源的列表项本身就是对象，字段规则（`chapterName: name`）
        // 若落到 JSONPath 会报“路径必须以 $ 开头”，把整源目录清空（中文万维 1→0 即此因）。
        let key = rule.trim().strip_prefix('@').unwrap_or(rule.trim());
        if is_simple_json_key(key) {
            if let Some(v) = json_item_field(content, key) {
                return Ok(v);
            }
        }
        // 裸抽取标记：`text` / `href` / `src` 等没有选择器前缀时，表示“取当前节点”。
        // 不能当成 CSS 标签选择器（否则章节标题/链接会解析成空）。
        let bare = rule.trim();
        if !self.is_json && is_extract_keyword(bare) {
            return Ok(extract_first(content, bare));
        }
        // `@text` / `@href` / `@src` / `@html` 与裸标记等价：都作用于“当前节点”，
        // 属性不存在时返回空串（不能当 CSS 标签选择器解析成空）。
        if !self.is_json {
            if let Some(k) = bare.strip_prefix('@') {
                if is_extract_keyword(k) {
                    return Ok(extract_first(content, k));
                }
            }
        }
        // 默认：<js> / @js: 段切分管道
        let mut input = content.to_string();
        for (mode, text) in Self::split_js(rule) {
            let t = text.trim();
            if t.is_empty() {
                continue;
            }
            input = match mode {
                Mode::Js => self.eval_js(&input, t, base_url)?,
                _ => {
                    let m = if mode == Mode::Default {
                        Self::detect_mode(t, self.is_json, &input)
                    } else {
                        mode
                    };
                    self.apply_by_mode(&input, m, t, is_url)?
                }
            };
        }
        Ok(input)
    }

    pub(super) fn apply_by_mode(
        &self,
        input: &str,
        mode: Mode,
        rule: &str,
        first_only: bool,
    ) -> Result<String, RuleError> {
        match mode {
            Mode::XPath => self.apply_xpath(input, Self::strip_prefix(rule, "@xpath:")),
            Mode::Json => self.apply_json(input, Self::strip_prefix(rule, "@json:")),
            Mode::Regex => self.apply_regex(input, rule),
            _ => self.apply_css(input, Self::strip_prefix(rule, "@css:"), first_only),
        }
    }

    pub(super) fn apply_css(
        &self,
        html: &str,
        rule: &str,
        first_only: bool,
    ) -> Result<String, RuleError> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = parse_css_document(html);
        let mut els = select_initial(&doc, &selector);
        // 依次处理遍历标记（@tag.x / @children / 裸标签 / !下标），遇到末端抽取标记即停止
        let mut terminal: Option<String> = None;
        for tok in &tokens {
            let tok = *tok;
            if tok.is_empty() {
                continue;
            }
            match parse_token(tok) {
                Token::Terminal(t) => {
                    terminal = Some(t);
                    break;
                }
                Token::Children => {
                    els = els.into_iter().flat_map(child_elements).collect();
                }
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
            }
        }
        // 单值抽取：Legado `JSoup 规则.getString` = getResultLast 收集**全部**匹配
        // 元素的值（空值/重复属性跳过），多值再按 `\n` 合并；isUrl 走 getString 只取首个。
        match terminal {
            Some(t) => {
                let vals = terminal_values(&els, &t);
                Ok(if first_only {
                    vals.into_iter().next().unwrap_or_default()
                } else {
                    vals.join("\n")
                })
            }
            None => Ok(els.first().map(|e| e.inner_html()).unwrap_or_default()),
        }
    }

    pub(super) fn apply_json(&self, json: &str, rule: &str) -> Result<String, RuleError> {
        let v: Value = serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let vals = json_query(&v, rule)?;
        // Legado 的 getString 对多值结果按 `\n` 拼接（`$..image` 这类递归路径会命中多个节点）。
        // 此前只取最后一个，导致「章节多图」被压成 1 张，源规则里 `@js:result.split("\n")` 也只剩一行。
        if vals.is_empty() {
            return Ok(String::new());
        }
        Ok(vals
            .into_iter()
            .map(value_to_string)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub(super) fn apply_regex(&self, input: &str, rule: &str) -> Result<String, RuleError> {
        // 斜杠包裹规则：/pat/ 为提取（捕获组1），/pat/repl/ 为替换
        if let Some(interior) = rule.strip_prefix('/') {
            // interior 含结尾斜杠
            let slash_count = interior.matches('/').count();
            if slash_count >= 2 {
                // 替换形式：/pat/repl/
                let end1 = interior.find('/').unwrap();
                let re_str = &interior[..end1];
                let replacement = interior[end1 + 1..].trim_end_matches('/');
                let re = Regex::new(re_str).map_err(|e| RuleError::Regex(e.to_string()))?;
                return Ok(re.replace_all(input, replacement).to_string());
            }
            // 提取形式：/pat/
            let re_str = interior.trim_end_matches('/');
            let re = Regex::new(re_str).map_err(|e| RuleError::Regex(e.to_string()))?;
            if let Some(c) = re.captures(input) {
                if let Some(m) = c.get(1) {
                    return Ok(m.as_str().to_string());
                }
                return Ok(c.get(0).unwrap().as_str().to_string());
            }
            return Ok(String::new());
        }
        let re = Regex::new(rule).map_err(|e| RuleError::Regex(e.to_string()))?;
        if let Some(c) = re.captures(input) {
            if let Some(m) = c.get(1) {
                return Ok(m.as_str().to_string());
            }
            return Ok(c.get(0).unwrap().as_str().to_string());
        }
        Ok(String::new())
    }

    pub(super) fn apply_xpath(&self, content: &str, rule: &str) -> Result<String, RuleError> {
        use sxd_document::parser::parse as xml_parse;
        use sxd_xpath::evaluate_xpath;
        let package =
            xml_parse(content).map_err(|e| RuleError::Other(format!("xpath parse: {e:?}")))?;
        let doc = package.as_document();
        let val = evaluate_xpath(&doc, rule)
            .map_err(|e| RuleError::Other(format!("xpath eval: {e:?}")))?;
        Ok(match val {
            sxd_xpath::Value::String(s) => s,
            sxd_xpath::Value::Number(n) => n.to_string(),
            sxd_xpath::Value::Boolean(b) => b.to_string(),
            sxd_xpath::Value::Nodeset(ns) => {
                let mut s = String::new();
                for n in ns.iter() {
                    s.push_str(&n.string_value());
                }
                s
            }
        })
    }
}
