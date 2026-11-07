//! /k 的 comic 部分（move-only 从 kindle.rs 拆出）。

use super::*;

// ---------------- 漫画（一屏一页） ----------------

/// 漫画页：某张图的整图，或该图的第 `slice` 片（0 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComicPage {
    /// `images` 里的下标
    pub(super) img: usize,
    pub(super) slice: usize,
    /// 该图总片数（1 = 整图）
    pub(super) layers: usize,
    /// 显示宽（CSS px）
    pub(super) width: usize,
    /// 显示高（CSS px）；0 = 尺寸未知，渲染时退化到 `max-height`
    pub(super) height: usize,
}

/// 由图片列表与原始宽高构建页表，保证每页显示高 ≤ 漫画可用高（`vp.content_h()`，1268 → 1184）。
/// - 长条页（切片开关开启且 `slice_count_window` > 1）：一图拆 `layers` 页，
///   非末片按可用高、末片取剩余；
/// - 一屏内：整图按版心 928 宽，显示高超过可用高时按高度等比缩小；
/// - 尺寸未知：整图一页，只给宽度。
pub(super) fn comic_pages(
    images: &[BookImage],
    dims: &[Option<(u32, u32)>],
    cfg: &crate::config::Config,
    sp: &image::SliceParams,
) -> Vec<ComicPage> {
    let dw = sp.dw as usize;
    let content_h = sp.vh as usize;
    let mut pages = Vec::new();
    for i in 0..images.len() {
        let known = dims
            .get(i)
            .copied()
            .flatten()
            .filter(|(w, h)| *w > 0 && *h > 0);
        let Some((w, h)) = known else {
            pages.push(ComicPage {
                img: i,
                slice: 0,
                layers: 1,
                width: 0,
                height: 0,
            });
            continue;
        };
        // 开关关闭时恒按整图一页，与切片上线前的行为一致。
        let n = if cfg.comic_slice {
            image::slice_count_window(w, h, sp)
        } else {
            1
        };
        if n > 1 {
            // 切片的真实高度由服务端按近白行落点决定（实测 1150/667，窗口预判只有 1184/620），
            // 写死 height 会把图拉扁；这里只给宽度，页面高度由 `vh−24` 的 stage 兜住
            // （漫画页保留像素高：切片高度按 `sp.vh` 生成，改成拉伸填满会丢图）。
            for k in 0..n {
                pages.push(ComicPage {
                    img: i,
                    slice: k,
                    layers: n,
                    width: dw,
                    height: 0,
                });
            }
        } else {
            let scaled = ((h as f64) * dw as f64 / w as f64).round() as usize;
            if scaled > content_h {
                // 1.1 屏这类「不切但一屏放不下」的整图：按高度约束等比缩小，仍是一屏一页
                let cw = ((content_h as f64) * w as f64 / h as f64).round() as usize;
                pages.push(ComicPage {
                    img: i,
                    slice: 0,
                    layers: 1,
                    width: cw.max(1),
                    height: content_h,
                });
            } else {
                pages.push(ComicPage {
                    img: i,
                    slice: 0,
                    layers: 1,
                    width: dw,
                    height: scaled.max(1),
                });
            }
        }
    }
    pages
}

/// 渲染漫画页：`#k-stage` 与小说页同高同宽，图片给显式宽高（不用 `height:auto`，避免抖动）。
/// 只有长条页的切片才带 `slice=1&i=k`，整图页维持旧 URL（桌面端与 `slice=0` 路径完全不受影响）。
#[allow(clippy::too_many_arguments)] // 漫画渲染上下文（图集/页码/源/书/章/meta/视口），拆分会把同一屏的数据分散到多处。
pub(super) fn comic_stage(
    images: &[BookImage],
    pg: &ComicPage,
    source_id: &str,
    book_url: &str,
    chapter_url: &str,
    meta: &str,
    vp: Vp,
    sp: &image::SliceParams,
) -> String {
    let im = &images[pg.img];
    let mut src = format!(
        "/api/book/image?source={}&book_url={}&chapter_url={}&url={}&eink=1",
        qenc(source_id),
        qenc(book_url),
        qenc(chapter_url),
        qenc(&im.url)
    );
    if pg.layers > 1 {
        // 切片 URL 带 dw/vh：缓存 key 天然按尺寸隔离，换视口不会命中别的尺寸的旧片
        src.push_str(&format!(
            "&slice=1&i={}&dw={}&vh={}",
            pg.slice, sp.dw, sp.vh
        ));
    }
    let img = if pg.height > 0 {
        format!(
            "<img src=\"{src}\" width=\"{w}\" height=\"{h}\" style=\"display:block;max-width:100%\">",
            src = src,
            w = pg.width,
            h = pg.height
        )
    } else if pg.width > 0 {
        // 切片：真实高度由服务端按落点决定，这里只给宽度，避免把图拉扁
        format!(
            "<img src=\"{src}\" width=\"{w}\" style=\"display:block;max-width:100%\">",
            src = src,
            w = pg.width
        )
    } else {
        format!(
            "<img src=\"{src}\" style=\"display:block;max-width:100%\">",
            src = src
        )
    };
    format!(
        "<div id=\"k-stage\" style=\"position:relative;top:0;left:0;right:auto;bottom:auto;margin-top:{nav}px;height:{h}px;width:{w}px;padding:0 {pad}px {pb}px;overflow:hidden\">{img}<div id=\"k-page-no\">{meta}</div></div>",
        nav = READ_NAV_H,
        h = vp.layout_h().saturating_sub(READ_NAV_H),
        w = vp.page_w(),
        pad = SIDE_PAD,
        pb = vp.bar_h() + 8,
        img = img,
        meta = esc(meta)
    )
}

/// 取本章图片：先读 `book_content.images` 缓存，未命中回源并按 `/api/comic/chapter` 同规则回写。
pub(super) async fn load_comic_images(
    s: &AppState,
    pool: &SqlitePool,
    source: &BookSource,
    chapter: &BookChapter,
    book_url: &str,
) -> Result<Vec<BookImage>, String> {
    if let Ok(Some(c)) = crate::routes::comic::load_cached_images(
        pool,
        &chapter.url,
        &source.book_source_url,
        s.cfg.content_ttl_secs,
    )
    .await
    {
        if c.content_type == "image" {
            let (v, _) = crate::routes::comic::decode_images(c.images_json.as_deref());
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    let parsed = web_book::chapter_images(source, chapter)
        .await
        .map_err(|e| format!("本章图片抓取失败: {e}"))?;
    let images = parsed.images;
    if images.is_empty() {
        return Ok(images);
    }
    let payload = serde_json::to_string(&web_book::ChapterImages {
        images: images.clone(),
        truncated: parsed.truncated,
    })
    .unwrap_or_default();
    let c = BookContent {
        url: chapter.url.clone(),
        origin: source.book_source_url.clone(),
        book_url: book_url.to_string(),
        title: chapter.title.clone(),
        content: Some(String::new()),
        chapter_index: chapter.index,
        content_type: "image".into(),
        images_json: Some(payload),
        ..Default::default()
    };
    let limit = match replace::user_cache_limit_mb(pool).await {
        Some(mb) => (mb * 1024 * 1024).min(s.cfg.cache_limit_bytes()),
        None => s.cfg.cache_limit_bytes(),
    };
    if let Err(e) = save_content_cached(pool, &c, limit).await {
        tracing::warn!(url = %chapter.url, error = %e, "comic content cache write failed");
    }
    Ok(images)
}
