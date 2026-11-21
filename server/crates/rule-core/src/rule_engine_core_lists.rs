//! AnalyzeRule 的列表取词与 CSS/JSON 辅助（move-only 从 rule_engine_core.rs 拆出）。

use super::*;

impl AnalyzeRule {
    /// 列表：返回多个元素的字符串（HTML 取 inner_html；JSON 取数组元素字符串）
    pub fn get_string_list(&self, content: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        self.current_content.replace(content.to_string());
        let rule = self.apply_put_get(rule, content, false, "");
        let rule = self.expand_vars(&rule, content);
        // 上游规则约定：规则以 `-` 开头表示「结果倒序」（多见于章节列表，如 `-$..comics.*`）。
        if let Some(rest) = rule.trim_start().strip_prefix('-') {
            let mut v = self.get_string_list(content, rest.trim_start())?;
            v.reverse();
            return Ok(v);
        }
        // 列表级 `||` 回退：第一个非空列表胜出（如 `$..list[*]||$.books[*]`）
        if let Some(parts) = split_outside_js(&rule, "||") {
            for p in &parts {
                let v = self.get_string_list(content, p)?;
                if !v.is_empty() {
                    return Ok(v);
                }
            }
            return Ok(vec![]);
        }
        // 混合管道：`<js>预处理</js>$..list[*]` 先用 JS 改写输入，再把尾段按**列表**语义求值
        let js_parts = Self::split_js(&rule);
        if js_parts.len() >= 2 && js_parts.iter().any(|(m, _)| *m == Mode::Js) {
            let last = js_parts.len() - 1;
            let mut input = content.to_string();
            // Default 段在**列表**语义里先求成 List<String>（上游规则 AnalyzeRule.getStringList
            // 用规则引擎的多值抽取），JS 段拿到的是数组而不是单个字符串：
            // 漫画搬运/穿越小说这类 `CSS前缀 + <js>` 的目录规则都依赖 `Array.from(result)`。
            let mut input_list: Option<Vec<String>> = None;
            for (i, (mode, text)) in js_parts.iter().enumerate() {
                let t = text.trim();
                if t.is_empty() {
                    continue;
                }
                if i == last {
                    if *mode == Mode::Default {
                        return self.get_string_list(&input, t);
                    }
                    let res = match &input_list {
                        Some(items) => self.eval_js_list(items, t, "")?,
                        None => self.eval_js(&input, t, "")?,
                    };
                    if let Some(items) = expand_json_list_items(&res, true) {
                        return Ok(items);
                    }
                    return Ok(vec![res]);
                }
                if *mode == Mode::Js {
                    input = self.eval_js(&input, t, "")?;
                    input_list = None;
                } else {
                    let list = self.get_string_list(&input, t)?;
                    input = list.join("\n");
                    input_list = Some(list);
                }
            }
            return Ok(match input_list {
                Some(items) => items,
                None => vec![input],
            });
        }
        let r = rule.trim_start();
        if starts_with_ci(r, "@js:") || contains_ci(r, "@js:") || rule.contains("<js>") {
            // 走与单值相同的分段管道：`CSS前缀<js>…</js>` 只把 JS 段交给求值器，
            // 不再把 `<js>` 标签和前缀一起塞进 boa（否则会在 `<js>` 处语法报错）。
            let res = self.get_string_inner(content, &rule, false, "")?;
            // 整段 JS 常返回数组（如 java.getElements 的结果），序列化为 JSON 数组则拆成多项；
            // 对象数组（`[{text,href}]`）按上游规则章节语义逐项展开，供后续字段规则取用。
            if let Some(items) = expand_json_list_items(&res, true) {
                return Ok(items);
            }
            return Ok(vec![res]);
        }
        let mode = Self::detect_mode(&rule, self.is_json, content);
        match mode {
            Mode::Json => self.json_list(content, Self::strip_prefix(&rule, "@json:")),
            _ => self.css_list(content, Self::strip_prefix(&rule, "@css:")),
        }
    }

    /// 列表求值（保留末端抽取标记）：用于 `nextTocUrl` / 漫画图片等“可能返回多条”的规则。
    /// 与上游规则 `getStringList` 对齐：`||` 取首个非空列表、`&&` 合并列表、`%%` 交错，
    /// `##` 先解析出列表再**逐项**替换，`@js:` 结果按数组或换行拆项。
    pub fn get_string_list_all(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let rule = self.apply_put_get(rule, content, false, base_url);
        let rule = self.expand_vars(&rule, content);
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        self.eval_list(content, r, base_url)
    }

    /// 列表层求值：先按上游规则的优先级切分操作符，再调用单段解析。
    pub(super) fn eval_list(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let rule = rule.trim();
        if rule.is_empty() {
            return vec![];
        }
        // `##` 在解析完成后逐项应用（`getStringList` 的语义，不是先把列表拼成字符串）
        if let Some(parts) = split_outside_js(rule, "##") {
            let mut list = self.eval_list(content, parts[0].trim(), base_url);
            let pattern = parts.get(1).copied().unwrap_or("");
            if !pattern.is_empty() {
                let replacement = parts.get(2).copied().unwrap_or("");
                let replace_first = parts.len() > 3;
                list = list
                    .into_iter()
                    .map(|s| apply_replace_regex(&s, pattern, replacement, replace_first))
                    .collect();
            }
            return list;
        }
        // `||`：第一个非空列表胜出
        if let Some(parts) = split_outside_js(rule, "||") {
            for p in &parts {
                let v = self.eval_list(content, p, base_url);
                if !v.is_empty() {
                    return v;
                }
            }
            return vec![];
        }
        // `&&`：列表拼接
        if let Some(parts) = split_outside_js(rule, "&&") {
            let mut out = Vec::new();
            for p in &parts {
                out.extend(self.eval_list(content, p, base_url));
            }
            return out;
        }
        // `%%`：按首个列表的下标交错合并
        if let Some(parts) = split_outside_js(rule, "%%") {
            let lists: Vec<Vec<String>> = parts
                .iter()
                .map(|p| self.eval_list(content, p, base_url))
                .collect();
            let mut out = Vec::new();
            if let Some(first) = lists.first() {
                for i in 0..first.len() {
                    for l in &lists {
                        if let Some(x) = l.get(i) {
                            out.push(x.clone());
                        }
                    }
                }
            }
            return out;
        }
        self.eval_segment_list(content, rule, base_url)
    }

    /// 不含 `&&`/`||`/`%%`/`##` 的单段列表求值。
    pub(super) fn eval_segment_list(
        &self,
        content: &str,
        rule: &str,
        base_url: &str,
    ) -> Vec<String> {
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        // JS 段：数组按 JSON 拆项，普通字符串按 `\n` 拆（上游规则 getStringList 对 String 的处理）
        if starts_with_ci(r, "@js:") || contains_ci(r, "@js:") || r.contains("<js>") {
            let js_parts = Self::split_js(r);
            if js_parts.len() >= 2 {
                let last = js_parts.len() - 1;
                let mut input = content.to_string();
                for (i, (mode, text)) in js_parts.iter().enumerate() {
                    let t = text.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if i == last {
                        return match self.eval_js(&input, t, base_url) {
                            Ok(s) if !s.is_empty() => match expand_json_list_items(&s, false) {
                                // 图片列表等“纯值”语义：对象项取 src/url/href 字段
                                Some(items) => items,
                                None => s.split('\n').map(|x| x.to_string()).collect(),
                            },
                            _ => vec![],
                        };
                    }
                    if *mode == Mode::Js {
                        input = self.eval_js(&input, t, base_url).unwrap_or_default();
                    } else {
                        // 列表语义：JSONPath 指向数组时先摊平成多项，再以 `\n` 交给 JS。
                        // 爱看 `$.data.current_chapter.chapter_img_list@js:result.split("\n")…`
                        // 依赖的就是「每条图片 URL 一行」的输入（上游 getStringList 的 List 语义）。
                        input = self.eval_list(&input, t, base_url).join("\n");
                    }
                }
            }
            return match self.get_string_inner(content, r, false, base_url) {
                Ok(s) if !s.is_empty() => match expand_json_list_items(&s, false) {
                    Some(items) => items,
                    None => s.split('\n').map(|x| x.to_string()).collect(),
                },
                _ => vec![],
            };
        }
        match Self::detect_mode(r, self.is_json, content) {
            Mode::Json => self
                .json_list(content, Self::strip_prefix(r, "@json:"))
                .unwrap_or_default(),
            Mode::Css => self.css_extract_all(content, r),
            _ => match self.get_string_inner(content, r, false, base_url) {
                Ok(s) if !s.trim().is_empty() => vec![s],
                _ => vec![],
            },
        }
    }

    /// 纯 CSS 列表抽取：对每个匹配元素分别取末端标记（`@href` / `@text` / `@all` 等）。
    pub(super) fn css_extract_all(&self, html: &str, rule: &str) -> Vec<String> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = scraper::Html::parse_document(html);
        let mut els = select_initial(&doc, &selector);
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
        match terminal {
            Some(t) if t == "textNodes" => els.iter().map(|e| text_nodes(e)).collect(),
            Some(t) => els.iter().map(|e| extract_token(*e, &t)).collect(),
            None => els.iter().map(|e| e.html()).collect(),
        }
    }

    pub(super) fn css_list(&self, html: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        Ok(elements_html(html, rule))
    }

    pub(super) fn json_list(&self, json: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let v: Value = serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let mut out = Vec::new();
        // JSONPath 直接命中数组时（`$['hits']` / `$.data.topics`）按数组元素展开：
        // 上游规则的 getElements 会把 List 的每个元素当作列表项，不把整个数组当成一项。
        for val in json_query(&v, rule)? {
            match val {
                Value::Array(a) => {
                    out.extend(a.into_iter().filter(|x| !x.is_null()).map(value_to_string))
                }
                Value::Null => {}
                other => out.push(value_to_string(other)),
            }
        }
        Ok(out)
    }
}
