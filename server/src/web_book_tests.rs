//! web_book 单测（move-only 从 web_book.rs 拆出）。

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
        r#"https://www.dingdian888.com/s.php,{"method":"POST","body":"s={{key}}&submit="}"#.into(),
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
    let html = r#"<div class="pages"><a href="/toc/2.html">2</a><a href="/toc/3.html">3</a></div>"#;
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
    let out = apply_legado_replacements("第12章 abc", &Some(r#"第(\d+)章##Chapter$1"#.to_string()));
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
    let out = search_all_with_errors_budgeted(&sources, "书", 1, Duration::from_millis(300)).await;
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
    let item =
        r#"<div class="ch"><i class="vip">true</i><i class="pay">1</i><i class="free">0</i></div>"#;
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
                "var out=[];for(var i=result.length-1;i>=0;i--){out.push(result[i]);}out".into(),
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
            book_list: Some("$.data.topics||$.data.cards||$.hits.topicMessageList||$.hits".into()),
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

    let qm_body =
        r#"{"data":{"list":[{"id":3095,"url":"/comic/3095"},{"id":23833,"url":"/comic/23833"}]}}"#;
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
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
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
