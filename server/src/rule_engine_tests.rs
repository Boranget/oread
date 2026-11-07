//! rule_engine 内部单测（move-only 从 rule_engine.rs 拆出）。
//!
//! 通过父模块的 `#[path] mod tests` 挂载；`use super::*` 仍可访问父模块与兄弟模块。

use super::html::*;
use super::js::*;
use super::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
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
fn class_shorthand_tokens_are_not_dropped() {
    // 错层小说网的真实详情规则：`.content@.book.0@.bookcover@tag.img@src`
    // 以及 `.content@.book.0@.bookinfo@tag.a.1@text`。此前 `.book`/`.bookcover`
    // 被当作末端抽取标记，选择器结果整段丢失。
    let ar = AnalyzeRule::new();
    let html = r#"
          <div class="content">
            <div class="book pt10">
              <div class="bookcover"><img src="/cover.jpg" /></div>
              <div class="bookinfo">
                <h1 class="booktitle">斗破苍穹</h1>
                <p class="booktag"><a>天蚕土豆</a><a>玄幻</a></p>
              </div>
            </div>
          </div>"#;
    let cover = ar
        .get_string(html, ".content@.book.0@.bookcover@tag.img@src", false, "")
        .unwrap();
    assert_eq!(cover, "/cover.jpg");
    let kind = ar
        .get_string(html, ".content@.book.0@.bookinfo@tag.a.1@text", false, "")
        .unwrap();
    assert_eq!(kind, "玄幻");
}

#[test]
fn single_value_rule_joins_all_matched_elements() {
    // 单值抽取按规则语义收集全部匹配元素的值，
    // 非空才保留；多值按 "\n" 合并（isUrl 只取首个）。
    // 夹具对应真实源 101/102/103 的 `@a@text`、`@a@href` 规则。
    let ar = AnalyzeRule::new();
    let html = r#"<div class="book">
            <a href="/b/1"></a>
            <a href="/b/1">剑来</a>
            <a href="/b/2">剑来 第84章</a>
        </div>"#;
    assert_eq!(
        ar.get_string(html, "class.book@a@text", false, "").unwrap(),
        "剑来\n剑来 第84章"
    );
    // 属性：空值与重复值跳过（上游 `if (url.isBlank() || textS.contains(url)) continue`）
    assert_eq!(
        ar.get_string(html, "class.book@a@href", false, "").unwrap(),
        "/b/1\n/b/2"
    );
    // is_url=true：只取首个
    assert_eq!(
        ar.get_string(html, "class.book@a@href", true, "").unwrap(),
        "/b/1"
    );
}

#[test]
fn table_row_fragment_keeps_structure_for_nth_of_type() {
    // 真实源 103 的列表项是 `<tr>`；上游拿到的是 Element（结构完整）。
    // 片段字符串重解析若丢掉 tr/td，p:nth-of-type 会把各单元格的 p 拉平成兄弟节点。
    let ar = AnalyzeRule::new();
    let row = r#"<tr>
            <td><p><a href="/book/1">书名</a></p></td>
            <td><p><a href="/author/x">作者</a></p></td>
            <td><p>连载</p></td>
        </tr>"#;
    assert_eq!(
        ar.get_string(row, "@CSS:p:nth-of-type(1) a@text", false, "")
            .unwrap(),
        "书名\n作者"
    );
    // 每个 td 内只有一个 p，因此不存在 nth-of-type(2)
    assert_eq!(
        ar.get_string(row, "@CSS:p:nth-of-type(2) a@text", false, "")
            .unwrap(),
        ""
    );
}

#[test]
fn html_terminal_returns_outer_html_without_scripts() {
    // 上游 getResultLast("html")：先删 script/style，再取 Elements.outerHtml()。
    let ar = AnalyzeRule::new();
    let html = r#"<div class="text"><p>第一段</p><script>alert(1)</script><style>.x{color:red}</style><p>第二段</p></div>"#;
    let v = ar.get_string(html, "class.text@html", false, "").unwrap();
    assert!(
        v.starts_with("<div class=\"text\">"),
        "应保留元素本身（outerHtml），实际: {v}"
    );
    assert!(v.contains("<p>第一段</p>") && v.contains("<p>第二段</p>"));
    assert!(
        !v.contains("alert(1)") && !v.contains(".x{"),
        "应剔除 script/style，实际: {v}"
    );
}

#[test]
fn regex_replacement_group_followed_by_letter() {
    // Java Matcher 的 `$1s` = 组 1 + 字面量 s；regex crate 会把 `$1s` 当命名组名。
    let ar = AnalyzeRule::new();
    let html = r#"<div class="n">第12章 剑来</div>"#;
    let v = ar
        .get_string(html, r"class.n@text##第(\d+)章##Chapter$1s", false, "")
        .unwrap();
    assert_eq!(v, "Chapter12s 剑来");
}

#[test]
fn unsupported_js_api_reports_feature_name() {
    // 垫片抛出的未支持 API 必须带出具体名称，而不是只显示 JsError/Opaque
    let ar = AnalyzeRule::new();
    // java.webView 现在覆盖“取整页源码”形态；带 js 参数仍需 WebView DOM，保持显式未支持
    let err = ar
        .get_string(
            "",
            "<js>java.webView('', '', 'return document.title')</js>",
            false,
            "",
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("未支持的 JS 特性"),
        "错误文案应给出可读提示，实际: {err}"
    );
    assert!(err.contains("webView"), "应点名缺失的 API，实际: {err}");
}

#[test]
fn browser_apis_return_str_response_without_network() {
    let ar = AnalyzeRule::new();
    // 空 URL 不联网：connect 失败时按上游规则的容错写成 body，code 为 0
    let v = ar
        .get_string("", "<js>java.connect('').code()</js>", false, "")
        .unwrap();
    assert_eq!(v, "0");
    let v = ar
        .get_string("", "<js>typeof java.connect('').body</js>", false, "")
        .unwrap();
    assert_eq!(v, "function");
    let v = ar
        .get_string(
            "",
            "<js>java.startBrowserAwait('').body().indexOf('error') >= 0</js>",
            false,
            "",
        )
        .unwrap();
    assert_eq!(v, "true");
    // webView 没有 url 时直接返回 html，不触发网络
    let v = ar
        .get_string("", "<js>java.webView('<p>x</p>', '', '')</js>", false, "")
        .unwrap();
    assert_eq!(v, "<p>x</p>");
    // startBrowser 无 UI，空操作且不报错
    let v = ar
        .get_string("", "<js>java.startBrowser('http://x','t')</js>", false, "")
        .unwrap();
    assert_eq!(v, "");
}

// ---- jsLib / java.importScript（哔哩哔哩漫画整源依赖 Reload）----

#[test]
fn js_lib_urls_parses_real_source_map() {
    // sources.json 里哔哩哔哩漫画的 jsLib 原值（少数非空 jsLib 之一）
    let map = r#"{"哔哩哔哩漫画":"https://qyyuapi.com/sy/js/哔哩哔哩漫画/jsLib.js"}"#;
    let urls = js_lib_urls(map);
    assert_eq!(
        urls,
        vec!["https://qyyuapi.com/sy/js/哔哩哔哩漫画/jsLib.js".to_string()]
    );
    // 常见空字段/空 map/非法 JSON 都不该产出地址，也不该让规则求值报错
    assert!(js_lib_urls("").is_empty());
    assert!(js_lib_urls("{}").is_empty());
    assert!(js_lib_urls("不是JSON").is_empty());
    // 个别源把整个字段写成单个 URL
    assert_eq!(js_lib_urls("https://a.test/lib.js").len(), 1);
}

#[test]
fn js_lib_json_is_not_injected_as_statements() {
    // 回归：jsLib 曾把 JSON map 原样插进脚本（`{"库":"url"}` 出现在语句位置），
    // 导致该源每一次 JS 求值都 SyntaxError。现在只解析出 URL 去下载，规则照常求值。
    let mut ar = AnalyzeRule::new();
    ar.js_lib = r#"{"哔哩哔哩漫画":"ftp://example.invalid/jsLib.js"}"#.to_string();
    assert_eq!(ar.get_string("", "<js>1+1</js>", false, "").unwrap(), "2");
}

#[test]
fn import_script_rejects_non_http_forms_explicitly() {
    // 上游规则还支持 file/content/storage；服务端不放开（避免读部署机文件），
    // 必须抛可读的未支持错误，而不是让 SyntaxError/IO 错误泄漏。
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string("", "<js>java.importScript('ftp://x/y.js')</js>", false, "")
        .unwrap_err()
        .to_string();
    assert!(err.contains("未支持的 JS 特性"), "实际: {err}");
    assert!(err.contains("importScript"), "应点名 API，实际: {err}");
    assert!(
        !err.contains("SyntaxError"),
        "不应泄漏 SyntaxError，实际: {err}"
    );
}

#[test]
fn import_script_empty_content_reports_readable_error() {
    // 上游规则 p0.i0 对空内容抛「内容获取失败或者为空」，这里同样给可读文案。
    let url = "https://lib.test/empty.js";
    store_script(url, "  ");
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string(
            "",
            &format!("<js>java.importScript('{url}')</js>"),
            false,
            "",
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("内容获取失败或者为空"), "实际: {err}");
}

#[test]
fn java_head_requires_url() {
    // head 已接抓取层；空 url 显式报错、不发起网络请求。
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string("", "<js>java.head('')</js>", false, "")
        .unwrap_err()
        .to_string();
    assert!(err.contains("java.head"), "应点名 API，实际: {err}");
    assert!(err.contains("缺少 url"), "实际: {err}");
}

#[test]
fn js_lib_reload_and_import_script_work_in_rule_scope() {
    // 哔哩哔哩漫画 jsLib 的结构：`const { java, source, cookie, cache } = this;`
    // 解构共享作用域，再由 Reload 用 java.importScript 取子规则脚本。
    // 两个脚本都预置进 TTL 缓存，走完整链路但不发起真实网络请求。
    let lib_url = "https://lib.test/bili-manga/jsLib.js";
    let sub_url = "https://lib.test/bili-manga/detail.js";
    store_script(
        lib_url,
        r#"const { java, cache } = this;
function Reload(e){
  var etag = '';
  // 空 URL 会让 java.head 立刻报“缺少 url”，既能走 catch 分支，又不会触发真实网络。
  try { etag = String(java.head('', {}).header('etag')); }
  catch (err) { etag = String(cache.get(e) || 'noetag'); }
  return String(java.importScript(e));
}"#,
    );
    store_script(sub_url, "(function(){ return 'sub-rule-ok'; })()");
    let mut ar = AnalyzeRule::new();
    ar.js_lib = serde_json::json!({"哔哩哔哩漫画": lib_url}).to_string();
    assert_eq!(
        ar.get_string(
            "",
            &format!("<js>eval(String(Reload('{sub_url}')));</js>"),
            false,
            ""
        )
        .unwrap(),
        "sub-rule-ok"
    );
}

/// 目录页结构样例：`java.getElements` 取 `dl` 的 children，
/// 再用 `String(node)` + 正则过滤出正文分卷的 `dd`。
#[test]
fn dom_bridge_yetianlian_toc_rule_returns_chapter_nodes() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="listmain">
          <dl>
            <dt>《雪中》最新章节列表</dt>
            <dd><a href="/yt110289/41770731.html">第1章 新章节</a></dd>
            <dd><a href="/yt110289/41770732.html">第2章 新章节</a></dd>
            <dt>《雪中》正文卷</dt>
            <dd><a href="/yt110289/1.html">第一章 小地瓜</a></dd>
            <dd><a href="/yt110289/2.html">第二章 北凉</a></dd>
            <dd><a href="/yt110289/3.html">第三章 剑来</a></dd>
          </dl>
        </div>"#;
    let rule = r#"@js:
var nodes = java.getElements("class.listmain@tag.dl@children");
var inFullToc = false;
nodes.filter(function (node) {
    var html = String(node);
    if (/^<dt\b/i.test(html)) { inFullToc = html.indexOf("最新章节") === -1; }
    return inFullToc && /^<dd\b/i.test(html);
});"#;
    let items = ar.get_string_list(html, rule).unwrap();
    assert_eq!(items.len(), 3, "应只保留正文卷的 3 个 dd，实际: {items:?}");
    assert!(items[0].contains("/yt110289/1.html"));
    // 逐项规则仍照常可用（目录名/URL 解析链路不变）
    assert_eq!(
        ar.get_string(&items[0], "a@text", false, "").unwrap(),
        "第一章 小地瓜"
    );
    assert_eq!(
        ar.get_string(&items[0], "a@href", false, "").unwrap(),
        "/yt110289/1.html"
    );
}

#[test]
fn dom_bridge_get_string_and_string_list_on_current_page() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="head"><h1 class="title">测试书</h1></div>
            <div class="item">甲</div><div class="item">乙</div>"#;
    let title = ar
        .get_string(
            html,
            r#"@js: java.getString("class.title@text")"#,
            false,
            "",
        )
        .unwrap();
    assert_eq!(title, "测试书");
    let items = ar
        .get_string_list(html, r#"@js: java.getStringList("class.item")"#)
        .unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].contains("甲"));
}

// ---- org.jsoup JS-Java 桥（按 147 源实际调用面）----

const JSOUP_HTML: &str = r#"<ul class="list"><li><a href="/b/1" class="t">第一章</a><span class="n">1</span></li><li><a href="/b/2">第二章</a></li></ul>"#;

fn run_js(rule: &str) -> Result<String, RuleError> {
    AnalyzeRule::new().get_string(JSOUP_HTML, rule, false, "https://x.com/")
}

#[test]
fn jsoup_parse_select_and_element_accessors() {
    // size/get/first/last/attr/text/html/outerHtml/parent/nextSibling/is/hasAttr/tagName
    let out = run_js(
        r#"@js:var d=org.jsoup.Jsoup.parse(result);var a=d.select(".list a");
a.size()+"|"+a.get(1).attr("href")+"|"+a.first().text()+"|"+a.last().text()+"|"+
a.first().html()+"|"+
a.first().parent().tagName()+"|"+
a.first().nextSiblingElements().size()+"|"+a.first().hasAttr("href")+"|"+a.first().is("a.t")"#,
    )
    .unwrap();
    assert_eq!(out, "2|/b/2|第一章|第二章|第一章|li|1|true|true");
    // outerHtml：html5ever 的属性序列化顺序不保证（与 jsoup 源码顺序可能不同），
    // 因此用单属性元素断言，避免测试受 HashMap 顺序影响。
    let outer =
        run_js(r#"@js:var s=org.jsoup.Jsoup.parse(result).select(".n").first(); s.outerHtml()"#)
            .unwrap();
    assert_eq!(outer, "<span class=\"n\">1</span>");
    // eq 返回 Elements（jsoup 语义），可继续 size()/get()
    let eq = run_js(
            r#"@js:var a=org.jsoup.Jsoup.parse(result).select(".list a");a.eq(-1).size()+"|"+a.eq(0).get(0).text()"#,
        )
        .unwrap();
    assert_eq!(eq, "1|第一章");
}

#[test]
fn jsoup_elements_for_in_only_enumerates_indexes() {
    // 书源里大量 `for (i in elements)`：__list/length 不可枚举，否则会拿到非元素对象
    let out = run_js(
            r#"@js:var a=org.jsoup.Jsoup.parse(result).select(".list a");var o=[];for(var i in a){o.push(a[i].text())}o.join(",")"#,
        )
        .unwrap();
    assert_eq!(out, "第一章,第二章");
}

#[test]
fn jsoup_parse_accepts_element_and_array_helpers() {
    // `Jsoup.parse(element)` 依赖 Element.toString → outerHtml；Array.from/toArray/reverse 链条
    let out = run_js(
            r#"@js:var li=org.jsoup.Jsoup.parse(result).select("li");var href=org.jsoup.Jsoup.parse(li[1]).select("a")[0].attr("href");
var rev=Array.from(org.jsoup.Jsoup.parse(result).select(".list a")).reverse().map(function(e){return e.attr("href")}).join(",");
href+"|"+rev"#,
        )
        .unwrap();
    assert_eq!(out, "/b/2|/b/2,/b/1");
}

#[test]
fn jsoup_abs_attr_uses_base_uri() {
    let out = run_js(
            r#"@js:var a=org.jsoup.Jsoup.parse(result,"https://ex.com/book/").select("a").first();a.attr("abs:href")"#,
        )
        .unwrap();
    assert_eq!(out, "https://ex.com/b/1");
}

#[test]
fn jsoup_extended_selector_reports_unsupported() {
    let err = run_js(r#"@js:org.jsoup.Jsoup.parse(result).select("a:nth(1)")"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("未支持的 JS 特性"), "实际: {err}");
    assert!(err.contains("jsoup 专有"), "实际: {err}");
}

// jsoup 扩展伪类走 select 结果集过滤（与上游规则/jsoup 语义一致），不再整体抛未支持。
#[test]
fn jsoup_pseudo_classes_select() {
    // li:matches 用合并文本（2 个 li 命中），li:matchesOwn 只用自身文本（li 内只有子元素 → 0），
    // a:matchesOwn 命中 2 个标题。
    let out = run_js(
            r#"@js:var d=org.jsoup.Jsoup.parse(result);d.select("li:has(span)").size()+"|"+d.select("li:contains(第二章)").size()+"|"+d.select("li:containsOwn(第二章)").size()+"|"+d.select("li:matches(第.章)").size()+"|"+d.select("li:matchesOwn(第.章)").size()+"|"+d.select("a:matchesOwn(第.章)").size()"#,
        )
        .unwrap();
    assert_eq!(out, "1|1|0|2|0|2");
}

// ---- AES / 字节桥（哔哩漫画解图链路的基础）----

#[test]
fn js_aes_symmetric_roundtrip_and_bytes_to_str() {
    let out = run_js(
            r#"@js:var c=java.createSymmetricCrypto("aes/cbc/pkcs7padding","0123456789abcdef","fedcba9876543210");
var enc=c.encrypt("hello 漫"); java.bytesToStr(c.decrypt(enc))"#,
        )
        .unwrap();
    assert_eq!(out, "hello 漫");
}

#[test]
fn java_time_format_utc_uses_offset() {
    let ar = AnalyzeRule::new();
    let f = ar
        .get_string(
            "",
            r#"@js:java.timeFormatUTC("2026-01-01T00:00:00Z","yyyy-MM-dd HH:mm:ss",8)"#,
            false,
            "",
        )
        .unwrap();
    assert_eq!(f, "2026-01-01 08:00:00");
}

// `java.getElement` 基于当前页面文档（如漫画网 book_url 的 @onclick 规则）
#[test]
fn java_get_element_uses_current_page() {
    let out = run_js(r#"@js:java.getElement(".list a").text()"#).unwrap();
    assert_eq!(out, "第一章");
}

// `java.aesBase64DecodeToString`（全免漫画 init 规则用法）
#[test]
fn js_aes_base64_decode_to_string() {
    use base64::Engine;
    let key = b"0123456789abcdef";
    let iv = b"fedcba9876543210";
    let cipher =
        crate::crypto::encrypt_bytes("秘密内容".as_bytes(), key, "AES/CBC/PKCS5Padding", iv)
            .unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher);
    let ar = AnalyzeRule::new();
    let rule = format!(
        r#"@js:java.aesBase64DecodeToString("{b64}","0123456789abcdef","AES/CBC/PKCS5Padding","fedcba9876543210")"#
    );
    let out = ar.get_string("", &rule, false, "").unwrap();
    assert_eq!(out, "秘密内容");
}

// `pre_update_js` 可改写 book.bookUrl，并回写到编排层（refreshTocUrl 依赖它）
#[test]
fn js_book_url_rewrite_is_returned_to_rust() {
    let ar = AnalyzeRule::new();
    ar.book_url.replace("https://old.example/book/1".into());
    let out = ar
        .eval_script(
            r#"book.bookUrl = book.bookUrl.replace('/book/1','/manga/1'); book.bookUrl"#,
            "",
            "",
        )
        .unwrap();
    assert_eq!(out, "https://old.example/manga/1");
    assert_eq!(ar.book_url.borrow().as_str(), "https://old.example/manga/1");
}

#[test]
fn js_refresh_toc_url_sets_flag() {
    let ar = AnalyzeRule::new();
    ar.eval_script(r#"java.refreshTocUrl(); "ok""#, "", "")
        .unwrap();
    assert!(ar.refresh_toc.get());
}

/// 快看的图片规则首段：CSS `img@data-src` 必须按列表返回每个属性值。
#[test]
fn get_list_css_attr_data_src() {
    let html = r#"<div class="ComicPics"><img data-src="https://cdn/x1.webp?a=1&amp;t=2" src="s1"><img data-src="https://cdn/x2.webp" src="s2"></div>"#;
    let ar = AnalyzeRule::new();
    let vals = ar.get_string_list_all(html, "img@data-src", "");
    assert_eq!(
        vals,
        vec![
            "https://cdn/x1.webp?a=1&t=2".to_string(),
            "https://cdn/x2.webp".to_string()
        ]
    );
    // 接着 JS 段：拼成 `<img src="url,{headers}">` 形态
    let rule = "img@data-src\n<js>\nlet headers = JSON.stringify({\"headers\":{\"Referer\":baseUrl}});\nresult.split('\\n').map(x=>`<img src=\"${x},${headers}\">`).join('\\n');\n</js>";
    let out = ar.get_string_list_all(html, rule, "https://m.kuaikanmanhua.com/");
    assert_eq!(out.len(), 2, "实际: {out:?}");
    assert!(
        out[0].starts_with("<img src=\"https://cdn/x1.webp?a=1&t=2,"),
        "实际: {out:?}"
    );
}

#[test]
fn dom_bridge_dynamic_argument_reports_unsupported() {
    // 动态拼串不预处理，必须在运行时给出具名提示，而不是静默空结果
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string(
            r#"<div class="item">x</div>"#,
            r#"@js: java.getElements("class." + "item")"#,
            false,
            "",
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("未支持的 JS 特性"), "实际: {err}");
    assert!(err.contains("java.getElements"), "实际: {err}");
    assert!(
        err.contains("字面量"),
        "应提示只支持字面量参数，实际: {err}"
    );
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
    // @children 取直接子节点（li），再取 a 的 href；上游 getString 对多值按 "\n" 合并
    let r = ar
        .get_string(html, "@CSS:ul@children@tag.a@href", false, "")
        .unwrap();
    assert_eq!(r, "/x\n/y");
    let html2 = r#"<div>前缀<span>内</span>后缀</div>"#;
    let t = ar
        .get_string(html2, "@CSS:div@textNodes", false, "")
        .unwrap();
    // 上游 textNodes：直接文本节点逐个 trim，非空值按 "\n" 合并
    assert_eq!(t, "前缀\n后缀");
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
        .get_string(
            "",
            "<js>java.toNumChapter(\"第一百一十二\")</js>",
            false,
            "",
        )
        .unwrap();
    assert_eq!(r, "112");
}

#[test]
fn put_then_get_in_same_scope() {
    let ar = AnalyzeRule::new();
    let html = "<div><a>lit</a></div>";
    // 上游语义：@put 是副作用，摘掉后主体规则照常求值（快看/爱看都是嵌在规则尾部）
    let stored = ar
        .get_string(html, "a@text@put:{x:a@text}", false, "")
        .unwrap();
    assert_eq!(stored, "lit");
    let got = ar.get_string(html, "@get:{x}", false, "").unwrap();
    assert_eq!(got, "lit");
    // 纯 @put 规则自身求值为空串（上游规则 splitPutRule 后规则正文为空）
    let only_put = ar.get_string(html, "@put:{y:a@text}", false, "").unwrap();
    assert_eq!(only_put, "");
    assert_eq!(ar.get_string("", "@get:{y}", false, "").unwrap(), "lit");
}

#[test]
fn get_missing_returns_empty() {
    let ar = AnalyzeRule::new();
    let got = ar.get_string("", "@get:{nope}", false, "").unwrap();
    assert_eq!(got, "");
}

#[test]
fn put_get_jsonpath_and_underscore_key() {
    let ar = AnalyzeRule::new();
    let json = r#"{"comic_name":"海贼王","comic_id":106492}"#;
    // 爱看形态：name 规则尾部 put，toc_url 里 get；key 允许下划线
    let name = ar
        .get_string(json, "$.comic_name@put:{comic_bid:$.comic_id}", false, "")
        .unwrap();
    assert_eq!(name, "海贼王");
    let url = ar
        .get_string(
            json,
            "https://m.kanman.com/api/getchapterlist?comic_id=@get:{comic_bid}",
            true,
            "",
        )
        .unwrap();
    assert_eq!(
        url,
        "https://m.kanman.com/api/getchapterlist?comic_id=106492"
    );
    // 多个 put 键
    let json2 = r#"{"a":1,"b":2}"#;
    ar.get_string(json2, "@put:{k1:$.a,k2:$.b}", false, "")
        .unwrap();
    assert_eq!(ar.get_string("", "@get:{k1}", false, "").unwrap(), "1");
    assert_eq!(ar.get_string("", "@get:{k2}", false, "").unwrap(), "2");
}

// ---- P1: @get/@put 与 JS 侧写变量贯通 ----

#[test]
fn js_put_visible_to_later_get_and_rule() {
    let ar = AnalyzeRule::new();
    // init 常见形态：JS 里 put，后续 toc_url 用 @get 取
    ar.eval_js("", "java.put('apiurl', 'https://ex.com/toc')", "")
        .unwrap();
    let got = ar.get_string("", "@get:{apiurl}", false, "").unwrap();
    assert_eq!(got, "https://ex.com/toc");
    // 取到后继续参与规则求值（拼 URL），而不是只做字符串替换就结束
    let url = ar.get_string("", "@get:{apiurl}?page=2", true, "").unwrap();
    assert_eq!(url, "https://ex.com/toc?page=2");
}

#[test]
fn js_get_sees_rule_level_put() {
    let ar = AnalyzeRule::new();
    let html = r#"<div id="bid">42</div>"#;
    // @put 先于 JS 求值执行；JS 里 java.get 能读到，并再写回新键
    let out = ar
            .get_string(
                html,
                r#"@put:{bid:div@text}@js:var n=java.get('bid');java.put('n2', n+'0');result=java.get('n2')"#,
                false,
                "",
            )
            .unwrap();
    assert_eq!(out, "420");
    assert_eq!(ar.get_string("", "@get:{n2}", false, "").unwrap(), "420");
    // 缺失键读作空串，不报错
    let miss = ar
        .eval_js("", "result = java.get('nope') + '|'", "")
        .unwrap();
    assert_eq!(miss, "|");
}

#[test]
fn js_put_isolated_between_scopes() {
    // 两个独立 AnalyzeRule（不同源/不同请求）并发写入同名 key：互不串味
    let a = std::thread::spawn(|| {
        let ar = AnalyzeRule::new();
        ar.eval_js("", "java.put('k', 'A')", "").unwrap();
        ar.get_string("", "@get:{k}", false, "").unwrap()
    });
    let b = std::thread::spawn(|| {
        let ar = AnalyzeRule::new();
        ar.eval_js("", "java.put('k', 'B')", "").unwrap();
        ar.get_string("", "@get:{k}", false, "").unwrap()
    });
    assert_eq!(a.join().unwrap(), "A");
    assert_eq!(b.join().unwrap(), "B");
}

#[test]
fn js_put_supports_nested_get_reference() {
    // 嵌套引用：@put 的值本身又是 @get（链式传递），且取不到的键读作空串不报错
    let ar = AnalyzeRule::new();
    ar.set_var("b", "B");
    let out = ar.get_string("", "@put:{a:@get:{b}}", false, "").unwrap();
    assert!(out.is_empty(), "整条规则只剩 @put 时应为空: {out:?}");
    assert_eq!(ar.get_string("", "@get:{a}", false, "").unwrap(), "B");
    // 嵌套但上游缺键：仍不报错，值为空
    ar.get_string("", "@put:{c:@get:{missing}}", false, "")
        .unwrap();
    assert_eq!(ar.get_string("", "@get:{c}", false, "").unwrap(), "");
}

#[test]
fn js_put_combines_with_json_modifier() {
    let ar = AnalyzeRule::new();
    let json = r#"{"data":{"id":7}}"#;
    // @put 的值走完整规则链（JSONPath），再被 JS 读取
    let out = ar
        .get_string(
            json,
            r#"@put:{cid:$.data.id}@js:result = java.get('cid')"#,
            false,
            "",
        )
        .unwrap();
    assert_eq!(out, "7");
    assert_eq!(ar.get_string("", "@get:{cid}", false, "").unwrap(), "7");
}

// ---- P2: JS 对象数组展开 ----

#[test]
fn js_list_pipeline_binds_array_result() {
    // `CSS前缀 + <js>` 的目录规则：上游规则 getStringList 把前一段求成 List<String>，
    // JS 里的 `result` 是数组（漫画搬运 `Array.from(result)`、穿越小说 `r.length`）。
    let ar = AnalyzeRule::new();
    let html =
        r#"<ul><li><a href="/1.html">第一章</a></li><li><a href="/2.html">第二章</a></li></ul>"#;
    let mapped = ar
            .get_string_list(
                html,
                "a\n@js:Array.from(result).map(x=>org.jsoup.Jsoup.parse(x).select('a')[0].attr('href'))",
            )
            .unwrap();
    assert_eq!(mapped, vec!["/1.html", "/2.html"]);
    let objects = ar
            .get_string_list(
                html,
                "a\n@js:Array.from(result).map(x=>({n:org.jsoup.Jsoup.parse(x).select('a')[0].text(),u:org.jsoup.Jsoup.parse(x).select('a')[0].attr('href')}))",
            )
            .unwrap();
    assert_eq!(objects.len(), 2);
    assert_eq!(
        ar.get_string(&objects[1], "n", false, "").unwrap(),
        "第二章"
    );
    assert_eq!(
        ar.get_string(&objects[1], "u", true, "").unwrap(),
        "/2.html"
    );
}

#[test]
fn js_object_array_expands_and_fields_resolve() {
    let ar = AnalyzeRule::new();
    let items = ar
        .get_string_list(
            "",
            r#"@js:[{text:'第一章',href:'/1.html'},{title:'第二章',url:'/2.html'}]"#,
        )
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        ar.get_string(&items[0], "text", false, "").unwrap(),
        "第一章"
    );
    assert_eq!(
        ar.get_string(&items[0], "href", true, "").unwrap(),
        "/1.html"
    );
    assert_eq!(
        ar.get_string(&items[1], "title", false, "").unwrap(),
        "第二章"
    );
    assert_eq!(
        ar.get_string(&items[1], "url", true, "").unwrap(),
        "/2.html"
    );
    assert!(ar
        .get_string(&items[0], "volume", false, "")
        .unwrap()
        .is_empty());
}

#[test]
fn js_binary_and_nested_arrays_expand() {
    let ar = AnalyzeRule::new();
    let items = ar
        .get_string_list("", r#"@js:[['第一章','/1'],['第二章','/2']]"#)
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        ar.get_string(&items[0], "text", false, "").unwrap(),
        "第一章"
    );
    assert_eq!(ar.get_string(&items[0], "href", true, "").unwrap(), "/1");
    // 多一层嵌套仍按章节项展开
    let nested = ar
        .get_string_list("", r#"@js:[[['甲','/a'],['乙','/b']]]"#)
        .unwrap();
    assert_eq!(nested.len(), 2);
    assert_eq!(ar.get_string(&nested[1], "href", true, "").unwrap(), "/b");
}

#[test]
fn js_object_array_fields_resolve_on_json_source() {
    // 中文万维：源级 is_json=true（bookList 以 `$.` 开头），目录章节项字段仍是裸名，
    // 不能被当成 JSONPath（否则报“路径必须以 $ 开头”并把整源目录清空）。
    let mut ar = AnalyzeRule::new();
    ar.is_json = true;
    let items = ar
            .get_string_list(
                "",
                r#"@js:[{name:'第一章',url:'https://read.example/c?bookId=9&chapterId=1'},{name:'第二章',url:'https://read.example/c?bookId=9&chapterId=2'}]"#,
            )
            .unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(
        ar.get_string(&items[0], "name", false, "").unwrap(),
        "第一章"
    );
    assert_eq!(
        ar.get_string(&items[0], "url", true, "").unwrap(),
        "https://read.example/c?bookId=9&chapterId=1"
    );
}

#[test]
fn js_grouped_volume_structure_expands() {
    let ar = AnalyzeRule::new();
    let items = ar
            .get_string_list(
                "",
                r#"@js:[{header:'第一卷',list:[{text:'一',href:'/1'},{text:'二',href:'/2'}]},{header:'第二卷',list:[{text:'三',href:'/3'}]}]"#,
            )
            .unwrap();
    assert_eq!(items.len(), 5, "2 个分卷头 + 3 章: {items:?}");
    assert_eq!(
        ar.get_string(&items[0], "text", false, "").unwrap(),
        "第一卷"
    );
    assert_eq!(
        ar.get_string(&items[0], "volume", false, "").unwrap(),
        "true"
    );
    assert_eq!(ar.get_string(&items[1], "href", true, "").unwrap(), "/1");
    assert_eq!(ar.get_string(&items[4], "text", false, "").unwrap(), "三");
}

#[test]
fn js_grouped_volume_structure_expands_with_json_input() {
    // 运行期 content 是目录页 JSON（非空），与上一条空 content 的用例对照。
    let ar = AnalyzeRule::new();
    let items = ar
            .get_string_list(
                r#"{"chapters":[{"t":"第一章","u":"/ch/1"}]}"#,
                r#"@js:JSON.stringify([{header:'卷一',list:[{title:'一',url:'/ch/1'},{title:'二',url:'/ch/2'}]},{header:'卷二',list:[{title:'三',url:'/ch/3'}]}])"#,
            )
            .unwrap();
    assert_eq!(items.len(), 5, "2 个分卷头 + 3 章: {items:?}");
}

#[test]
fn js_array_skips_empty_items() {
    let ar = AnalyzeRule::new();
    let items = ar
        .get_string_list(
            "",
            r#"@js:['', {text:'',href:''}, null, '   ', {text:'有效',href:'/x'}]"#,
        )
        .unwrap();
    assert_eq!(items, vec![r#"{"href":"/x","text":"有效"}"#.to_string()]);
}

#[test]
fn js_image_object_array_takes_src() {
    let ar = AnalyzeRule::new();
    let imgs = ar.get_string_list_all("", r#"@js:[{src:'a.jpg',name:'p1'},{url:'b.jpg'}]"#, "");
    assert_eq!(imgs, vec!["a.jpg".to_string(), "b.jpg".to_string()]);
}

// ---- P5: @js 段内 {{}} 先展开 ----

#[test]
fn js_template_expands_jsonpath_before_eval() {
    // p11 同构：JS 源码字符串里的 {{$.data}} 必须先展开（JSONPath → JSON 文本 → 转义），
    // 否则 JSON.parse 拿到字面量 `{{$.data}}` 直接 SyntaxError。
    let ar = AnalyzeRule::new();
    let out = ar
        .get_string(
            r#"{"data":{"a":1}}"#,
            r#"@js:JSON.parse("{{$.data}}").a"#,
            false,
            "",
        )
        .unwrap();
    assert_eq!(out, "1");
}

#[test]
fn js_segment_expands_template_before_eval() {
    let ar = AnalyzeRule::new();
    ar.set_var("page", "3");
    // p11 形态：模板参与算术，展开结果必须是合法 JS
    let out = ar
        .get_string("", r#"@js:result = '/' + ({{page}} * 32)"#, false, "")
        .unwrap();
    assert_eq!(out, "/96");
}

#[test]
fn js_template_escapes_quotes_in_string_literal() {
    let ar = AnalyzeRule::new();
    ar.set_var("key", "a\"b");
    let out = ar
        .get_string("", r#"@js:var u = "{{key}}"; result = u"#, false, "")
        .unwrap();
    assert_eq!(out, "a\"b");
    ar.set_var("key2", "c\\d");
    let out2 = ar
        .get_string("", r#"@js:var u = "{{key2}}"; result = u"#, false, "")
        .unwrap();
    assert_eq!(out2, "c\\d");
}

#[test]
fn js_template_failure_keeps_literal() {
    let ar = AnalyzeRule::new();
    // `1 1` 不是合法 JS 表达式：保留原文本，不静默丢成空串
    let out = ar
        .get_string("", r#"@js:result = "{{1 1}}""#, false, "")
        .unwrap();
    assert_eq!(out, "{{1 1}}");
}

#[test]
fn cache_get_put_in_same_scope() {
    let ar = AnalyzeRule::new();
    ar.eval_js("", "cache.put('kkmh', 'payload')", "").unwrap();
    // 同一作用域的下一次 eval 仍能读到（快看 init → chapter_list 的用法）
    let got = ar
        .eval_js("", "result = String(cache.get('kkmh'))", "")
        .unwrap();
    assert_eq!(got, "payload");
    // 未命中返回 null（上游 CacheManager.get 返回 String?）
    let miss = ar
        .eval_js("", "result = String(cache.get('nope'))", "")
        .unwrap();
    assert_eq!(miss, "null");
    // 非字符串按 JSON 串存
    ar.eval_js("", "cache.put('obj', {a:1})", "").unwrap();
    let obj = ar.eval_js("", "result = cache.get('obj')", "").unwrap();
    assert_eq!(obj, "{\"a\":1}");
    // delete 后可写
    ar.eval_js("", "cache.delete('kkmh')", "").unwrap();
    let gone = ar
        .eval_js("", "result = String(cache.get('kkmh'))", "")
        .unwrap();
    assert_eq!(gone, "null");
}

#[test]
fn cache_does_not_leak_across_sources() {
    let a = AnalyzeRule::new();
    let b = AnalyzeRule::new();
    a.eval_js("", "cache.put('kk', 'A')", "").unwrap();
    // 另一个源（另一个 AnalyzeRule）读不到
    let b_got = b
        .eval_js("", "result = String(cache.get('kk'))", "")
        .unwrap();
    assert_eq!(b_got, "null");
    let a_got = a
        .eval_js("", "result = String(cache.get('kk'))", "")
        .unwrap();
    assert_eq!(a_got, "A");
    // 变量表也不跨作用域
    assert_eq!(b.get_string("", "@get:{kk}", false, "").unwrap(), "");
}

#[test]
fn vars_do_not_leak_across_scopes() {
    let a = AnalyzeRule::new();
    let b = AnalyzeRule::new();
    let html = "<a>111</a>";
    a.get_string(html, "@put:{bid:a@text}", false, "").unwrap();
    // 另一个源（另一个 AnalyzeRule）读不到
    assert_eq!(b.get_string("", "@get:{bid}", false, "").unwrap(), "");
    assert_eq!(a.get_string("", "@get:{bid}", false, "").unwrap(), "111");
}

/// 跨请求：搜索阶段 `@put` 的 bid 在 info/toc 的新 AnalyzeRule 上读回（爱看链路）。
#[test]
fn book_vars_survive_new_request_scope() {
    let json = r#"{"comic_name":"海贼王","comic_id":106492}"#;
    // 请求 1（搜索）：写入 book 层变量
    let req1 = AnalyzeRule::new();
    req1.begin_book_scope("bk-1".into(), HashMap::new(), HashMap::new());
    let name = req1
        .get_string(json, "$.comic_name@put:{bid:$.comic_id}", false, "")
        .unwrap();
    assert_eq!(name, "海贼王");
    assert_eq!(
        req1.book_vars_snapshot().get("bid").map(|s| s.as_str()),
        Some("106492")
    );

    // 请求 2（目录）：新实例预载同一 book_key 的快照后 @get 生效
    let req2 = AnalyzeRule::new();
    req2.begin_book_scope("bk-1".into(), req1.book_vars_snapshot(), HashMap::new());
    let url = req2
        .get_string(
            "",
            "https://m.kanman.com/api/getchapterlist?comic_id=@get:{bid}",
            true,
            "",
        )
        .unwrap();
    assert_eq!(
        url,
        "https://m.kanman.com/api/getchapterlist?comic_id=106492"
    );

    // 另一本书（不同 book_key）不会读到
    let other = AnalyzeRule::new();
    other.begin_book_scope("bk-2".into(), HashMap::new(), HashMap::new());
    assert_eq!(other.get_string("", "@get:{bid}", false, "").unwrap(), "");
}

/// 跨请求 cache：info 阶段 `cache.put` 的 JSON，目录阶段预载后可 `cache.get`。
#[test]
fn cache_survives_new_request_scope() {
    let req1 = AnalyzeRule::new();
    req1.begin_book_scope("bk-1".into(), HashMap::new(), HashMap::new());
    req1.eval_js("", "cache.put('kkmh', '{\"data\":[1,2]}')", "")
        .unwrap();
    let snap = req1.cache_snapshot();
    assert_eq!(
        snap.get("kkmh").map(|s| s.as_str()),
        Some("{\"data\":[1,2]}")
    );

    let req2 = AnalyzeRule::new();
    req2.begin_book_scope("bk-1".into(), HashMap::new(), snap);
    let got = req2
        .eval_js("", "result = String(cache.get('kkmh'))", "")
        .unwrap();
    assert_eq!(got, "{\"data\":[1,2]}");
    // 目录阶段 delete 后快照里也没了（供路由层按 diff 删库）
    req2.eval_js("", "cache.delete('kkmh')", "").unwrap();
    assert!(req2.cache_snapshot().is_empty());
}

/// 快看形态：`java.setContent(json)` → `java.getElements('$.data[*].comicList[*]')`。
#[test]
fn set_content_then_get_elements_jsonpath_like_source() {
    let nuxt = r#"{"data":[{"comicList":[{"id":1,"title":"第1话"},{"id":2,"title":"第2话"}]},{"comicList":[{"id":3,"title":"第3话"}]}]}"#;
    let code = format!(
        "java.setContent({}); result = java.getElements('$.data[*].comicList[*]');",
        serde_json::to_string(nuxt).unwrap()
    );
    let ar = AnalyzeRule::new();
    let out = ar.eval_js("", &code, "").unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let arr = v.as_array().expect("应返回章节节点数组");
    assert_eq!(arr.len(), 3, "两页 comicList 共 3 章: {out}");
    let first: Value = serde_json::from_str(arr[0].as_str().unwrap()).unwrap();
    assert_eq!(first["title"], "第1话");
}

/// setContent 也只作用于本次 eval（每次 eval 都是新的 IIFE）。
#[test]
fn set_content_scoped_to_call() {
    let ar = AnalyzeRule::new();
    ar.eval_js("", "java.setContent('{\"a\":[1,2]}'); result = 1", "")
        .unwrap();
    let err = ar
        .eval_js("", "result = java.getElements('$.a[*]')", "")
        .unwrap_err();
    assert!(
        err.to_string().contains("仅支持字面量选择器"),
        "下一次调用不应复用上次的文档（应回落到预抽取表并报未支持）：{err}"
    );
}

/// setContent 也支持 HTML 文档（CSS 选择器），下游 getString 同文档求值。
#[test]
fn set_content_html_css_selectors() {
    let ar = AnalyzeRule::new();
    let out = ar
            .eval_js(
                "",
                "java.setContent('<div class=\"x\"><p>甲</p></div>'); result = java.getString('div.x p');",
                "",
            )
            .unwrap();
    assert_eq!(out, "甲");
}

#[test]
fn brace_var_expansion() {
    let ar = AnalyzeRule::new();
    ar.set_var("y", "world");
    // {{var}} 在 JS 段内展开
    let r = ar.get_string("", "<js>\"{{y}}\"</js>", false, "").unwrap();
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
    let href = ar
        .get_string(html, "@css:p:nth-of-type(1) a@href", true, "")
        .unwrap();
    assert_eq!(href, "/book/1");
}

#[test]
fn bare_tag_and_excluded_indices() {
    let ar = AnalyzeRule::new();
    let html = r#"<dl id="list"><dd>第1</dd><dd>第2</dd><dd>第3</dd><dd>第4</dd><dd>第5</dd></dl>"#;
    // 裸标签遍历（@dd）+ 多值按 "\n" 合并（上游 getResultLast 收集全部元素）
    let t1 = ar
        .get_string(html, "@CSS:id.list@dd@text", false, "")
        .unwrap();
    assert_eq!(t1, "第1\n第2\n第3\n第4\n第5");
    // 上游规则的 `!0:1` 是“排除第 0、1 个”，剩余第 3/4/5 个
    let t2 = ar
        .get_string(html, "@CSS:id.list@dd!0:1@text", false, "")
        .unwrap();
    assert_eq!(t2, "第3\n第4\n第5");
    // `!2` 排除第 3 个
    let t3 = ar
        .get_string(html, "@CSS:id.list@dd!2@text", false, "")
        .unwrap();
    assert_eq!(t3, "第1\n第2\n第4\n第5");
}

// B3：初始选择器带 `!下标`（精华书阁真实形态 `li!0:1:2:3:4@a` / `option!0@value`）
#[test]
fn initial_selector_excludes_indices() {
    let ar = AnalyzeRule::new();
    // 前 5 个 li 是书头信息，后面才是章节
    let html = r#"<ul class="chapter"><li>书头0</li><li>书头1</li><li>书头2</li><li>书头3</li><li>书头4</li><li><a href="/c6">第6章</a></li><li><a href="/c7">第7章</a></li></ul>"#;
    let items = ar.get_string_list(html, "li!0:1:2:3:4@a").unwrap();
    assert_eq!(items.len(), 2, "前 5 个 li 应被 `!0:1:2:3:4` 排除");
    assert_eq!(
        ar.get_string(&items[0], "text", false, "").unwrap(),
        "第6章"
    );
    assert_eq!(ar.get_string(&items[0], "href", true, "").unwrap(), "/c6");

    let html2 =
        r#"<select><option value="">请选择</option><option value="/c1">1</option></select>"#;
    assert_eq!(
        ar.get_string(html2, "option!0@value", true, "").unwrap(),
        "/c1"
    );
}

// B4：裸 `text` / `href`（无选择器前缀）表示取当前节点
#[test]
fn bare_text_and_href_rules() {
    let ar = AnalyzeRule::new();
    let item = r#"<a href="/book/1">书名A</a>"#;
    assert_eq!(ar.get_string(item, "text", false, "").unwrap(), "书名A");
    assert_eq!(ar.get_string(item, "href", true, "").unwrap(), "/book/1");
    // 真实形态：h4 内嵌 a，裸 text 取整个节点的文本
    let h4 = r#"<h4><a href="/b/2">书名B</a></h4>"#;
    assert_eq!(ar.get_string(h4, "text", false, "").unwrap(), "书名B");
}

// B5：`@tag.x` 应命中后代，而不是只看直接子元素（得奇 `tag.h1@tag.a@href`）
#[test]
fn tag_traversal_hits_descendants() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="item"><div class="itemtxt"><p>简介</p><h1><a href="/b/9">书名C</a></h1></div></div>"#;
    // h1 在 div.itemtxt 内部（非直接子元素）
    assert_eq!(
        ar.get_string(html, "@CSS:class.itemtxt@tag.h1@tag.a@href", true, "")
            .unwrap(),
        "/b/9"
    );
    assert_eq!(
        ar.get_string(html, "@CSS:class.itemtxt@tag.h1@tag.a@text", false, "")
            .unwrap(),
        "书名C"
    );
}

#[test]
fn js_segment_transforms() {
    let ar = AnalyzeRule::new();
    let input = "hello world";
    let out = ar
        .get_string(input, "<js>result.toUpperCase()</js>", false, "")
        .unwrap();
    // 纯字符串场景：JS 段把 result 转大写
    assert_eq!(out, "HELLO WORLD");
}

#[test]
fn tag_prefix_selector_works() {
    // 上游规则常见写法 `tag.a@href`：`tag.` 前缀此前被当成非法 CSS 选择器，搜索因此全空
    let ar = AnalyzeRule::new();
    let html = r#"<div class="item"><a href="/b/1">书A</a><span class="au">作者</span></div>"#;
    assert_eq!(ar.get_string(html, "tag.a@href", true, "").unwrap(), "/b/1");
    assert_eq!(ar.get_string(html, "tag.a@text", false, "").unwrap(), "书A");
    assert_eq!(
        ar.get_string(html, "class.au@text", false, "").unwrap(),
        "作者"
    );
}

#[test]
fn text_prefix_selector_matches_own_text() {
    // 上游规则 `text.下一页` = 自身文本包含（jsoup getElementsContainingOwnText）。
    // 147 源中 28 源用它做正文翻页/目录翻页/完整目录入口，此前被当成 CSS
    // 「标签 text + 类名 下一页」而永远为空。
    let ar = AnalyzeRule::new();
    let html = r#"
        <div class="pages">
          <a href="/p/1">上一页</a>
          <a href="/p/3">下一页</a>
        </div>
        <div class="wrap"><a href="/toc">完整目录</a></div>"#;
    assert_eq!(
        ar.get_string(html, "text.下一页@href", true, "").unwrap(),
        "/p/3"
    );
    assert_eq!(
        ar.get_string(html, "text.完整目录@href", true, "").unwrap(),
        "/toc"
    );
}

#[test]
fn text_prefix_uses_own_text_not_descendant_text() {
    // 上游取的是「自身直接文本」：容器只有子元素文本时不能命中，
    // 否则 `text.下一页@href` 会先选中容器并取到空 href。
    let ar = AnalyzeRule::new();
    let html = r#"<div class="wrap"><a href="/p/2">下一页</a></div>"#;
    assert_eq!(
        ar.get_string(html, "text.下一页@href", true, "").unwrap(),
        "/p/2"
    );
    // 容器自身无直接文本 → 不会被选中（选中容器会取到空 href）
    assert_eq!(
        ar.get_string(html, "text.下一页@tag.span@href", true, "")
            .unwrap(),
        ""
    );
}

#[test]
fn text_prefix_is_traversal_step_after_at() {
    // `class.next_chapter@text.下一页@href`（cn.bzmanga 章末翻页的原始写法）。
    let ar = AnalyzeRule::new();
    let html = r#"<div class="next_chapter"><a href="/next">下一页</a></div>"#;
    assert_eq!(
        ar.get_string(html, "class.next_chapter@text.下一页@href", true, "")
            .unwrap(),
        "/next"
    );
}

#[test]
fn text_prefix_handles_empty_and_case_insensitive() {
    let ar = AnalyzeRule::new();
    let html = r#"<a href="/n">Next Page</a><span>无关</span>"#;
    // 大小写不敏感（上游对 needle 与文本都做了 lowercase）
    assert_eq!(
        ar.get_string(html, "text.next page@href", true, "")
            .unwrap(),
        "/n"
    );
    // 未命中 → 空串，不 panic
    assert_eq!(
        ar.get_string(html, "text.没有这段@href", true, "").unwrap(),
        ""
    );
    assert_eq!(ar.get_string("", "text.下一页@href", true, "").unwrap(), "");
}

// pass4 真实日志：JSON 源里“局部模板”展开后的字面量被当成 JSONPath 求值，
// 报“路径必须以 $ 开头”，kind / cover_url / last_chapter 全空（187 条）。
#[test]
fn partial_template_field_is_literal_on_json() {
    let mut ar = AnalyzeRule::new();
    ar.is_json = true;
    let json = r#"{"categoryName":"玄幻","grade":"8.5","chapterNum":"184","NovelCover":"2020/09/x.jpg","title":"书名A"}"#;
    // 免费追书 / 全免漫画的 kind：多段模板拼接
    assert_eq!(
        ar.get_string(json, "{{$.categoryName}}\n{{$.grade}}分", false, "")
            .unwrap(),
        "玄幻\n8.5分"
    );
    // 米读看书的 last_chapter：单花括号模板
    assert_eq!(
        ar.get_string(json, "第{$.chapterNum}章", false, "")
            .unwrap(),
        "第184章"
    );
    // SF 轻小说搜索的 coverUrl：URL 前缀 + 模板（该字段不是 is_url）
    assert_eq!(
        ar.get_string(json, "https://rs.x/{{$.NovelCover}}", false, "")
            .unwrap(),
        "https://rs.x/2020/09/x.jpg"
    );
    // 普通 JSONPath 不受影响
    assert_eq!(ar.get_string(json, "$.title", false, "").unwrap(), "书名A");
}

// 爱轻写真 search.intro：`{{@@.ltim@text}}` 是模板里嵌套的选择器规则，不是 JS。
#[test]
fn template_with_nested_css_rule_expands() {
    let ar = AnalyzeRule::new();
    let html = r#"<li><div class="ltim">2025-01-02</div><h2>标题</h2></li>"#;
    assert_eq!(
        ar.get_string(html, "发布：{{@@.ltim@text}}", false, "")
            .unwrap(),
        "发布：2025-01-02"
    );
    // 展开成 HTML 字面量的模板整体返回原文，不能再当选择器解析成空串
    let intro = ar
        .get_string(
            html,
            r#"<br>📆{{@@.ltim@text}} 发布<br>{"©2025"}"#,
            false,
            "",
        )
        .unwrap();
    assert!(intro.contains("<br>📆2025-01-02 发布"), "intro={intro}");
}

#[test]
fn whole_rule_template_returns_value() {
    // 整条规则恰为 `{{…}}`：上游直接返回模板值；旧实现把展开结果当 CSS，返回空串。
    let ar = AnalyzeRule::new();
    assert_eq!(ar.get_string("", "{{1+1}}", false, "").unwrap(), "2");
    assert_eq!(
        ar.get_string(r#"{"title":"书名A"}"#, "{{$.title}}", false, "")
            .unwrap(),
        "书名A"
    );
    // 展开失败时按原有行为返回空串，不 panic
    assert_eq!(
        ar.get_string("", "{{没有这个变量}}", false, "").unwrap(),
        ""
    );
    // 非“整条模板”的写法不受影响
    assert_eq!(
        ar.get_string("<a href='/x'>下一页</a>", "{{'a'}}@text", false, "")
            .unwrap(),
        "下一页"
    );
}

#[test]
fn js_book_meta_is_visible() {
    // 上游规则的 `book` 绑定是完整 Book 实体；快看/漫画源在详情简介模板里直接读
    // `book.name/book.author/book.kind`，缺这些字段会把 `undefined` 拼进简介。
    let ar = AnalyzeRule::new();
    ar.set_book_meta(BookMeta {
        name: "书名".into(),
        author: "作者".into(),
        kind: "类型".into(),
        intro: "简介".into(),
        cover_url: "https://c/x.jpg".into(),
    });
    assert_eq!(
        ar.get_string("", "{{book.name}}", false, "").unwrap(),
        "书名"
    );
    assert_eq!(
        ar.get_string("", "@js:book.kind + '/' + book.author", false, "")
            .unwrap(),
        "类型/作者"
    );
    assert_eq!(
        ar.get_string("", "@js:book.coverUrl", false, "").unwrap(),
        "https://c/x.jpg"
    );
    // 未注入时是空串而非 `undefined`（避免 JS 模板拼出字面量 undefined）
    assert_eq!(
        AnalyzeRule::new()
            .get_string("", "@js:String(book.name)", false, "")
            .unwrap(),
        ""
    );
}

#[test]
fn whole_rule_js_does_not_recurse() {
    let ar = AnalyzeRule::new();
    // 整条规则以 `@js:` 开头：旧实现在 "||" 切分处把整条规则再喂回 get_string，
    // 无限递归直到栈溢出（小栈 tokio worker 上直接导致进程崩溃）。
    assert!(split_outside_js("@js: 1", "||").is_none());
    let out = ar
        .get_string("", "@js: JSON.stringify([{title:'a',url:'u'}])", false, "")
        .unwrap();
    assert!(out.starts_with('['), "unexpected js result: {out}");
}

#[test]
fn js_runtime_limit_does_not_poison_following_rules() {
    // 单例 Context 复用会让「出错脚本留下的全局状态」污染后续规则；
    // 修复方式是 RuntimeLimit 后重建 Context。这里用全局变量探针固住：
    // 出错脚本先写 `globalThis.__P=1` 再栈溢出，随后必须读不到这个变量。
    let ar = AnalyzeRule::new();
    let bad = "@js:(globalThis.__P=1,(function f(){return f();})())";
    let err = ar.get_string("", bad, false, "").unwrap_err();
    assert!(
        err.to_string().contains("RuntimeLimit"),
        "expected RuntimeLimit, got: {err}"
    );
    // 旧实现会返回 \"number\"（Context 被复用），修复后必须是 \"undefined\"
    assert_eq!(
        ar.get_string("", "@js:typeof __P", false, "").unwrap(),
        "undefined"
    );
    assert_eq!(ar.get_string("", "@js:1+1", false, "").unwrap(), "2");
    // 连续 3 轮：每次都从污染中恢复，不累积
    for round in 0..3 {
        let _ = ar.get_string("", bad, false, "");
        assert_eq!(
            ar.get_string("", "@js:1+1", false, "").unwrap(),
            "2",
            "round {round}: js context still poisoned"
        );
    }
}

#[test]
fn variable_expansion_works() {
    let ar = AnalyzeRule::new();
    ar.set_var("sel", ".name");
    let html = r#"<div class="name">测试书名</div>"#;
    let r = ar.get_string(html, "${sel}", false, "").unwrap();
    assert_eq!(r, "测试书名");
}

// `##正则##` 替换支持 `$1` 捕获组（上游规则原生写法）
#[test]
fn regex_replacement_supports_capture_group() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="n">第12章 标题</div>"#;
    let r = ar
        .get_string(html, r"@css:.n@text##第(\d+)章##Chapter$1", false, "")
        .unwrap();
    assert_eq!(r, "Chapter12 标题");
    // 非正则替换仍然可用
    let r2 = ar
        .get_string(html, "@css:.n@text##第12章##卷", false, "")
        .unwrap();
    assert_eq!(r2, "卷 标题");
}

// `page` 变量可被规则模板引用（多页目录/搜索分页共用）
#[test]
fn page_variable_expands_in_rule() {
    let ar = AnalyzeRule::new();
    ar.set_var("page", "2");
    let html = r#"<div class="page2"><a href="/b">2</a></div>"#;
    // `${page}` 先展开为 2，再作为 CSS 选择器 `.page2 a` 执行
    assert_eq!(
        ar.get_string(html, "@css:.page${page} a@href", true, "")
            .unwrap(),
        "/b"
    );
}

// nextTocUrl 需要“对每个元素抽取属性”，而不是只取首个元素
#[test]
fn list_all_extracts_multiple_hrefs() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="pages"><a href="/toc/2.html">2</a><a href="/toc/3.html">3</a></div>"#;
    let list = ar.get_string_list_all(html, "@css:.pages a@href", "");
    assert_eq!(list, vec!["/toc/2.html", "/toc/3.html"]);
    // 单值规则仍取首个
    assert_eq!(
        ar.get_string(html, "@css:.pages a@href", true, "").unwrap(),
        "/toc/2.html"
    );
}

// @js: 高频内置：md5（含 16 位）、base64、hex、timeFormat、变量
#[test]
fn js_high_frequency_builtins() {
    let ar = AnalyzeRule::new();
    assert_eq!(
        ar.get_string("", "<js>java.md5Encode('abc')</js>", false, "")
            .unwrap(),
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert_eq!(
        ar.get_string("", "<js>java.md5Encode16('abc')</js>", false, "")
            .unwrap(),
        "3cd24fb0d6963f7d"
    );
    let b64 = ar
        .get_string("", "<js>java.base64Encode('中文')</js>", false, "")
        .unwrap();
    assert_eq!(b64, "5Lit5paH");
    assert_eq!(
        ar.get_string("", "<js>java.base64Decode('5Lit5paH')</js>", false, "")
            .unwrap(),
        "中文"
    );
    assert_eq!(
        ar.get_string("", "<js>java.hexEncode('A')</js>", false, "")
            .unwrap(),
        "41"
    );
    assert_eq!(
        ar.get_string("", "<js>java.hexDecode('41')</js>", false, "")
            .unwrap(),
        "A"
    );
    // base64 同族（上游 JsExtensions.kt L676-730 签名）
    assert_eq!(
        ar.get_string(
            "",
            "<js>java.base64Decode('5Lit5paH','UTF-8')</js>",
            false,
            ""
        )
        .unwrap(),
        "中文"
    );
    assert_eq!(
        ar.get_string("", "<js>java.base64Decode('5Lit5paH',2)</js>", false, "")
            .unwrap(),
        "中文"
    );
    // URL-safe 变体（flags=8）编码后可原样解回
    assert_eq!(
        ar.get_string(
            "",
            "<js>var e=java.base64Encode('a?b',8); java.base64Decode(e)</js>",
            false,
            ""
        )
        .unwrap(),
        "a?b"
    );
    // flags=1 去 padding
    assert_eq!(
        ar.get_string("", "<js>java.base64Encode('a',1)</js>", false, "")
            .unwrap(),
        "YQ"
    );
    assert_eq!(
        ar.get_string(
            "",
            "<js>java.base64DecodeToByteArray('5Lit5paH').length</js>",
            false,
            ""
        )
        .unwrap(),
        "6"
    );
    assert_eq!(
        ar.get_string("", "<js>java.hexEncodeToString('中文')</js>", false, "")
            .unwrap(),
        "e4b8ade69687"
    );
    assert_eq!(
        ar.get_string(
            "",
            "<js>java.hexDecodeToString('e4b8ade69687')</js>",
            false,
            ""
        )
        .unwrap(),
        "中文"
    );
    // 毫秒时间戳 + Java 风格 pattern
    assert_eq!(
        ar.get_string(
            "",
            "<js>java.timeFormat(1609459200000, 'yyyy-MM-dd')</js>",
            false,
            ""
        )
        .unwrap(),
        "2021-01-01"
    );
    // 同一次 JS 执行内的变量读写
    assert_eq!(
        ar.get_string(
            "",
            "<js>java.putVariable('k', 'v'); java.getVariable('k')</js>",
            false,
            ""
        )
        .unwrap(),
        "v"
    );
    let n = ar
        .get_string("", "<js>java.randomInt(1, 10)</js>", false, "")
        .unwrap();
    let n: i64 = n.parse().unwrap();
    assert!((1..10).contains(&n));
}

// JSONPath 兼容：递归下降 `$..field`、数组通配 `[*]`、下标 `[n]`（抽检样本直接形态）
#[test]
fn jsonpath_recursive_descent_and_wildcard() {
    let ar = AnalyzeRule::new();
    let json = r#"{"code":0,"data":{"page":1,"books":[{"id":"b1","book_name":"书A"},{"id":"b2","book_name":"书B"}]},"booklist":[{"book_name":"书C"}]}"#;
    let books = ar.get_string_list(json, "$..books[*]").unwrap();
    assert_eq!(books.len(), 2, "递归下降 + [*] 应取到嵌套 books");
    assert!(books[0].contains("b1"));
    let names = ar.get_string_list(json, "$.booklist[*].book_name").unwrap();
    assert_eq!(names, vec!["书C"]);
    // 单值：递归下降命中多个值时，与上游规则一致按 `\n` 拼接（源规则常据此 `split("\n")`）
    assert_eq!(
        ar.get_string(json, "$..book_name", false, "").unwrap(),
        "书C\n书A\n书B"
    );
    // 下标（含负下标）
    assert_eq!(
        ar.get_string(json, "$.data.books[1].id", false, "")
            .unwrap(),
        "b2"
    );
    assert_eq!(
        ar.get_string(json, "$.data.books[-1].id", false, "")
            .unwrap(),
        "b2"
    );
}

// 上游规则用的 Jayway json-path 容忍 `$.data.[*]`（点与方括号混用），爱看目录规则就是这种写法
#[test]
fn jsonpath_tolerates_dot_before_bracket() {
    let ar = AnalyzeRule::new();
    let json = r#"{"data":[{"id":1},{"id":2}]}"#;
    assert_eq!(
        ar.get_string(json, "$.data.[0].id", false, "").unwrap(),
        "1"
    );
    assert_eq!(
        ar.get_string_list(json, "$.data.[*].id").unwrap(),
        vec!["1", "2"]
    );
}

// 不支持的 JSONPath 不再原样抛 PestError，而是可读中文提示
#[test]
fn jsonpath_unsupported_syntax_has_readable_error() {
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string(r#"{"a":1}"#, "$.a..", false, "")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("JSONPath 语法不受支持"),
        "unexpected error: {err}"
    );
    assert!(!err.contains("PestError"), "不应暴露 PestError: {err}");
}

// `{{$.field}}` 模板：列表项字段可拼进字面量 URL 与 POST body
#[test]
fn jsonpath_template_expands_in_url_and_body() {
    let ar = AnalyzeRule::new();
    let item = r#"{"id":"42","book_name":"书A"}"#;
    assert_eq!(
        ar.get_string(item, "https://api.x/read?bid={{$.id}}", true, "")
            .unwrap(),
        "https://api.x/read?bid=42"
    );
    assert_eq!(
        ar.get_string(item, "{\"bid\":\"{{$.id}}\"}", false, "")
            .unwrap(),
        "{\"bid\":\"42\"}"
    );
}

// 切分 JS 时字符串/正则里的 `</js>`、`||`、引号不能截断代码
#[test]
fn js_split_ignores_tags_inside_strings_and_regex() {
    let parts =
        AnalyzeRule::split_js(r#"<js>result.replace("</js>", "").replace(/a||b/g, "")</js>"#);
    assert_eq!(parts.len(), 1, "整段应只有一个 JS 段");
    let code = match &parts[0] {
        (Mode::Js, c) => c.clone(),
        _ => panic!("unexpected part: not a JS segment"),
    };
    assert!(code.contains("</js>"), "字符串里的 </js> 不应截断: {code}");
    let ar = AnalyzeRule::new();
    assert_eq!(
        ar.get_string(
            "x</js>y",
            r#"<js>result.replace("</js>", "")</js>"#,
            false,
            ""
        )
        .unwrap(),
        "xy"
    );
}

// 混合规则：`<js>预处理</js>$..list[*]||$.books[*]`（懒人听书 bookList 的形态）
#[test]
fn js_prefix_then_jsonpath_list_with_fallback() {
    let ar = AnalyzeRule::new();
    let json = r#"{"books":[{"name":"A"},{"name":"B"}]}"#;
    let list = ar
        .get_string_list(json, r#"<js>result;</js>$..list[*]||$.books[*]"#)
        .unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].contains("\"A\""));
    // 纯 JSONPath 的 || 回退：第一条为空时取第二条
    let fallback = ar
        .get_string_list(json, "$..missing[*]||$.books[*]")
        .unwrap();
    assert_eq!(fallback.len(), 2);
}

// java.t2s：常用简→繁映射
#[test]
fn js_t2s_common_chars() {
    let ar = AnalyzeRule::new();
    assert_eq!(
        ar.get_string("", "<js>java.t2s('国学说话书龙')</js>", false, "")
            .unwrap(),
        "國學說話書龍"
    );
}

// java.ajax：网络桥接受私网策略约束（127.0.0.1 应被拒绝且给出可读原因）
#[test]
fn js_ajax_rejects_private_target() {
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string("", "<js>java.ajax('http://127.0.0.1:1/')</js>", false, "")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("拒绝访问") || err.contains("java.ajax"),
        "unexpected error: {err}"
    );
}

/// `java.ajax` 的同步语义不变，但求值发生在专用 JS 线程（非 tokio worker），
/// 因此嵌套 `block_on` 不会在 worker 上触发 "Cannot start a runtime from within
/// a runtime" 的 panic。这里从多线程 runtime 的任务里跑一次，固住该不变式。
#[test]
fn js_ajax_inside_runtime_does_not_panic_on_nested_block_on() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(async {
            tokio::spawn(async {
                AnalyzeRule::new().get_string(
                    "",
                    "<js>java.ajax('http://127.0.0.1:1/')</js>",
                    false,
                    "",
                )
            })
            .await
            .unwrap()
        })
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("拒绝访问") || err.contains("java.ajax"),
        "unexpected error: {err}"
    );
}

// ---------------- JS 调度层：慢 JS 不得占住 tokio worker ----------------

/// 构造一条「可控墙钟耗时」的慢 JS 规则。
///
/// 每轮做一次 64 KiB 字符串分配，保证在 dev 快档（依赖 opt-level=2）下也是墙钟先到、
/// 迭代次数远低于 `loop_iteration_limit`(500k)，而不会先撞上 RuntimeLimit。
fn slow_js_rule(ms: u64) -> String {
    format!(
            "@js:var t0=Date.now(), n=0; while(Date.now()-t0<{ms}) {{ 'abcdefghijklmnop'.repeat(4000); n++; }} n"
        )
}

/// 在独立线程里跑规则，避免 `AnalyzeRule`（!Send）进入 tokio 任务状态机。
fn run_rule_in_thread(rule: String) -> Result<String, RuleError> {
    AnalyzeRule::new().get_string("", &rule, false, "")
}

#[test]
fn slow_js_does_not_block_other_requests() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // 与生产一致的 runtime 形状（main.rs：2 worker + max_blocking_threads 4）
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .unwrap();
    const SLOW_MS: u64 = 500;
    const NORMAL_MS: u64 = 30;
    // 旧实现：两个 worker 都被阻塞，普通请求要等第一条慢 JS 跑完（≈SLOW_MS）
    let latencies = Arc::new(Mutex::new(Vec::<Duration>::new()));

    let lat = latencies.clone();
    rt.block_on(async move {
        // 两个并发请求各命中一条慢 JS 源（单 JS 线程串行执行，两条各 500ms）。
        // 用 ready 信号确保两个 worker 分别进入 JS 求值后，再发起普通请求，
        // 否则任务可能集中在一个 worker 的本地队列里，无法复现“两个 worker 同时被占”。
        let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel::<()>(2);
        let mut slow = Vec::new();
        for _ in 0..2 {
            let rule = slow_js_rule(SLOW_MS);
            let tx = ready_tx.clone();
            slow.push(tokio::spawn(async move {
                let _ = tx.send(()).await;
                run_rule_in_thread(rule)
            }));
        }
        drop(ready_tx);
        let _ = ready_rx.recv().await;
        let _ = ready_rx.recv().await;
        // 两个 worker 此刻都在 JS 里同步阻塞；block_on 线程用 std sleep，
        // 不依赖 timer driver（此时它可能没线程来驱动）。
        std::thread::sleep(Duration::from_millis(20));

        // 普通请求：只需要一个空闲 worker 就能按时完成。
        // 计时从 spawn 前开始，包含“排队等 worker”的时间（旧实现的真正代价）。
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let lat2 = lat.clone();
        let normal_started = Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(NORMAL_MS)).await;
            lat2.lock().unwrap().push(normal_started.elapsed());
            let _ = done_tx.send(());
        });
        let _ = done_rx.await;

        for h in slow {
            let _ = h.await;
        }
    });

    let latency = *latencies.lock().unwrap().first().expect("普通请求未执行");
    let budget = Duration::from_millis(SLOW_MS / 2);
    assert!(
        latency < budget,
        "普通请求被慢 JS 拖慢：{latency:?} >= {budget:?}（SLOW_MS={SLOW_MS}）"
    );
}

/// 未调用 `init_js` 时（单测/规则试跑等）必须采用配置模块的默认预算，
/// 不再回落到早期写死的 200ms（否则腾讯漫画图片规则会被误判超时）。
#[test]
fn uninitialized_js_conf_uses_config_default_budget() {
    assert_eq!(
        js_conf().timeout_ms,
        crate::config::DEFAULT_JS_TIMEOUT_MS,
        "JsConf 默认预算应与 config::DEFAULT_JS_TIMEOUT_MS 一致"
    );
}

/// 超预算行为保持不变：报错文案含 exceeded/budget，且不污染后续求值。
/// 这里用 `#[tokio::test]`（current_thread）同时验证降级路径不会 panic。
#[tokio::test]
async fn slow_js_rule_reports_budget_timeout() {
    // 测试进程未调用 init_js：预算为默认值，慢规则比默认预算多跑 500ms 以确保超限。
    let over_budget_ms = crate::config::DEFAULT_JS_TIMEOUT_MS + 500;
    let err = AnalyzeRule::new()
        .get_string("", &slow_js_rule(over_budget_ms), false, "")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("exceeded") && err.contains("budget"),
        "unexpected error: {err}"
    );
    assert_eq!(
        AnalyzeRule::new()
            .get_string("", "@js:1+1", false, "")
            .unwrap(),
        "2"
    );
}

// ---------------- 选择器扩展 ----------------

const NEWS_HTML: &str = r#"<ul class="news-list">
          <li class="item"><a href="/1.html">第一章</a></li>
          <li class="item"><a href="/2.html">第二章</a></li>
          <li class="item"><a href="/3.html">第三章</a></li>
          <li class="item"><a href="/4.html">第四章</a></li>
        </ul>"#;

#[test]
fn jquery_pseudo_first_last_eq() {
    let ar = AnalyzeRule::new();
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:first@a@text", false, "")
            .unwrap(),
        "第一章"
    );
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:last@a@text", false, "")
            .unwrap(),
        "第四章"
    );
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:eq(1)@a@text", false, "")
            .unwrap(),
        "第二章"
    );
    // 上游 elementSiblingIndex 语义不是 jQuery：负下标不从尾取
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:eq(-1)@a@text", false, "")
            .unwrap(),
        ""
    );
}

#[test]
fn jquery_pseudo_lt_gt_and_contains() {
    let ar = AnalyzeRule::new();
    // get_string_list 的 CSS 模式返回元素 HTML（与既有契约一致），按 href 校验结果集
    let lt = ar.get_string_list(NEWS_HTML, "li:lt(2)@a").unwrap();
    assert_eq!(lt.len(), 2, "实际: {lt:?}");
    assert!(lt[0].contains("/1.html") && lt[1].contains("/2.html"));
    let gt = ar.get_string_list(NEWS_HTML, "li:gt(1)@a").unwrap();
    assert_eq!(gt.len(), 2, "实际: {gt:?}");
    assert!(gt[0].contains("/3.html") && gt[1].contains("/4.html"));
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:contains(第三章)@a@text", false, "")
            .unwrap(),
        "第三章"
    );
    // 遍历段里的伪类（@li:eq）同样生效
    assert_eq!(
        ar.get_string(NEWS_HTML, "ul@li:eq(3)@a@text", false, "")
            .unwrap(),
        "第四章"
    );
}

// jsoup 扩展伪类在规则 CSS 路径同样生效，且语义对齐 jsoup：
// `:contains` 不分大小写、`:containsOwn` 只看自身文本、`:has` 看后代。
#[test]
fn jsoup_pseudo_classes_full_semantics() {
    let ar = AnalyzeRule::new();
    // 大小写不敏感（jsoup Element.text() 包含匹配）
    let html = r#"<div id="x">Hello World</div>"#;
    assert_eq!(
        ar.get_string(html, "div:contains(hello)@text", false, "")
            .unwrap(),
        "Hello World"
    );
    // contains 命中后代文本；containsOwn 只认自身直接文本
    let nested = r#"<div id="y"><span>子文本</span></div>"#;
    assert_eq!(
        ar.get_string(nested, "div:contains(子文本)@span@text", false, "")
            .unwrap(),
        "子文本"
    );
    assert_eq!(
        ar.get_string(nested, "div:containsOwn(子文本)", false, "")
            .unwrap(),
        ""
    );
    // :has 要求存在匹配后代
    let has = r#"<div class="a"><p>正文</p></div><div class="b"><span>x</span></div>"#;
    assert_eq!(
        ar.get_string(has, "div:has(p)@p@text", false, "").unwrap(),
        "正文"
    );
    assert_eq!(ar.get_string(has, "div.b:has(p)", false, "").unwrap(), "");
    // :eq 是 elementSiblingIndex 语义，不支持负数从尾取
    assert_eq!(
        ar.get_string(NEWS_HTML, "li:eq(-2)@a@text", false, "")
            .unwrap(),
        ""
    );
}

// 上游 jsoup 的 :eq/:lt/:gt 比较 elementSiblingIndex（父节点下元素序号），
// 不是结果集下标、也不支持负数从尾取。用混合子节点验证与 jQuery 语义的差异。
#[test]
fn jsoup_index_pseudos_use_element_sibling_index() {
    let ar = AnalyzeRule::new();
    // ul 下元素顺序：li(0) span(1) li(2) li(3)
    let html = r#"<ul><li>a</li><span>b</span><li>c</li><li>d</li></ul>"#;
    // jQuery 结果集下标 :eq(2) 会取到 d；jsoup 取元素序号为 2 的 c
    assert_eq!(
        ar.get_string(html, "li:eq(2)@text", false, "").unwrap(),
        "c"
    );
    // 越界序号无匹配
    assert_eq!(ar.get_string(html, "li:eq(9)@text", false, "").unwrap(), "");
    // 负值不从尾取：eq/lt 无匹配，gt 匹配全部非根元素
    assert_eq!(
        ar.get_string(html, "li:eq(-1)@text", false, "").unwrap(),
        ""
    );
    assert_eq!(
        ar.get_string(html, "li:lt(-1)@text", false, "").unwrap(),
        ""
    );
    let gt = ar.get_string_list(html, "li:gt(-1)").unwrap();
    assert_eq!(gt.len(), 3, "实际: {gt:?}");
    // :lt(2) 只保留元素序号 0 的 a（jQuery 会取前两个结果 a、c）；
    // get_string_list 的 CSS 模式返回元素 HTML（既有契约），故按 HTML 断言。
    let lt = ar.get_string_list(html, "li:lt(2)@text").unwrap();
    assert_eq!(
        lt,
        vec!["<li>a</li>".to_string()],
        "实际: {lt:?} (元素序号 2 的 c 不满足 <2)"
    );
}

#[test]
fn attribute_tokens_and_text_are_boundary_safe() {
    let ar = AnalyzeRule::new();
    let html = r#"<a class="link">链接文字</a>"#;
    // 属性不存在 → 空串，不回退成整节点文本
    assert_eq!(ar.get_string(html, "@href", false, "").unwrap(), "");
    assert_eq!(ar.get_string(html, "@data-id", false, "").unwrap(), "");
    assert_eq!(ar.get_string(html, ".link@src", false, "").unwrap(), "");
    // text 与 @text 行为一致
    assert_eq!(ar.get_string(html, "text", false, "").unwrap(), "链接文字");
    assert_eq!(ar.get_string(html, "@text", false, "").unwrap(), "链接文字");
    // 存在的任意属性正常取到
    let with_attr = r#"<a class="link" data-id="42" href="/x">详情</a>"#;
    assert_eq!(
        ar.get_string(with_attr, ".link@data-id", false, "")
            .unwrap(),
        "42"
    );
}

#[test]
fn class_and_id_shorthand_are_selectors() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="item"><span class="title">测试书名</span></div>
                      <div id="box"><p>简介内容</p></div>"#;
    assert_eq!(
        ar.get_string(html, "class.item@class.title@text", false, "")
            .unwrap(),
        "测试书名"
    );
    assert_eq!(
        ar.get_string(html, "id.box@tag.p@text", false, "").unwrap(),
        "简介内容"
    );
}

// ---- P1：`class.a b` 是 AND（同时具备），不是 CSS 的后代选择器 ----

#[test]
fn p1_class_multi_name_is_and_not_descendant() {
    let ar = AnalyzeRule::new();
    // 初始选择器：只有同时具备 author/extra 的节点命中
    let html = r#"<div class="author extra"><span>甲</span></div>
                      <div class="author"><span>乙</span></div>"#;
    assert_eq!(
        ar.get_string(html, "class.author extra@text", false, "")
            .unwrap()
            .trim(),
        "甲"
    );
    // 下标解析必须发生在归一化之前：`class.author extra.-1` 取最后一个命中
    let html2 = r#"<div class="author extra">A</div><div class="author extra">B</div><div class="author">C</div>"#;
    assert_eq!(
        ar.get_string(html2, "class.author extra.-1@text", false, "")
            .unwrap(),
        "B"
    );
    // 遍历标记路径（parse_token）同样按 AND 处理
    let html3 = r#"<div class="item"><p class="author extra">作者甲</p></div>
                       <div class="item"><p class="author">作者乙</p></div>"#;
    assert_eq!(
        ar.get_string(html3, "class.item@class.author extra@text", false, "")
            .unwrap(),
        "作者甲"
    );
}

// ---- P2：首段裸 `tag.N` / `tag.-N` ----

#[test]
fn p2_bare_tag_index_in_initial_selector() {
    let ar = AnalyzeRule::new();
    let html =
        r#"<div class="list"><a href="/1">一</a><a href="/2">二</a><a href="/3">三</a></div>"#;
    assert_eq!(ar.get_string(html, "a.0@href", true, "").unwrap(), "/1");
    assert_eq!(ar.get_string(html, "a.-1@href", true, "").unwrap(), "/3");
    // 等价写法不得回归
    assert_eq!(ar.get_string(html, "tag.a.0@href", true, "").unwrap(), "/1");
    assert_eq!(ar.get_string(html, "a[0]@href", true, "").unwrap(), "/1");
}

// ---------------- JSONPath 谓词 / 回退链 / @json: ----------------

const BOOKS_JSON: &str = r#"{"data":{"list":[
          {"title":"A","author":"天蚕土豆","price":12.5,"score":4},
          {"title":"B","author":"天蚕土豆","price":8.0,"score":4},
          {"title":"C","author":"我吃西红柿","price":20,"score":4},
          {"title":"D","author":"天蚕土豆","price":12.5,"score":6}
        ]}}"#;

#[test]
fn jsonpath_predicate_equality() {
    let ar = AnalyzeRule::new();
    let titles = ar
        .get_string_list(BOOKS_JSON, "$.data.list[?(@.author=='天蚕土豆')].title")
        .unwrap();
    assert_eq!(titles, vec!["A", "B", "D"]);
    // 方括号字段语法
    let titles2 = ar
        .get_string_list(
            BOOKS_JSON,
            "$.data.list[?(@['author']=='我吃西红柿')].title",
        )
        .unwrap();
    assert_eq!(titles2, vec!["C"]);
    // 数值比较
    let cheap = ar
        .get_string_list(BOOKS_JSON, "$.data.list[?(@.price<10)].title")
        .unwrap();
    assert_eq!(cheap, vec!["B"]);
}

#[test]
fn jsonpath_predicate_compare_and_and() {
    let ar = AnalyzeRule::new();
    let hit = ar
        .get_string_list(
            BOOKS_JSON,
            "$.data.list[?(@.author=='天蚕土豆' && @.price>=10)].title",
        )
        .unwrap();
    assert_eq!(hit, vec!["A", "D"]);
    let none = ar
        .get_string_list(
            BOOKS_JSON,
            "$.data.list[?(@.author=='我吃西红柿' && @.price<10)].title",
        )
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn jsonpath_predicate_or() {
    let ar = AnalyzeRule::new();
    let hit = ar
        .get_string_list(
            BOOKS_JSON,
            "$.data.list[?(@.author=='我吃西红柿' || @.score>=6)].title",
        )
        .unwrap();
    assert_eq!(hit, vec!["C", "D"]);
}

#[test]
fn jsonpath_fallback_chain_and_at_json_prefix() {
    let ar = AnalyzeRule::new();
    let chain = ar
        .get_string_list(
            BOOKS_JSON,
            "$.missing.list[*]||$.data.missing[*]||$.data.list[*].title",
        )
        .unwrap();
    assert_eq!(chain, vec!["A", "B", "C", "D"]);
    // @json: 前缀与裸 JSONPath 等价
    let prefixed = ar
        .get_string_list(
            BOOKS_JSON,
            "@json:$.data.list[?(@.author=='天蚕土豆')].title",
        )
        .unwrap();
    assert_eq!(prefixed, vec!["A", "B", "D"]);
}

#[test]
fn jsonpath_unsupported_predicate_has_named_error() {
    let ar = AnalyzeRule::new();
    let err = ar
        .get_string_list(BOOKS_JSON, "$.data.list[?(@.title==)].title")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("JSONPath 语法不受支持"),
        "unexpected error: {err}"
    );
    assert!(!err.contains("PestError"), "不应暴露 PestError: {err}");
}

// ---- P0-7：模式前缀大小写不敏感 ----

#[test]
fn p0_7_mode_prefix_is_case_insensitive() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="list">ABC</div>"#;
    for rule in [
        "@CSS:div.list@text",
        "@Css:div.list@text",
        "@css:div.list@text",
    ] {
        assert_eq!(
            ar.get_string(html, rule, false, "").unwrap(),
            "ABC",
            "{rule}"
        );
    }
    let json = r#"{"title":"T"}"#;
    for rule in ["@Json:$.title", "@JSON:$.title", "@json:$.title"] {
        assert_eq!(ar.get_string(json, rule, false, "").unwrap(), "T", "{rule}");
    }
    let xml = "<root><name>N</name></root>";
    for rule in ["@XPath:/root/name/text()", "@XPATH:/root/name/text()"] {
        assert_eq!(ar.get_string(xml, rule, false, "").unwrap(), "N", "{rule}");
    }
    assert_eq!(ar.get_string("", "@JS:1+1", false, "").unwrap(), "2");
}

// ---- P0-1：列表规则不再折叠为单值 ----

#[test]
fn p0_1_fallback_returns_full_list_not_first_value() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="list"><li>A</li><li>B</li><li>C</li></div><div class="box"></div>"#;
    let v = ar.get_string_list_all(html, ".list li@text||.box li@text", "");
    assert_eq!(v, vec!["A", "B", "C"]);
}

#[test]
fn p0_1_regex_replace_applies_per_list_item() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="list"><a href="/1?from=home">1</a><a href="/2?from=list">2</a><a href="/3?from=home">3</a></div>"#;
    let v = ar.get_string_list_all(html, ".list a@href##\\?from=home", "");
    assert_eq!(v, vec!["/1", "/2?from=list", "/3"]);
}

#[test]
fn p0_1_and_merges_lists_in_list_mode() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="a"><li>A1</li><li>A2</li><li>A3</li></div><div class="b"><li>B1</li><li>B2</li><li>B3</li></div>"#;
    let v = ar.get_string_list_all(html, ".a li@text&&.b li@text", "");
    assert_eq!(v, vec!["A1", "A2", "A3", "B1", "B2", "B3"]);
}

// ---- P0-2：`&&` 多值用 `\n` 分隔 ----

#[test]
fn p0_2_and_joins_values_with_newline() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="a">A</div><div class="b">B</div>"#;
    assert_eq!(
        ar.get_string(html, ".a@text&&.b@text", false, "").unwrap(),
        "A\nB"
    );
    let json = r#"{"a":"A","b":"B"}"#;
    assert_eq!(ar.get_string(json, "$.a&&$.b", false, "").unwrap(), "A\nB");
}

// ---- P0-3：`##` 三种形态 ----

#[test]
fn p0_3_regex_replace_delete_all() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="n">第12章 标题</div>"#;
    assert_eq!(
        ar.get_string(html, ".n@text##第(\\d+)章", false, "")
            .unwrap()
            .trim(),
        "标题"
    );
}

#[test]
fn p0_3_regex_replace_all_and_first_only() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="n">第12章 甲 第34章 乙</div>"#;
    // 三段：全替换
    assert_eq!(
        ar.get_string(html, ".n@text##第(\\d+)章##第$1节", false, "")
            .unwrap(),
        "第12节 甲 第34节 乙"
    );
    // 四段（尾部 `###`）：只替换第一个匹配，替换片段之后的原文必须保留
    assert_eq!(
        ar.get_string(html, ".n@text##第(\\d+)章##第$1节###", false, "")
            .unwrap(),
        "第12节 甲 第34章 乙"
    );
    // 无匹配时原样返回（而不是丢弃整串）
    assert_eq!(
        ar.get_string(html, ".n@text##第(\\d+)卷##第$1节###", false, "")
            .unwrap(),
        "第12章 甲 第34章 乙"
    );
}

// ---- P0-4：负索引 ----

#[test]
fn p0_4_negative_index_forms() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="list"><li>A</li><li>B</li><li>C</li></div>"#;
    assert_eq!(
        ar.get_string(html, "class.list@tag.li.-1@text", false, "")
            .unwrap(),
        "C"
    );
    let pics = r#"<div class="pic"><img src="1.jpg"><img src="2.jpg"><img src="3.jpg"></div>"#;
    assert_eq!(
        ar.get_string(pics, "class.pic@tag.img[-1]@src", false, "")
            .unwrap(),
        "3.jpg"
    );
    assert_eq!(
        ar.get_string_list_all(pics, "class.pic@tag.img[-2:]@src", ""),
        vec!["2.jpg", "3.jpg"]
    );
}

// ---- P0-5：HTML 实体反转义（只做一次） ----

#[test]
fn p0_5_html_entities_unescaped_once() {
    let ar = AnalyzeRule::new();
    let json = r#"{"title":"A &amp; B"}"#;
    assert_eq!(ar.get_string(json, "$.title", false, "").unwrap(), "A & B");
    // html5ever 已解一层，最终再解一层：`&amp;amp;` -> `&amp;` -> `&`
    let html = r#"<a class="x" href="/s?q=1&amp;amp;p=2">link</a>"#;
    assert_eq!(
        ar.get_string(html, ".x@href", false, "").unwrap(),
        "/s?q=1&p=2"
    );
}

// ---- P0-6：`{{…}}` 支持 JS 表达式 ----

#[test]
fn p0_6_template_js_expression() {
    let ar = AnalyzeRule::new();
    ar.set_var("page", "2");
    assert_eq!(
        ar.get_string("", "https://x/list/{{page*20}}", true, "")
            .unwrap(),
        "https://x/list/40"
    );
    assert_eq!(
        ar.get_string("", "https://x/list/{{(page-1)*20+10}}", true, "")
            .unwrap(),
        "https://x/list/30"
    );
    ar.set_var("key", "abc");
    assert_eq!(
        ar.get_string("", "https://x/{{java.base64Encode(key)}}", true, "")
            .unwrap(),
        "https://x/YWJj"
    );
}

// ---- P0-8：CSS 区间包含端点 / 步长 ----

#[test]
fn p0_8_css_range_is_inclusive_with_step() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="pic">
            <img src="1"><img src="2"><img src="3"><img src="4"><img src="5"><img src="6">
        </div>"#;
    // 闭区间：0..=2 共 3 个
    assert_eq!(
        ar.get_string_list_all(html, "class.pic@tag.img[0:2]@src", ""),
        vec!["1", "2", "3"]
    );
    // 负区间：最后 3 个
    assert_eq!(
        ar.get_string_list_all(html, "class.pic@tag.img[-3:]@src", ""),
        vec!["4", "5", "6"]
    );
    // 步长：0,2,4
    assert_eq!(
        ar.get_string_list_all(html, "class.pic@tag.img[::2]@src", ""),
        vec!["1", "3", "5"]
    );
    // 倒序：[-1:0] 全部反转
    assert_eq!(
        ar.get_string_list_all(html, "class.pic@tag.img[-1:0]@src", ""),
        vec!["6", "5", "4", "3", "2", "1"]
    );
}

// ---- F3：`.class.N` 点前缀索引（上游 findIndexSet 把尾部 `.N` 拆成下标） ----

#[test]
fn f3_dot_prefix_index_in_initial_selector() {
    let ar = AnalyzeRule::new();
    let html =
        r#"<div><span class="author">作者甲</span><span class="author">字数：1.0万</span></div>"#;
    // 元素索引解析：`.author.0` -> beforeRule=`.author`、split='.'、index=[0]
    assert_eq!(
        ar.get_string(html, ".author.0@text", false, "").unwrap(),
        "作者甲"
    );
    assert_eq!(
        ar.get_string(html, ".author.1@text", false, "").unwrap(),
        "字数：1.0万"
    );
    // 与 `##` 替换组合（case 0/99 的实际写法）
    assert_eq!(
        ar.get_string(html, ".author.0@text##作者：", false, "")
            .unwrap(),
        "作者甲"
    );
}

// ---- F4：`class.x.y` 三段链只取第一个类名分段（上游 rules[1]） ----

#[test]
fn f4_class_chain_uses_first_segment_like_upstream() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="list"><div class="list_tit">目录</div><ul>
            <li><a href="/1">第一章</a></li><li><a href="/2">第二章</a></li></ul></div>"#;
    // 集合级元素抽取：beforeRule.split(".") 后 class 分支只用 rules[1]="list"
    let chapters = ar.get_string_list(html, "class.list.ul@li").unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(
        ar.get_string(&chapters[0], "a@text", false, "").unwrap(),
        "第一章"
    );
    assert_eq!(
        ar.get_string(&chapters[1], "a@href", false, "").unwrap(),
        "/2"
    );
}

// ---- F5：`@text` 块级元素之间补空格（jsoup Element.text() 行为） ----

#[test]
fn f5_text_inserts_space_between_block_elements() {
    let ar = AnalyzeRule::new();
    let html = r#"<div class="text"><p>二月二，龙抬头。</p><p>暮色里，小镇名叫泥瓶巷。</p></div>"#;
    assert_eq!(
        ar.get_string(html, "class.text@text", false, "").unwrap(),
        "二月二，龙抬头。 暮色里，小镇名叫泥瓶巷。"
    );
}

// ---------------- 慢 JS 隔离 ----------------

// 在途名额满额后只等一个短窗口就快速失败（而不是排到前一条慢规则跑完）。
#[test]
fn js_gate_fast_fails_when_saturated() {
    let gate = JsGate::new();
    let held = gate.acquire(1, Duration::ZERO).expect("首个名额");
    let t = Instant::now();
    assert!(
        gate.acquire(1, Duration::from_millis(120)).is_none(),
        "满额时必须在等待窗口后返回 None"
    );
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(100),
        "应先等满窗口再放弃，实际 {waited:?}"
    );
    assert!(
        waited < Duration::from_millis(600),
        "失败必须快，实际 {waited:?}"
    );
    drop(held);
    assert!(
        gate.acquire(1, Duration::from_millis(50)).is_some(),
        "名额归还后应能重新拿到"
    );
}

// 慢 JS 只许占住 JS 线程，不许饿死 tokio worker：等价 /healthz 的轻任务必须持续被调度。
#[test]
fn slow_js_does_not_starve_multi_thread_runtime() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(512 * 1024)
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime");
    // 40000 次拼接在压测机上约 5.3s；15000 次约 0.7s（平方缩放），两条脚本串行耗时足够覆盖心跳窗口。
    let slow =
        "@js:var s='';for(var i=0;i<15000;i++){s=s+'boranlib-slow-js-padding-0123456789';}s.length";
    let worst = rt.block_on(async move {
        // 模拟搜索路径：在 async 任务里同步调用规则引擎，占住 2 个 worker
        for _ in 0..2 {
            let code = slow.to_string();
            tokio::spawn(async move {
                let ar = AnalyzeRule::new();
                let _ = ar.get_string("", &code, false, "");
            });
        }
        // 等价 /healthz 的心跳：每 50ms 一次，记录最大调度间隔
        let mut last = Instant::now();
        let mut worst = Duration::ZERO;
        for _ in 0..24 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let now = Instant::now();
            worst = worst.max(now - last);
            last = now;
        }
        worst
    });
    assert!(
        worst < Duration::from_millis(500),
        "慢 JS 占住了 worker，心跳最大间隔 {worst:?}"
    );
}

// ---- pass6：source.put/get 与 cache.getFile/putFile（懒人听书真实 JS） ----

#[test]
fn p6_source_put_get_roundtrip_across_instances() {
    // 真实规则：`source.put('bookIds', bookIds)`（数组）→ 后续 `String(source.get('bookIds')).split(",")`。
    // 上游规则的 put 参数是 String，Rhino 会把数组 toString 成逗号串；本服务同口径。
    let ar1 = AnalyzeRule::new();
    ar1.get_string("", "<js>source.put('p6ids', [101,102,103])</js>", false, "")
        .unwrap();
    let ar2 = AnalyzeRule::new();
    assert_eq!(
        ar2.get_string("", "<js>source.get('p6ids')</js>", false, "")
            .unwrap(),
        "101,102,103"
    );
    // 缺省返回空串（上游规则 BaseSource.get 的 `?: ""`）
    assert_eq!(
        ar2.get_string("", "<js>source.get('p6-missing')</js>", false, "")
            .unwrap(),
        ""
    );
    // put 返回写入值（上游规则返回 value）
    assert_eq!(
        ar1.get_string("", "<js>source.put('p6ret','x')</js>", false, "")
            .unwrap(),
        "x"
    );
}

#[test]
fn p6_cache_file_put_get_is_sandboxed() {
    // 落盘目录只来自服务端固定配置；key 被 md5 成文件名，不能当路径用。
    let dir = std::env::temp_dir().join(format!("boranlib-rule-cache-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    init_rule_cache_dir(dir.clone());
    let ar = AnalyzeRule::new();
    assert_eq!(
        ar.get_string("", "<js>cache.putFile('p6file','hello')</js>", false, "")
            .unwrap(),
        ""
    );
    assert_eq!(
        ar.get_string("", "<js>cache.getFile('p6file')</js>", false, "")
            .unwrap(),
        "hello"
    );
    assert_eq!(
        ar.get_string("", "<js>cache.getFile('p6-nope')</js>", false, "")
            .unwrap(),
        ""
    );
    // 穿越型 key 也不会读到任意部署机文件
    assert_eq!(
        ar.get_string("", "<js>cache.getFile('../../etc/passwd')</js>", false, "")
            .unwrap(),
        ""
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- pass6：`{{规则##正则[##替换[###]]}}` 与上游规则对齐 ----

#[test]
fn p6_template_jsonpath_replace_delete() {
    // 免费追书/♨️免费追书 真实规则 `{{$.lastUpdateChapterDate##\s.*}}`：
    // 单段 `##` = 正则替换为空（此前 `##…` 被当作 JSONPath 的一部分，整段变空）。
    let ar = AnalyzeRule::new();
    let json = r#"{"lastUpdateChapterDate":"2026-11-05 12:00:00"}"#;
    assert_eq!(
        ar.get_string(json, r"{{$.lastUpdateChapterDate##\s.*}}", true, "")
            .unwrap(),
        "2026-11-05"
    );
}

#[test]
fn p6_template_jsonpath_replace_with_replacement() {
    // 两段 `##pat##repl` = 全替换；含 `$.tags..name` 深扫描路径。
    let ar = AnalyzeRule::new();
    let json = r#"{"lastUpdateChapterDate":"2026-11-05 12:00:00","tags":[{"name":"奇幻 冒险"}]}"#;
    assert_eq!(
        ar.get_string(json, r"{{$.lastUpdateChapterDate##\s##🔸}}", true, "")
            .unwrap(),
        "2026-11-05🔸12:00:00"
    );
    assert_eq!(
        ar.get_string(json, r"{{$.tags..name##\s##，}}", true, "")
            .unwrap(),
        "奇幻，冒险"
    );
}

#[test]
fn p6_template_jsonpath_replace_first_only() {
    // 快看漫画 真实写法 `…##(\d+)##$1章###`：第 4 段存在 → 只替换第一个匹配。
    let ar = AnalyzeRule::new();
    let json = r#"{"data":[{"topicInfo":{"topic_intro":"12 34 56"}}]}"#;
    assert_eq!(
        ar.get_string(
            json,
            r"{{$.data[0].topicInfo.topic_intro##(\d+)##$1章###}}",
            true,
            ""
        )
        .unwrap(),
        "12章 34 56"
    );
}

#[test]
fn p6_template_nested_css_replace() {
    // 免费追书 真实规则 `{{@@[property="og:novel:update_time"]@content##\s##🔸}}`。
    let ar = AnalyzeRule::new();
    let html = r#"<p property="og:novel:update_time" content="2026-11-05 12:00:00"></p>"#;
    assert_eq!(
        ar.get_string(
            html,
            r#"{{@@[property="og:novel:update_time"]@content##\s##🔸}}"#,
            false,
            ""
        )
        .unwrap(),
        "2026-11-05🔸12:00:00"
    );
}

#[test]
fn p6_composed_literal_with_trailing_replace() {
    // 免费追书 intro：字面量前缀 + 模板 + 整串 `##…##$1<br>`。
    // 前半段展开后是字面量，不能再当选择器解析（此前整条 intro 变空）。
    let ar = AnalyzeRule::new();
    let json = r#"{"introduction":"第一句。第二句！第三句？"}"#;
    let rule = r#"📂 简介：{{$.introduction}}##(^|[。！？]+[”」）】]?)##$1<br>"#;
    assert_eq!(
        ar.get_string(json, rule, true, "").unwrap(),
        "<br>📂 简介：第一句。<br>第二句！<br>第三句？<br>"
    );
}
