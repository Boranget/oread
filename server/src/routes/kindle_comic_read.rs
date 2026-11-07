//! /k 的 comic_read 部分（move-only 从 kindle.rs 拆出）。

use super::*;

/// 漫画阅读页：一屏一页（整图或切片），翻页/进度/字号与小说页同一套底栏与热区。
#[allow(clippy::too_many_arguments)] // 阅读页渲染上下文（状态/池/源/目录/章/查询/字号），与小说页保持同一组参数。
pub(super) async fn comic_read(
    s: &AppState,
    pool: &SqlitePool,
    source: &BookSource,
    chapters: &[BookChapter],
    ch: i64,
    q: &ReadQ,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let chapter = &chapters[ch as usize];
    let images = match load_comic_images(s, pool, source, chapter, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_page(&e),
    };
    if images.is_empty() {
        return err_page("本章没有图片");
    }
    // 尺寸：源页面已带宽高的直接用，缺失的批量探测（进程内缓存，同章重复打开不再回源）
    let mut dims: Vec<Option<(u32, u32)>> = images
        .iter()
        .map(|im| match (im.width, im.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        })
        .collect();
    let missing: Vec<String> = images
        .iter()
        .zip(dims.iter())
        .filter(|(_, d)| d.is_none())
        .map(|(im, _)| im.url.clone())
        .collect();
    if !missing.is_empty() {
        let probed = image::probe_dims(
            source,
            Some(&chapter.url),
            Some(&q.book),
            &missing,
            s.cfg.image_max_body_bytes.max(1024),
        )
        .await;
        let mut it = probed.into_iter();
        for d in dims.iter_mut() {
            if d.is_none() {
                *d = it.next().flatten();
            }
        }
    }
    // 切片参数按请求视口推导：版心宽 = vw − 144（1072 → 928），
    // 可用高 = 文档高 − 底栏 − 8 − 顶栏（1268 → 1184 − 56 = 1128），
    // 少切这一刀，整图/切片才不会被新顶栏压掉最后 56px
    let sp = image::SliceParams::from_config(
        &s.cfg,
        Some(vp.content_w() as u32),
        Some(vp.content_h().saturating_sub(READ_NAV_H) as u32),
        None,
    );
    let pages = comic_pages(&images, &dims, &s.cfg, &sp);
    if pages.is_empty() {
        return err_page("本章没有图片");
    }
    let n = pages.len() as i64;
    let p = q.p.unwrap_or(1).clamp(1, n);
    let pg = &pages[(p - 1) as usize];

    let toc_url = toc_link(q, fs, ch, p, vp);
    // 打开漫画页即落库（与小说页一致，幂等）
    let _ = record_progress(
        pool,
        &q.book,
        ch,
        p as f64 / n as f64,
        Some(chapters.len() as i64),
    )
    .await;
    let (prev, next) = read_nav(q, fs, ch, p, n, chapters.len() as i64, &toc_url, vp);
    let meta = format!("{p}/{n}");
    let next_fs = cycle_fs(fs);
    let fs_href = read_link(q, next_fs, ch, p, vp);

    let stage = comic_stage(
        &images,
        pg,
        &q.source,
        &q.book,
        &chapter.url,
        &meta,
        vp,
        &sp,
    );
    let bottom = fixed_bar(
        &format!(
            "{prev}{fs}{next}",
            prev = bar_cell_prev(prev.as_ref(), vp),
            fs = bar_cell(&fs_href, FS_SPECS[fs - 1].label, vp),
            next = bar_cell_next(next.as_ref(), vp),
        ),
        vp,
    );
    let title = chapter
        .title
        .clone()
        .unwrap_or_else(|| format!("第 {} 章", ch + 1));
    // 与小说页同一套顶栏：书名优先，取不到回落章节名
    let book_name = load_book_meta(pool, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_default();
    let nav_title = if book_name.is_empty() {
        title.as_str()
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
    let mut res = page_css(&title, fs, &body, &read_only_css(vp), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
