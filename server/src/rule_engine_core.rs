//! AnalyzeRule 核心：规则解析与动作执行（move-only 从 rule_engine.rs 拆出）。
//!
//! 只搬移实现，不改规则语义；父模块保留 RuleError 与子模块入口。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;

use regex::Regex;
use serde_json::Value;

use super::html::*;
use super::js::*;
use super::json_path::json_query;
use super::{Mode, RuleError};

pub struct AnalyzeRule {
    pub variables: RefCell<HashMap<String, String>>,
    /// Legado JS 环境里的 `cache`（上游 规则语义 `bindings["cache"] = CacheManager`）。
    /// 最小等价版：同作用域可读写的字符串 map。上游 CacheManager 是 App 级单例 + SQLite，
    /// 本服务**每作用域一份**（随 AnalyzeRule 存活），不共享、不持久化。
    pub cache: RefCell<HashMap<String, String>>,
    /// 跨请求的 book 层变量（Legado `book.putVariable`，上游 规则语义）。
    /// `@put:{key:rule}` 除了写当前 `variables`，也写这里；路由层在请求结束后按
    /// `book_key` 落库，下一次同书请求（info → toc → content）预载回来。
    pub book_vars: RefCell<HashMap<String, String>>,
    /// 当前作用域所属的 book_key（由路由层/编排层设置，None 表示没有 book 维度）。
    pub book_key: RefCell<Option<String>>,
    pub js_lib: String,
    pub is_json: bool,
    /// 本次顶层规则求值的原始 content；供 `java.getElement`（jsoup 桥）在 JS 中取页面。
    pub current_content: RefCell<String>,
    /// 当前书源 key（`source.getKey()`），由编排层注入。
    pub source_key: RefCell<String>,
    /// JS 环境里的 `source.loginUrl`（登录源常用 `eval(String(source.loginUrl))`）。
    pub source_login_url: RefCell<String>,
    /// 本次 JS 是否调用了 `java.refreshTocUrl()`。
    pub refresh_toc: Cell<bool>,
    /// JS 环境里的 `book.bookUrl`（`pre_update_js` 可能改写，需回写编排层）。
    pub book_url: RefCell<String>,
    /// JS 环境里的 `book.tocUrl`。
    pub book_toc_url: RefCell<String>,
    /// JS 环境里的 `book.name/book.author/book.kind/book.intro/book.coverUrl`。
    /// 上游 `book` 绑定的是完整 Book 实体，规则（尤其简介模板）会直接读这些元信息；
    /// 编排层在详情/目录/正文阶段按当前 Book 值注入（搜索阶段为空）。
    pub book_meta: RefCell<BookMeta>,
}

/// `book` 绑定里的只读元信息（对应 Legado `Book` 的同名属性）。
#[derive(Clone, Default)]
pub struct BookMeta {
    pub name: String,
    pub author: String,
    pub kind: String,
    pub intro: String,
    pub cover_url: String,
}

impl Default for AnalyzeRule {
    fn default() -> Self {
        Self {
            variables: RefCell::new(HashMap::new()),
            cache: RefCell::new(HashMap::new()),
            book_vars: RefCell::new(HashMap::new()),
            book_key: RefCell::new(None),
            js_lib: String::new(),
            is_json: false,
            current_content: RefCell::new(String::new()),
            source_key: RefCell::new(String::new()),
            source_login_url: RefCell::new(String::new()),
            refresh_toc: Cell::new(false),
            book_url: RefCell::new(String::new()),
            book_toc_url: RefCell::new(String::new()),
            book_meta: RefCell::new(BookMeta::default()),
        }
    }
}

impl AnalyzeRule {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_var(&self, k: impl Into<String>, v: impl Into<String>) {
        self.variables.borrow_mut().insert(k.into(), v.into());
    }

    /// 注入 `book.*` 只读元信息（详情/目录/正文阶段由编排层调用）。
    pub fn set_book_meta(&self, meta: BookMeta) {
        *self.book_meta.borrow_mut() = meta;
    }

    /// 执行一段独立 JS（`pre_update_js` 等编排层钩子）；`content` 作为 `result`/`__input`。
    pub fn eval_script(
        &self,
        code: &str,
        content: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        self.eval_js(content, code, base_url)
    }

    /// 图片解密（`ContentRule.image_decode`）：`result`/`bytes` 为图片原始字节，`src` 为图片地址。
    /// 返回解密后的字节；JS 抛错由调用方回退原图并记规则异常。
    pub fn eval_image_decode(
        &self,
        code: &str,
        bytes: &[u8],
        src: &str,
    ) -> Result<Vec<u8>, RuleError> {
        let json = serde_json::to_string(bytes)
            .map_err(|e| RuleError::Other(format!("image bytes 序列化失败：{e}")))?;
        let wrapped = format!(
            "var result=JSON.parse(result); var bytes=result; var src=String(__base);\n{code}"
        );
        let out = self.eval_js(&json, &wrapped, src)?;
        let t = out.trim();
        if t.starts_with('[') {
            let v: Vec<i64> = serde_json::from_str(t)
                .map_err(|e| RuleError::Other(format!("image_decode 未返回字节数组：{e}")))?;
            Ok(v.into_iter().map(|n| n.clamp(0, 255) as u8).collect())
        } else {
            // Rhino 里解图脚本也可能返回二进制字符串：按 charCode 逐字节还原
            Ok(t.chars()
                .map(|c| c as u32)
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .collect())
        }
    }

    /// 批量注入变量；生产路径逐条调用 `set_var`，本方法仅单测使用。
    #[allow(dead_code)]
    pub fn set_vars(&self, m: HashMap<String, String>) {
        self.variables.borrow_mut().extend(m);
    }

    /// 进入一本书的作用域：清空请求内变量，载入该书持久化的变量与 cache 快照。
    /// 之后 `@put` 同时写入 `variables`（本请求可见）与 `book_vars`（请求结束落库）。
    pub fn begin_book_scope(
        &self,
        key: String,
        vars: HashMap<String, String>,
        cache: HashMap<String, String>,
    ) {
        let mut v = self.variables.borrow_mut();
        v.clear();
        v.extend(vars.iter().map(|(k, val)| (k.clone(), val.clone())));
        self.book_vars.replace(vars);
        self.cache.replace(cache);
        self.book_key.replace(Some(key));
    }

    pub fn book_scope_key(&self) -> Option<String> {
        self.book_key.borrow().clone()
    }

    pub fn book_vars_snapshot(&self) -> HashMap<String, String> {
        self.book_vars.borrow().clone()
    }

    pub fn cache_snapshot(&self) -> HashMap<String, String> {
        self.cache.borrow().clone()
    }

    /// 变量/JSONPath 模板展开：支持 `${var}`、`{{var}}`，以及 `{{$.field}}` / `${$.field}` / `{$.field}`
    /// （JSONPath 形式按**当前规则输入** `content` 求值，用于书源里
    /// `.../read?bid={{$.id}}`、`https://api.x/topics/{$.id}` 这类“列表项字段拼进 URL/POST body”的写法）。
    fn expand_vars(&self, rule: &str, content: &str) -> String {
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
    fn template_rule_replace(
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
    fn expand_js_template(&self, code: &str, content: &str) -> String {
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

    fn detect_mode(rule: &str, is_json: bool, content: &str) -> Mode {
        // Legado 的 `@CSS:`/`@XPath:`/`@Json:` 前缀全部大小写不敏感。
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
    fn js_code_spans(rule: &str) -> Vec<(usize, usize)> {
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
    pub(super) fn split_js(rule: &str) -> Vec<(Mode, String)> {
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

    /// 大小写不敏感的前缀剥离（Legado `startsWith(..., ignoreCase = true)`）。
    fn strip_prefix<'a>(rule: &'a str, p: &str) -> &'a str {
        if starts_with_ci(rule, p) {
            &rule[p.len()..]
        } else {
            rule
        }
    }

    /// Legado `@put:{key:rule}` / `@get:{key}`：
    /// - 上游 `AnalyzeRule.splitPutRule`（L516，正则 L1027 `@put:(\{[^}]+?\})`，大小写不敏感）
    ///   先把 `@put:{…}` 从规则正文里摘掉；求值时 `putRule`（L507）把每个 value 当**完整规则**
    ///   在当前 content 上求值后 `put(key, value)`（L856；chapter→book→ruleData→source 依次落库）。
    /// - `@get:{key}` 由 `makeUpRule`（L698/L718）替换成变量值，未命中返回空串（`get` L875）。
    /// - key 允许下划线；`value` 走完整求值链（含 `@js:`、`$.path`）；put 失败不影响主结果。
    ///
    /// 本服务没有 Book/Chapter 实体，作用域退化为一次 `AnalyzeRule`（一次请求内新建）的
    /// 变量表，不写任何全局状态，因此不同源/不同请求天然隔离。
    fn apply_put_get(&self, rule: &str, content: &str, is_url: bool, base_url: &str) -> String {
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
    fn get_string_inner(
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

    fn apply_by_mode(
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

    fn apply_css(&self, html: &str, rule: &str, first_only: bool) -> Result<String, RuleError> {
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

    fn apply_json(&self, json: &str, rule: &str) -> Result<String, RuleError> {
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

    fn apply_regex(&self, input: &str, rule: &str) -> Result<String, RuleError> {
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

    fn apply_xpath(&self, content: &str, rule: &str) -> Result<String, RuleError> {
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

    /// DOM 桥 Phase 1：按规则里出现的字面量选择器，在当前页面 HTML 上预抽取。
    /// 返回 (`getElements`/`getStringList` 的 HTML 列表映射, `getString` 的单值映射)。
    fn dom_bridge(
        &self,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> (
        serde_json::Map<String, serde_json::Value>,
        serde_json::Map<String, serde_json::Value>,
    ) {
        let mut lists = serde_json::Map::new();
        let mut singles = serde_json::Map::new();
        // 页面超过上限不做预抽取：垫片会返回“仅支持字面量选择器”的具名提示
        if input.len() > DOM_MAX_INPUT_BYTES {
            return (lists, singles);
        }
        let mut total_bytes = 0usize;
        for (kind, sel) in scan_dom_calls(code) {
            let sel = sel.trim().to_string();
            // 选择器自身含 JS 段时不预处理，避免递归
            if sel.is_empty() || sel.contains("@js:") || sel.contains("<js>") {
                continue;
            }
            if lists.contains_key(&sel) || singles.contains_key(&sel) {
                continue;
            }
            if kind == "getString" {
                if let Ok(v) = self.get_string(input, &sel, false, base_url) {
                    singles.insert(sel, serde_json::Value::String(v));
                }
                continue;
            }
            let Ok(mut vals) = self.get_string_list(input, &sel) else {
                continue;
            };
            vals.truncate(DOM_MAX_NODES);
            total_bytes += vals.iter().map(|v| v.len()).sum::<usize>();
            if total_bytes > DOM_MAX_INPUT_BYTES {
                break;
            }
            lists.insert(
                sel,
                serde_json::Value::Array(vals.into_iter().map(serde_json::Value::String).collect()),
            );
        }
        (lists, singles)
    }

    /// 把规则变量注入 JS 作用域（等价于 Legado evalJS 前绑定 page/title 等变量）。
    fn js_vars(&self) -> String {
        const RESERVED: &[&str] = &[
            "result",
            "__input",
            "__base",
            "baseUrl",
            "__DOM",
            "__DOMS",
            "__r",
            "__content",
            "__nativeSelect",
            "__nativeAjax",
            "__nativeSourceVarGet",
            "__nativeSourceVarPut",
            "__nativeCacheFileGet",
            "__nativeCacheFilePut",
            "java",
            "cache",
            "__cache",
            "__out",
            "cookie",
            "source",
            "book",
        ];
        let mut out = String::new();
        for (k, v) in self.variables.borrow().iter() {
            let mut chars = k.chars();
            let ok_first = chars
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                .unwrap_or(false);
            if !ok_first
                || !k
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                || RESERVED.contains(&k.as_str())
            {
                continue;
            }
            out.push_str("var ");
            out.push_str(k);
            out.push('=');
            out.push_str(&js_string_literal(v));
            out.push_str(";\n");
        }
        out
    }

    pub(super) fn eval_js(
        &self,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let lit = js_string_literal(input);
        self.eval_js_lit(&lit, input, code, base_url)
    }

    /// 列表语义的 JS 求值：`result` 绑定为 JS 数组（Legado `getStringList` 的
    /// `evalJS(rule, List)`，见 AnalyzeRule.getStringList：Default 段先出 List<String>）。
    fn eval_js_list(
        &self,
        items: &[String],
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let lit = serde_json::to_string(items).unwrap_or_else(|_| "[]".into());
        let joined = items.join("\n");
        self.eval_js_lit(&lit, &joined, code, base_url)
    }

    /// `lit` 是注入到 `var result=` 的 JS 字面量（单值为带引号字符串，列表为数组字面量）。
    fn eval_js_lit(
        &self,
        lit: &str,
        input: &str,
        code: &str,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let conf = js_conf();
        if !conf.enabled {
            return Err(RuleError::Other("JS rules disabled by JS_ENABLED=0".into()));
        }
        // 规则级 `{{…}}` 模板先于脚本展开（Legado `makeUpRule` 的模板语义），
        // 覆盖 `@js:` 与 `<js>` 段，也覆盖 searchUrl 的 `@js:` 分支（p11 根因）。
        let code = &self.expand_js_template(code, input);
        // 整段包成 IIFE，避免不同书源/不同次执行之间的全局变量污染
        let shim = format!(
            "{}\n{}\n{}\n{}\n{}",
            CN_TO_NUM_JS, JS_UTF8_HELPER, JS_MD5, JS_T2S, JAVA_BOOK_SHIM
        );
        // DOM 桥 Phase 1：把规则里“字面量选择器”的求值结果预抽取成 JSON 映射，
        // 注入到同一个 IIFE 作用域；JS 里 java.getElements 只做查表，不再抛错。
        let (dom, doms) = self.dom_bridge(input, code, base_url);
        let dom_js = serde_json::to_string(&dom).unwrap_or_else(|_| "{}".into());
        let doms_js = serde_json::to_string(&doms).unwrap_or_else(|_| "{}".into());
        let vars_js = self.js_vars();
        let cache_init =
            serde_json::to_string(&self.cache.borrow().clone()).unwrap_or_else(|_| "{}".into());
        // `java.put` / `book.putVariable` 写入的变量表要在本次求值开始时就可见
        // （`@put` 在规则层先执行，JS 里 `java.get` 应能读到）。
        let vars_init =
            serde_json::to_string(&self.variables.borrow().clone()).unwrap_or_else(|_| "{}".into());
        let script = format!(
            "(function(){{ var result={r}; var __input={r}; var __base={b}; var baseUrl=__base;\nvar __page={page}; var __sourceKey={sk}; var __sourceLoginUrl={lu}; var __refreshToc=false; var __bookUrl={bu}; var __bookTocUrl={bt};\nvar __bookName={bn}; var __bookAuthor={ba}; var __bookKind={bk}; var __bookIntro={bi}; var __bookCoverUrl={bc};\n{vars}\n{cachejs}\n{shim}\n{lib}\n__vars={varsinit};\n__cache={cacheinit};\n__DOM={dom};\n__DOMS={doms};\nvar __t0 = Date.now();\nvar __r = eval({code});\nvar __ms = Date.now() - __t0;\nvar __out;\nif (Array.isArray(__r)) {{ try {{ __out = JSON.stringify(__r); }} catch(__e) {{}} }}\nelse __out = String(__r);\nif (typeof __out === 'undefined') __out = String(__r);\nreturn __out + {vsent} + JSON.stringify(__vars) + {sent} + JSON.stringify(__cache) + {book} + JSON.stringify({{bookUrl: book.bookUrl, tocUrl: book.tocUrl}}) + {tocs} + (__refreshToc ? '1' : '0') + {msent} + String(__ms); }})()",
            r = lit,
            b = js_string_literal(base_url),
            page = js_string_literal(&self.current_content.borrow()),
            sk = js_string_literal(&self.source_key.borrow()),
            lu = js_string_literal(&self.source_login_url.borrow()),
            bu = js_string_literal(&self.book_url.borrow()),
            bt = js_string_literal(&self.book_toc_url.borrow()),
            bn = js_string_literal(&self.book_meta.borrow().name),
            ba = js_string_literal(&self.book_meta.borrow().author),
            bk = js_string_literal(&self.book_meta.borrow().kind),
            bi = js_string_literal(&self.book_meta.borrow().intro),
            bc = js_string_literal(&self.book_meta.borrow().cover_url),
            vars = vars_js,
            varsinit = vars_init,
            cachejs = JS_CACHE,
            // jsLib 是共享作用域脚本，必须排在 shim 之后求值（见 js_lib_preload_js）。
            lib = js_lib_preload_js(&js_lib_urls(&self.js_lib)),
            shim = shim,
            cacheinit = cache_init,
            dom = dom_js,
            doms = doms_js,
            code = js_string_literal(code),
            vsent = js_string_literal(JS_VARS_SENTINEL),
            sent = js_string_literal(JS_CACHE_SENTINEL),
            book = js_string_literal(JS_BOOK_SENTINEL),
            tocs = js_string_literal(JS_REFRESH_TOC_SENTINEL),
            msent = js_string_literal(JS_RULE_MS_SENTINEL),
        );
        let (tx, rx) = channel::<(Result<String, String>, u64, u64)>();
        // 慢 JS 只阻塞本次规则求值，不再占住 tokio worker；在途超限则快速降级（见 `run_js_job`）。
        let (result, net_ms, js_elapsed) = run_js_job(JsJob { script, reply: tx }, rx)?;
        // 结果串尾部依次带回 cache 变更与规则自身耗时（非法/超大都忽略，不影响主结果）
        let mut rule_ms: Option<u64> = None;
        let result = match result {
            Ok(mut s) => {
                if let Some(idx) = s.rfind(JS_RULE_MS_SENTINEL) {
                    let tail = s[idx + JS_RULE_MS_SENTINEL.len()..].trim().to_string();
                    if let Ok(ms) = tail.parse::<u64>() {
                        rule_ms = Some(ms);
                    }
                    s.truncate(idx);
                }
                if let Some(idx) = s.rfind(JS_REFRESH_TOC_SENTINEL) {
                    let flag = s[idx + JS_REFRESH_TOC_SENTINEL.len()..].trim();
                    if flag.starts_with('1') {
                        self.refresh_toc.set(true);
                    }
                    s.truncate(idx);
                }
                // `pre_update_js` 可能改写 book.bookUrl / book.tocUrl，回写到规则上下文
                if let Some(idx) = s.rfind(JS_BOOK_SENTINEL) {
                    let tail = s[idx + JS_BOOK_SENTINEL.len()..].trim().to_string();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tail) {
                        if let Some(u) = v.get("bookUrl").and_then(|x| x.as_str()) {
                            if !u.is_empty() {
                                self.book_url.replace(u.to_string());
                            }
                        }
                        if let Some(u) = v.get("tocUrl").and_then(|x| x.as_str()) {
                            if !u.is_empty() {
                                self.book_toc_url.replace(u.to_string());
                            }
                        }
                    }
                    s.truncate(idx);
                }
                if let Some(idx) = s.rfind(JS_CACHE_SENTINEL) {
                    let tail = s[idx + JS_CACHE_SENTINEL.len()..].to_string();
                    if tail.len() <= 1_048_576 {
                        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&tail) {
                            self.cache.replace(map);
                        }
                    }
                    s.truncate(idx);
                }
                // JS 侧 `java.put` / `book.putVariable` / `java.putVariable` 的写入回填到
                // 规则变量表：本请求后续规则（含 `@get:{key}`）立即可见；book 层同步写
                // `book_vars`，由编排层按 book_key 落库，与 Legado `book.putVariable` 一致。
                if let Some(idx) = s.rfind(JS_VARS_SENTINEL) {
                    let tail = s[idx + JS_VARS_SENTINEL.len()..].to_string();
                    if tail.len() <= 1_048_576 {
                        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&tail) {
                            let mut vars = self.variables.borrow_mut();
                            for (k, v) in map {
                                vars.insert(k.clone(), v.clone());
                                self.book_vars.borrow_mut().insert(k, v);
                            }
                        }
                    }
                    s.truncate(idx);
                }
                Ok(s)
            }
            Err(e) => Err(e),
        };
        let (result, net_ms) = (result.map_err(RuleError::Other), net_ms);
        // 预算只算规则自身耗时（页面里 Date.now 包裹 eval 的那段），固定垫片/lib 的
        // 解析成本与机器负载相关，不该算到源作者头上；`java.ajax` 联网时间也扣掉。
        // 拿不到回传耗时（脚本抛错等）时退回整段耗时，保守但不会放过慢规则。
        let budget_ms = rule_ms.unwrap_or(js_elapsed).saturating_sub(net_ms);
        // 排队等单一 JS 线程的时间不计入预算：并发测试/并发请求下同一规则仍应通过。
        JS_LAST_MS.store(js_elapsed, Ordering::Relaxed);
        JS_MAX_MS.fetch_max(budget_ms, Ordering::Relaxed);
        // 已经报错的脚本保留原始错误（如 RuntimeLimit），不要被“超预算”盖掉
        if result.is_ok() && budget_ms > conf.timeout_ms {
            tracing::warn!(
                elapsed_ms = js_elapsed,
                rule_ms = budget_ms,
                net_ms,
                budget_ms = conf.timeout_ms,
                "js rule exceeded time budget"
            );
            return Err(RuleError::Other(format!(
                "js eval exceeded {}ms budget (took {}ms)",
                conf.timeout_ms, budget_ms
            )));
        }
        result
    }

    /// 列表：返回多个元素的字符串（HTML 取 inner_html；JSON 取数组元素字符串）
    pub fn get_string_list(&self, content: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        self.current_content.replace(content.to_string());
        let rule = self.apply_put_get(rule, content, false, "");
        let rule = self.expand_vars(&rule, content);
        // Legado 约定：规则以 `-` 开头表示「结果倒序」（多见于章节列表，如 `-$..comics.*`）。
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
            // Default 段在**列表**语义里先求成 List<String>（Legado AnalyzeRule.getStringList
            // 用 JSoup 规则.getStringList），JS 段拿到的是数组而不是单个字符串：
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
            // 对象数组（`[{text,href}]`）按 Legado 章节语义逐项展开，供后续字段规则取用。
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
    /// 与 Legado `getStringList` 对齐：`||` 取首个非空列表、`&&` 合并列表、`%%` 交错，
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

    /// 列表层求值：先按 Legado 的优先级切分操作符，再调用单段解析。
    fn eval_list(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
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
    fn eval_segment_list(&self, content: &str, rule: &str, base_url: &str) -> Vec<String> {
        let r = rule.trim();
        if r.is_empty() {
            return vec![];
        }
        // JS 段：数组按 JSON 拆项，普通字符串按 `\n` 拆（Legado getStringList 对 String 的处理）
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
    fn css_extract_all(&self, html: &str, rule: &str) -> Vec<String> {
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

    fn css_list(&self, html: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        Ok(elements_html(html, rule))
    }

    fn json_list(&self, json: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let v: Value = serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let mut out = Vec::new();
        // JSONPath 直接命中数组时（`$['hits']` / `$.data.topics`）按数组元素展开：
        // Legado 的 getElements 会把 List 的每个元素当作列表项，不把整个数组当成一项。
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

/// 生成安全的 JS 字符串字面量（复用 serde_json 转义，并处理 JS 行分隔符）。
fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// 把 JSON 元素转成纯文本：字符串取内部值（不含 JSON 引号），其余 JSON 序列化。
fn value_to_string(v: Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}
