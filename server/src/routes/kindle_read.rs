//! /k 的 read 部分（move-only 从 kindle.rs 拆出）。

use super::*;

// ---------------- 正文 ----------------

pub(super) async fn read(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<ReadQ>,
) -> Response {
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let vp = vp_of(&s, &headers, q.vw, q.vh);
    let (source, chapters) = match ensure_chapters(&s, &db.0, &q.source, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_to_page(e),
    };
    if chapters.is_empty() {
        return err_page("目录为空");
    }
    let ch = q.ch.unwrap_or(0).clamp(0, chapters.len() as i64 - 1);
    let chapter: &BookChapter = &chapters[ch as usize];
    // 目录已不展示分卷行；兜底拦截直接访问（合成 URL `volume:`）时给出友好提示，不抛错误页。
    if volume_entry(chapter) {
        return volume_notice(&q, fs, explicit, vp);
    }
    // 漫画源：一屏一页（整图或切片），不走正文分页
    if source.book_source_type == 2 {
        return comic_read(&s, &db.0, &source, &chapters, ch, &q, fs, explicit, vp).await;
    }

    let mut c: BookContent = match load_cached_content(
        &db.0,
        &chapter.url,
        &source.book_source_url,
        s.cfg.content_ttl_secs,
    )
    .await
    {
        Ok(Some(c)) => c,
        _ => {
            let mut fetched = match web_book::chapter_content(&source, chapter).await {
                Ok(c) => c,
                Err(e) => return err_page(&format!("正文抓取失败: {e}")),
            };
            fetched.origin = source.book_source_url.clone();
            let limit = match replace::user_cache_limit_mb(&db.0).await {
                Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
                None => s.cfg.cache_limit_bytes(),
            };
            let _ = save_content_cached(&db.0, &fetched, limit).await;
            fetched
        }
    };
    if c.book_url.trim().is_empty() {
        c.book_url = q.book.clone();
    }
    if c.title.is_none() {
        c.title = chapter.title.clone();
    }
    // 与主站一致：正文输出前应用用户替换规则
    let book_name = load_book_meta(&db.0, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_default();
    let (title, body_html) = replace::purify(
        &db.0,
        &book_name,
        &q.source,
        c.title.clone(),
        c.content.clone(),
    )
    .await;
    let mut text = html_to_text(body_html.as_deref().unwrap_or(""));
    let truncated = truncate_to_chars(&mut text, K_MAX_TEXT_CHARS);
    let spec = &FS_SPECS[fs - 1];
    // 每页行数由「正文可用高 ÷ 行高」动态算出（字号四档不再有 24/21/17/25 硬编码）
    let budget = line_budget(vp, spec.line);
    let plan = plan_pages(&text, spec, budget, vp.page_w());
    let n = plan.pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let (block_idx, cur_start, cur_end) = plan.pages[(p - 1) as usize];
    let cur = &text[cur_start..cur_end];
    let blocks = plan.chunks.len();

    let ch_title = title.clone().unwrap_or_else(|| format!("第 {} 章", ch + 1));
    // 打开阅读页即落库：进度在服务端渲染路径自动 upsert（幂等，重复渲染不写脏）
    let _ = record_progress(
        &db.0,
        &q.book,
        ch,
        p as f64 / n as f64,
        Some(chapters.len() as i64),
    )
    .await;
    let toc_url = toc_link(&q, fs, ch, p, vp);
    let (prev, next) = read_nav(&q, fs, ch, p, n, chapters.len() as i64, &toc_url, vp);
    let mut meta = if blocks > 1 {
        // 超长章节按 20 万字符分块：底栏“目录”格下方同时给出页内进度、页码与分块号
        let pct = ((p as f64 * 100.0 / n as f64).round() as usize).clamp(1, 100);
        format!("加载中{pct}%·{p}/{n}·{}/{}块", block_idx + 1, blocks)
    } else {
        format!("{p}/{n}")
    };
    if truncated {
        meta.push_str("·已截断");
    }
    // 进度提交已自动化（打开页面即写库），底栏不再有「记录」格
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(&q, next_fs, ch, p, vp);

    let mut paras = String::new();
    let gap = para_gap_px(spec.line);
    let mut first_para = true;
    for para in cur.split('\n') {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        let mt = if first_para { 0 } else { gap };
        first_para = false;
        paras.push_str(&format!(
            "<p style=\"font-size:{size}px;line-height:{line}px;text-indent:2em;margin-top:{mt}px;word-wrap:break-word\">{para}</p>",
            size = spec.size,
            line = spec.line,
            mt = mt,
            para = esc(para)
        ));
    }
    // 阅读页正文区：高度 = 运行时可用高 − 顶栏 − 底栏（`read_only_css` 里写死），
    // 服务端按 `line_budget` 切好一屏文字，`data-lb` 供探针回报「本屏行数」。
    let stage = format!(
        "<div id=\"k-stage\" data-lb=\"{lb}\">{paras}<div id=\"k-page-no\">{meta}</div></div>",
        lb = budget,
        paras = paras,
        meta = esc(&meta)
    );
    // 贴底固定栏 3 格等分：前 ｜ 当前字号档位（点一下切下一档）｜ 后。
    // 目录入口只在顶栏右上角保留，底栏不再放「目录」；格内只留主标签，无第二行小字。
    let bottom = fixed_bar(
        &format!(
            "{prev}{fs}{next}",
            prev = bar_cell_prev(prev.as_ref(), vp),
            fs = bar_cell(&fs_href, spec.label, vp),
            next = bar_cell_next(next.as_ref(), vp),
        ),
        vp,
    );

    // 顶栏：书名（无书名时回落到章节名）+「← 书架」+「目录」，两者都带当前 fs/vw/vh
    let nav_title = if book_name.is_empty() {
        ch_title.as_str()
    } else {
        book_name.as_str()
    };
    let nav = read_nav_html(nav_title, &qlink("/k", fs, vp), &toc_url, vp);

    let body = format!(
        "{nav}{stage}{hot}{bottom}",
        nav = nav,
        stage = stage,
        hot = hot_zones(
            prev.as_ref().map(|x| x.0.as_str()),
            next.as_ref().map(|x| x.0.as_str()),
        ),
        bottom = bottom
    );
    let mut res = page_css(&ch_title, fs, &body, &read_only_css(vp), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
