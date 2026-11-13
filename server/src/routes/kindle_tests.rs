//! /k 路由单测（move-only 从 kindle.rs 拆出）。

use super::*;

/// `/k` 默认 curated（首屏必须快）；只有显式 all/expanded 才走全量。
#[test]
fn k_scope_defaults_to_curated() {
    assert_eq!(k_scope(None), crate::req::SearchScope::Curated);
    assert_eq!(k_scope(Some("")), crate::req::SearchScope::Curated);
    assert_eq!(k_scope(Some("unknown")), crate::req::SearchScope::Curated);
    assert_eq!(k_scope(Some("curated")), crate::req::SearchScope::Curated);
    assert_eq!(k_scope(Some(" all ")), crate::req::SearchScope::All);
    assert_eq!(k_scope(Some("ENHANCED")), crate::req::SearchScope::All);
}

/// 搜索中页：自动刷新、进度计数、任务/冷却参数都要保留。
#[tokio::test]
async fn search_progress_page_refreshes_and_keeps_job_context() {
    let p = crate::routes::books::JobProgress {
        key: "诡秘之主".into(),
        source_total: 12,
        finished: 3,
        pending: 9,
        skipped: 1,
        cancelled: 0,
        failed: 1,
    };
    let res = progress_page("job-1", &p, true, 2, false, vp());
    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(
        html.contains("http-equiv=\"refresh\""),
        "进度页必须自动刷新: {html}"
    );
    assert!(html.contains("已完成 3 / 12 个源"), "缺少进度: {html}");
    assert!(html.contains("进行中 9 个"), "缺少进行中: {html}");
    assert!(html.contains("失败 1 个"), "缺少失败数: {html}");
    assert!(html.contains("跳过 1 个"), "缺少跳过数: {html}");
    assert!(html.contains("job=job-1"), "链接必须保留任务: {html}");
    assert!(
        html.contains("include_cooled=1"),
        "链接必须保留冷却参数: {html}"
    );
}

/// 默认 KPW3 视口（1072×1268）：所有历史几何断言都以它为基准。
fn vp() -> Vp {
    Vp::default()
}
/// 指定尺寸的「已带参」视口（fixed = true，meta viewport 锁宽）。
fn vps(w: usize, h: usize) -> Vp {
    Vp { w, h, fixed: true }
}
/// 某档字号在默认视口下的每页行预算。
fn budget(spec: &FsSpec) -> usize {
    line_budget(vp(), spec.line)
}
/// 默认视口下的切片参数（与 `comic_read` 推出来的完全一致：漫画可用高再扣 56 顶栏）。
fn slice_p() -> image::SliceParams {
    image::SliceParams::from_config(
        &test_cfg(true),
        Some(vp().content_w() as u32),
        Some(vp().content_h().saturating_sub(READ_NAV_H) as u32),
        None,
    )
}

#[test]
fn paginate_lines_respects_budget_and_char_boundaries() {
    // 行预算由视口高动态算出（标准档 1088/52 = 20 行）：前 4 段占 16 行，
    // 第 5 段再放会超预算，余文进第 2 页。
    let text = "字".repeat(100)
        + "\n"
        + &"字".repeat(100)
        + "\n"
        + &"字".repeat(100)
        + "\n"
        + &"字".repeat(100)
        + "\n"
        + &"字".repeat(100)
        + "\n"
        + &"字".repeat(100);
    let spec = &FS_SPECS[1];
    let lines = budget(spec);
    let pages = paginate_lines(&text, spec, lines, vp().page_w());
    assert_eq!(pages.len(), 2, "行数预算下 6 段应分 2 页");
    let p1 = &text[pages[0].0..pages[0].1];
    let p2 = &text[pages[1].0..pages[1].1];
    assert_eq!(
        p1.matches('字').count() + p2.matches('字').count(),
        600,
        "分页不应丢字"
    );
    // 行当量 = 文本行 + 段间 0.6 行，任何一页都不得超预算；第 1 页也不该大量留白
    let em = line_em(spec, vp().page_w());
    let gap = para_gap_px(spec.line) as f64 / spec.line as f64;
    let cost = |p: &str| -> f64 {
        let paras: Vec<&str> = p
            .split('\n')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let lines: usize = paras.iter().map(|s| para_lines(s, em)).sum();
        lines as f64 + gap * (paras.len().saturating_sub(1)) as f64
    };
    let c1 = cost(p1);
    let c2 = cost(p2);
    assert!(c1 <= lines as f64, "第 1 页行当量 {c1} 超预算");
    assert!(c2 <= lines as f64, "第 2 页行当量 {c2} 超预算");
    assert!(
        c1 >= lines as f64 - 5.0,
        "第 1 页填充率过低：{c1} / {lines}"
    );
    assert!(pages.windows(2).all(|w| w[0].1 <= w[1].0), "页区间不重叠");
    // 同一文本同一档位 → 分页结果恒定
    assert_eq!(paginate_lines(&text, spec, lines, vp().page_w()), pages);
}

#[test]
fn paginate_empty_is_one_blank_page() {
    assert_eq!(
        paginate_lines("", &FS_SPECS[1], budget(&FS_SPECS[1]), vp().page_w()),
        vec![(0, 0)]
    );
}

/// 任意正文（含 ASCII/长英文词/全角标点）分页后，每页估算行数不得超过该档预算。
#[test]
fn no_page_exceeds_line_budget() {
    let mut text = String::new();
    for i in 0..80 {
        if i % 7 == 0 {
            text.push_str(&"A".repeat(120)); // 无空格长串，按 0.5em 折行
        } else if i % 5 == 0 {
            text.push_str(&format!("第{i}段，短句。"));
        } else {
            text.push_str(&"正文句子。".repeat(12));
        }
        text.push('\n');
    }
    for spec in &FS_SPECS {
        let em = line_em(spec, vp().page_w());
        let budget = budget(spec);
        let pages = paginate_lines(&text, spec, budget, vp().page_w());
        for &(s, e) in &pages {
            let piece = &text[s..e];
            let lines: usize = piece
                .split('\n')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| para_lines(p, em))
                .sum();
            assert!(
                lines <= budget,
                "fs={} 页内 {} 行超过预算 {}",
                spec.size,
                lines,
                budget
            );
            assert!(piece.chars().count() > 0, "不应产生空页");
            assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
        }
        // 页首尾相接（去掉间隔换行），不丢字
        let joined: String = pages
            .iter()
            .map(|&(s, e)| text[s..e].replace('\n', ""))
            .collect();
        assert_eq!(joined, text.replace('\n', ""), "fs={} 分页丢字", spec.size);
    }
}

#[test]
fn chunk_ranges_split_at_200k_chars() {
    let text = "字".repeat(300_000);
    let chunks = chunk_ranges(&text, K_CHUNK_CHARS);
    assert_eq!(chunks.len(), 2, "30 万字应切成 2 块");
    assert_eq!(&text[chunks[0].0..chunks[0].1].chars().count(), &200_000);
    assert_eq!(&text[chunks[1].0..chunks[1].1].chars().count(), &100_000);

    let plan = plan_pages(&text, &FS_SPECS[1], budget(&FS_SPECS[1]), vp().page_w());
    assert_eq!(plan.chunks.len(), 2);
    assert!(plan.pages.len() > 2);
    assert_eq!(plan.pages[0].0, 0);
    assert_eq!(plan.pages.last().unwrap().0, 1, "末页应属于第 2 块");
    assert!(plan
        .pages
        .iter()
        .all(|&(ci, s, e)| ci < 2 && s <= e && e <= text.len()));
}

#[test]
fn truncate_to_chars_is_in_place_and_char_safe() {
    let mut text = "字".repeat(900_000);
    assert!(
        truncate_to_chars(&mut text, K_MAX_TEXT_CHARS),
        "90 万字应触发截断"
    );
    assert_eq!(text.chars().count(), K_MAX_TEXT_CHARS);
    assert!(text.is_char_boundary(text.len()));

    let mut short = "短".repeat(100);
    assert!(!truncate_to_chars(&mut short, K_MAX_TEXT_CHARS));
    assert_eq!(short.chars().count(), 100);
}

/// 目录含分卷行：总数只算非分卷章节，分页按过滤后的列表，链接 index 保持原始值。
#[test]
fn toc_skips_volumes_and_keeps_original_index() {
    let mut chapters = Vec::new();
    for i in 0..45_i64 {
        let is_volume = matches!(i, 0 | 10 | 20 | 30 | 40);
        chapters.push(BookChapter {
            url: if is_volume {
                format!("volume:book#{i}")
            } else {
                format!("https://example.test/ch/{i}")
            },
            title: Some(format!("第 {i} 条")),
            is_volume,
            index: i,
            ..Default::default()
        });
    }

    let visible = visible_chapters(&chapters);
    assert_eq!(visible.len(), 40);
    assert!(visible.iter().all(|c| !volume_entry(c)));
    assert_eq!(visible[0].index, 1);
    assert_eq!(visible[39].index, 44);

    let total = visible.len() as i64;
    let pages = (total + K_TOC_PAGE as i64 - 1) / K_TOC_PAGE as i64;
    assert_eq!(pages, 2, "40 条非分卷章节应分 2 页");
    let page1 = &visible[0..K_TOC_PAGE];
    let page2 = &visible[K_TOC_PAGE..visible.len()];
    assert_eq!(page1.len(), 30, "每页两列各 15 行");
    assert_eq!(page2.len(), 10);
    // 分页区间仍指向原 Vec 里的章节，index 单调且跳过被过滤的分卷下标。
    assert!(page1.windows(2).all(|w| w[0].index < w[1].index));
    assert!(page2.windows(2).all(|w| w[0].index < w[1].index));
    assert!(page1
        .iter()
        .all(|c| !matches!(c.index, 0 | 10 | 20 | 30 | 40)));
    assert_eq!(page1.last().unwrap().index, 33);
    assert_eq!(page2.first().unwrap().index, 34);
}

#[test]
fn volume_entry_detects_flag_and_synthetic_url() {
    assert!(volume_entry(&BookChapter {
        is_volume: true,
        url: "https://example.test/ch/1".into(),
        ..Default::default()
    }));
    assert!(volume_entry(&BookChapter {
        is_volume: false,
        url: "volume:book#3".into(),
        ..Default::default()
    }));
    assert!(!volume_entry(&BookChapter {
        is_volume: false,
        url: "https://example.test/ch/3".into(),
        ..Default::default()
    }));
}

#[test]
fn html_to_text_strips_tags() {
    let html = "<p>第一段&nbsp;内容</p><p>第二段<br>换行</p><div class=\"x\">尾</div>";
    let t = html_to_text(html);
    assert!(t.contains("第一段 内容"));
    assert!(t.contains("第二段\n换行"));
    assert!(t.contains("尾"));
    assert!(!t.contains('<'));
}

/// 阅读页版面高 = 运行时现算可用高（vp.h）；四档行预算按
/// 「正文可用高 − 56 顶栏 − 36 上留白 − 56 页码」动态算出。
#[test]
fn font_specs_fit_read_stage() {
    assert_eq!(vp().h, 1268);
    assert_eq!(vp().layout_h(), 1268);
    assert_eq!(vp().content_h(), vp().h - vp().bar_h() - 8);
    // 1072×1268（运行时高 1268，底栏派生 65）下表：1055/46=22、1055/52=20、1055/62=17、1055/36=29
    let expect = [(28, 46, 22), (32, 52, 20), (38, 62, 17), (20, 36, 29)];
    assert_eq!(FS_SPECS.len(), 4, "应有四档字号");
    for (spec, (size, line, lines)) in FS_SPECS.iter().zip(expect) {
        assert_eq!((spec.size, spec.line), (size, line));
        assert_eq!(budget(spec), lines, "fs={size} 行预算错误");
        let per_line = (vp().page_w() - SIDE_PAD * 2) / size;
        let chars = lines * per_line;
        assert!(
            lines * spec.line <= vp().text_h(),
            "fs={size} 行高总和 {} 超过正文区 {}",
            lines * spec.line,
            vp().text_h()
        );
        assert!(
            (350..=1500).contains(&chars),
            "fs={size} 每页字数 {chars} 超出合理区间（顶栏占 56px，最大档少 1 行）"
        );
    }
    // 标准档 32px：20 行 × 29 字/行 = 580 字，落在 500–800 的参考区间
    let std_chars = budget(&FS_SPECS[1]) * (vp().page_w() - SIDE_PAD * 2) / FS_SPECS[1].size;
    assert!(
        (500..=800).contains(&std_chars),
        "标准档每页字数应在 500–800，实际 {std_chars}"
    );
}

/// 字号轮换按物理大小 超小(4)→小(1)→标准(2)→大(3)→超小(4)，旧档位语义不变。
#[test]
fn font_cycle_keeps_existing_tiers() {
    assert_eq!(resolve_fs(&HeaderMap::new(), Some(4)).0, 4);
    assert_eq!(
        resolve_fs(&HeaderMap::new(), Some(9)).0,
        2,
        "越界回落标准档"
    );
    assert_eq!(cycle_fs(4), 1);
    assert_eq!(cycle_fs(1), 2);
    assert_eq!(cycle_fs(2), 3);
    assert_eq!(cycle_fs(3), 4);
}

/// /k 页面骨架：viewport 宽度随请求视口，唯一的内联脚本只做「缺参补参一次」。
#[test]
fn viewport_is_fixed_and_page_is_self_contained() {
    let html = page_html("测试", 2, "<div>正文</div>", vp());
    assert!(
        html.contains("<meta name=\"viewport\" content=\"width=1072,initial-scale=1"),
        "缺少 viewport meta"
    );
    // 视图宽跟着请求参数走：同一骨架换 1024 视口，viewport 也要跟着变
    let html_small = page_html("测试", 2, "<div>正文</div>", vps(1024, 1200));
    assert!(
        html_small.contains("content=\"width=1024,initial-scale=1"),
        "{html_small}"
    );
    // 缺参首屏：meta 必须放开给设备真实宽度（否则 innerWidth 恒等 vw，补参永远量不出真尺寸）
    let html_naked = page_html(
        "测试",
        2,
        "<div>正文</div>",
        Vp::resolve(None, None, (K_W, K_H)),
    );
    assert!(
        html_naked.contains("content=\"width=device-width,initial-scale=1"),
        "{html_naked}"
    );
    // 三个脚本：错误兜底（head 内）+ 视口标定（head 内）+ 自报探针
    assert_eq!(
        html.matches("<script").count(),
        3,
        "只允许错误兜底、视口标定与自报探针"
    );
    // 运行时现算：outerHeight − 1 为主、availHeight 作上限、innerHeight 兜底
    assert!(html.contains("(window.outerHeight||0)-1"), "{html}");
    assert!(html.contains("sh.availHeight"), "{html}");
    assert!(html.contains("window.__kvh=H"), "算出的高必须可供探针上报");
    assert!(
        html.contains("location.replace(location.pathname+'?'"),
        "补参用 replace，不产生历史记录"
    );
    assert!(
        html.contains("document.links") && html.contains("a.setAttribute('href'"),
        "load 后必须把同一 vh 补到所有站内链接"
    );
    // 不落任何持久化：Cookie / sessionStorage / localStorage 一律不出现
    assert!(!html.contains("document.cookie"), "不得用 Cookie 缓存高度");
    assert!(!html.contains("sessionStorage") && !html.contains("localStorage"));
    assert!(!html.contains("kfix=1"), "不得再带 kfix 永久闩锁");
    assert!(
        html.contains("/api/k/probe?"),
        "探针必须上报到 /api/k/probe"
    );
    for f in [
        "vw_param=",
        "vh_param=",
        "inner_w=",
        "inner_h=",
        "client_w=",
        "client_h=",
        "scroll_w=",
        "scroll_h=",
        "screen_w=",
        "screen_h=",
        "avail_h=",
        "outer_w=",
        "outer_h=",
        "doc_h=",
        "doc_minus_outer=",
        "doc_minus_client=",
        "outer_minus_doc=",
        "rt_h=",
        "lines=",
        "body_h=",
        "stage_h=",
        "bar_top=",
        "bar_bottom=",
        "bar_rect_top=",
        "bar_top_delta=",
        "page_y=",
        "doc_top=",
        "scroll_top_body=",
        "scroll_top_doc=",
        "scroll_y=",
        "max_seen=",
        "body_scroll_h=",
        "body_rect_bottom=",
        "lowest_el_bottom=",
        "scroll_evt=",
        "outer_minus_scroll=",
        "inner_scroll_top=",
    ] {
        assert!(K_PROBE_JS.contains(f), "探针字段缺失：{f}");
    }
    assert!(
        K_PROBE_JS.contains("stt=setTimeout(function(){s('move','settle')},800)"),
        "最后一次 scroll 后 800ms 必须补一条 settle：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("if(d>max_seen)max_seen=d"),
        "max_seen 必须把底栏 top 变化量（pan 代理量）计入峰值：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("addEventListener('scroll',rs,false)"),
        "必须注册 scroll 事件补报滑动后的证据：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("if(t-last<200)return"),
        "scroll 打点必须 200ms 节流，避免洪水：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("var i=new Image()"),
        "img 打点是唯一通道，不依赖 load/XHR：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("setTimeout(function(){s('sync','-')},0)"),
        "sync 必须延后到首次布局后再发（否则 inner_h=0）：{K_PROBE_JS}"
    );
    assert!(
        !K_PROBE_JS.contains("XMLHttpRequest"),
        "不得再有 XHR 回退通道（同 stage 会重复打点）：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("getElementById('k-stage')"),
        "stage_h 必须取 #k-stage 的 clientHeight"
    );
    assert!(
        K_PROBE_JS.contains("getElementById('k-scroll')"),
        "内层滚动容器 scrollTop 必须上报（当前结构下预期为 '-'）"
    );
    // 交互只走 click：pageX/pageY 必须有值并上报
    assert!(
        K_PROBE_JS.contains("ev.pageX") && K_PROBE_JS.contains("ev.pageY"),
        "click 必须上报 pageX/pageY：{K_PROBE_JS}"
    );
    assert!(
        K_PROBE_JS.contains("stage=click&click_x="),
        "click 打点必须带分区坐标：{K_PROBE_JS}"
    );
    assert!(
        !K_PROBE_JS.contains("Promise") && !K_PROBE_JS.contains("fetch("),
        "探针必须是 ES5（img 单通道，不用 Promise/fetch）"
    );
    assert!(
        !K_PROBE_JS.contains("location.replace") && !K_PROBE_JS.contains("location.href"),
        "探针不得改写 location（不能引起额外重载）"
    );
    assert!(
        html.contains("window.__kerr") && html.contains("window.onerror"),
        "必须在页面最前面注册全局错误兜底"
    );
    // 删除所有「骗浏览器」的补偿与已弃用字段
    for gone in [
        "touchmove",
        "touchstart",
        "touchend",
        "preventDefault",
        "calib",
        "meas_y",
        "k_ck",
        "meta_h",
        "vw_meta_height",
        "visible_h",
        "::-webkit-scrollbar",
        "scrollbar-width",
    ] {
        assert!(!html.contains(gone), "已弃用的补偿/字段不得残留：{gone}");
    }
    assert!(
        html.contains("location.replace"),
        "补参用 replace，不产生历史记录"
    );
    assert!(!html.contains("<link"), "不允许子资源");
    assert!(!html.contains("<img"), "不允许子资源");
}

/// 入口门禁只看 HTTP UA：真机 Kindle/3.0+ 命中；页面内 JS 的 534.26+ 不命中。
#[test]
fn kindle_ua_gate_only_matches_kindle_and_silk() {
    assert!(is_kindle_ua(
            "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+"
        ));
    assert!(is_kindle_ua(
            "Mozilla/5.0 (Linux; Android 4.4; KFAPWI Build/KTU84P) AppleWebKit/537.36 Silk/3.68 like Chrome/34.0"
        ));
    assert!(!is_kindle_ua(
        "AppleWebKit/534.26+ (KHTML, like Gecko) Version/5.0 Safari/534.26+"
    ));
    assert!(!is_kindle_ua(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15"
    ));
    assert!(!is_kindle_ua(""));
}

/// 跳转范围：`/` 与 SPA 入口跳；`/k` 自身、API、健康检查、带扩展名的静态资源不跳。
#[test]
fn spa_entry_gate_excludes_k_page_and_assets() {
    assert!(is_spa_entry("/"));
    assert!(is_spa_entry("/index.html"));
    assert!(is_spa_entry("/reader"));
    assert!(!is_spa_entry("/k"));
    assert!(!is_spa_entry("/k/read"));
    assert!(!is_spa_entry("/api/sources"));
    assert!(!is_spa_entry("/healthz"));
    assert!(!is_spa_entry("/assets/index-CyUAYOCA.js"));
    assert!(!is_spa_entry("/favicon.ico"));
}

/// 版式硬约束：底栏高随运行时可用高派生、不压正文；
/// 正文区 = 文档高 − 底栏 − 56（顶栏）− 36（顶部留白）− 56（页码行）；标准档显式 32/52。
#[test]
fn fixed_bar_keeps_content_clear_of_bar() {
    assert_eq!(vp().body_h(), vp().h - vp().bar_h());
    assert_eq!(
        vp().text_h(),
        vp().body_h() - READ_NAV_H - READ_PAD_TOP - READ_PAGENO_H
    );
    assert!(vp().text_h() <= vp().content_h(), "正文不得越过一屏可用高");
    let spec = &FS_SPECS[1];
    assert_eq!(
        spec.size, 32,
        "标准档必须显式 32px（实测 32/52，勿低于 30）"
    );
    assert_eq!(spec.line, 52);
    let per_line = (vp().page_w() - SIDE_PAD * 2) / spec.size;
    let chars = budget(spec) * per_line;
    assert!(
        (500..=800).contains(&chars),
        "默认档每页字数应在 500–800，实际 {}",
        chars
    );
    assert!(budget(spec) <= vp().text_h() / spec.line);
}

/// 四档行预算全部由视口高动态算出（已扣 56px 顶栏）：
/// 1072×1268 → 22/20/17/29，1024×1200 同档位按更低可用高再算一遍（21/19/15/27），
/// 且总高始终 ≤ 可用高。
#[test]
fn read_line_budget_matches_reference() {
    assert_eq!(
        FS_SPECS
            .iter()
            .map(|s| line_budget(vp(), s.line))
            .collect::<Vec<_>>(),
        vec![22, 20, 17, 29]
    );
    for s in FS_SPECS.iter() {
        assert!(budget(s) * s.line <= vp().text_h(), "{}px 行高溢出", s.line);
        assert!(budget(s) * s.line <= vp().content_h());
    }
    // 换尺寸：行预算随可用高变化，不会沿用 1072×1268 的常量
    let small = vps(1024, 1200);
    let small_lines: Vec<usize> = FS_SPECS
        .iter()
        .map(|s| line_budget(small, s.line))
        .collect();
    assert_eq!(
        small_lines,
        vec![21, 19, 15, 27],
        "1024×1200 行预算应按可用高重算"
    );
    for s in FS_SPECS.iter() {
        assert!(line_budget(small, s.line) * s.line <= small.text_h());
    }
    // 运行时低高（vh=1224）：底栏派生 63 → 正文可用 1013 → 22/19/16/28
    let fb = vps(1072, 1224);
    let fb_lines: Vec<usize> = FS_SPECS.iter().map(|s| line_budget(fb, s.line)).collect();
    assert_eq!(
        fb_lines,
        vec![22, 19, 16, 28],
        "1224 行预算应按运行时可用来重算"
    );
    for s in FS_SPECS.iter() {
        let last_bottom = 56 + 36 + line_budget(fb, s.line) * s.line;
        assert!(
            last_bottom <= fb.layout_h() - fb.bar_h(),
            "{}px 末行越界：{last_bottom} > {}",
            s.line,
            fb.layout_h() - fb.bar_h()
        );
        assert!(line_budget(fb, s.line) * s.line <= fb.text_h());
    }
}

/// 列表页（书架/目录/搜索）每页条数按「页面高 − 顶栏 − 分页条 − 安全余量」实算：
/// 1072×1268 与 1024×1200 两档都必须装得下，也不允许多留一整行。
#[test]
fn list_pages_fit_inside_stage_at_reference_viewports() {
    // 1268：正文区 1195；书架 10×112+64=1184、目录 15×2=30 行、搜索 9×112+152=1160
    let v = vp();
    assert_eq!((v.nav_h(), v.shelf_layout()), (1195, (10, 112)));
    assert_eq!(toc_page_size(v), 30);
    assert_eq!(search_page_size_extra(v, 0), 9);
    let s = vps(1024, 1200);
    assert_eq!((s.nav_h(), s.shelf_layout()), (1127, (10, 106)));
    assert_eq!(toc_page_size(s), 28);
    assert_eq!(search_page_size_extra(s, 0), 8);
    for vp in [v, s] {
        let (pp, rh) = vp.shelf_layout();
        assert!(pp * rh + BAR_H <= vp.nav_h(), "书架列表溢出正文区");
        assert!(
            (pp + 1) * rh + BAR_H > vp.nav_h(),
            "书架应贴近填满，别多留一整行"
        );
        let t = toc_page_size(vp);
        assert!(t / K_TOC_COLS * 72 + BAR_H <= vp.nav_h(), "目录溢出");
        let q = search_page_size_extra(vp, 0);
        assert!(q * 112 + BAR_H + 88 <= vp.nav_h(), "搜索结果溢出");
    }
    // 极矮视口：行高触底后减少每页本数，绝不溢出
    let tiny = vps(1072, 600);
    let (pp, rh) = tiny.shelf_layout();
    assert!(pp * rh + BAR_H <= tiny.nav_h(), "极矮视口书架溢出");
}

/// 搜索页状态提示/回退说明/升级按钮各占一行：每多一行就少放一条结果，且至少保留 1 条。
#[test]
fn search_page_size_extra_reserves_note_rows() {
    let v = vp();
    let base = search_page_size_extra(v, 0);
    assert_eq!(base, 9);
    assert_eq!(search_page_size_extra(v, 1), base - 1);
    assert_eq!(search_page_size_extra(v, 2), base - 2);
    assert_eq!(search_page_size_extra(v, 99), 1, "不能扣到 0 条");
}

/// 一屏一页：文档高 = 请求带回的运行时可用高，正文区写死「文档高 − 顶栏 − 底栏」并
/// `overflow:hidden`，结构上无滚内容；不做任何隐藏滚动条的补偿。
#[test]
fn read_page_locks_one_screen_without_scrollbar_hacks() {
    let dev = vps(1072, 1201);
    let css = read_only_css(dev);
    assert!(
        css.contains(
            "html,body{position:relative;top:0;left:0;width:100%;height:1201px;overflow:hidden}"
        ),
        "文档高必须等于运行时可用高：{css}"
    );
    assert!(
        !css.contains("position:fixed"),
        "不得用 fixed 锚布局视口：{css}"
    );
    assert!(!css.contains("103%"), "不得再有 body 103% hack：{css}");
    assert!(!css.contains("k-scroll"), "不得再有嵌套滚动容器：{css}");
    assert!(
            css.contains(&format!(
                "#k-stage{{position:absolute;top:56px;left:36px;width:{}px;height:{}px;padding:36px 36px 0;overflow:hidden}}",
                dev.page_w(),
                dev.layout_h() - READ_NAV_H - dev.bar_h()
            )),
            "正文区高度必须由运行时高派生：{css}"
        );
    // 页面里彻底没有滚动条相关规则
    let html_css = page_html_css("测试", 2, "", "", vp());
    assert!(
        !html_css.contains("scrollbar") && !html_css.contains("scrollbar-width"),
        "不得保留任何滚动条隐藏规则：{html_css}"
    );
    // 运行时视口脚本：outerHeight 为主、availHeight 上限保护、innerHeight 兜底；
    // 不写 Cookie / sessionStorage；把 vh 写回 URL 并补到页内链接。
    let vpjs = K_VP_JS;
    assert!(vpjs.contains("(window.outerHeight||0)-1"), "{vpjs}");
    assert!(vpjs.contains("screen||{}"), "{vpjs}");
    assert!(vpjs.contains("availHeight"), "{vpjs}");
    assert!(vpjs.contains("innerHeight"), "{vpjs}");
    assert!(!vpjs.contains("cookie"), "不得写 Cookie：{vpjs}");
    assert!(!vpjs.contains("sessionStorage"), "不得持久化高度：{vpjs}");
    assert!(vpjs.contains("location.replace"), "{vpjs}");
    assert!(vpjs.contains("document.links"), "链接要补 vh：{vpjs}");
    // 底栏：相对 body 贴底 + 内建 12px 安全区，高度随运行时高派生（不写死）
    let bar = fixed_bar("", vp());
    assert!(
        bar.contains(&format!(
            "position:absolute;left:0;right:0;bottom:0;height:{}px;padding-bottom:12px",
            vp().bar_h()
        )),
        "底栏高度必须派生：{bar}"
    );
    assert!(!bar.contains("position:fixed"), "{bar}");
    assert_ne!(
        vp().bar_h(),
        vps(1072, 1000).bar_h(),
        "不同可用高应得不同底栏高"
    );
}

/// 阅读页顶栏：左「← 书架」回 `/k`、右「目录」回目录页，两者都透传当前 fs/vw/vh；
/// 书名按像素截断成单行（超长加 `…`），z-index 12 高于翻页热区 5。
#[test]
fn read_page_has_nav_with_shelf_entry_and_params() {
    let v = vp();
    let back = qlink("/k", 2, v);
    let toc = qlink("/k/toc", 2, v);
    assert!(
        back.contains("fs=2") && back.contains("vw=1072") && back.contains("vh=1268"),
        "书架入口必须透传参数：{back}"
    );
    let long = "一个特别特别长的书名".repeat(6);
    let nav = read_nav_html(&long, &back, &toc, v);
    assert!(nav.contains("id=\"k-read-nav\""), "{nav}");
    assert!(nav.contains("&larr; 书架"), "必须有返回书架入口：{nav}");
    assert!(nav.contains("目录"), "必须有目录入口：{nav}");
    assert!(nav.contains(&format!("href=\"{}\"", back)), "{nav}");
    assert!(nav.contains(&format!("href=\"{}\"", toc)), "{nav}");
    assert!(
        nav.contains('…'),
        "超长书名必须按像素截断并用省略号收尾：{nav}"
    );
    let short = read_nav_html("三体", &back, &toc, v);
    assert!(!short.contains('…'), "短书名不得加省略号：{short}");
    assert!(short.contains(">三体<"), "{short}");
    // 顶栏整格可点（链接 display:block 撑满 55px），热区又让开 56px
    assert!(
        read_only_css(v).contains("z-index:12"),
        "顶栏须高于热区/底栏"
    );
}

/// 高度不做持久化：请求未带 `vh` 时用环境默认，带了就完全按参数排版（每次现算）。
#[test]
fn viewport_height_comes_from_request_only() {
    let d = (1072, 1201);
    assert_eq!(Vp::resolve(None, Some(1201), d).layout_h(), 1201);
    assert_eq!(Vp::resolve(None, None, d).layout_h(), 1201, "缺参用默认");
    assert_eq!(
        Vp::resolve(None, Some(999), d).layout_h(),
        999,
        "按参数原样排版"
    );
    let g = vps(1072, 1201).content_h();
    assert!(g < 1201 && g > 0, "版面必须比可用高矮（给底栏让位）");
}

/// 只保留宽度/缩放，viewport meta 不再输出 `height=`（也不写任何高度 Cookie）。
#[test]
fn viewport_meta_has_no_height_and_no_cookie() {
    let v = vp();
    let html = page_html_css("测试", 2, "", "", v);
    assert!(
        html.contains("content=\"width=1072,initial-scale=1"),
        "viewport 只声明宽度：{html}"
    );
    assert!(
        !html.contains("height=1224") && !html.contains(",height="),
        "不得输出 meta height"
    );
    let res = page_css("测试", 2, "", "", v);
    assert!(
        res.headers().get(SET_COOKIE).is_none(),
        "页面不再写高度 Cookie"
    );
}

/// 版面高完全跟随请求：不同高度得出的行预算不同，且末行永不越过正文区。
#[test]
fn line_budget_tracks_runtime_height() {
    let small = vps(1072, 1100);
    let big = vps(1072, 1280);
    let s_lines: Vec<usize> = FS_SPECS
        .iter()
        .map(|s| line_budget(small, s.line))
        .collect();
    let b_lines: Vec<usize> = FS_SPECS.iter().map(|s| line_budget(big, s.line)).collect();
    assert!(
        s_lines.iter().zip(b_lines.iter()).all(|(a, b)| a < b),
        "高越大行越多"
    );
    for v in [small, big] {
        for s in FS_SPECS.iter() {
            let last_bottom = READ_NAV_H + READ_PAD_TOP + line_budget(v, s.line) * s.line;
            assert!(
                last_bottom <= v.layout_h() - v.bar_h(),
                "{}px 末行越界",
                s.line
            );
        }
    }
}

/// 顶栏只有一条、白底 + 1px #ccc，不再有整条灰底；书架/目录/搜索页不再渲染阅读页底栏。
#[test]
fn top_nav_is_single_and_has_no_gray_fill() {
    let nav = top_bar("书架", None, &nav_link("/k/search", "搜索"), vp());
    assert!(nav.contains("height:73px;border-bottom:1px solid #ccc;background:#fff"));
    assert!(nav.contains("font-size:28px;line-height:72px;font-weight:700"));
    assert!(nav.contains("font-size:20px"), "动作链接 20px");
    assert!(nav.contains("padding-left:32px"), "动作间距 32px");
    assert!(!nav.contains("e6e6e6"), "不得再用灰底");
    assert_eq!(nav.matches("id=\"k-nav\"").count(), 1, "顶栏只能有一条");
}

/// 空书架：1px 描边块 + 满区居中（table 100% + valign=middle）+ 240×72 描边按钮。
#[test]
fn empty_shelf_is_outlined_and_centered() {
    let html = empty_shelf(2, vp());
    assert!(html.contains("border:1px solid #000"), "{html}");
    assert!(html.contains("width=\"1000\" height=\"1195\""), "{html}");
    assert!(
        html.contains("align=\"center\" valign=\"middle\""),
        "{html}"
    );
    assert!(html.contains("书架还是空的"), "{html}");
    assert!(html.contains("width:240px;height:72px;line-height:70px;border:1px solid #000"));
}

/// 底栏 3 格等宽（各 33.33%）、整格 click 热区，只有一行主标签（无下方小字）。
/// 「目录」已删除（入口只留顶栏右上角），中间格显示当前字号档位。
#[test]
fn bottom_bar_has_three_equal_cells() {
    let row = format!(
        "{}{}{}",
        bar_cell("/k?a", "前", vp()),
        bar_cell("/k?b", "中", vp()),
        bar_cell_off("后", vp()),
    );
    let bar = fixed_bar(&row, vp());
    let ch = vp().bar_content_h().saturating_sub(1);
    assert_eq!(bar.matches("<td ").count(), 3, "{bar}");
    assert_eq!(bar.matches("width=\"33.33%\"").count(), 3, "{bar}");
    assert_eq!(bar.matches(&format!("height=\"{ch}\"")).count(), 3, "{bar}");
    assert!(bar.contains("font-size:34px"), "{bar}");
    assert!(!bar.contains("border:1px solid #000"), "格子不得再套方框");
    assert!(bar.contains("<a href=\"/k?a\""), "上一页必须是链接");
    assert_eq!(bar.matches("<a ").count(), 2, "前/字号两格是链接");
    assert!(!bar.contains("<form"), "记录表单已删除");
    assert!(
        !bar.contains("目录") && !bar.contains(">目<"),
        "目录格已删除"
    );
    assert!(
        !bar.contains("上一页") && !bar.contains("下一页") && !bar.contains("上一章"),
        "不再有下方小字"
    );
    assert!(bar.contains("color:#555"), "禁用态用弱灰 #555");
    assert!(!bar.contains("background:#e6e6e6"));
    // 底栏高随运行时可用高派生：不同高度得到不同像素，不写死
    let tall = fixed_bar(&row, vps(1072, 1280));
    assert!(
        tall.contains(&format!("height:{}px", vps(1072, 1280).bar_h())),
        "{tall}"
    );
    assert_ne!(vps(1072, 1280).bar_h(), vp().bar_h());
}

/// 左右热区：CSS `top:56px;bottom:0;width:33%`（从顶栏下沿开始）跟随真实视口
/// （1072 → 353），无下一页时右区不渲染。
#[test]
fn hot_zones_are_one_third_screen_and_clickable() {
    assert_eq!(vp().hot_w(), 1072 / 3);
    assert_eq!(vp().w - vp().hot_w(), 1072 - vp().hot_w());
    assert!(vp().hot_w() >= 200, "热区宽必须 ≥ 200px");
    assert!(vp().h >= 200, "热区高必须 ≥ 200px");
    let z = hot_zones(Some("/k/read?p=1"), Some("/k/read?p=2"));
    assert_eq!(z.matches("<a ").count(), 2);
    assert!(z.contains("left:0"), "{z}");
    assert!(z.contains("right:0"), "{z}");
    assert!(
        z.contains("top:56px;bottom:0;width:33%") && !z.contains("height:"),
        "热区必须从顶栏下沿跟随真实视口（top/bottom + 33%），不得写死像素：{z}"
    );
    // 顶栏整格可点，热区 z-index 5 < 顶栏 12：点书名/「书架」不会落到翻页区
    assert!(z.contains("z-index:5"), "{z}");
    assert_eq!(
        hot_zones(Some("/k/read?p=1"), None).matches("<a ").count(),
        1
    );
    assert_eq!(hot_zones(None, None).matches("<a ").count(), 0);
}

/// `vw`/`vh` 参数优先，缺参回落默认 1072×1268，越界值直接忽略。
#[test]
fn viewport_params_override_defaults_and_clamp() {
    assert_eq!(
        Vp::resolve(Some(1024), Some(1200), (K_W, K_H)),
        vps(1024, 1200)
    );
    // 缺参：几何回默认，但 fixed = false（meta 放开 device-width 才能量出真实尺寸）
    let naked = Vp::resolve(None, None, (K_W, K_H));
    assert_eq!((naked.w, naked.h), (K_W, K_H));
    assert!(!naked.fixed, "缺参首屏不得锁宽");
    // 越界/非法尺寸不许把版心算成负数
    let bad = Vp::resolve(Some(100), Some(5000), (K_W, K_H));
    assert_eq!((bad.w, bad.h), (K_W, K_H));
    for (w, h) in [(1024usize, 1200usize), (800, 600), (1072, 1268)] {
        let v = vps(w, h);
        assert!(v.content_w() >= 320, "版心宽不能小于 320");
        assert!(v.content_h() >= 320, "可用高不能小于 320");
        assert_eq!(v.content_w(), w - 144);
        assert_eq!(v.content_h(), h - v.bar_h() - 8);
        assert_eq!(
            v.text_h(),
            h - v.bar_h() - READ_NAV_H - READ_PAD_TOP - READ_PAGENO_H,
            "正文可用高必须扣掉 56px 顶栏与页码行"
        );
    }
}

/// 翻页/换章链接必须把 vw/vh 原样带下去（客户端零测量）。
#[test]
fn pagination_links_carry_viewport_params() {
    let q = ReadQ {
        book: "b".into(),
        source: "s".into(),
        ch: Some(3),
        p: Some(2),
        fs: Some(2),
        vw: None,
        vh: None,
    };
    let next = read_link(&q, 2, 3, 3, vp());
    assert!(
        next.contains("vw=1072") && next.contains("vh=1268"),
        "{next}"
    );
    let toc = toc_link(&q, 2, 3, 2, vp());
    assert!(toc.contains("vw=1072") && toc.contains("vh=1268"), "{toc}");
    // 换尺寸后链接里的参数跟着变
    let small = read_link(&q, 2, 3, 3, vps(1024, 1200));
    assert!(
        small.contains("vw=1024") && small.contains("vh=1200"),
        "{small}"
    );
    assert!(!small.contains("vw=1072"), "不得残留默认尺寸");
}

/// 测试用配置：切片参数取默认值，开关按用例需要打开/关闭。
fn test_cfg(slice: bool) -> crate::config::Config {
    let mut cfg = crate::config::Config::from_env();
    cfg.comic_slice = slice;
    cfg.comic_slice_dw = 928;
    cfg.comic_slice_vh = 1196;
    cfg.comic_slice_ov = 64;
    cfg.comic_slice_mu = 200;
    cfg.comic_slice_sd = 30;
    cfg.comic_slice_r = 96;
    cfg
}

/// 漫画页表：长条页按窗口拆片（非末片 1196、末片取剩余），一屏内整图 1 页且高 ≤ 1196。
#[test]
fn comic_pages_slice_long_strip_and_keep_one_screen() {
    let images: Vec<BookImage> = (0..3)
        .map(|i| BookImage {
            index: i + 1,
            url: format!("https://example.test/{i}.jpg"),
            width: Some(1200),
            height: Some(2266),
            ..Default::default()
        })
        .collect();
    let dims: Vec<Option<(u32, u32)>> = vec![Some((1200, 2266)); 3];
    let cfg = test_cfg(true);
    let sp = slice_p();
    let pages = comic_pages(&images, &dims, &cfg, &sp);
    assert_eq!(pages.len(), 6, "三张两片长条页 = 6 页");
    for (i, pg) in pages.iter().enumerate() {
        assert_eq!(pg.layers, 2, "第 {i} 页片数");
        assert_eq!(pg.slice, i % 2, "第 {i} 页片下标");
        assert_eq!(pg.width, 928, "版心宽固定 928");
        // 切片真实高度由服务端落点决定，页表只给宽度（≠0 才写 height，避免拉扁）
        assert_eq!(pg.height, 0, "第 {i} 页不写死切片高度");
    }
    // 开关关闭：同一批长条页退回「一图一页」，且不得带切片参数
    let off = comic_pages(&images, &dims, &test_cfg(false), &sp);
    assert_eq!(off.len(), 3, "关闭时三张图各一页");
    assert!(off.iter().all(|p| p.layers == 1 && p.slice == 0));
    assert_eq!(
        off[0].height,
        vp().content_h() - READ_NAV_H,
        "整图按高度约束缩到一屏（扣顶栏）"
    );
    // 1.1 屏（1024×1471）：不切，一屏一页，按高度约束等比缩小
    let one = vec![BookImage {
        index: 1,
        url: "https://example.test/one.jpg".into(),
        width: Some(1024),
        height: Some(1471),
        ..Default::default()
    }];
    let one_dims = vec![Some((1024, 1471))];
    let p1 = comic_pages(&one, &one_dims, &cfg, &sp);
    assert_eq!(p1.len(), 1, "1.1 屏不切");
    assert_eq!(p1[0].layers, 1);
    assert_eq!(p1[0].height, vp().content_h() - READ_NAV_H);
    assert!(p1[0].width <= 928, "等比缩小后不得宽于版心");
    // 尺寸未知：整图一页，height=0（渲染退化为 max-height）
    let unknown = vec![BookImage {
        index: 1,
        url: "https://example.test/x.jpg".into(),
        ..Default::default()
    }];
    let u = comic_pages(&unknown, &[None], &cfg, &sp);
    assert_eq!(u.len(), 1);
    assert_eq!((u[0].layers, u[0].height), (1, 0));
}

/// 整图页 URL 不带切片参数（桌面端/`slice=0` 路径字节不变）；长条页才带 `slice=1&i=k`。
#[test]
fn comic_stage_uses_explicit_height_and_slice_only_for_long_pages() {
    let images = vec![BookImage {
        index: 1,
        url: "https://example.test/a.jpg".into(),
        width: Some(1024),
        height: Some(1200),
        ..Default::default()
    }];
    let sp0 = slice_p();
    let pages = comic_pages(&images, &[Some((1024, 1200))], &test_cfg(true), &sp0);
    assert_eq!(pages.len(), 1);
    let html = comic_stage(
        &images,
        &pages[0],
        "src",
        "book",
        "https://example.test/ch",
        "1/1",
        vp(),
        &sp0,
    );
    assert!(!html.contains("slice=1"), "整图页不得带切片参数");
    // 1024×1200 缩到 928 宽 → 高 1088（≤ 运行时漫画可用高），显式宽高防抖动
    assert!(html.contains("width=\"928\" height=\"1088\""), "{html}");
    assert!(html.contains("max-width:100%"), "禁止横向溢出");
    // 漫画页保留像素高（运行时 1268 − 56 顶栏 = 1212），并整体下移到顶栏下沿
    assert!(html.contains("id=\"k-stage\"") && html.contains("height:1212px"));
    // 漫画页同样清零 read_only_css 的绝对定位偏移，否则整页会被 right 挤向右
    assert!(
        html.contains(
            "position:relative;top:0;left:0;right:auto;bottom:auto;margin-top:56px;height:1212px"
        ),
        "{html}"
    );
    // 短图在开关开/关两种模式下 HTML 必须逐字节一致（回归要求）
    let off_html = {
        let off_pages = comic_pages(&images, &[Some((1024, 1200))], &test_cfg(false), &sp0);
        comic_stage(
            &images,
            &off_pages[0],
            "src",
            "book",
            "https://example.test/ch",
            "1/1",
            vp(),
            &sp0,
        )
    };
    assert_eq!(html, off_html, "短图在切片开关两种模式下必须字节一致");

    let strip = vec![BookImage {
        index: 1,
        url: "https://example.test/s.jpg".into(),
        width: Some(1200),
        height: Some(2266),
        ..Default::default()
    }];
    let sp1 = comic_pages(&strip, &[Some((1200, 2266))], &test_cfg(true), &sp0);
    assert_eq!(sp1.len(), 2);
    let s2 = comic_stage(
        &strip,
        &sp1[1],
        "src",
        "book",
        "https://example.test/ch",
        "2/2",
        vp(),
        &sp0,
    );
    assert!(s2.contains("slice=1&i=1"), "{s2}");
    // 切片 URL 必须带 dw/vh：缓存 key 按尺寸天然隔离（vh 取运行时漫画可用高 1139）
    assert!(s2.contains("dw=928") && s2.contains("vh=1139"), "{s2}");
    assert!(s2.contains("width=\"928\""), "{s2}");
    assert!(
        !s2.contains("height=\""),
        "切片页不写死高度，避免与真实落点错位拉扁"
    );
}
