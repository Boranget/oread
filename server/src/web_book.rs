//! WebBook 编排（五类解析：搜索/发现/详情/目录/正文）
//! 输入 HTML/JSON + 书源规则 -> 输出结构化书籍数据（纯函数管线）。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde_json::Value;
use url::Url;

use crate::entities::{
    Book, BookChapter, BookContent, BookImage, BookInfoRule, BookSource, ContentRule, ExploreRule,
    SearchBook, SearchRule, TocRule,
};
use crate::req;
use crate::rule_engine::{AnalyzeRule, BookMeta};

/// 判断书源是否为 JSON 型（规则以 $. 开头或 url 含 json）
fn is_json_source(source: &BookSource) -> bool {
    source
        .search_url
        .as_deref()
        .map(|u| u.contains("json"))
        .unwrap_or(false)
        || source
            .rule_search
            .as_ref()
            .and_then(|r| r.book_list.as_deref())
            .map(|b| b.starts_with("$."))
            .unwrap_or(false)
}

fn make_ar(source: &BookSource) -> AnalyzeRule {
    let mut ar = AnalyzeRule::new();
    ar.js_lib = source.js_lib.clone().unwrap_or_default();
    ar.is_json = is_json_source(source);
    // Legado JS 里的 `source.key` / `source.getKey()` 即 bookSourceUrl
    ar.source_key.replace(source.book_source_url.clone());
    ar.source_login_url
        .replace(source.login_url.clone().unwrap_or_default());
    ar
}

// ---------------- 跨请求的 book 层状态（变量 + cache） ----------------
//
// Legado 的 `@put`/`cache.put` 落在 Book 对象上，同书的 info→toc→content 共享；
// 本服务是纯函数管线，因此由编排层（本模块）在请求开始时按 `book_key` 预载，
// 请求结束时把快照交给路由层落库。存储实现（SQLite）在 `routes/books.rs`。

use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// `book_key = sha256(source_url + '\n' + book_url)`：不把原始长 URL 当主键。
pub fn book_key(source: &BookSource, book_url: &str) -> String {
    let origin = source.book_source_url.trim().trim_end_matches('/');
    let url = book_url.trim().trim_end_matches('/');
    let mut h = Sha256::new();
    h.update(format!("{origin}\n{url}").as_bytes());
    format!("{:x}", h.finalize())
}

#[derive(Default)]
struct StoreInner {
    /// 请求开始时的持久化快照（用于 diff 出增量）
    seed_vars: HashMap<String, HashMap<String, String>>,
    seed_cache: HashMap<String, HashMap<String, String>>,
    /// 请求结束时的快照
    cur_vars: HashMap<String, HashMap<String, String>>,
    cur_cache: HashMap<String, HashMap<String, String>>,
}

/// 一本书在两次请求之间的增量：变量全量快照（None 表示无变化）+ cache 增删。
pub struct BookChange {
    pub key: String,
    pub vars: Option<HashMap<String, String>>,
    pub cache_upserts: Vec<(String, String)>,
    pub cache_deletes: Vec<String>,
}

/// 请求级的 book 状态容器（可跨并发任务共享）。
#[derive(Default)]
pub struct BookStore {
    inner: Mutex<StoreInner>,
}

impl BookStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预载某本书的持久化状态（缺失时为两个空 map）。
    pub fn seed(&self, key: String, vars: HashMap<String, String>, cache: HashMap<String, String>) {
        let mut g = self.inner.lock().unwrap();
        g.seed_vars.insert(key.clone(), vars);
        g.seed_cache.insert(key, cache);
    }

    pub fn vars(&self, key: &str) -> HashMap<String, String> {
        self.inner
            .lock()
            .unwrap()
            .seed_vars
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn cache(&self, key: &str) -> HashMap<String, String> {
        self.inner
            .lock()
            .unwrap()
            .seed_cache
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// 记录某本书本次请求结束后的完整快照。
    pub fn record(
        &self,
        key: String,
        vars: HashMap<String, String>,
        cache: HashMap<String, String>,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.cur_vars.insert(key.clone(), vars);
        g.cur_cache.insert(key, cache);
    }

    /// 取走自上次调用以来的增量（并发任务安全：每本书只由记录它的任务消费一次）。
    pub fn take_changes(&self) -> Vec<BookChange> {
        let mut g = self.inner.lock().unwrap();
        let keys: HashSet<String> = g
            .cur_vars
            .keys()
            .chain(g.cur_cache.keys())
            .cloned()
            .collect();
        let mut out = Vec::new();
        for key in keys {
            let cur_vars = g.cur_vars.remove(&key).unwrap_or_default();
            let cur_cache = g.cur_cache.remove(&key).unwrap_or_default();
            let seed_vars = g.seed_vars.get(&key).cloned().unwrap_or_default();
            let seed_cache = g.seed_cache.get(&key).cloned().unwrap_or_default();
            if cur_vars.is_empty() && cur_cache.is_empty() && seed_vars.is_empty() {
                continue;
            }
            let vars = if !cur_vars.is_empty() && cur_vars != seed_vars {
                Some(cur_vars.clone())
            } else {
                None
            };
            let mut cache_upserts = Vec::new();
            for (k, v) in &cur_cache {
                if seed_cache.get(k) != Some(v) {
                    cache_upserts.push((k.clone(), v.clone()));
                }
            }
            let cache_deletes: Vec<String> = seed_cache
                .keys()
                .filter(|k| !cur_cache.contains_key(*k))
                .cloned()
                .collect();
            if vars.is_some() || !cache_upserts.is_empty() || !cache_deletes.is_empty() {
                out.push(BookChange {
                    key: key.clone(),
                    vars,
                    cache_upserts,
                    cache_deletes,
                });
            }
            // 已落库的状态成为新的基线，避免同一请求内重复写
            g.seed_vars.insert(key.clone(), cur_vars);
            g.seed_cache.insert(key, cur_cache);
        }
        out
    }
}

/// 把当前 Book 的元信息注入 JS 的 `book.*`（Legado 规则里的
/// `book.name/author/kind/intro/coverUrl`，简介模板类规则会直接引用）。
fn inject_book_meta(ar: &AnalyzeRule, book: &Book) {
    ar.set_book_meta(BookMeta {
        name: book.name.clone().unwrap_or_default(),
        author: book.author.clone().unwrap_or_default(),
        kind: book.kind.clone().unwrap_or_default(),
        intro: book.intro.clone().unwrap_or_default(),
        cover_url: book.cover_url.clone().unwrap_or_default(),
    });
}

/// 按 `book_url` 创建带 book 作用域的 AnalyzeRule（空 URL 时退化为普通请求作用域）。
fn make_ar_book(source: &BookSource, store: Option<&BookStore>, book_url: &str) -> AnalyzeRule {
    let ar = make_ar(source);
    ar.book_url.replace(book_url.to_string());
    if let Some(st) = store {
        if !book_url.trim().is_empty() {
            let key = book_key(source, book_url);
            ar.begin_book_scope(key.clone(), st.vars(&key), st.cache(&key));
        }
    }
    ar
}

/// `java.refreshTocUrl()` 的同步等价：重抓详情页并按 `toc_url` 规则重算目录地址。
async fn refresh_toc_url_from_info(
    source: &BookSource,
    store: Option<&BookStore>,
    book_url: &str,
) -> Option<String> {
    let info = source.rule_book_info.as_ref()?;
    let headers = req::source_headers(source, book_url);
    let html = match req::fetch_for_source_limited(
        book_url,
        "GET",
        &headers,
        None,
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                source_url = %source.book_source_url,
                error = %e,
                "refreshTocUrl: refetch book info failed; keep old toc url"
            );
            return None;
        }
    };
    let ar = make_ar_book(source, store, book_url);
    let mut content = html.to_string();
    if let Some(init) = &info.init {
        if let Ok(out) = ar.get_string(&html, init, false, book_url) {
            let t = out.trim();
            if !t.is_empty() && t != "null" && t != "undefined" {
                content = out;
            }
        }
    }
    let toc = eval_field(
        source,
        "toc_url",
        &ar,
        &content,
        info.toc_url.as_deref().unwrap_or(""),
        true,
        book_url,
    );
    if toc.is_empty() {
        None
    } else {
        Some(abs(book_url, &toc))
    }
}

/// 该源的正文规则是否声明了 `image_decode`（加密图片）。
pub fn has_image_decode(source: &BookSource) -> bool {
    source
        .rule_content
        .as_ref()
        .and_then(|c| c.image_decode.as_deref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// 对加密图片跑 `ContentRule.image_decode`：失败时回退原字节（不让整章图片 502）。
pub fn decode_image(source: &BookSource, src: &str, bytes: Vec<u8>) -> Vec<u8> {
    let Some(code) = source
        .rule_content
        .as_ref()
        .and_then(|c| c.image_decode.as_deref())
        .filter(|s| !s.trim().is_empty())
    else {
        return bytes;
    };
    let ar = make_ar(source);
    match ar.eval_image_decode(code, &bytes, src) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => bytes,
        Err(e) => {
            tracing::warn!(
                source_url = %source.book_source_url,
                error = %e,
                "image_decode failed; fallback to raw bytes"
            );
            crate::source_health::note_rule_error(
                &source.book_source_url,
                "image_decode",
                &e.to_string(),
            );
            bytes
        }
    }
}

/// 把 AnalyzeRule 的 book 层变量/cache 快照登记到 store，等待落库。
fn record_ar(ar: &AnalyzeRule, store: Option<&BookStore>) {
    if let Some(st) = store {
        if let Some(key) = ar.book_scope_key() {
            st.record(key, ar.book_vars_snapshot(), ar.cache_snapshot());
        }
    }
}

/// 规则未命中时 JsonPath/JS 可能返回字面量 `"null"`/`"None"`/`"undefined"`。
/// 这些值不是 URL，若不拦截会拼出 `/toc/null` 并重复追加同一目录（验收 P1）。
fn blank_value(s: &str) -> bool {
    matches!(s.trim(), "" | "null" | "None" | "none" | "undefined")
}

/// Legado 的布尔规则返回字符串，这里按常见写法判定（空/false/0/否 为假）。
fn truthy(s: &str) -> bool {
    !matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "" | "false" | "0" | "no" | "none" | "null" | "undefined" | "否"
    )
}

/// 把相对 URL 归并为绝对 URL
fn abs(base: &str, maybe: &str) -> String {
    // Legado 允许 URL 末尾附带 `,{...}` 选项（webView/headers/body 等）；本服务无 WebView，
    // 但链接本身必须去掉选项才能请求（开心漫画的章节链接即带 `,{"webView": true}`）。
    let maybe = strip_url_options(maybe);
    if maybe.starts_with("http://") || maybe.starts_with("https://") {
        return maybe.to_string();
    }
    if let Ok(b) = Url::parse(base) {
        if let Ok(u) = b.join(maybe) {
            return u.to_string();
        }
    }
    maybe.to_string()
}

/// 去尾巴式 Legado URL 选项：只有后缀能被 JSON 解析成 `{...}` 时才截断，避免误伤普通逗号路径。
/// 相对链接经 `Url::join` 后选项会被百分号编码（`,%7B…`），这种形态按 `,{` 等价处理。
fn strip_url_options(url: &str) -> &str {
    let mut cut = None;
    if let Some(pos) = url.rfind(",{") {
        if serde_json::from_str::<serde_json::Value>(&url[pos + 1..]).is_ok() {
            cut = Some(pos);
        }
    }
    if cut.is_none() {
        if let Some(pos) = url.rfind(",%7B").or_else(|| url.rfind(",%7b")) {
            cut = Some(pos);
        }
    }
    match cut {
        Some(pos) => url[..pos].trim_end(),
        None => url,
    }
}

/// JS 对象项里 `volume/isVolume/is_volume=true` 时视为分卷行（源没配 isVolume 规则时的兜底）。
fn item_json_volume(item: &str) -> bool {
    let t = item.trim_start();
    if !t.starts_with('{') {
        return false;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
        return false;
    };
    let Some(m) = v.as_object() else {
        return false;
    };
    ["volume", "isVolume", "is_volume"]
        .iter()
        .any(|k| m.get(*k).and_then(|x| x.as_bool()) == Some(true))
}

/// 对可选的布尔标志子规则求值（isVolume / isVip / isPay 共用）：
/// 规则缺省或求值失败按 false，其余交给 `truthy` 判定。
fn flag(
    source: &BookSource,
    field: &str,
    ar: &AnalyzeRule,
    item: &str,
    rule: &Option<String>,
    base: &str,
) -> bool {
    rule.as_ref()
        .map(|r| truthy(&eval_field(source, field, ar, item, r, false, base)))
        .unwrap_or(false)
}

/// Legado 多行规则：一行结果作为下一行输入（如快看
/// `$.id||$.topic_id\n@js:"/mobile/"+result+"/list/"`）。
/// 规则引擎把整串交给 `||` 回退链时，第二行的 `@js:` 会被并进第二个候选项，
/// `$.id` 一命中就丢掉 JS 段，于是 bookUrl 只剩 `3095`（旧版更糟：把整段 JSON 拼进 URL）。
/// 这里先按「顶层换行」切行再逐行管道求值，不改引擎本身的单行语义。
fn split_rule_lines(rule: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut in_js_tag = false;
    let mut chars = rule.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let rest = &rule[i..];
        if !in_js_tag && starts_with_ci_local(rest, "@js:") {
            // `@js:` 一直延伸到规则末尾（内部换行属于 JS 代码，不能再切行）
            buf.push_str(rest);
            break;
        }
        if !in_js_tag && rest.starts_with("<js>") {
            in_js_tag = true;
            buf.push_str("<js>");
            for _ in 0..3 {
                chars.next();
            }
            continue;
        }
        if in_js_tag && rest.starts_with("</js>") {
            in_js_tag = false;
            buf.push_str("</js>");
            for _ in 0..4 {
                chars.next();
            }
            continue;
        }
        if c == '\n' && !in_js_tag {
            out.push(std::mem::take(&mut buf));
            continue;
        }
        buf.push(c);
    }
    out.push(buf);
    out.retain(|l| !l.trim().is_empty());
    out
}

fn starts_with_ci_local(s: &str, p: &str) -> bool {
    s.get(..p.len())
        .map(|h| h.eq_ignore_ascii_case(p))
        .unwrap_or(false)
}

/// 单值规则求值（含多行管道）：逐行作用，上一行结果作为下一行输入。
fn eval_rule(
    ar: &AnalyzeRule,
    content: &str,
    rule: &str,
    is_url: bool,
    base: &str,
) -> Result<String, crate::rule_engine::RuleError> {
    let lines = split_rule_lines(rule);
    let mut input = content.to_string();
    let mut last = String::new();
    for line in lines.iter() {
        // URL 语义整条只取首值；文本字段保留多值（`\n` 拼接），供 `@js:result.split("\n")` 使用
        last = ar.get_string(&input, line, is_url, base)?;
        input = last.clone();
    }
    Ok(last)
}

/// 字段级规则求值：异常（JS TypeError、JSONPath 不匹配、选择器/正则非法等）
/// 一律视为**该字段为空**，并记一条可观测日志 + 进程内台账；不向上传播，
/// 因此不会把整个源判失败（只有网络层失败才会）。与 Legado 的行为一致：
/// 规则抛错 → 结果为空 → 继续走下一条规则/兜底；必需字段为空才按空解析处理。
fn eval_field(
    source: &BookSource,
    field: &str,
    ar: &AnalyzeRule,
    content: &str,
    rule: &str,
    is_url: bool,
    base: &str,
) -> String {
    match eval_rule(ar, content, rule, is_url, base) {
        Ok(v) => v,
        Err(e) => {
            note_field_error(source, field, &e);
            String::new()
        }
    }
}

/// 列表级规则求值，容错语义与 [`eval_field`] 相同（异常 → 空列表）。
fn eval_list_field(
    source: &BookSource,
    field: &str,
    ar: &AnalyzeRule,
    content: &str,
    rule: &str,
) -> Vec<String> {
    match ar.get_string_list(content, rule) {
        Ok(v) => v,
        Err(e) => {
            note_field_error(source, field, &e);
            Vec::new()
        }
    }
}

/// 记录字段级规则异常：日志含源名/字段/异常摘要，并写入 `/api/sources` 可见的台账。
fn note_field_error(source: &BookSource, field: &str, err: &crate::rule_engine::RuleError) {
    tracing::warn!(
        source_url = %source.book_source_url,
        source_name = %source.book_source_name,
        field,
        error = %err,
        "field rule eval failed; treated as empty"
    );
    crate::source_health::note_rule_error(&source.book_source_url, field, &err.to_string());
}

/// 对可选子规则求值，空结果返回 None
fn opt(
    source: &BookSource,
    field: &str,
    ar: &AnalyzeRule,
    item: &str,
    rule: &Option<String>,
    base: &str,
) -> Option<String> {
    rule.as_ref()
        .map(|r| eval_field(source, field, ar, item, r, false, base))
        .filter(|s| !s.is_empty())
}

/// 规则命中才覆盖：规则为空/求值失败/结果为空时保持原值（详情字段的 Legado 语义）。
fn assign_opt(
    target: &mut Option<String>,
    source: &BookSource,
    field: &str,
    ar: &AnalyzeRule,
    item: &str,
    rule: &Option<String>,
    base: &str,
) {
    if let Some(v) = opt(source, field, ar, item, rule, base) {
        *target = Some(v);
    }
}

// ---------------- 搜索 ----------------
/// JS `encodeURIComponent` 的等价实现：保留 unreserved 字符（字母数字与 `-_.!~*'()`），
/// 其余按 UTF-8 逐字节百分号编码。源里 `{{encodeURIComponent(key)}}` 走这里，
/// 避免中文/空格关键词在 URL 里原样出现或被 reqwest 双重编码。
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

/// 展开 searchUrl / POST body 的模板：
/// - `{{key}}`/`${key}`：原样代入关键词；
/// - `{{encodeURIComponent(key)}}`：百分号编码后代入；
/// - `{{page}}`/`${page}`：代入页码；
/// - `{{page*N}}`/`${page*N}`：代入 `page * N`（老源用它算 offset / 每页条数）。
///
/// 这是「快路径」：只处理确定可静态替换的写法，JS 关闭时也能工作。
fn expand_search_fast(tpl: &str, key: &str, page: i32) -> String {
    static PAGE_MULT: OnceLock<regex::Regex> = OnceLock::new();
    let re = PAGE_MULT.get_or_init(|| {
        regex::Regex::new(r"\$\{page\*(\d+)\}|\{\{page\*(\d+)\}\}").expect("page*N 模板正则")
    });
    let out = re
        .replace_all(tpl, |c: &regex::Captures| {
            let n: i32 = c
                .get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            (page * n).to_string()
        })
        .to_string();
    out.replace("${encodeURIComponent(key)}", &encode_component(key))
        .replace("{{encodeURIComponent(key)}}", &encode_component(key))
        .replace("{{key}}", key)
        .replace("${key}", key)
        .replace("{{page}}", &page.to_string())
        .replace("${page}", &page.to_string())
}

/// 把一段 JS 表达式按 Legado `AnalyzeUrl` 语义求值：
/// `null`/`undefined` → 空串，数值按 JS 默认字符串化（`64` 而非 `64.0`）。
fn eval_search_expr(
    ar: &AnalyzeRule,
    expr: &str,
    base: &str,
) -> Result<String, crate::rule_engine::RuleError> {
    let v = ar.eval_script(expr, "", base)?;
    let v = v.trim().to_string();
    if v == "undefined" || v == "null" {
        return Ok(String::new());
    }
    Ok(v)
}

/// 完整展开 searchUrl（对齐 Legado `AnalyzeUrl.initUrl` 的求值顺序）：
/// 1. `<js>...</js>` 块就地替换；
/// 2. `@js:` 前缀整段求值（oread 扩展，Legado 仅支持 `<js>`）；
/// 3. 其余 `{{...}}` 逐个按 JS 表达式求值（`key`/`page` 已绑定）。
///
/// 单个表达式异常时按「字段容错」语义返回空串并记异常台账，不中断整源。
fn expand_search_template(source: &BookSource, tpl: &str, key: &str, page: i32) -> String {
    let fast = expand_search_fast(tpl, key, page);
    let has_js_block = fast.to_ascii_lowercase().contains("<js>");
    if !has_js_block && !fast.contains("{{") && !fast.trim_start().starts_with("@js:") {
        return fast;
    }
    let ar = make_ar(source);
    ar.set_var("key", key);
    ar.set_var("page", page.to_string());
    let base = source.book_source_url.as_str();

    // 1) `<js> ... </js>`：与 Legado analyzeJs 一致，把块替换为执行结果
    static JS_BLOCK: OnceLock<regex::Regex> = OnceLock::new();
    let re = JS_BLOCK.get_or_init(|| regex::Regex::new(r"(?is)<js>(.*?)</js>").expect("js 块正则"));
    let mut out = re
        .replace_all(&fast, |c: &regex::Captures| {
            let code = c.get(1).map(|m| m.as_str()).unwrap_or("");
            match eval_search_expr(&ar, code, base) {
                Ok(v) => v,
                Err(e) => {
                    note_field_error(source, "search_url", &e);
                    String::new()
                }
            }
        })
        .to_string();

    // 2) `@js:` 前缀：整段交给 JS
    let trimmed = out.trim_start();
    if let Some(code) = trimmed.strip_prefix("@js:") {
        return match eval_search_expr(&ar, code, base) {
            Ok(v) => v,
            Err(e) => {
                note_field_error(source, "search_url", &e);
                String::new()
            }
        };
    }

    // 3) 剩余的 `{{...}}`：Legado 用 JS 求值（不是简单字符串替换）
    static INNER: OnceLock<regex::Regex> = OnceLock::new();
    let re = INNER.get_or_init(|| regex::Regex::new(r"\{\{(.+?)\}\}").expect("{{}} 正则"));
    out = re
        .replace_all(&out, |c: &regex::Captures| {
            let expr = c.get(1).map(|m| m.as_str()).unwrap_or("");
            match eval_search_expr(&ar, expr, base) {
                Ok(v) => v,
                Err(e) => {
                    note_field_error(source, "search_url", &e);
                    String::new()
                }
            }
        })
        .to_string();
    out
}

/// 解析 Legado 搜索 URL：支持 `url,{"method":"POST",...}` 形式；纯 URL 则按 GET 处理。
/// 返回 (最终 url, method, 请求头, 可选 body)。
fn build_search_request(
    source: &BookSource,
    key: &str,
    page: i32,
) -> (String, String, HashMap<String, String>, Option<String>) {
    let url_tpl = source.search_url.clone().unwrap_or_default();
    let (base, config) = if let Some(idx) = url_tpl.find(",{") {
        (&url_tpl[..idx], Some(&url_tpl[idx + 1..]))
    } else {
        (url_tpl.as_str(), None)
    };
    let base = expand_search_template(source, base, key, page);
    // 相对 searchUrl（如 `search.php?kw={{key}}`、`/book/so/...`）必须按书源根地址归并，
    // 否则 reqwest 收到相对 URL 直接失败，整源搜索恒为 0 条（错层小说网、三八小说实测）。
    let base = abs(&source.book_source_url, &base);
    // 逐源透传：JSON / 多行 / `@js:` header 都在这里按源求值
    let mut headers = req::source_headers(source, &base);
    let mut method = "GET".to_string();
    let mut body: Option<String> = None;
    if let Some(cfg_str) = config {
        if let Ok(v) = serde_json::from_str::<Value>(cfg_str) {
            if let Some(m) = v.get("method").and_then(|x| x.as_str()) {
                method = m.to_uppercase();
            }
            if let Some(b) = v.get("body").and_then(|x| x.as_str()) {
                body = Some(expand_search_template(source, b, key, page));
            }
            if let Some(h) = v.get("headers").and_then(|x| x.as_object()) {
                for (k, val) in h {
                    if let Some(s) = val.as_str() {
                        headers.insert(k.clone(), s.to_string());
                    }
                }
            }
        }
    }
    (base, method, headers, body)
}

/// 搜索结果里的类型标识（与 content_type 一致：text / image）
fn type_token(t: i64) -> &'static str {
    if t == 2 {
        "image"
    } else {
        "text"
    }
}

pub async fn search(source: &BookSource, key: &str, page: i32) -> anyhow::Result<Vec<SearchBook>> {
    search_book(source, key, page, None).await
}

/// 带 book 作用域的搜索：每条结果的 `@put`（如爱看的 `bid`）按 book_key 落库。
pub async fn search_book(
    source: &BookSource,
    key: &str,
    page: i32,
    store: Option<&BookStore>,
) -> anyhow::Result<Vec<SearchBook>> {
    let rule: &SearchRule = match &source.rule_search {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let (url, method, headers, body) = build_search_request(source, key, page);
    let html = req::fetch_for_source_limited(
        &url,
        &method,
        &headers,
        body.as_deref(),
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await?;
    let ar = make_ar(source);
    let items = eval_list_field(
        source,
        "book_list",
        &ar,
        &html,
        rule.book_list.as_deref().unwrap_or(""),
    );
    let mut books = Vec::new();
    for item in items {
        let book_url = eval_field(
            source,
            "book_url",
            &ar,
            &item,
            rule.book_url.as_deref().unwrap_or(""),
            true,
            &url,
        );
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(&url, &book_url);
        // 每条结果独立作用域：先把上一条的 @put 落库，再按本书 book_key 预载。
        if let Some(st) = store {
            let kb = book_key(source, &book_url);
            ar.begin_book_scope(kb.clone(), st.vars(&kb), st.cache(&kb));
        }
        let sb = SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            book_type: Some(type_token(source.book_source_type).to_string()),
            name: opt(source, "name", &ar, &item, &rule.name, &url),
            author: opt(source, "author", &ar, &item, &rule.author, &url),
            cover_url: opt(source, "cover_url", &ar, &item, &rule.cover_url, &url),
            intro: opt(source, "intro", &ar, &item, &rule.intro, &url),
            kind: opt(source, "kind", &ar, &item, &rule.kind, &url),
            latest_chapter_title: opt(source, "last_chapter", &ar, &item, &rule.last_chapter, &url),
            ..Default::default()
        };
        record_ar(&ar, store);
        books.push(sb);
    }
    Ok(books)
}

// ---------------- 发现 ----------------
pub async fn explore(source: &BookSource, url: &str) -> anyhow::Result<Vec<SearchBook>> {
    let rule: &ExploreRule = match &source.rule_explore {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let headers = req::source_headers(source, &source.book_source_url);
    let html = req::fetch_for_source_limited(
        url,
        "GET",
        &headers,
        None,
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await?;
    let ar = make_ar(source);
    let items = eval_list_field(
        source,
        "book_list",
        &ar,
        &html,
        rule.book_list.as_deref().unwrap_or(""),
    );
    let mut books = Vec::new();
    for item in items {
        let book_url = eval_field(
            source,
            "book_url",
            &ar,
            &item,
            rule.book_url.as_deref().unwrap_or(""),
            true,
            url,
        );
        if book_url.is_empty() {
            continue;
        }
        let book_url = abs(url, &book_url);
        books.push(SearchBook {
            book_url,
            origin: Some(source.book_source_url.clone()),
            origin_name: Some(source.book_source_name.clone()),
            book_type: Some(type_token(source.book_source_type).to_string()),
            name: opt(source, "name", &ar, &item, &rule.name, url),
            author: opt(source, "author", &ar, &item, &rule.author, url),
            cover_url: opt(source, "cover_url", &ar, &item, &rule.cover_url, url),
            intro: opt(source, "intro", &ar, &item, &rule.intro, url),
            kind: opt(source, "kind", &ar, &item, &rule.kind, url),
            latest_chapter_title: opt(source, "last_chapter", &ar, &item, &rule.last_chapter, url),
            ..Default::default()
        });
    }
    Ok(books)
}

// ---------------- 书籍详情 ----------------
pub async fn book_info(source: &BookSource, book: &mut Book) -> anyhow::Result<()> {
    book_info_book(source, book, None).await
}

/// 带 book 作用域的详情：预载同书变量（如搜索阶段写入的 `bid`），结束后回写。
pub async fn book_info_book(
    source: &BookSource,
    book: &mut Book,
    store: Option<&BookStore>,
) -> anyhow::Result<()> {
    // 无详情规则时不抓页面（与原行为一致，避免额外请求）
    if source.rule_book_info.is_none() {
        return Ok(());
    }
    let url = book.book_url.clone();
    let headers = req::source_headers(source, &url);
    let html = req::fetch_for_source_limited(
        &url,
        "GET",
        &headers,
        None,
        &source.book_source_url,
        req::source_timeout(source),
        req::source_limit(source),
    )
    .await?;
    parse_book_info_html(source, book, &html, store)
}

/// 对已抓取的详情页 HTML 应用详情规则（不发请求）。
/// 「按链接导入」复用它，避免为了兜底再抓一次同一页面。
pub fn parse_book_info_html(
    source: &BookSource,
    book: &mut Book,
    html: &str,
    store: Option<&BookStore>,
) -> anyhow::Result<()> {
    let rule: &BookInfoRule = match &source.rule_book_info {
        Some(r) => r,
        None => return Ok(()),
    };
    let url = book.book_url.clone();
    let ar = make_ar_book(source, store, &url);
    inject_book_meta(&ar, book);
    // `init` 的返回值在 Legado 里就是后续详情规则的输入内容（快看用它把
    // `__NUXT__` JSON 设为当前文档，name 等规则再在 JSON 上求值）。
    let mut content = html.to_string();
    if let Some(init) = &rule.init {
        match ar.get_string(html, init, false, &url) {
            Ok(init_out) => {
                let t = init_out.trim();
                if !t.is_empty() && t != "null" && t != "undefined" {
                    content = init_out;
                }
            }
            Err(e) => note_field_error(source, "init", &e),
        }
    }
    // Legado 语义：详情规则为空或未命中时**保留调用方带入的值**（搜索/书架已有
    // name/author），不能写成 null。错层小说网的 name/author/intro 规则就是空串。
    assign_opt(
        &mut book.name,
        source,
        "name",
        &ar,
        &content,
        &rule.name,
        &url,
    );
    assign_opt(
        &mut book.author,
        source,
        "author",
        &ar,
        &content,
        &rule.author,
        &url,
    );
    assign_opt(
        &mut book.intro,
        source,
        "intro",
        &ar,
        &content,
        &rule.intro,
        &url,
    );
    assign_opt(
        &mut book.cover_url,
        source,
        "cover_url",
        &ar,
        &content,
        &rule.cover_url,
        &url,
    );
    assign_opt(
        &mut book.kind,
        source,
        "kind",
        &ar,
        &content,
        &rule.kind,
        &url,
    );
    assign_opt(
        &mut book.latest_chapter_title,
        source,
        "last_chapter",
        &ar,
        &content,
        &rule.last_chapter,
        &url,
    );
    let toc_url = eval_field(
        source,
        "toc_url",
        &ar,
        &content,
        rule.toc_url.as_deref().unwrap_or(""),
        true,
        &url,
    );
    record_ar(&ar, store);
    // Legado 的 tocUrl 常直接写死绝对 URL（不是选择器），此时规则引擎会落空，需原样使用
    let literal = rule.toc_url.as_deref().map(str::trim).unwrap_or("");
    let toc_url = if toc_url.trim().is_empty()
        && (literal.starts_with("http://") || literal.starts_with("https://"))
    {
        literal.to_string()
    } else {
        toc_url
    };
    if !toc_url.trim().is_empty() {
        book.toc_url = Some(abs(&url, &toc_url));
    }
    Ok(())
}

/// 计算 `nextTocUrl` 的下页候选：
/// - 字面 URL（含 `{{page}}`/`${page}` 模板时按**下一页**页码代入）；
/// - 纯 CSS 规则（`@href`/`@value` 等）返回**所有**匹配值（多页目录一次性给出全部页码链接）；
/// - 其他复合规则（`&&`/`||`/`##`/`@js:`）退回单值求值。
fn next_toc_urls(ar: &AnalyzeRule, html: &str, rule: &str, base: &str, page: i32) -> Vec<String> {
    let raw = rule.trim();
    if raw.is_empty() {
        return vec![];
    }
    let next_page = (page + 1).to_string();
    let tpl = raw
        .replace("{{page}}", &next_page)
        .replace("${page}", &next_page);
    if tpl != raw
        && (tpl.starts_with("http://") || tpl.starts_with("https://") || tpl.starts_with('/'))
    {
        return vec![abs(base, &tpl)];
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return vec![raw.to_string()];
    }
    let mut vals = ar.get_string_list_all(html, raw, base);
    if vals.is_empty() {
        if let Ok(s) = ar.get_string(html, raw, true, base) {
            vals.push(s);
        }
    }
    vals.into_iter()
        .filter(|s| !blank_value(s))
        .map(|s| abs(base, &s))
        .collect()
}

// ---------------- 目录 ----------------
pub async fn chapter_list(source: &BookSource, book: &Book) -> anyhow::Result<Vec<BookChapter>> {
    chapter_list_book(source, book, None).await
}

/// 带 book 作用域的目录：预载同书变量与 cache（快看目录要从 info 阶段缓存的
/// `kkmh` 里 `java.setContent` 解析章节；爱看要用搜索阶段写的 `bid`）。
pub async fn chapter_list_book(
    source: &BookSource,
    book: &Book,
    store: Option<&BookStore>,
) -> anyhow::Result<Vec<BookChapter>> {
    let rule: &TocRule = match &source.rule_toc {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let headers = req::source_headers(source, &book.book_url);
    let mut url = book
        .toc_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| book.book_url.clone());
    // 目录地址本身也可以是分页模板（`.../toc_{{page}}.html`），首屏固定为第 1 页
    url = url.replace("{{page}}", "1").replace("${page}", "1");
    let ar = make_ar_book(source, store, &book.book_url);
    inject_book_meta(&ar, book);
    ar.book_toc_url.replace(url.clone());
    // Legado `TocRule.preUpdateJs`：刷新目录前执行；可改写 book.bookUrl / 调 refreshTocUrl。
    // 上游规则异常会中断目录更新；本服务按“解析不拖垮源”的原则降级为日志 + 台账后继续。
    if let Some(pre) = &rule.pre_update_js {
        if let Err(e) = ar.eval_script(pre, &book.book_url, &book.book_url) {
            note_field_error(source, "pre_update_js", &e);
        }
        record_ar(&ar, store);
        let refreshed_book_url = ar.book_url.borrow().clone();
        let book_url_changed =
            !refreshed_book_url.is_empty() && refreshed_book_url != book.book_url;
        if ar.refresh_toc.get() || book_url_changed {
            // `java.refreshTocUrl()` 的同步等价：重抓详情页并按 `toc_url` 规则重算目录地址，
            // 然后再走下面的目录抓取（否则会继续用旧的 book.tocUrl）。
            let target = if refreshed_book_url.is_empty() {
                book.book_url.clone()
            } else {
                refreshed_book_url
            };
            if let Some(new_toc) = refresh_toc_url_from_info(source, store, &target).await {
                url = new_toc.replace("{{page}}", "1").replace("${page}", "1");
            }
        }
    }
    let mut chaps = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: std::collections::VecDeque<(String, i32)> = std::collections::VecDeque::new();
    queue.push_back((url, 1));
    // 最多抓 10 页目录，与正文多页保护一致，避免书源死链把请求数放大
    while let Some((url, page)) = queue.pop_front() {
        if seen.len() >= 10 || seen.contains(&url) {
            continue;
        }
        seen.insert(url.clone());
        // 让规则里的 `${page}` / `{{page}}` 拿到当前页号
        ar.set_var("page", page.to_string());
        let html = req::fetch_for_source_limited(
            &url,
            "GET",
            &headers,
            None,
            &source.book_source_url,
            req::source_timeout(source),
            req::source_limit(source),
        )
        .await?;
        let rule_text = rule.chapter_list.as_deref().unwrap_or("");
        // Legado 目录规则前缀：`-` 表示倒序（如 `-class.list@tag.li`），`+` 表示保持原序
        // （`+@js:…` 的「正序」标记，见 WebBookChapterList 对 chapterList 的前缀剥离）；
        // 直接当选择器/JS 会抓空或语法报错。
        let (list_rule, reverse) = match rule_text.trim().strip_prefix('-') {
            Some(rest) => (rest.trim(), true),
            None => match rule_text.trim().strip_prefix('+') {
                Some(rest) => (rest.trim(), false),
                None => (rule_text, false),
            },
        };
        let mut items = eval_list_field(source, "chapter_list", &ar, &html, list_rule);
        if reverse {
            items.reverse();
        }
        for item in &items {
            // `@js` 返回 `[{text,href}]` 展开后的对象项：空项/缺字段项跳过并记一条日志
            let is_volume = flag(source, "is_volume", &ar, item, &rule.is_volume, &url)
                || item_json_volume(item);
            let title = opt(source, "chapter_name", &ar, item, &rule.chapter_name, &url);
            let raw_url = eval_field(
                source,
                "chapter_url",
                &ar,
                item,
                rule.chapter_url.as_deref().unwrap_or(""),
                true,
                &url,
            );
            if !is_volume
                && title.as_deref().map(blank_value).unwrap_or(true)
                && blank_value(&raw_url)
            {
                tracing::warn!(
                    source = source.book_source_url.as_str(),
                    "目录项缺少标题与链接，已跳过"
                );
                continue;
            }
            let idx = chaps.len() as i64;
            // 分卷行通常没有真实链接：用稳定合成 URL 作主键，避免覆盖正文章节或产生假 URL
            let url_final = if is_volume {
                format!("volume:{}#{idx}", book.book_url)
            } else if blank_value(&raw_url) {
                format!("{}{}", book.book_url, idx)
            } else {
                abs(&url, &raw_url)
            };
            let is_vip = flag(source, "is_vip", &ar, item, &rule.is_vip, &url);
            // isPay：付费章节标记，与 isVip 走同一条「书源字段映射 -> 布尔」路径
            let is_pay = flag(source, "is_pay", &ar, item, &rule.is_pay, &url);
            chaps.push(BookChapter {
                url: url_final,
                title,
                is_volume,
                is_vip,
                is_pay,
                update_time: opt(source, "update_time", &ar, item, &rule.update_time, &url),
                book_url: Some(book.book_url.clone()),
                base_url: Some(url.clone()),
                index: idx,
                ..Default::default()
            });
        }
        let nexts = next_toc_urls(
            &ar,
            &html,
            rule.next_toc_url.as_deref().unwrap_or(""),
            &url,
            page,
        );
        for next in nexts {
            if next != url && !seen.contains(&next) {
                queue.push_back((next, page + 1));
            }
        }
    }
    record_ar(&ar, store);
    Ok(chaps)
}

// ---------------- 正文 ----------------
/// 抓取正文所有分页：返回 `(当前页 URL, 原始 HTML)`。
/// 正文与图片章节共用同一套 nextContentUrl 多页逻辑，规则各自只应用一次。
async fn fetch_html_pages(
    source: &BookSource,
    chapter: &BookChapter,
    rule: &ContentRule,
) -> anyhow::Result<Vec<(String, String)>> {
    let headers = req::source_headers(source, &chapter.url);
    let mut url = chapter.url.clone();
    let mut pages: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut guard = 0;
    loop {
        if guard >= 10 || seen.contains(&url) {
            break;
        }
        guard += 1;
        seen.insert(url.clone());
        let html = req::fetch_for_source_limited(
            &url,
            "GET",
            &headers,
            None,
            &source.book_source_url,
            req::source_timeout(source),
            req::source_limit(source),
        )
        .await?;
        let ar = make_ar(source);
        let next = eval_field(
            source,
            "next_content_url",
            &ar,
            &html,
            rule.next_content_url.as_deref().unwrap_or(""),
            true,
            &url,
        );
        pages.push((url.clone(), html));
        if blank_value(&next) {
            break;
        }
        let next_abs = abs(&url, &next);
        if next_abs == url {
            break;
        }
        url = next_abs;
    }
    Ok(pages)
}

pub async fn chapter_content(
    source: &BookSource,
    chapter: &BookChapter,
) -> anyhow::Result<BookContent> {
    chapter_content_book(source, chapter, None).await
}

/// 正文规则以 `@textNodes` 结尾时，生成等价的 `@html` 回退规则。
/// `@html` 保留 <p>/<br> 等分段，前端能按段渲染；仅用于「抽取结果为空」的兜底。
fn content_text_fallback(rule: &str) -> Option<String> {
    let trimmed = rule.trim_end();
    let head = trimmed.strip_suffix("@textNodes")?;
    if head.is_empty() {
        return None;
    }
    Some(format!("{head}@html"))
}

/// 带 book 作用域的正文（正文规则里的 `@get` 变量按同书预载）。
pub async fn chapter_content_book(
    source: &BookSource,
    chapter: &BookChapter,
    store: Option<&BookStore>,
) -> anyhow::Result<BookContent> {
    let rule: &ContentRule = match &source.rule_content {
        Some(r) => r,
        None => return Ok(BookContent::default()),
    };
    let pages = fetch_html_pages(source, chapter, rule).await?;
    let book_url = chapter.book_url.clone().unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for (page_url, html) in &pages {
        let ar = make_ar_book(source, store, &book_url);
        let content_rule = rule.content.as_deref().unwrap_or("");
        let mut part = eval_field(source, "content", &ar, html, content_rule, false, page_url);
        // `@textNodes` 只取直接子文本节点；部分站点把正文包在 <p> 里，直接文本节点只有空白。
        // 结果为空时用同一选择器的 `@html` 回退一次，避免整章空白（不改变普通规则的语义）。
        if part.trim().is_empty() {
            if let Some(fallback) = content_text_fallback(content_rule) {
                let retried = eval_field(
                    source,
                    "content_fallback",
                    &ar,
                    html,
                    &fallback,
                    false,
                    page_url,
                );
                if !retried.trim().is_empty() {
                    tracing::debug!(
                        rule = content_rule,
                        fallback = fallback.as_str(),
                        "content @textNodes empty, fell back to @html"
                    );
                    part = retried;
                }
            }
        }
        record_ar(&ar, store);
        parts.push(part);
    }
    // 书源自带的净化规则：先 sourceRegex（源清洗）再 replaceRegex（可读性替换）
    let raw = parts.join("\n");
    let cleaned_source = apply_legado_replacements(&raw, &rule.source_regex);
    let cleaned = apply_legado_replacements(&cleaned_source, &rule.replace_regex);
    Ok(BookContent {
        book_url: chapter.book_url.clone().unwrap_or_default(),
        url: chapter.url.clone(),
        title: chapter.title.clone(),
        content: Some(cleaned),
        chapter_index: chapter.index,
        content_type: "text".into(),
        ..Default::default()
    })
}

/// 单章最多抽取的图片数（防止异常书源把一次请求放大成数百张）
pub const MAX_CHAPTER_IMAGES: usize = 300;

/// 图片章节抽取结果
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChapterImages {
    pub images: Vec<BookImage>,
    pub truncated: bool,
}

/// `img` 标签与属性解析用的静态正则，避免每张图重复编译。
fn img_tag_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap())
}

fn attr_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#,
        )
        .unwrap()
    })
}

fn parse_attrs(tag: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for c in attr_re().captures_iter(tag) {
        let name = c
            .get(1)
            .map(|m| m.as_str().to_ascii_lowercase())
            .unwrap_or_default();
        let val = c
            .get(2)
            .or_else(|| c.get(3))
            .or_else(|| c.get(4))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        if !name.is_empty() {
            map.insert(name, val);
        }
    }
    map
}

fn parse_dim(v: Option<&String>) -> Option<u32> {
    let s = v?.trim().trim_end_matches("px").trim();
    s.parse::<u32>().ok()
}

/// 是否 1px（或更小）的占位图：宽/高属性或内联样式里出现 1px。
fn is_placeholder_size(attrs: &HashMap<String, String>) -> bool {
    let w = parse_dim(attrs.get("width"));
    let h = parse_dim(attrs.get("height"));
    if w.map(|n| n <= 1).unwrap_or(false) || h.map(|n| n <= 1).unwrap_or(false) {
        return true;
    }
    if let Some(style) = attrs.get("style") {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            regex::Regex::new(r"(?i)(?:^|;)\s*(?:width|height)\s*:\s*([0-9.]+)\s*px").unwrap()
        });
        for c in re.captures_iter(style) {
            if c.get(1)
                .and_then(|m| m.as_str().parse::<f32>().ok())
                .map(|n| n <= 1.0)
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// 一张图片的“原始”信息（尚未去重/编号）
#[derive(Debug, Clone)]
struct RawImage {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// 归一图片地址：部分书源（如 1kkk/酸奶漫画）把请求头 JSON 直接拼在 `src` 里，
/// 产出 `url,{"headers":...}`；HTML 解析只能在第一个引号处截断，会留下 `,{` 尾巴。
/// 这里按 `,{` 切掉后缀，避免代理把无效后缀带到源站导致 404。
fn clean_image_url(raw: &str) -> String {
    let u = raw.trim();
    let head = match u.find(",{") {
        Some(pos) => u[..pos].trim(),
        None => u,
    };
    head.trim_end_matches(',').trim().to_string()
}

fn push_raw(out: &mut Vec<RawImage>, url: &str, width: Option<u32>, height: Option<u32>) {
    let u = clean_image_url(url);
    if u.is_empty() || u.starts_with("data:") {
        return;
    }
    out.push(RawImage {
        url: u,
        width,
        height,
    });
}

fn is_usable_src(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && !t.starts_with("data:")
        && !t.starts_with("javascript:")
        && !t.starts_with('#')
        && !t.eq_ignore_ascii_case("about:blank")
}

/// 选图片地址：懒加载属性优先（src 常是 1px 占位图），再退回 src / srcset。
fn pick_img_src(attrs: &HashMap<String, String>) -> &str {
    for key in [
        "data-src",
        "data-original",
        "data-lazy-src",
        "data-original-src",
        "data-echo",
        "data-url",
    ] {
        if let Some(v) = attrs.get(key).filter(|s| is_usable_src(s)) {
            return v;
        }
    }
    if let Some(v) = attrs.get("src").filter(|s| is_usable_src(s)) {
        return v;
    }
    for key in ["srcset", "data-srcset"] {
        if let Some(v) = attrs.get(key) {
            let first = v.split(',').next().unwrap_or("").trim();
            let url = first.split_whitespace().next().unwrap_or("");
            if is_usable_src(url) {
                return url;
            }
        }
    }
    ""
}

/// 从一段 HTML 里按页面顺序提取 `<img src>`（含懒加载 `data-src`/`data-original` 兜底）。
fn images_from_html(html: &str) -> Vec<RawImage> {
    let mut out = Vec::new();
    for tag in img_tag_re().find_iter(html) {
        let attrs = parse_attrs(tag.as_str());
        let src = pick_img_src(&attrs);
        if is_placeholder_size(&attrs) {
            continue;
        }
        push_raw(
            &mut out,
            src,
            parse_dim(attrs.get("width")),
            parse_dim(attrs.get("height")),
        );
    }
    out
}

/// JS 数组/JSON 结果 -> 图片项；支持 `["url"]`、`[{url,width,height}]`、
/// `{images:[...]}` 以及 `{url:...}` 单对象。
fn images_from_json(text: &str) -> Option<Vec<RawImage>> {
    let t = text.trim();
    if !(t.starts_with('[') || t.starts_with('{')) {
        return None;
    }
    let v: Value = serde_json::from_str(t).ok()?;
    let mut out = Vec::new();
    collect_json_images(&v, &mut out);
    Some(out)
}

fn collect_json_images(v: &Value, out: &mut Vec<RawImage>) {
    match v {
        Value::String(s) => push_raw(out, s, None, None),
        Value::Array(items) => {
            for it in items {
                collect_json_images(it, out);
            }
        }
        Value::Object(obj) => {
            for key in ["images", "list", "data"] {
                if let Some(inner) = obj.get(key) {
                    collect_json_images(inner, out);
                    return;
                }
            }
            let url = obj
                .get("url")
                .or_else(|| obj.get("src"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let width = obj
                .get("width")
                .and_then(|x| {
                    x.as_u64()
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .map(|n| n as u32);
            let height = obj
                .get("height")
                .and_then(|x| {
                    x.as_u64()
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .map(|n| n as u32);
            push_raw(out, url, width, height);
        }
        _ => {}
    }
}

/// 单条规则结果 -> 图片项。兼容 HTML 片段、纯 URL、JSON 数组三种形态。
fn images_from_value(value: &str) -> Vec<RawImage> {
    let t = value.trim();
    if t.is_empty() {
        return vec![];
    }
    if let Some(v) = images_from_json(t) {
        return v;
    }
    let from_html = images_from_html(t);
    if !from_html.is_empty() {
        return from_html;
    }
    // 纯 URL（无标签、无空白）：content 规则常直接写 `@css:img@src` 或 `@js:` 返回单链
    if !t.contains('<') && !t.chars().any(char::is_whitespace) {
        let mut out = Vec::new();
        push_raw(&mut out, t, None, None);
        return out;
    }
    from_html
}

/// 图片章节正文：所有 `<img src>` 按页面顺序去重、过滤占位图，`index` 从 1 开始。
pub async fn chapter_images(
    source: &BookSource,
    chapter: &BookChapter,
) -> anyhow::Result<ChapterImages> {
    chapter_images_book(source, chapter, None).await
}

/// 带 book 作用域的图片章节（正文规则里的变量/cache 按同书预载）。
pub async fn chapter_images_book(
    source: &BookSource,
    chapter: &BookChapter,
    store: Option<&BookStore>,
) -> anyhow::Result<ChapterImages> {
    let rule: &ContentRule = match &source.rule_content {
        Some(r) => r,
        None => return Ok(ChapterImages::default()),
    };
    // 没有 content 规则时不要退化成“整页图片”（会混入广告/图标）
    let content_rule = rule.content.as_deref().unwrap_or("").trim();
    if content_rule.is_empty() {
        return Ok(ChapterImages::default());
    }
    let pages = fetch_html_pages(source, chapter, rule).await?;
    let book_url = chapter.book_url.clone().unwrap_or_default();
    let mut per_page: Vec<(String, Vec<RawImage>)> = Vec::new();
    for (page_url, html) in &pages {
        // 规则本身可能直接产出多条（CSS 列表 / JS 数组），逐条归一
        let ar = make_ar_book(source, store, &book_url);
        let values = ar.get_string_list_all(html, content_rule, page_url);
        record_ar(&ar, store);

        let mut raws: Vec<RawImage> = if values.is_empty() {
            Vec::new()
        } else {
            values.iter().flat_map(|v| images_from_value(v)).collect()
        };
        tracing::debug!(page_url = %page_url, values = values.len(), raws = raws.len(), "image page extracted");
        // 兼容 `class.ComicPics@img@html`：`@html` 在图片节点上是空的内层 HTML，
        // 换成 `@all`（outer HTML）再解析 `<img src>`；只在原规则零结果时兜底。
        if raws.is_empty() {
            if let Some(head) = content_rule.strip_suffix("@html") {
                let alt_rule = format!("{}@all", head.trim());
                let alt_values = ar.get_string_list_all(html, &alt_rule, page_url);
                raws = alt_values
                    .iter()
                    .flat_map(|v| images_from_value(v))
                    .collect();
            }
        }
        per_page.push((page_url.clone(), raws));
    }
    Ok(build_images(&per_page, MAX_CHAPTER_IMAGES))
}

/// 把“页面 URL + 原始图片”归一为最终列表：相对地址补全、去重、编号从 1 开始、超限截断。
fn build_images(pages: &[(String, Vec<RawImage>)], limit: usize) -> ChapterImages {
    let mut images: Vec<BookImage> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut truncated = false;
    for (page_url, raws) in pages {
        for raw in raws {
            let absolute = abs(page_url, &raw.url);
            if !seen.insert(absolute.clone()) {
                continue;
            }
            if images.len() >= limit {
                truncated = true;
                break;
            }
            let index = images.len() as i64 + 1;
            images.push(BookImage {
                index,
                url: absolute,
                width: raw.width,
                height: raw.height,
                bytes: None,
                priority: Some(if index <= 3 { "high" } else { "normal" }.to_string()),
            });
        }
        if truncated {
            break;
        }
    }
    ChapterImages { images, truncated }
}

/// 单个书源的失败信息（聚合搜索时单源失败不影响整体）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceFailure {
    pub source: String,
    pub source_name: String,
    pub error: String,
}

/// 聚合搜索结果：书列表 + 各源失败明细
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub books: Vec<SearchBook>,
    pub errors: Vec<SourceFailure>,
}

/// 发现分类（由 `exploreUrl` 展开）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExploreCategory {
    pub title: String,
    pub url: String,
}

/// 单次搜索会话的稳定排序状态（仅 `/api/search/result` 使用）：
/// - `order`：去重键 → 首次入池时确定的 `(bucket, seq)`，之后不再变化；
/// - `next_seq`：单调递增的入池序号；
/// - `snapshot`：一旦用户开始翻页（cursor / page>1），冻结整份有序结果，
///   后续页都从同一份快照读取，保证同一会话翻页不重不漏。
#[derive(Default)]
pub(crate) struct RankFreeze {
    pub order: HashMap<String, (u8, u64)>,
    pub next_seq: u64,
    pub snapshot: Option<Vec<SearchBook>>,
}

/// 去重键：归一化书名 + 归一化作者；书名为空时退化为 book_url。
pub(crate) fn dedupe_key(b: &SearchBook) -> String {
    let name = b.name.as_deref().unwrap_or("").trim().to_lowercase();
    let author = b.author.as_deref().unwrap_or("").trim().to_lowercase();
    if name.is_empty() {
        b.book_url.clone()
    } else {
        format!("{name}|{author}")
    }
}

/// 一条去重后的候选（内部结构，排序前构造）。
struct RankedBook {
    book: SearchBook,
    relevance: u8,
    weight: i64,
    custom_order: i64,
    /// 命中该书的源：`(url, 展示名)`，按 url 去重。
    origins: Vec<(String, String)>,
    bucket: u8,
    seq: u64,
}

impl RankedBook {
    fn score(&self) -> u8 {
        crate::search_rank::combined_score(self.relevance, self.weight, self.origins.len())
    }
}

/// 聚合去重 + 排序。
///
/// - `query` 非空（搜索结果）：先丢弃与查询词完全无关的条目，再按相关性降序排序——
///   书名精确/前缀/包含/近似命中优先，只命中查询词片段（如「百年」之于「百年孤独」）
///   固定为 `SCORE_PARTIAL`，严格排在所有书名命中之后。同分再按源权重降序、
///   `customOrder` 升序、`book_url` 升序。源权重只作为同档内的小幅加分（≤3），
///   不会让低相关的高权重源压过完整命中。
///   多源同书合并后按真实来源数计算共识加分，并在结果里带出 `origins`。
///   强命中（exact/prefix/contains/approx）≥ `STRONG_MIN_RESULTS` 时，
///   只命中片段的 partial 被折叠掉，避免首屏只剩“可能相关”。
/// - `query` 为空（发现/探索页）：保持旧的"纯源权重"排序，不做相关性过滤。
///
/// `freeze` 为 `Some` 时用于 job 分页：条目首次入池即固定 `(bucket, seq)`，
/// 后到结果不会挤动已翻过的页。
pub(crate) fn dedupe_and_sort_with(
    books: Vec<SearchBook>,
    sources: &[BookSource],
    query: &str,
    freeze: Option<&mut RankFreeze>,
) -> Vec<SearchBook> {
    let order: HashMap<&str, (i64, i64)> = sources
        .iter()
        .map(|s| (s.book_source_url.as_str(), (s.weight, s.custom_order)))
        .collect();
    let weight_order = |b: &SearchBook| -> (i64, i64) {
        b.origin
            .as_deref()
            .and_then(|o| order.get(o))
            .copied()
            .unwrap_or((0, 0))
    };
    let q = crate::search_rank::normalize_title(query);

    // 先去重：同书多源合并成一条，累计真实来源（共识加分依据）。
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut ranked: Vec<RankedBook> = Vec::new();
    for b in books {
        let rel = if q.is_empty() {
            0
        } else {
            crate::search_rank::search_relevance(
                &q,
                b.name.as_deref().unwrap_or(""),
                b.author.as_deref().unwrap_or(""),
            )
        };
        if !q.is_empty() && rel == 0 {
            continue;
        }
        let (w, o) = weight_order(&b);
        let origin_url = b.origin.clone().unwrap_or_default();
        let origin_name = b
            .origin_name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| origin_url.clone());
        let key = dedupe_key(&b);
        match index.get(&key) {
            Some(&i) => {
                let e = &mut ranked[i];
                if !origin_url.is_empty() && !e.origins.iter().any(|(u, _)| u == &origin_url) {
                    e.origins.push((origin_url, origin_name));
                }
                // 同书多源：保留更相关（同相关时权重更高）的那一行做展示。
                if rel > e.relevance || (rel == e.relevance && w > e.weight) {
                    let origins = std::mem::take(&mut e.origins);
                    *e = RankedBook {
                        book: b,
                        relevance: rel,
                        weight: w,
                        custom_order: o,
                        origins,
                        bucket: 0,
                        seq: 0,
                    };
                }
            }
            None => {
                index.insert(key, ranked.len());
                let origins = if origin_url.is_empty() {
                    Vec::new()
                } else {
                    vec![(origin_url, origin_name)]
                };
                ranked.push(RankedBook {
                    book: b,
                    relevance: rel,
                    weight: w,
                    custom_order: o,
                    origins,
                    bucket: 0,
                    seq: 0,
                });
            }
        }
    }

    let mut frozen = false;
    if let Some(f) = freeze {
        frozen = true;
        for e in ranked.iter_mut() {
            let key = dedupe_key(&e.book);
            let (bucket, seq) = match f.order.get(&key).copied() {
                Some(v) => v,
                None => {
                    let seq = f.next_seq;
                    f.next_seq += 1;
                    let b = crate::search_rank::bucket(e.score());
                    f.order.insert(key, (b, seq));
                    (b, seq)
                }
            };
            e.bucket = bucket;
            e.seq = seq;
            e.book.rank_bucket = Some(bucket);
            e.book.rank_seq = Some(seq);
        }
    }

    // 排序：job 分页用固定 (bucket, seq)，其余用分数 + 权重 + 顺序 + url。
    if frozen {
        ranked.sort_by(|a, b| b.bucket.cmp(&a.bucket).then(a.seq.cmp(&b.seq)));
    } else if !q.is_empty() {
        ranked.sort_by(|a, b| {
            b.score()
                .cmp(&a.score())
                .then_with(|| b.weight.cmp(&a.weight))
                .then_with(|| a.custom_order.cmp(&b.custom_order))
                .then_with(|| a.book.book_url.cmp(&b.book.book_url))
        });
    } else {
        ranked.sort_by(|a, b| {
            b.weight
                .cmp(&a.weight)
                .then_with(|| a.custom_order.cmp(&b.custom_order))
                .then_with(|| a.book.book_url.cmp(&b.book.book_url))
        });
    }

    // 相关性档位 + origins 回填。
    for e in ranked.iter_mut() {
        if !q.is_empty() {
            e.book.relevance = Some(crate::search_rank::relevance_class(e.relevance).to_string());
        }
        e.book.origins = e
            .origins
            .iter()
            .map(|(_, name)| name.clone())
            .filter(|n| !n.is_empty())
            .collect();
    }

    // 有条件折叠：强命中足够多时，partial 不再进入首屏（整体丢弃）。
    // 空查询（发现页）不过滤、不折叠。
    if !q.is_empty() {
        let strong = ranked
            .iter()
            .filter(|e| e.relevance >= crate::search_rank::SCORE_APPROX_MIN)
            .count();
        if crate::search_rank::should_fold_partials(strong) {
            ranked.retain(|e| e.relevance != crate::search_rank::SCORE_PARTIAL);
        }
    }

    ranked.into_iter().map(|e| e.book).collect()
}

pub(crate) fn dedupe_and_sort(
    books: Vec<SearchBook>,
    sources: &[BookSource],
    query: &str,
) -> Vec<SearchBook> {
    dedupe_and_sort_with(books, sources, query, None)
}

/// 多源搜索：并发请求所有启用书源；单源失败只记录错误，不影响整体。
/// 每个书源有独立的搜索预算（默认 10s），超时只丢该源，避免一个慢站拖住整页。
pub async fn search_all_with_errors(sources: &[BookSource], key: &str, page: i32) -> SearchOutcome {
    search_all_with_errors_budgeted(sources, key, page, req::search_budget()).await
}

pub async fn search_all_with_errors_budgeted(
    sources: &[BookSource],
    key: &str,
    page: i32,
    budget: std::time::Duration,
) -> SearchOutcome {
    // 旧调用方语义：budget 同时是整批与单源预算，走 curated 并发池。
    search_all_budgeted_scoped(
        sources,
        key,
        page,
        budget,
        budget,
        crate::req::SearchScope::Curated,
    )
    .await
}

/// 按 scope 使用与 `/api/search/start` 相同的单源/整批预算与并发池：
/// `curated` 5s/6s、`all` 6s/10s。legacy `/api/search` 走这里，
/// 保证同一关键词在同一源集合下拿到的结果集合与两段式一致。
pub async fn search_all_with_errors_scoped(
    sources: &[BookSource],
    key: &str,
    page: i32,
    scope: crate::req::SearchScope,
) -> SearchOutcome {
    search_all_budgeted_scoped(
        sources,
        key,
        page,
        crate::req::search_budget_for(scope),
        crate::req::search_total_budget(scope),
        scope,
    )
    .await
}

async fn search_all_budgeted_scoped(
    sources: &[BookSource],
    key: &str,
    page: i32,
    per_source: std::time::Duration,
    total: std::time::Duration,
    scope: crate::req::SearchScope,
) -> SearchOutcome {
    let deadline = tokio::time::Instant::now() + total;
    let mut tasks = Vec::new();
    for s in sources {
        if !crate::entities::is_searchable_source(s) {
            continue;
        }
        let src = s.clone();
        let comic = crate::req::is_comic_source_type(src.book_source_type);
        let fut = async move {
            // 先排队拿源级并发槽（不计入单源预算），再开始计时。
            let _slot = crate::req::acquire_search_slot(comic, deadline, scope).await?;
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                anyhow::bail!("全局搜索预算耗尽（排队未及执行）");
            }
            match tokio::time::timeout(remain.min(per_source), search(&src, key, page)).await {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!(
                    "搜索超时（单源 {}s 预算）",
                    per_source.as_secs()
                )),
            }
        };
        tasks.push((s.clone(), fut));
    }
    let results = futures::future::join_all(
        tasks
            .into_iter()
            .map(|(src, fut)| async move { (src, fut.await) }),
    )
    .await;
    let mut outcome = SearchOutcome::default();
    for (src, r) in results {
        match r {
            Ok(mut books) => outcome.books.append(&mut books),
            Err(e) => outcome.errors.push(SourceFailure {
                source: src.book_source_url.clone(),
                source_name: src.book_source_name.clone(),
                error: e.to_string(),
            }),
        }
    }
    outcome.books = dedupe_and_sort(outcome.books, sources, key);
    outcome
}

pub async fn search_all(sources: &[BookSource], key: &str, page: i32) -> Vec<SearchBook> {
    search_all_with_errors(sources, key, page).await.books
}

/// 展开书源的 `exploreUrl`：每行 `分类名::url`（无分类名时用书源名），
/// 支持 `{{page}}` 模板；空行/注释行忽略。
/// `exploreUrl` 为 `@js:` / `<js>` 时交给规则引擎执行（受 JS_ENABLED 与耗时预算约束），
/// 结果既可为 `标题::url` 多行文本，也可为 `[{title,url}]` JSON 数组。
pub fn explore_categories(source: &BookSource, page: i32) -> anyhow::Result<Vec<ExploreCategory>> {
    let raw = match source.explore_url.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(vec![]),
    };
    let raw = if raw.starts_with("@js:") || raw.contains("<js>") {
        let ar = make_ar(source);
        ar.get_string("", &raw, false, "")
            .map_err(|e| anyhow::anyhow!("exploreUrl JS 执行失败: {e}"))?
    } else {
        raw
    };
    // JS 结果可能是 JSON 数组
    if raw.trim_start().starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(raw.trim()) {
            let mut out = Vec::new();
            for it in items {
                let title = it
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&source.book_source_name)
                    .to_string();
                let url = match it.get("url").and_then(|v| v.as_str()) {
                    Some(u) if !u.trim().is_empty() => u.trim().to_string(),
                    _ => continue,
                };
                out.push(ExploreCategory { title, url });
            }
            return Ok(out);
        }
    }
    let mut out: Vec<ExploreCategory> = Vec::new();
    let mut seen = HashSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (title, url) = match line.split_once("::") {
            Some((t, u)) => (t.trim().to_string(), u.trim().to_string()),
            None => (source.book_source_name.clone(), line.to_string()),
        };
        if url.is_empty() {
            continue;
        }
        let url = url
            .replace("{{page}}", &page.to_string())
            .replace("{{key}}", "");
        if !seen.insert(url.clone()) {
            continue;
        }
        out.push(ExploreCategory {
            title: if title.is_empty() {
                source.book_source_name.clone()
            } else {
                title
            },
            url,
        });
    }
    Ok(out)
}

/// 多源发现：并发请求所有启用且开启发现的书源（按同一个分类 URL）
pub async fn explore_all(sources: &[BookSource], url: &str) -> Vec<SearchBook> {
    let mut tasks = Vec::new();
    for s in sources {
        if !s.enabled
            || !s.enabled_explore
            || !crate::entities::is_supported_source_type(s.book_source_type)
            || s.rule_explore.is_none()
        {
            continue;
        }
        tasks.push(explore(s, url));
    }
    let results = futures::future::join_all(tasks).await;
    let mut out = Vec::new();
    for mut books in results.into_iter().flatten() {
        out.append(&mut books);
    }
    dedupe_and_sort(out, sources, "")
}

/// 按 Legado 约定执行 `正则##替换` 形式的净化规则（多行可多条）。
/// 兼容 `/pat/repl/` 单条形式；无效正则跳过，不影响正文返回。
fn apply_legado_replacements(input: &str, rules: &Option<String>) -> String {
    let rules = match rules {
        Some(r) if !r.trim().is_empty() => r,
        _ => return input.to_string(),
    };
    let mut out = input.to_string();
    for line in rules.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pat, repl) = if let Some((p, r)) = line.split_once("##") {
            (p.trim(), r)
        } else if line.starts_with('/') && line.len() > 2 && line.ends_with('/') {
            let inner = &line[1..line.len() - 1];
            match inner.split_once('/') {
                Some((p, r)) => (p, r),
                None => (inner, ""),
            }
        } else {
            (line, "")
        };
        if pat.is_empty() {
            continue;
        }
        if let Ok(re) = regex::Regex::new(pat) {
            out = re.replace_all(&out, repl).to_string();
        }
    }
    out
}

#[allow(dead_code)]
fn _unused(_: HashMap<String, String>) {}

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造书源/结果，便于对照默认值。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_strips_legado_url_options() {
        // 开心漫画章节链接形如 `https://…/chapter/xxx,{"webView": true}`；带选项请求会 404
        assert_eq!(
            abs(
                "https://www.kaixinman.com/",
                "/chapter/xxx,{\"webView\": true}"
            ),
            "https://www.kaixinman.com/chapter/xxx"
        );
        // 相对链接 join 后选项会被编码成 `,%7B…`，同样要剥掉
        assert_eq!(
            strip_url_options("https://www.kaixinman.com/chapter/xxx,%7B%22webView%22:%20true%7D"),
            "https://www.kaixinman.com/chapter/xxx"
        );
        // 不是 JSON 选项的逗号尾巴保持原样
        assert_eq!(
            abs("https://www.kaixinman.com/", "/a,b.html"),
            "https://www.kaixinman.com/a,b.html"
        );
    }

    #[test]
    fn chapter_url_append_suffix_is_stripped_as_option() {
        let src = BookSource::default();
        let ar = make_ar(&src);
        let html = r#"<a href="/chapter/x">第1话</a>"#;
        let raw = ar
            .get_string(
                html,
                "href##$##,{\"webView\": true}",
                true,
                "https://www.kaixinman.com/comic/1",
            )
            .unwrap();
        assert_eq!(raw, "/chapter/x,{\"webView\": true}");
        assert_eq!(
            abs("https://www.kaixinman.com/comic/1/", &raw),
            "https://www.kaixinman.com/chapter/x"
        );
    }

    #[test]
    fn content_text_nodes_fallback_recovers_p_wrapped_text() {
        // 真实形态（biquge7《百年孤独》）：`.text` 里只有 <p>，直接文本节点全是空白，
        // `@textNodes` 得到空串；回退到同一选择器的 `@html` 才能拿到正文。
        let src = BookSource::default();
        let ar = make_ar(&src);
        let html = r#"<div class="text">
            <p>第一段正文。</p>
            <p>第二段正文。</p>
        </div>"#;
        let rule = "class.text@textNodes";
        assert!(ar
            .get_string(html, rule, false, "https://x/")
            .unwrap()
            .trim()
            .is_empty());
        let fallback = content_text_fallback(rule).expect("should produce fallback");
        assert_eq!(fallback, "class.text@html");
        let out = ar.get_string(html, &fallback, false, "https://x/").unwrap();
        assert!(out.contains("第一段正文"), "out={out}");
        assert!(out.contains("第二段正文"), "out={out}");
        assert!(
            out.contains("<p>"),
            "should keep paragraph tags for reader: {out}"
        );
    }

    #[test]
    fn content_text_nodes_fallback_is_not_generated_for_other_rules() {
        assert!(content_text_fallback("class.text@text").is_none());
        assert!(content_text_fallback("class.text@textNodes@trim").is_none());
        assert!(content_text_fallback("@textNodes").is_none());
    }

    #[test]
    fn comic_img_html_falls_back_to_outer_html() {
        // `class.X@img@html` 在图片节点上得到空的内层 HTML；`@all` 才是 outer HTML。
        // 真实书源（如快看）用 `@html` 取 <img>，因此零结果时必须回退到 `@all`。
        let src = BookSource::default();
        let ar = make_ar(&src);
        let html = r#"<div class="ComicPics comic-pics"><img src="https://cdn/a.jpg"><img src="https://cdn/b.jpg"></div>"#;
        let inner = ar.get_string_list_all(html, "class.ComicPics@img@html", "https://x/");
        assert_eq!(inner, vec!["", ""], "内层 HTML 为空，不能直接当图片列表");
        let outer = ar.get_string_list_all(html, "class.ComicPics@img@all", "https://x/");
        assert_eq!(outer.len(), 2);
        assert!(outer[0].contains("a.jpg"));
        assert!(outer[1].contains("b.jpg"));
    }

    // B1：相对 searchUrl 必须按 bookSourceUrl 归并（真实源最小形态）
    #[test]
    fn relative_search_url_is_joined_with_source_url() {
        let mut s = BookSource::default();
        // 形态一：根路径相对的 `search.php?kw=`（三八小说）
        s.book_source_url = "https://www.38xs.com".into();
        s.search_url = Some("search.php?kw={{key}}".into());
        let (url, method, _, _) = build_search_request(&s, "abc", 1);
        assert_eq!(url, "https://www.38xs.com/search.php?kw=abc");
        assert_eq!(method, "GET");

        // 形态二：斜杠开头的模板（错层小说网）
        s.book_source_url = "https://m.cuoceng.org/".into();
        s.search_url = Some("/book/so/{{key}}/{{page}}.html".into());
        let (url2, _, _, _) = build_search_request(&s, "abc", 1);
        assert_eq!(url2, "https://m.cuoceng.org/book/so/abc/1.html");
    }

    // 绝对 searchUrl（带 POST 配置）不受归并影响
    #[test]
    fn absolute_search_url_with_post_config_is_kept() {
        let mut s = BookSource::default();
        s.book_source_url = "https://www.dingdian888.com".into();
        s.search_url = Some(
            r#"https://www.dingdian888.com/s.php,{"method":"POST","body":"s={{key}}&submit="}"#
                .into(),
        );
        let (url, method, _, body) = build_search_request(&s, "完美世界", 1);
        assert_eq!(url, "https://www.dingdian888.com/s.php");
        assert_eq!(method, "POST");
        assert_eq!(body.as_deref(), Some("s=完美世界&submit="));
    }

    // nextTocUrl：一次返回多个页码链接（多页目录链式抓取）
    #[test]
    fn next_toc_urls_supports_multiple_links() {
        let ar = AnalyzeRule::new();
        let html =
            r#"<div class="pages"><a href="/toc/2.html">2</a><a href="/toc/3.html">3</a></div>"#;
        let v = next_toc_urls(
            &ar,
            html,
            "@css:.pages a@href",
            "https://x.com/toc/1.html",
            1,
        );
        assert_eq!(
            v,
            vec![
                "https://x.com/toc/2.html".to_string(),
                "https://x.com/toc/3.html".to_string()
            ]
        );
    }

    // nextTocUrl：字面 URL 模板里的 {{page}} 取“下一页”页码，而不是当前页
    #[test]
    fn next_toc_urls_expands_page_template() {
        let ar = AnalyzeRule::new();
        let v = next_toc_urls(
            &ar,
            "",
            "https://x.com/toc/list_{{page}}.html",
            "https://x.com",
            1,
        );
        assert_eq!(v, vec!["https://x.com/toc/list_2.html".to_string()]);
        // 相对路径模板同样按当前页归并
        let v2 = next_toc_urls(
            &ar,
            "",
            "/toc/list_${page}.html",
            "https://x.com/toc/1.html",
            2,
        );
        assert_eq!(v2, vec!["https://x.com/toc/list_3.html".to_string()]);
    }

    // 目录页内的净化规则支持 `##正则##$1`
    #[test]
    fn legado_replacement_supports_capture_group() {
        let out =
            apply_legado_replacements("第12章 abc", &Some(r#"第(\d+)章##Chapter$1"#.to_string()));
        assert_eq!(out, "Chapter12 abc");
    }

    // 布尔规则字符串按 Legado 常见写法判定
    #[test]
    fn toc_boolean_rules_are_parsed() {
        for t in ["true", "1", "yes", "是", "TRUE"] {
            assert!(truthy(t), "{t} 应为真");
        }
        for f in ["", "false", "0", "no", "null", "否"] {
            assert!(!truthy(f), "{f:?} 应为假");
        }
    }

    // 单源超时：慢源按预算被切断，快源结果照常返回（聚合不被拖住）
    #[tokio::test]
    async fn slow_source_is_cut_by_budget_and_fast_source_still_returns() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // 慢源：读完请求后挂起 3s 不给响应
        let slow = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slow_addr = slow.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = slow.accept().await {
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });

        // 快源：返回一条可解析结果
        let fast = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fast_addr = fast.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = fast.accept().await {
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await;
                let body = r#"<div class="item"><a href="/b/1">测试书</a></div>"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let mk = |name: &str, base: std::net::SocketAddr| {
            let mut s = BookSource::default();
            s.enabled = true;
            s.book_source_url = format!("http://{base}");
            s.book_source_name = name.into();
            s.search_url = Some(format!("http://{base}/s?q={{{{key}}}}"));
            s.rule_search = Some(SearchRule {
                book_list: Some("class.item".into()),
                name: Some("a@text".into()),
                book_url: Some("a@href".into()),
                ..Default::default()
            });
            s
        };
        let sources = vec![mk("slow", slow_addr), mk("fast", fast_addr)];
        let started = std::time::Instant::now();
        let out =
            search_all_with_errors_budgeted(&sources, "书", 1, Duration::from_millis(300)).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "慢源不应拖住整体: {:?}",
            started.elapsed()
        );
        let slow_err = out
            .errors
            .iter()
            .find(|e| e.source_name == "slow")
            .expect("慢源应记一条错误");
        assert!(
            slow_err.error.contains("搜索超时"),
            "超时文案应可识别: {}",
            slow_err.error
        );
        assert_eq!(out.books.len(), 1, "快源结果应照常返回");
        assert_eq!(out.books[0].name.as_deref(), Some("测试书"));
        assert!(!out.errors.iter().any(|e| e.source_name == "fast"));
    }

    // isVip / isPay 共用布尔规则解析：真值、数字真值、假值以及规则缺省
    #[test]
    fn flag_rule_maps_vip_and_pay_fields() {
        let src = BookSource {
            book_source_url: "https://flag.example".into(),
            book_source_name: "flag".into(),
            ..Default::default()
        };
        let ar = AnalyzeRule::new();
        let item = r#"<div class="ch"><i class="vip">true</i><i class="pay">1</i><i class="free">0</i></div>"#;
        let base = "https://x.com/toc";
        assert!(
            flag(&src, "is_vip", &ar, item, &Some(".vip@text".into()), base),
            "true 应为付费/VIP"
        );
        assert!(
            flag(&src, "is_pay", &ar, item, &Some(".pay@text".into()), base),
            "1 应为付费"
        );
        assert!(
            !flag(&src, "is_free", &ar, item, &Some(".free@text".into()), base),
            "0 应为 false"
        );
    }

    // 字段级规则异常不拖垮整源：JS TypeError → 该字段空值 + 台账记录，不返回 Err
    #[test]
    fn field_rule_exception_yields_empty_and_records_anomaly() {
        let src = BookSource {
            book_source_url: "https://rule-error.example".into(),
            book_source_name: "规则异常源".into(),
            ..Default::default()
        };
        let ar = AnalyzeRule::new();
        let html = r#"<div class="item"><a href="/b/1" onclick="go('/b/1')">书</a></div>"#;
        // 模拟阅友小说：`result` 是字符串，`.match()` 返回 null 后再取 [1] 会抛 TypeError
        let v = eval_field(
            &src,
            "book_url",
            &ar,
            html,
            "@js:result.match(/notfound/)[1]",
            true,
            "https://x/",
        );
        assert!(v.is_empty(), "异常必须退化为空值，不能传播失败");
        let a = crate::source_health::rule_anomaly("https://rule-error.example")
            .expect("应写入规则异常台账");
        assert_eq!(a.last_field, "book_url");
        assert!(a.count >= 1);
        assert!(!a.last_error.is_empty());
    }

    // 列表规则异常同样退化为空列表
    #[test]
    fn list_rule_exception_yields_empty_list() {
        let src = BookSource {
            book_source_url: "https://list-error.example".into(),
            book_source_name: "列表异常源".into(),
            ..Default::default()
        };
        let ar = AnalyzeRule::new();
        let out = eval_list_field(
            &src,
            "book_list",
            &ar,
            "<html></html>",
            "@js:result.nonexistent.deep",
        );
        assert!(out.is_empty());
        assert!(crate::source_health::rule_anomaly("https://list-error.example").is_some());
    }

    // 「空结果」不是异常：规则正常求值但没命中，不应写台账（区分异常与空结果）
    #[test]
    fn empty_rule_result_is_not_recorded_as_anomaly() {
        let src = BookSource {
            book_source_url: "https://empty-result.example".into(),
            book_source_name: "空结果源".into(),
            ..Default::default()
        };
        let ar = AnalyzeRule::new();
        let v = eval_field(
            &src,
            "name",
            &ar,
            "<div></div>",
            ".missing@text",
            false,
            "https://x/",
        );
        assert!(v.is_empty());
        assert!(
            crate::source_health::rule_anomaly("https://empty-result.example").is_none(),
            "空结果不应算规则异常"
        );
    }

    // image_decode：字节入参 → 规则改写 → 字节出参（哔哩漫画解图链路的桥）
    #[test]
    fn image_decode_runs_js_with_byte_bindings() {
        let src = BookSource {
            book_source_url: "https://img-dec.example".into(),
            book_source_name: "解图源".into(),
            rule_content: Some(crate::entities::ContentRule {
                image_decode: Some(
                    "var out=[];for(var i=result.length-1;i>=0;i--){out.push(result[i]);}out"
                        .into(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(has_image_decode(&src));
        let decoded = decode_image(&src, "https://img/x.webp", vec![1, 2, 3, 4]);
        assert_eq!(decoded, vec![4, 3, 2, 1]);
    }

    // image_decode 抛错时回退原图，并记规则异常（不产生 502）
    #[test]
    fn image_decode_failure_falls_back_to_raw_bytes() {
        let src = BookSource {
            book_source_url: "https://img-err.example".into(),
            book_source_name: "解图异常源".into(),
            rule_content: Some(crate::entities::ContentRule {
                image_decode: Some("throw new Error('boom')".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let raw = vec![9, 8, 7];
        assert_eq!(decode_image(&src, "https://img/x", raw.clone()), raw);
        assert!(crate::source_health::rule_anomaly("https://img-err.example").is_some());
    }

    // 图片抽取：相对地址按章节页归并、去重、顺序与页面一致
    #[test]
    fn image_extraction_joins_relative_and_dedups() {
        let html = r#"
            <div class="content">
              <img src="/img/1.jpg" width="800" height="1200">
              <img src="img/2.png">
              <img src="/img/1.jpg">
            </div>"#;
        let raws = images_from_value(html);
        assert_eq!(raws.len(), 3, "未去重前应保留 3 条");
        let pages = vec![("https://m.example.com/ch/12.html".to_string(), raws)];
        let out = build_images(&pages, 300);
        assert_eq!(out.images.len(), 2, "重复地址应被去重");
        assert_eq!(out.images[0].url, "https://m.example.com/img/1.jpg");
        assert_eq!(out.images[1].url, "https://m.example.com/ch/img/2.png");
        assert_eq!(out.images[0].index, 1);
        assert_eq!(out.images[1].index, 2);
        assert_eq!(out.images[0].width, Some(800));
        assert_eq!(out.images[0].height, Some(1200));
        assert!(!out.truncated);
    }

    // 去重：同一绝对地址只保留第一次出现，编号连续
    #[test]
    fn image_dedup_keeps_first_occurrence() {
        let raws = images_from_html(
            r#"<img src="//cdn.example.com/a.jpg"><img src="https://cdn.example.com/a.jpg">"#,
        );
        let pages = vec![("https://x.com/1".to_string(), raws)];
        let out = build_images(&pages, 300);
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].url, "https://cdn.example.com/a.jpg");
    }

    // 上限：超过 300 张截断并标记 truncated，编号从 1 到 limit
    #[test]
    fn image_limit_truncates() {
        let html: String = (0..305)
            .map(|i| format!(r#"<img src="https://img.example/{i}.jpg">"#))
            .collect();
        let raws = images_from_html(&html);
        assert_eq!(raws.len(), 305);
        let pages = vec![("https://x.com/1".to_string(), raws)];
        let out = build_images(&pages, 300);
        assert_eq!(out.images.len(), 300);
        assert!(out.truncated);
        assert_eq!(out.images[299].index, 300);
    }

    // 占位图/空图过滤：1px 宽高、样式 1px、data: 一律丢弃
    #[test]
    fn image_filters_placeholder_and_data_uri() {
        let html = r#"
            <img src="https://img.example/real.jpg">
            <img src="https://img.example/track.gif" width="1" height="1">
            <img src="https://img.example/track2.gif" style="width:1px;height:1px">
            <img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7">
            <img src="">
        "#;
        let raws = images_from_html(html);
        assert_eq!(raws.len(), 1, "只应保留真实图片");
        assert_eq!(raws[0].url, "https://img.example/real.jpg");
    }

    // @js: 返回 JSON 数组/对象也要能解析，并保留宽高
    #[test]
    fn image_extraction_supports_js_json() {
        let arr = r#"["https://img.example/1.jpg", {"url":"/2.png","width":600,"height":900}]"#;
        let raws = images_from_value(arr);
        assert_eq!(raws.len(), 2);
        assert_eq!(raws[1].width, Some(600));
        let pages = vec![("https://x.com/a/1".to_string(), raws)];
        let out = build_images(&pages, 300);
        assert_eq!(out.images[1].url, "https://x.com/2.png");

        let obj = r#"{"images":["https://img.example/3.jpg","https://img.example/4.jpg"]}"#;
        assert_eq!(images_from_value(obj).len(), 2);
    }

    // 1kkk/酸奶漫画：@js 把请求头拼进 src，HTML 解析只截到 `url,{`，必须切掉后续 JSON 片段
    #[test]
    fn image_url_strips_trailing_header_json() {
        let html = r#"<img src="https://cdn.example.com/1.jpg?cid=12&amp;type=1,{"headers":"{\"Referer\":\"https://m.1kkk.com/\"}"}">"#;
        let raws = images_from_value(html);
        assert_eq!(raws.len(), 1);
        assert_eq!(
            raws[0].url,
            "https://cdn.example.com/1.jpg?cid=12&amp;type=1"
        );
        // 纯 URL 形态（无标签）同样归一
        assert_eq!(
            clean_image_url(r#"https://cdn.example.com/2.jpg?a=1,{"x":"y"}"#),
            "https://cdn.example.com/2.jpg?a=1"
        );
        assert_eq!(
            clean_image_url("https://cdn.example.com/3.jpg"),
            "https://cdn.example.com/3.jpg"
        );
    }

    // 搜索结果的 type 标识：0 小说 -> text，2 漫画 -> image
    #[test]
    fn search_type_token_distinguishes_comic() {
        assert_eq!(type_token(0), "text");
        assert_eq!(type_token(2), "image");
    }

    // searchUrl / POST body 模板展开：{{key}}、${key}、{{page}}、{{page*20}}、{{encodeURIComponent(key)}}
    #[test]
    fn search_url_template_expands_key_page_and_encoder() {
        let mut s = BookSource::default();
        s.book_source_url = "https://api.example.com".into();
        s.search_url = Some(
            "https://api.example.com/search?q={{encodeURIComponent(key)}}&offset={{page*20}}&page={{page}}"
                .into(),
        );
        let (url, _, _, _) = build_search_request(&s, "斗罗大陆", 3);
        assert_eq!(
            url,
            "https://api.example.com/search?q=%E6%96%97%E7%BD%97%E5%A4%A7%E9%99%86&offset=60&page=3"
        );

        // `${key}` / `${page}` / `${page*N}` 同义写法
        s.search_url = Some("https://api.example.com/s?k=${key}&skip=${page*20}".into());
        let (url2, _, _, _) = build_search_request(&s, "abc", 2);
        assert_eq!(url2, "https://api.example.com/s?k=abc&skip=40");

        // POST body 同样展开（老源的 JSON body 里常写 {{key}}）
        s.search_url = Some(
            r#"https://api.example.com/post,{"method":"POST","body":"{\"kw\":\"{{key}}\",\"start\":{{page*20}}}"}"#
                .into(),
        );
        let (_, method, _, body) = build_search_request(&s, "斗罗大陆", 2);
        assert_eq!(method, "POST");
        assert_eq!(body.as_deref(), Some(r#"{"kw":"斗罗大陆","start":40}"#));
        // encodeURIComponent 只按 unreserved 保留，其余按 UTF-8 编码
        assert_eq!(encode_component("a b&c/d"), "a%20b%26c%2Fd");
    }

    // searchUrl 里的 JS：`{{表达式}}`、`<js>` 块、`@js:` 前缀都按 Legado AnalyzeUrl 语义求值
    #[test]
    fn search_url_js_expression_and_block_are_evaluated() {
        let mut s = BookSource::default();
        s.book_source_url = "https://api.example.com".into();
        // `{{page-1}}` 不是静态快路径，走 JS 求值；`{{key}}` 仍走快路径
        s.search_url = Some(
            "https://api.example.com/s?offset={{page-1}}&q={{key}}&kind={{page>2?\"late\":\"early\"}}"
                .into(),
        );
        let (url, _, _, _) = build_search_request(&s, "斗罗大陆", 3);
        assert_eq!(
            url, "https://api.example.com/s?offset=2&q=斗罗大陆&kind=late",
            "{{...}} 必须按 JS 求值"
        );

        // `<js>` 块直接拼进 URL（与 Legado analyzeJs 一致）
        s.search_url = Some(
            r#"https://api.example.com/<js>"s?k=" + encodeURIComponent(key) + "&p=" + page</js>"#
                .into(),
        );
        let (url2, _, _, _) = build_search_request(&s, "斗 罗", 2);
        assert_eq!(
            url2,
            "https://api.example.com/s?k=%E6%96%97%20%E7%BD%97&p=2"
        );

        // `@js:` 前缀整段求值（oread 扩展，覆盖曼哈漫画这类写法）
        s.search_url = Some(
            "@js:\nresult = `https://api.example.com/manga?limit=32&offset=${(page-1)*32}&title=${key}`;\nresult;"
                .into(),
        );
        let (url3, _, _, _) = build_search_request(&s, "abc", 2);
        assert_eq!(
            url3,
            "https://api.example.com/manga?limit=32&offset=32&title=abc"
        );
    }

    // JS 求值异常按字段容错：URL 变空、记异常台账，但调用方不 panic、不整源标记失败
    #[test]
    fn search_url_js_failure_is_tolerated_and_recorded() {
        let mut s = BookSource::default();
        s.book_source_url = "https://api.example.com".into();
        s.book_source_name = "js-失败源".into();
        let before = crate::source_health::rule_anomaly(&s.book_source_url)
            .map(|a| a.count)
            .unwrap_or(0);
        s.search_url = Some("@js: throw new Error('boom')".into());
        let (url, _, _, _) = build_search_request(&s, "abc", 1);
        assert!(url.is_empty() || url == "https://api.example.com/");
        let after = crate::source_health::rule_anomaly(&s.book_source_url).expect("应记录规则异常");
        assert!(after.count > before);
        assert_eq!(after.last_field, "search_url");
    }

    /// “搜索接口返回 JSON + 规则用 `$.path` 取字段”的最小矩阵：
    /// 快看（hits 数组 + `$.id||$.topic_id` 再拼 @js 前缀）、爱看（`$.data` 数组 + `{{$.comic_id}}` 模板）、
    /// 奇漫屋子（`$.data.list` + 相对 `$.url`）。取出的 book_url 必须是可用的详情页地址，
    /// 不能把整段 JSON 拼进 URL。
    #[test]
    fn json_search_rule_matrix_yields_usable_book_url() {
        // 用真实搜索响应结构做夹具（字段取自 2026-11 线上响应）
        let kk_body = r#"{"code":200,"total":2,"hits":[
            {"id":3095,"topic_id":3095,"title":"斗罗大陆外传唐门英雄传","vertical_image_url":"https://hs1-f2.v3mh.com/image/190115/6uDRSET7T.webp-w320.w"},
            {"id":23833,"topic_id":23833,"title":"某天成为王的女儿","vertical_image_url":"https://hs1-f2.v3mh.com/image/240816/yeIXGLktt.webp-w320.w"}
        ]}"#;
        let kk = BookSource {
            book_source_url: "https://m.kuaikanmanhua.com/".into(),
            search_url: Some(
                "/search/mini/topic/title_and_author?page={{page}}&size=24&q={{key}}".into(),
            ),
            book_source_type: 2,
            rule_search: Some(SearchRule {
                book_list: Some(
                    "$.data.topics||$.data.cards||$.hits.topicMessageList||$.hits".into(),
                ),
                book_url: Some("$.id||$.topic_id\n@js:\"/mobile/\"+result+\"/list/\"".into()),
                name: Some("$.title.text||$.title".into()),
                cover_url: Some("$.vertical_image_url".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let ak_body = r#"{"data":[{"comic_id":107477,"comic_name":"某天成为王的女儿"},
            {"comic_id":100037,"comic_name":"重生之都市修仙"}]}"#;
        let ak = BookSource {
            book_source_url: "https://m.kanman.com".into(),
            search_url: Some(
                "https://m.kanman.com/api/serachcomic/?product_id=1&productname=kmh&platformname=wap&serachKey={{encodeURIComponent(key)}}&topNumber=10".into(),
            ),
            book_source_type: 2,
            rule_search: Some(SearchRule {
                book_list: Some("$.data.data||$.data".into()),
                book_url: Some("https://m.kanman.com/{{$.comic_id}}/".into()),
                name: Some("$.comic_name".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let qm_body = r#"{"data":{"list":[{"id":3095,"url":"/comic/3095"},{"id":23833,"url":"/comic/23833"}]}}"#;
        let qm = BookSource {
            book_source_url: "https://m.qimanwu.app".into(),
            search_url: Some(
                "https://m.qimanwu.app/api/search?q={{encodeURIComponent(key)}}&page={{page*20}}"
                    .into(),
            ),
            book_source_type: 2,
            rule_search: Some(SearchRule {
                book_list: Some("$.data.list".into()),
                book_url: Some("$.url".into()),
                name: Some("$.id".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let kk_urls = map_search_book_urls(&kk, kk_body, "斗罗大陆", 1);
        assert_eq!(
            kk_urls,
            vec![
                "https://m.kuaikanmanhua.com/mobile/3095/list/".to_string(),
                "https://m.kuaikanmanhua.com/mobile/23833/list/".to_string()
            ],
            "快看 book_url 不应把整段 JSON 拼进 URL"
        );
        let ak_urls = map_search_book_urls(&ak, ak_body, "斗罗大陆", 1);
        assert_eq!(
            ak_urls,
            vec![
                "https://m.kanman.com/107477/".to_string(),
                "https://m.kanman.com/100037/".to_string()
            ]
        );
        let qm_urls = map_search_book_urls(&qm, qm_body, "斗罗大陆", 1);
        assert_eq!(
            qm_urls,
            vec![
                "https://m.qimanwu.app/comic/3095".to_string(),
                "https://m.qimanwu.app/comic/23833".to_string()
            ]
        );
        // 模板展开：encodeURIComponent 与 page*N 都要在真实 searchUrl 上生效
        let (qm_search, _, _, _) = build_search_request(&qm, "斗罗大陆", 2);
        assert_eq!(
            qm_search,
            "https://m.qimanwu.app/api/search?q=%E6%96%97%E7%BD%97%E5%A4%A7%E9%99%86&page=40"
        );
    }

    /// 复刻 `search()` 的取值链：book_list → 逐项 book_url（is_url）→ 按搜索 URL 归并。
    fn map_search_book_urls(src: &BookSource, body: &str, key: &str, page: i32) -> Vec<String> {
        let rule = src.rule_search.as_ref().expect("rule_search");
        let ar = make_ar(src);
        let (base, _, _, _) = build_search_request(src, key, page);
        let items = ar
            .get_string_list(body, rule.book_list.as_deref().unwrap_or(""))
            .expect("book_list");
        items
            .iter()
            .map(|item| {
                // 与 search() 一致：多行 book_url 逐行管道求值
                let u = eval_rule(
                    &ar,
                    item,
                    rule.book_url.as_deref().unwrap_or(""),
                    true,
                    &base,
                )
                .expect("book_url");
                abs(&base, &u)
            })
            .collect()
    }

    // ----- 相关性聚合排序 -----

    fn sb(name: &str, author: &str, origin: &str, url: &str) -> SearchBook {
        SearchBook {
            name: Some(name.into()),
            author: Some(author.into()),
            origin: Some(origin.into()),
            book_url: url.into(),
            ..Default::default()
        }
    }

    fn src(url: &str, weight: i64, order: i64) -> BookSource {
        BookSource {
            book_source_url: url.into(),
            weight,
            custom_order: order,
            ..Default::default()
        }
    }

    /// 完整命中排前、部分命中降权、完全不沾丢弃；高权重源也不该压过完整命中。
    #[test]
    fn dedupe_and_sort_puts_full_match_first_and_drops_unrelated() {
        let sources = vec![src("high", 9, 0), src("low", 0, 0)];
        let books = vec![
            sb("贯穿大明的百年世家", "某作者", "high", "u1"),
            sb("海贼王", "尾田荣一郎", "high", "u2"),
            sb("百年孤独", "加西亚·马尔克斯", "low", "u3"),
            sb("新百年孤独", "某作者", "high", "u4"),
        ];
        let out = dedupe_and_sort(books, &sources, "百年孤独");
        assert_eq!(out.len(), 3, "完全无关的《海贼王》应被丢弃");
        assert_eq!(out[0].name.as_deref(), Some("百年孤独"), "完整命中排第一");
        assert_eq!(out[1].name.as_deref(), Some("新百年孤独"), "包含命中其次");
        assert_eq!(
            out[2].name.as_deref(),
            Some("贯穿大明的百年世家"),
            "部分命中排最后"
        );
    }

    /// 同一本书（书名+作者相同）多源命中时，保留排序更靠前（相关性/权重更高）的那条。
    #[test]
    fn dedupe_and_sort_keeps_best_source_for_duplicate() {
        let sources = vec![src("high", 9, 0), src("low", 0, 0)];
        let books = vec![
            sb("百年孤独", "加西亚·马尔克斯", "low", "u1"),
            sb("百年孤独", "加西亚·马尔克斯", "high", "u2"),
        ];
        let out = dedupe_and_sort(books, &sources, "百年孤独");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].origin.as_deref(), Some("high"));
    }

    /// 空查询（发现/探索页）不做相关性过滤，仍按源权重排序。
    #[test]
    fn dedupe_and_sort_without_query_keeps_weight_order() {
        let sources = vec![src("high", 9, 0), src("low", 0, 0)];
        let books = vec![
            sb("海贼王", "尾田荣一郎", "low", "u1"),
            sb("斗罗大陆", "唐家三少", "high", "u2"),
        ];
        let out = dedupe_and_sort(books, &sources, "");
        assert_eq!(out.len(), 2, "空查询不得丢弃任何条目");
        assert_eq!(out[0].name.as_deref(), Some("斗罗大陆"), "高权重源仍排前");
    }

    /// 多源共识：同一本书命中 3 个源应排在同相关、同权重的单源书之前。
    #[test]
    fn multi_source_consensus_outranks_single_source() {
        let sources = vec![src("a", 0, 0), src("b", 0, 0), src("c", 0, 0)];
        let books = vec![
            sb("百年孤独", "马尔克斯", "a", "u1"),
            sb("百年孤独", "马尔克斯", "b", "u2"),
            sb("百年孤独", "马尔克斯", "c", "u3"),
            sb("百年孤独外传", "某人", "a", "u4"),
        ];
        let out = dedupe_and_sort(books, &sources, "百年孤独");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name.as_deref(), Some("百年孤独"), "3 源同书应排前");
        assert_eq!(out[0].origins.len(), 3, "origins 应累计 3 个源");
        assert_eq!(out[1].origins.len(), 1);
    }

    /// 共识加分不得跨档：partial 即使 3 源 + 满权重也压不过单源 approx。
    #[test]
    fn consensus_never_crosses_relevance_tier() {
        let sources = vec![
            src("a", 9, 0),
            src("b", 9, 0),
            src("c", 9, 0),
            src("d", 0, 0),
        ];
        let books = vec![
            sb("百年争战", "", "a", "u1"),
            sb("百年争战", "", "b", "u2"),
            sb("百年争战", "", "c", "u3"),
            sb("百年孤獨", "", "d", "u4"), // 近似档（异体字编辑距离命中）
        ];
        let out = dedupe_and_sort(books, &sources, "百年孤独");
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].relevance.as_deref(),
            Some("strong"),
            "approx 必须压过 3 源 partial"
        );
    }

    /// strong ≥ 5 时 partial 折叠掉；strong < 5 时 partial 保留且排在 strong 之后。
    #[test]
    fn partials_fold_only_when_enough_strong_hits() {
        let sources = vec![src("s", 0, 0)];
        let mut few: Vec<SearchBook> = vec![
            sb("百年孤独", "", "s", "u1"),
            sb("新百年孤独", "", "s", "u2"),
        ];
        few.extend(["百年争战", "百年世家", "百年修仙", "百年风华"].map(|n| sb(n, "", "s", n)));
        let out = dedupe_and_sort(few, &sources, "百年孤独");
        assert_eq!(out.len(), 6, "strong=2 < 5，partial 仍补位");
        assert_eq!(out[0].relevance.as_deref(), Some("strong"));
        assert_eq!(out[1].relevance.as_deref(), Some("strong"));
        assert!(out[2..]
            .iter()
            .all(|b| b.relevance.as_deref() == Some("partial")));

        let mut many: Vec<SearchBook> = (0..5)
            .map(|i| sb(&format!("百年孤独第{i}部"), "", "s", &format!("u{i}")))
            .collect();
        many.push(sb("百年争战", "", "s", "p1"));
        many.push(sb("百年世家", "", "s", "p2"));
        let out = dedupe_and_sort(many, &sources, "百年孤独");
        assert_eq!(out.len(), 5, "strong=5 ≥ 5，partial 应被折叠");
        assert!(out.iter().all(|b| b.relevance.as_deref() == Some("strong")));
    }

    // ----- 单源超时：四类抓取都必须带上 timeout_millisecond -----

    /// 起一个“只接受连接、故意不回包”的本地服务；请求应被源级超时提前打断。
    async fn slow_source(timeout_ms: i64) -> (BookSource, String) {
        use std::time::Duration;
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let base = format!("http://{addr}");
        let mut s = BookSource::default();
        s.book_source_url = base.clone();
        s.timeout_millisecond = timeout_ms;
        (s, base)
    }

    fn assert_source_timeout(err: anyhow::Error) {
        let msg = err.to_string();
        assert!(msg.contains("请求超时"), "应报源级超时: {msg}");
        assert!(msg.contains("1s"), "应带源级预算: {msg}");
    }

    #[tokio::test]
    async fn search_fetch_uses_source_timeout() {
        let (mut s, base) = slow_source(1000).await;
        s.search_url = Some(format!("{base}/search?kw={{{{key}}}}"));
        s.rule_search = Some(SearchRule {
            book_list: Some("class.list".into()),
            ..Default::default()
        });
        assert_source_timeout(search(&s, "abc", 1).await.unwrap_err());
    }

    #[tokio::test]
    async fn book_info_fetch_uses_source_timeout() {
        let (source, base) = slow_source(1000).await;
        let mut s = source;
        s.rule_book_info = Some(BookInfoRule::default());
        let mut book = Book {
            book_url: format!("{base}/book"),
            ..Default::default()
        };
        assert_source_timeout(book_info(&s, &mut book).await.unwrap_err());
    }

    #[tokio::test]
    async fn toc_fetch_uses_source_timeout() {
        let (source, base) = slow_source(1000).await;
        let mut s = source;
        s.rule_toc = Some(TocRule {
            chapter_list: Some("class.list".into()),
            ..Default::default()
        });
        let book = Book {
            book_url: format!("{base}/book"),
            toc_url: Some(format!("{base}/toc")),
            ..Default::default()
        };
        assert_source_timeout(chapter_list(&s, &book).await.unwrap_err());
    }

    #[tokio::test]
    async fn toc_plus_js_object_array_expands_with_fields() {
        // 复现运行期链路：目录页返回 JSON，`+@js`（正序标记）造出 [{header,list:[…]}] 分卷结构，
        // 章节字段规则是裸名（title/url），必须逐项解析出来（不能只剩分卷头）。
        // `+` 前缀不剥离会被当成 CSS/JS 规则，整源目录 0 章（武道文学即此因）。
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let body = r#"{"chapters":[{"t":"第一章","u":"/ch/1"}]}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let base = format!("http://{addr}");
        let mut s = BookSource::default();
        s.book_source_url = base.clone();
        s.rule_toc = Some(TocRule {
            chapter_list: Some(
                "+@js:JSON.stringify([{header:'卷一',list:[{title:'一',url:'/ch/1'},{title:'二',url:'/ch/2'}]},{header:'卷二',list:[{title:'三',url:'/ch/3'}]}])".into(),
            ),
            chapter_name: Some("title".into()),
            chapter_url: Some("url".into()),
            ..Default::default()
        });
        let book = Book {
            book_url: format!("{base}/book"),
            toc_url: Some(format!("{base}/toc")),
            ..Default::default()
        };
        let chaps = chapter_list(&s, &book).await.unwrap();
        assert_eq!(chaps.len(), 5, "2 个分卷头 + 3 章: {chaps:?}");
        assert_eq!(chaps.iter().filter(|c| !c.is_volume).count(), 3);
        assert_eq!(chaps[1].title.as_deref(), Some("一"));
        assert!(chaps[1].url.ends_with("/ch/1"), "{}", chaps[1].url);
    }

    #[tokio::test]
    async fn toc_chapter_url_strips_encoded_webview_option() {
        // 开心漫画：`href##$##,{"webView": true}` 把选项拼进链接，抓取前必须剥掉，
        // 否则相对链接 join 后变成 `,%7B…`，章节请求 404、正文恒为空。
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let body = r#"<ul class="chapter-list clearfix column6 overflow"><li><a href="/chapter/x">第1话</a></li></ul>"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let base = format!("http://{addr}");
        let mut s = BookSource::default();
        s.book_source_url = base.clone();
        s.rule_toc = Some(TocRule {
            chapter_list: Some(
                "[class=\"chapter-list clearfix column6 overflow\"]@li@a\n<js>result</js>".into(),
            ),
            chapter_name: Some("text".into()),
            chapter_url: Some("href##$##,{\"webView\": true}".into()),
            ..Default::default()
        });
        let book = Book {
            book_url: format!("{base}/comic/1"),
            toc_url: Some(format!("{base}/comic/1")),
            ..Default::default()
        };
        let chaps = chapter_list(&s, &book).await.unwrap();
        assert_eq!(chaps.len(), 1, "{chaps:?}");
        assert_eq!(chaps[0].title.as_deref(), Some("第1话"));
        assert_eq!(
            chaps[0].url,
            format!("{base}/chapter/x"),
            "{}",
            chaps[0].url
        );
    }

    #[tokio::test]
    async fn content_fetch_uses_source_timeout() {
        let (source, base) = slow_source(1000).await;
        let mut s = source;
        s.rule_content = Some(ContentRule {
            content: Some("class.text".into()),
            ..Default::default()
        });
        let chapter = BookChapter {
            url: format!("{base}/ch1"),
            book_url: Some(format!("{base}/book")),
            ..Default::default()
        };
        assert_source_timeout(chapter_content(&s, &chapter).await.unwrap_err());
    }
}
