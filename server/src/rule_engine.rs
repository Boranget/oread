//! Legado 规则引擎（AnalyzeRule 等价实现）
//!
//! 支持：CSS 选择器(scraper) / JsonPath / XPath(sxd-xpath, XML 源) / 正则 / 变量 `${name}` 展开 /
//! 整串 JS 段(`<js>`、`@js:`)切分 / 列表解析(get_string_list)。
//! 元素级 `&&`/`||`/`%%`/`@` 组合与索引/范围语法、WebView 渲染降级为后续增强项。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use jsonpath_rust::JsonPathQuery;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;

/// 中文数字转阿拉伯数字（章节名用，支持 零~九、十百千万亿、两）。
const CN_TO_NUM_JS: &str = r#"
function cn2num(s){
  if(s==null) return "";
  s=String(s);
  if(/^[0-9]+$/.test(s)) return s;
  var d={'零':0,'一':1,'二':2,'两':2,'三':3,'四':4,'五':5,'六':6,'七':7,'八':8,'九':9};
  var u={'十':10,'百':100,'千':1000,'万':10000,'亿':100000000};
  var total=0,section=0,num=0;
  for(var i=0;i<s.length;i++){
    var ch=s[i];
    if(d[ch]!==undefined){ num=d[ch]; }
    else if(u[ch]!==undefined){
      if(ch==='万'||ch==='亿'){ if(num===0) num=1; section+=num*u[ch]; total+=section; section=0; num=0; }
      else { if(num===0) num=1; section+=num*u[ch]; num=0; }
    } else if(ch==='零'){ num=0; }
  }
  total+=section+num;
  return total===0 ? s : String(total);
}
"#;

/// 给 boa 注入的 java / book 垫片。
/// 注：java.getElements 需要把规则交回 Rust 引擎解析 DOM，boa 0.19 无便捷原生函数绑定，
/// 暂以安全桩返回空数组（避免崩溃）；后续可改用 NativeFunction 接入 DOM 解析。
const JAVA_BOOK_SHIM: &str = r#"
var __vars = {};
var book = {
  getVariable: function(k){ return __vars[k] !== undefined ? __vars[k] : ""; },
  putVariable: function(k,v){ __vars[k] = String(v); }
};
var java = {
  toNumChapter: function(s){ return s==null ? "" : cn2num(String(s)); },
  getElements: function(rule){ return []; },
  base64Decode: function(s){ try { return decodeURIComponent(escape(atob(s))); } catch(e){ return ""; } },
  base64Encode: function(s){ try { return btoa(unescape(encodeURIComponent(s))); } catch(e){ return ""; } },
  encodeURI: function(s){ return encodeURIComponent(String(s)); },
  decodeURI: function(s){ return decodeURIComponent(String(s)); },
  md5: function(s){ return ""; }
};
"#;

#[derive(Debug)]
pub enum RuleError {
    Selector(String),
    Json(String),
    Regex(String),
    Other(String),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::Selector(s) => write!(f, "selector: {s}"),
            RuleError::Json(s) => write!(f, "json: {s}"),
            RuleError::Regex(s) => write!(f, "regex: {s}"),
            RuleError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RuleError {}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Css,
    XPath,
    Json,
    Js,
    Regex,
    Default,
}

pub struct AnalyzeRule {
    pub variables: RefCell<HashMap<String, String>>,
    pub js_lib: String,
    pub is_json: bool,
}

impl Default for AnalyzeRule {
    fn default() -> Self {
        Self {
            variables: RefCell::new(HashMap::new()),
            js_lib: String::new(),
            is_json: false,
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

    pub fn set_vars(&self, m: HashMap<String, String>) {
        self.variables.borrow_mut().extend(m);
    }

    fn expand_vars(&self, rule: &str) -> String {
        // 支持 Legado 的 ${var} 与 {{var}} 两种变量引用
        let re = Regex::new(r"\$\{([^}]+)\}|\{\{([^}]+)\}\}").unwrap();
        re.replace_all(rule, |caps: &regex::Captures| {
            let key = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str())
                .unwrap_or("");
            self.variables.borrow().get(key).cloned().unwrap_or_default()
        })
        .to_string()
    }

    fn detect_mode(rule: &str, is_json: bool) -> Mode {
        if rule.starts_with("@xpath:") {
            Mode::XPath
        } else if rule.starts_with("@json:") {
            Mode::Json
        } else if rule.starts_with("@css:") || rule.starts_with("@@") {
            Mode::Css
        } else if rule.starts_with("$.") || rule.starts_with("$[") || is_json {
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

    /// 整串层切分：抽出 `<js>…</js>` 与 `@js:…` 段作为 JS，其余作为默认模式
    fn split_js(rule: &str) -> Vec<(Mode, String)> {
        let mut parts = Vec::new();
        let re = Regex::new(r"<js>([\s\S]*?)</js>|@js:([\s\S]*)").unwrap();
        let mut last = 0;
        for caps in re.captures_iter(rule) {
            let m = caps.get(0).unwrap();
            let (start, end) = (m.start(), m.end());
            if start > last {
                parts.push((Mode::Default, rule[last..start].to_string()));
            }
            if let Some(c) = caps.get(1).or_else(|| caps.get(2)) {
                parts.push((Mode::Js, c.as_str().to_string()));
            }
            last = end;
        }
        if last < rule.len() {
            parts.push((Mode::Default, rule[last..].to_string()));
        }
        parts
    }

    fn strip_prefix<'a>(rule: &'a str, p: &str) -> &'a str {
        rule.strip_prefix(p).unwrap_or(rule)
    }

    /// 单值管道：对 content 应用一条规则串，逐段执行，上一步结果作为下一步输入
    pub fn get_string(
        &self,
        content: &str,
        rule: &str,
        is_url: bool,
        base_url: &str,
    ) -> Result<String, RuleError> {
        let rule = self.expand_vars(rule);
        if rule.trim().is_empty() {
            return Ok(String::new());
        }
        // @get:{key} / @put:{key:value} 规则级变量操作符（Legado 跨规则变量传递）
        let trimmed = rule.trim();
        if let Some(rest) = trimmed.strip_prefix("@get:{") {
            if rest.ends_with('}') {
                let key = &rest[..rest.len() - 1];
                return Ok(self
                    .variables
                    .borrow()
                    .get(key)
                    .cloned()
                    .unwrap_or_default());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("@put:{") {
            if let Some(end) = rest.rfind('}') {
                let inner = &rest[..end];
                if let Some(colon) = inner.find(':') {
                    let key = inner[..colon].trim().to_string();
                    let mut val = inner[colon + 1..].trim();
                    // 去掉可能的引号包裹
                    if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                        val = &val[1..val.len() - 1];
                    }
                    let res = self.get_string(content, val, is_url, base_url)?;
                    self.variables.borrow_mut().insert(key, res.clone());
                    return Ok(res);
                }
            }
        }
        // 顶层操作符（不在 <js>…</js> / @js: 段内）：|| 回退、&& 拼接、## 正则替换
        if let Some(parts) = split_outside_js(&rule, "||") {
            for p in &parts {
                let r = self.get_string(content, p, is_url, base_url)?;
                if !r.is_empty() {
                    return Ok(r);
                }
            }
            return Ok(String::new());
        }
        if let Some(parts) = split_outside_js(&rule, "&&") {
            let mut out = String::new();
            for p in &parts {
                out.push_str(&self.get_string(content, p, is_url, base_url)?);
            }
            return Ok(out);
        }
        if let Some(parts) = split_outside_js(&rule, "##") {
            let sub = self.get_string(content, parts[0], is_url, base_url)?;
            if parts.len() >= 2 && !parts[1].trim().is_empty() {
                if let Ok(re) = Regex::new(parts[1].trim()) {
                    let repl = parts.get(2).map(|s| s.trim()).unwrap_or("");
                    return Ok(re.replace_all(&sub, repl).to_string());
                }
            }
            return Ok(sub);
        }
        // 默认：<js> / @js: 段切分管道
        let mut input = content.to_string();
        for (mode, text) in Self::split_js(&rule) {
            let t = text.trim();
            if t.is_empty() {
                continue;
            }
            input = match mode {
                Mode::Js => self.eval_js(&input, t, base_url)?,
                _ => {
                    let m = if mode == Mode::Default {
                        Self::detect_mode(t, self.is_json)
                    } else {
                        mode
                    };
                    self.apply_by_mode(&input, m, t)?
                }
            };
        }
        Ok(input)
    }

    fn apply_by_mode(&self, input: &str, mode: Mode, rule: &str) -> Result<String, RuleError> {
        match mode {
            Mode::XPath => self.apply_xpath(input, Self::strip_prefix(rule, "@xpath:")),
            Mode::Json => self.apply_json(input, Self::strip_prefix(rule, "@json:")),
            Mode::Regex => self.apply_regex(input, rule),
            _ => self.apply_css(input, Self::strip_prefix(rule, "@css:")),
        }
    }

    fn apply_css(&self, html: &str, rule: &str) -> Result<String, RuleError> {
        let (selector, tokens) = split_css_rule(rule);
        let doc = scraper::Html::parse_document(html);
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
                Token::Tag { name, index, indices } => {
                    let mut m = apply_tag(els, &build_spec(&name, index));
                    if !indices.is_empty() {
                        m = m
                            .into_iter()
                            .enumerate()
                            .filter(|(i, _)| indices.contains(i))
                            .map(|(_, e)| e)
                            .collect();
                    }
                    els = m;
                }
            }
        }
        // 单值抽取：Legado getString 只取首个匹配元素（getString 返回 list.get(0)）
        match terminal {
            Some(t) if t == "textNodes" => Ok(els
                .first()
                .map(|e| text_nodes(e))
                .unwrap_or_default()),
            Some(t) => Ok(els
                .first()
                .map(|e| extract_token(*e, &t))
                .unwrap_or_default()),
            None => Ok(els
                .first()
                .map(|e| e.inner_html())
                .unwrap_or_default()),
        }
    }

    fn apply_json(&self, json: &str, rule: &str) -> Result<String, RuleError> {
        use jsonpath_rust::JsonPathQuery;
        let v: Value =
            serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        let inst: Value = v
            .path(rule)
            .map_err(|e| RuleError::Json(format!("{e:?}")))?;
        let val = match inst {
            Value::Array(mut arr) => arr.pop().unwrap_or(Value::Null),
            other => other,
        };
        match val {
            Value::String(s) => Ok(s),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            other => Ok(other.to_string()),
        }
    }

    fn apply_regex(&self, input: &str, rule: &str) -> Result<String, RuleError> {
        // 斜杠包裹规则：/pat/ 为提取（捕获组1），/pat/repl/ 为替换
        if rule.starts_with('/') {
            let interior = &rule[1..]; // 含结尾斜杠
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
        let package = xml_parse(content).map_err(|e| RuleError::Other(format!("xpath parse: {e:?}")))?;
        let doc = package.as_document();
        let val =
            evaluate_xpath(&doc, rule).map_err(|e| RuleError::Other(format!("xpath eval: {e:?}")))?;
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

    fn eval_js(&self, input: &str, code: &str, base_url: &str) -> Result<String, RuleError> {
        use boa_engine::{Context, Source};
        let mut ctx = Context::default();
        // 注入上一阶段结果 result，以及当前页面 __input / 基准地址 __base
        let _ = ctx.eval(Source::from_bytes(
            format!(
                "var result = `{}`;\nvar __input = `{}`;\nvar __base = `{}`;",
                input.replace('\\', "\\\\").replace('`', "\\`"),
                input.replace('\\', "\\\\").replace('`', "\\`"),
                base_url.replace('\\', "\\\\").replace('`', "\\`"),
            )
            .as_bytes(),
        ));
        if !self.js_lib.is_empty() {
            let _ = ctx.eval(Source::from_bytes(self.js_lib.as_bytes()));
        }
        // 注入 java / book 垫片（含 toNumChapter 中文数字转阿拉伯）
        let shim = format!("{}\n{}", CN_TO_NUM_JS, JAVA_BOOK_SHIM);
        let _ = ctx.eval(Source::from_bytes(shim.as_bytes()));
        let res = ctx
            .eval(Source::from_bytes(code.as_bytes()))
            .map_err(|e| RuleError::Other(format!("js eval: {e:?}")))?;
        let js_str = res
            .to_string(&mut ctx)
            .map_err(|e| RuleError::Other(format!("js to_string: {e:?}")))?;
        Ok(js_str.to_std_string().unwrap_or_default())
    }

    /// 列表：返回多个元素的字符串（HTML 取 inner_html；JSON 取数组元素字符串）
    pub fn get_string_list(&self, content: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let rule = self.expand_vars(rule);
        let r = rule.trim_start();
        if r.starts_with("@js:") || rule.contains("<js>") {
            let code = if let Some(c) = rule.strip_prefix("@js:") {
                c
            } else {
                &rule
            };
            let res = self.eval_js(content, code, "")?;
            // 整段 JS 常返回数组（如 java.getElements 的结果），序列化为 JSON 数组则拆成多项
            if res.starts_with('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<String>>(&res) {
                    return Ok(arr);
                }
            }
            return Ok(vec![res]);
        }
        let mode = Self::detect_mode(&rule, self.is_json);
        match mode {
            Mode::Json => self.json_list(content, Self::strip_prefix(&rule, "@json:")),
            _ => self.css_list(content, Self::strip_prefix(&rule, "@css:")),
        }
    }

    fn css_list(&self, html: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        Ok(elements_html(html, rule))
    }

    fn json_list(&self, json: &str, rule: &str) -> Result<Vec<String>, RuleError> {
        let v: Value =
            serde_json::from_str(json).map_err(|e| RuleError::Json(e.to_string()))?;
        match v
            .path(rule)
            .map_err(|e| RuleError::Json(format!("{e:?}")))?
        {
            Value::Array(arr) => Ok(arr.into_iter().map(value_to_string).collect()),
            _ => Ok(vec![]),
        }
    }
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

/// 对单个元素按抽取标记取值（@text/@href/@content/@src/@html/@ownText/@all 等）。
fn extract_token(el: scraper::ElementRef, tok: &str) -> String {
    match tok {
        "text" | "ownText" => el.text().collect::<Vec<_>>().join(""),
        "html" => el.inner_html(),
        "all" => el.html(),
        "href" | "src" | "alt" | "content" | "id" | "value" | "class" | "style" | "title"
        | "name" => el.value().attr(tok).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// 抽取/遍历 token 解析结果。
enum Token {
    Children,
    Tag {
        name: String,
        index: Option<usize>,
        indices: Vec<usize>,
    },
    Terminal(String),
}

/// 已知的末端抽取关键字（@text/@href/@html/@all 等）。
fn is_extract_keyword(tok: &str) -> bool {
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
fn parse_token(tok: &str) -> Token {
    if tok == "children" || tok == "child" {
        return Token::Children;
    }
    // 下标筛选：dd!0:1:2 或 tag.a!0:1
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
        };
    }
    if let Some(rest) = tok.strip_prefix("tag.") {
        let (name, index) = split_name_index(rest);
        return Token::Tag {
            name,
            index,
            indices: vec![],
        };
    }
    if is_extract_keyword(tok) {
        return Token::Terminal(tok.to_string());
    }
    let (name, index) = split_name_index(tok);
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic()) {
        return Token::Tag { name, index, indices: vec![] };
    }
    Token::Terminal(tok.to_string())
}

/// 拼装 apply_tag 用的 spec（name 或 name.index）。
fn build_spec(name: &str, index: Option<usize>) -> String {
    match index {
        Some(i) => format!("{name}.{i}"),
        None => name.to_string(),
    }
}

/// 按规则选中元素并返回其 outer HTML 列表（供 bookList 嵌套解析与 java.getElements 复用）。
fn elements_html(html: &str, rule: &str) -> Vec<String> {
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
            Token::Tag { name, index, indices } => {
                let mut m = apply_tag(els, &build_spec(&name, index));
                if !indices.is_empty() {
                    m = m
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| indices.contains(i))
                        .map(|(_, e)| e)
                        .collect();
                }
                els = m;
            }
            Token::Terminal(_) => break, // 列表模式遇到抽取标记即停止遍历
        }
    }
    els.iter().map(|e| e.html()).collect()
}

/// 去掉 @css:/@CSS:/@@ 前缀，并按 '@' 切分为 (选择器, 抽取/遍历标记)。
fn split_css_rule<'a>(rule: &'a str) -> (String, Vec<&'a str>) {
    let rule = rule
        .strip_prefix("@css:")
        .or_else(|| rule.strip_prefix("@CSS:"))
        .unwrap_or(rule)
        .trim_start_matches("@@")
        .trim();
    let parts: Vec<&str> = rule.split('@').collect();
    let selector = parts[0].trim().to_string();
    let tokens: Vec<&str> = parts[1..].iter().map(|s| s.trim()).collect();
    (selector, tokens)
}

/// 解析 `name` 或 `name.0` 形式的规格，返回 (名称, 可选索引)。
fn split_name_index(s: &str) -> (String, Option<usize>) {
    if let Some(dot) = s.rfind('.') {
        let (name, idxs) = s.split_at(dot);
        if let Ok(i) = idxs[1..].parse::<usize>() {
            return (name.to_string(), Some(i));
        }
    }
    (s.to_string(), None)
}

/// 初始选择器：支持 Legado 的 `class.x` / `id.x`（按 class/id 选）及可选索引（取第 i 个）。
fn select_initial<'a>(doc: &'a scraper::Html, selector: &str) -> Vec<scraper::ElementRef<'a>> {
    let (css, idx) = if let Some(rest) = selector.strip_prefix("class.") {
        let (name, i) = split_name_index(rest);
        (format!(".{name}"), i)
    } else if let Some(rest) = selector.strip_prefix("id.") {
        let (name, i) = split_name_index(rest);
        (format!("#{name}"), i)
    } else {
        (selector.to_string(), None)
    };
    let sel = match scraper::Selector::parse(&css) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let els: Vec<scraper::ElementRef<'a>> = doc.select(&sel).collect();
    if let Some(i) = idx {
        if i < els.len() {
            vec![els[i]]
        } else {
            vec![]
        }
    } else {
        els
    }
}

/// 取元素的直接子元素（仅元素节点）。
fn child_elements<'a>(el: scraper::ElementRef<'a>) -> Vec<scraper::ElementRef<'a>> {
    el.children().filter_map(scraper::ElementRef::wrap).collect()
}

/// 从当前元素集合向下钻取到指定子标签（@tag.name[.index]）。
fn apply_tag<'a>(els: Vec<scraper::ElementRef<'a>>, spec: &str) -> Vec<scraper::ElementRef<'a>> {
    let (tag, idx) = split_name_index(spec);
    let mut matched: Vec<scraper::ElementRef<'a>> = Vec::new();
    for e in &els {
        for c in child_elements(*e) {
            if c.value().name() == tag {
                matched.push(c);
            }
        }
    }
    if let Some(i) = idx {
        if i < matched.len() {
            vec![matched[i]]
        } else {
            vec![]
        }
    } else {
        matched
    }
}

/// 取元素自身的直接文本节点（不含子元素文本），对应 @textNodes。
fn text_nodes(el: &scraper::ElementRef) -> String {
    el.children()
        .filter_map(|n| n.value().as_text().map(|t| t.to_string()))
        .collect()
}

/// 在规则串顶层按分隔符拆分，但忽略落在 `<js>…</js>` 或 `@js:` 段内的分隔符。
/// 仅当当前字节为 ASCII 时才尝试 `starts_with`（分隔符/标记均为 ASCII），避免多字节
/// UTF-8（如中文）字符内部切片越界。
fn split_outside_js<'a>(rule: &'a str, sep: &str) -> Option<Vec<&'a str>> {
    let mut parts: Vec<&str> = Vec::new();
    let bytes = rule.as_bytes();
    let n = bytes.len();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        if bytes[i] < 0x80 {
            let s = &rule[i..];
            if s.starts_with("<js>") {
                if let Some(rel) = s.find("</js>") {
                    let end = i + rel + 5;
                    i = end;
                    start = end;
                    continue;
                }
            }
            if s.starts_with("@js:") {
                if start < i {
                    parts.push(&rule[start..i]);
                }
                parts.push(&rule[i..]);
                return Some(parts);
            }
            if s.starts_with(sep) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_extracts_text() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="book"><span class="name">测试书名</span><span class="author">某作者</span></div>"#;
        let name = ar.get_string(html, ".name", false, "").unwrap();
        assert_eq!(name, "测试书名");
        let author = ar.get_string(html, ".author", false, "").unwrap();
        assert_eq!(author, "某作者");
    }

    #[test]
    fn tag_traversal_extracts_nested() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="itemtxt"><p>第1章</p><a href="/b/1">链接</a></div>"#;
        // class.itemtxt -> .itemtxt，再向下钻取到子 <a> 取 href
        let href = ar
            .get_string(html, "@CSS:class.itemtxt@tag.a@href", false, "")
            .unwrap();
        assert_eq!(href, "/b/1");
        // 取第 0 个 <p> 的文本
        let p = ar
            .get_string(html, "@CSS:class.itemtxt@tag.p.0@text", false, "")
            .unwrap();
        assert_eq!(p, "第1章");
    }

    #[test]
    fn class_selector_index() {
        let ar = AnalyzeRule::new();
        let html = r#"<div class="des">A</div><div class="des">B</div>"#;
        // class.des.0 -> 取第 0 个 .des
        let r = ar
            .get_string(html, "@CSS:class.des.0@text", false, "")
            .unwrap();
        assert_eq!(r, "A");
    }

    #[test]
    fn children_and_textnodes() {
        let ar = AnalyzeRule::new();
        let html = r#"<ul><li><a href="/x">甲</a></li><li><a href="/y">乙</a></li></ul>"#;
        // @children 取直接子节点（li），再取 a 的 href
        let r = ar
            .get_string(html, "@CSS:ul@children@tag.a@href", false, "")
            .unwrap();
        assert_eq!(r, "/x");
        let html2 = r#"<div>前缀<span>内</span>后缀</div>"#;
        let t = ar
            .get_string(html2, "@CSS:div@textNodes", false, "")
            .unwrap();
        assert_eq!(t, "前缀后缀");
    }

    #[test]
    fn jsonpath_extracts_value() {
        let mut ar = AnalyzeRule::new();
        ar.is_json = true;
        let json = r#"{"list":[{"name":"书A","author":"甲"},{"name":"书B","author":"乙"}]}"#;
        let name = ar.get_string(json, "$.list[0].name", false, "").unwrap();
        assert_eq!(name, "书A");
    }

    #[test]
    fn json_list_extracts_array() {
        let mut ar = AnalyzeRule::new();
        ar.is_json = true;
        let json = r#"{"list":[{"name":"书A"},{"name":"书B"}]}"#;
        let names = ar.get_string_list(json, "$.list[*].name").unwrap();
        assert_eq!(names, vec!["书A".to_string(), "书B".to_string()]);
    }

    #[test]
    fn regex_capture_works() {
        let ar = AnalyzeRule::new();
        let txt = "编号: 12345 结束";
        let r = ar.get_string(txt, "/编号:\\s*(\\d+)/", false, "").unwrap();
        assert_eq!(r, "12345");
    }

    #[test]
    fn java_to_num_chapter() {
        let ar = AnalyzeRule::new();
        let r = ar
            .get_string("", "<js>java.toNumChapter(\"第一百一十二\")</js>", false, "")
            .unwrap();
        assert_eq!(r, "112");
    }

    #[test]
    fn put_get_variables() {
        let ar = AnalyzeRule::new();
        let html = "<div><a>lit</a></div>";
        // @put 求值后存入变量，@get 取回
        let stored = ar.get_string(html, "@put:{x:a@text}", false, "").unwrap();
        assert_eq!(stored, "lit");
        let got = ar.get_string(html, "@get:{x}", false, "").unwrap();
        assert_eq!(got, "lit");
    }

    #[test]
    fn brace_var_expansion() {
        let ar = AnalyzeRule::new();
        ar.set_var("y", "world");
        // {{var}} 在 JS 段内展开
        let r = ar
            .get_string("", "<js>\"{{y}}\"</js>", false, "")
            .unwrap();
        assert_eq!(r, "world");
    }

    #[test]
    fn css_not_first_child_supported() {
        let ar = AnalyzeRule::new();
        let html = r#"<table id="bookcon"><tr><th>h</th></tr><tr><td><p><a href="/1">a</a></p></td></tr><tr><td><p><a href="/2">b</a></p></td></tr></table>"#;
        let items = ar
            .get_string_list(html, "@css:table#bookcon tr:not(:first-child)")
            .unwrap();
        assert_eq!(
            items.len(),
            2,
            "tr:not(:first-child) should select 2 data rows"
        );
    }

    #[test]
    fn single_value_takes_first_match() {
        let ar = AnalyzeRule::new();
        // 一个 item 内多个 <a> 时，单值抽取（@href/@text）应取首个，而非拼接
        let html = r#"<tr><td><p><a href="/book/1">书名A</a></p></td><td><p><a href="/book/2">最新</a></p></td></tr>"#;
        let href = ar.get_string(html, "@css:p:nth-of-type(1) a@href", true, "").unwrap();
        assert_eq!(href, "/book/1");
    }

    #[test]
    fn bare_tag_and_index_filter() {
        let ar = AnalyzeRule::new();
        let html = r#"<dl id="list"><dd>第1</dd><dd>第2</dd><dd>第3</dd><dd>第4</dd><dd>第5</dd></dl>"#;
        // 裸标签遍历（@dd）+ 单值取首个
        let t1 = ar.get_string(html, "@CSS:id.list@dd@text", false, "").unwrap();
        assert_eq!(t1, "第1");
        // !0:1 下标筛选取前 2 个后再取首个
        let t2 = ar.get_string(html, "@CSS:id.list@dd!0:1@text", false, "").unwrap();
        assert_eq!(t2, "第1");
        // !2 取第 3 个（index 2）
        let t3 = ar.get_string(html, "@CSS:id.list@dd!2@text", false, "").unwrap();
        assert_eq!(t3, "第3");
    }

    #[test]
    fn js_segment_transforms() {
        let ar = AnalyzeRule::new();
        let input = "hello world";
        let out = ar.get_string(input, "<js>result.toUpperCase()</js>", false, "").unwrap();
        // 纯字符串场景：JS 段把 result 转大写
        assert_eq!(out, "HELLO WORLD");
    }

    #[test]
    fn variable_expansion_works() {
        let mut ar = AnalyzeRule::new();
        ar.set_var("sel", ".name");
        let html = r#"<div class="name">测试书名</div>"#;
        let r = ar.get_string(html, "${sel}", false, "").unwrap();
        assert_eq!(r, "测试书名");
    }
}

