//! web_book 的规则注入/求值、搜索模板与请求构造（move-only 从 web_book.rs 拆出）。

use super::*;

/// 把当前 Book 的元信息注入 JS 的 `book.*`（Legado 规则里的
/// `book.name/author/kind/intro/coverUrl`，简介模板类规则会直接引用）。
pub(super) fn inject_book_meta(ar: &AnalyzeRule, book: &Book) {
    ar.set_book_meta(BookMeta {
        name: book.name.clone().unwrap_or_default(),
        author: book.author.clone().unwrap_or_default(),
        kind: book.kind.clone().unwrap_or_default(),
        intro: book.intro.clone().unwrap_or_default(),
        cover_url: book.cover_url.clone().unwrap_or_default(),
    });
}

/// 按 `book_url` 创建带 book 作用域的 AnalyzeRule（空 URL 时退化为普通请求作用域）。
pub(super) fn make_ar_book(
    source: &BookSource,
    store: Option<&BookStore>,
    book_url: &str,
) -> AnalyzeRule {
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
pub(super) async fn refresh_toc_url_from_info(
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
pub(super) fn record_ar(ar: &AnalyzeRule, store: Option<&BookStore>) {
    if let Some(st) = store {
        if let Some(key) = ar.book_scope_key() {
            st.record(key, ar.book_vars_snapshot(), ar.cache_snapshot());
        }
    }
}

/// 规则未命中时 JsonPath/JS 可能返回字面量 `"null"`/`"None"`/`"undefined"`。
/// 这些值不是 URL，若不拦截会拼出 `/toc/null` 并重复追加同一目录（验收 P1）。
pub(super) fn blank_value(s: &str) -> bool {
    matches!(s.trim(), "" | "null" | "None" | "none" | "undefined")
}

/// Legado 的布尔规则返回字符串，这里按常见写法判定（空/false/0/否 为假）。
pub(super) fn truthy(s: &str) -> bool {
    !matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "" | "false" | "0" | "no" | "none" | "null" | "undefined" | "否"
    )
}

/// 把相对 URL 归并为绝对 URL
pub(super) fn abs(base: &str, maybe: &str) -> String {
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
pub(super) fn strip_url_options(url: &str) -> &str {
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
pub(super) fn item_json_volume(item: &str) -> bool {
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
pub(super) fn flag(
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
pub(super) fn split_rule_lines(rule: &str) -> Vec<String> {
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

pub(super) fn starts_with_ci_local(s: &str, p: &str) -> bool {
    s.get(..p.len())
        .map(|h| h.eq_ignore_ascii_case(p))
        .unwrap_or(false)
}

/// 单值规则求值（含多行管道）：逐行作用，上一行结果作为下一行输入。
pub(super) fn eval_rule(
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
pub(super) fn eval_field(
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
pub(super) fn eval_list_field(
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
pub(super) fn note_field_error(
    source: &BookSource,
    field: &str,
    err: &crate::rule_engine::RuleError,
) {
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
pub(super) fn opt(
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
pub(super) fn assign_opt(
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
pub(super) fn encode_component(s: &str) -> String {
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
pub(super) fn expand_search_fast(tpl: &str, key: &str, page: i32) -> String {
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
pub(super) fn eval_search_expr(
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
pub(super) fn expand_search_template(
    source: &BookSource,
    tpl: &str,
    key: &str,
    page: i32,
) -> String {
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
pub(super) fn build_search_request(
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
pub(super) fn type_token(t: i64) -> &'static str {
    if t == 2 {
        "image"
    } else {
        "text"
    }
}
