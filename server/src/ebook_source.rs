//! 从书目录（目录即书）读取「可导出的书视图」。
//!
//! 只读：不抓网络、不写任何表。没有完整书目录时返回 409，由路由层直接透传。

use sqlx::SqlitePool;

use crate::entities::{BookChapter, BookContent};
use crate::error::{AppError, AppResult};
use crate::library_store::{comic, read as store_read, Store};

use boranlib_ebook::model::{BookInput, BookMeta, ChapterKind, ChapterRef, ComicInput, PageData};
use boranlib_ebook::pipeline::ChapterReader;

/// 目录里没有书源 URL 的章节（管理员新建等）用稳定合成 URL，保证阅读进度与搜索可引用。
pub fn local_chapter_url(book_url: &str, id: &str) -> String {
    format!("local:{book_url}#{id}")
}

/// 从合成 URL 取回章节 id；不是本书的合成 URL 返回 `None`。
pub fn local_chapter_id<'a>(book_url: &str, url: &'a str) -> Option<&'a str> {
    url.trim()
        .strip_prefix("local:")?
        .strip_prefix(book_url.trim())?
        .strip_prefix('#')
        .filter(|id| !id.is_empty())
}

/// 目录书的章节列表：文字书读 OPF spine，漫画读书目录边车。
///
/// 返回 `None` 表示没有可用的书目录；返回空列表表示目录存在但确实没有章节。
pub async fn load_dir_toc(store: &Store, book_url: &str) -> AppResult<Option<Vec<BookChapter>>> {
    if let Some(book) = store.open_book(book_url).await? {
        return Ok(Some(
            book.meta
                .chapters
                .iter()
                .enumerate()
                .map(|(i, c)| BookChapter {
                    url: c
                        .url
                        .clone()
                        .unwrap_or_else(|| local_chapter_url(book_url, &c.id)),
                    title: Some(c.title.clone()),
                    is_volume: c.is_volume,
                    book_url: Some(book_url.to_string()),
                    index: i as i64,
                    ..Default::default()
                })
                .collect(),
        ));
    }
    if let Some(book) = store.open_comic(book_url).await? {
        return Ok(Some(comic_book_chapters(book_url, &book.meta)));
    }
    Ok(None)
}

/// 漫画边车 → 与 `/api/book/toc` 一致的章节行。
pub fn comic_book_chapters(book_url: &str, meta: &comic::ComicMeta) -> Vec<BookChapter> {
    meta.chapters
        .iter()
        .enumerate()
        .map(|(i, c)| BookChapter {
            url: c
                .url
                .clone()
                .unwrap_or_else(|| local_chapter_url(book_url, &c.id)),
            title: Some(c.title.clone()),
            is_volume: c.is_volume,
            book_url: Some(book_url.to_string()),
            index: i as i64,
            ..Default::default()
        })
        .collect()
}

/// 导出视图：书目录是唯一来源，没有完整目录就不能导出。
pub async fn load_book_input_with_store(
    store: &Store,
    pool: &SqlitePool,
    book_url: &str,
) -> AppResult<BookInput> {
    let book_url = book_url.trim();
    if book_url.is_empty() {
        return Err(AppError::BadRequest("book_url 不能为空".into()));
    }
    match dir_book_input(store, pool, book_url).await? {
        Some(input) => Ok(input),
        None => Err(AppError::Conflict(
            "这本书还没有完整的本地目录，无法导出".into(),
        )),
    }
}

async fn dir_book_input(
    store: &Store,
    pool: &SqlitePool,
    book_url: &str,
) -> AppResult<Option<BookInput>> {
    let Some(opened) = store.open_book(book_url).await? else {
        return Ok(None);
    };
    if opened.meta.chapters.is_empty() {
        return Ok(None);
    }

    // 目录是元数据权威；书源字段只保留 OPF 没有的展示信息。
    let source: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT origin, origin_name FROM library_books WHERE book_url = ?1")
            .bind(book_url)
            .fetch_optional(pool)
            .await?;
    let (origin, origin_name) = source.unwrap_or_default();

    let mut chapters = Vec::with_capacity(opened.meta.chapters.len());
    let mut seq = 0i64;
    for item in &opened.meta.chapters {
        if item.is_volume {
            chapters.push(ChapterRef {
                url: item
                    .url
                    .clone()
                    .unwrap_or_else(|| local_chapter_url(book_url, &item.id)),
                title: item.title.clone(),
                seq: 0,
                is_volume: true,
                kind: ChapterKind::Text,
            });
        } else {
            seq += 1;
            chapters.push(ChapterRef {
                url: item
                    .url
                    .clone()
                    .unwrap_or_else(|| local_chapter_url(book_url, &item.id)),
                title: item.title.clone(),
                seq,
                is_volume: false,
                kind: ChapterKind::Text,
            });
        }
    }

    let mut cached_chapters = 0i64;
    let mut cached_bytes = 0i64;
    for item in &opened.meta.chapters {
        if item.is_volume {
            continue;
        }
        match store_read::read_chapter_file(&opened.dir, &item.href).await {
            Ok(Some(xhtml)) => {
                if store_read::parse_chapter(&xhtml, &item.title).is_some() {
                    cached_chapters += 1;
                    cached_bytes += xhtml.len() as i64;
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(
                book_url,
                href = %item.href,
                error = %err,
                "chapter file unreadable"
            ),
        }
    }

    let total_chapters = seq;
    let meta = BookMeta {
        book_url: book_url.to_string(),
        name: opened.meta.title.clone(),
        author: opened.meta.author.clone().unwrap_or_default(),
        language: Some(opened.meta.language.clone()).filter(|s| !s.trim().is_empty()),
        publisher: opened.meta.publisher.clone(),
        pubdate: opened.meta.pubdate.clone(),
        series_name: opened.meta.series.clone(),
        series_index: opened.meta.series_index,
        intro: opened.meta.description.clone(),
        source: origin_name.filter(|s| !s.trim().is_empty()).or(origin),
        identifiers: opened
            .meta
            .isbn
            .clone()
            .map(|isbn| vec![("isbn".to_string(), isbn)])
            .unwrap_or_default(),
        tags: opened.meta.subjects.clone(),
    };

    Ok(Some(BookInput {
        meta,
        chapters,
        total_chapters,
        cached_chapters,
        missing_chapters: (total_chapters - cached_chapters).max(0),
        cached_bytes,
        text_chapters: total_chapters,
        cached_text_chapters: cached_chapters,
    }))
}

/// 读路径的单章视图：目录命中返回归一化正文，未命中/占位返回 `None`。
pub async fn load_dir_content(
    store: &Store,
    pool: &SqlitePool,
    book_url: &str,
    chapter_url: &str,
) -> AppResult<Option<BookContent>> {
    let Some(opened) = store.open_book(book_url).await? else {
        return Ok(None);
    };
    let Some((pos, item)) = opened
        .meta
        .chapter_index_by_ref(book_url, chapter_url)
        .map(|i| (i, &opened.meta.chapters[i]))
    else {
        return Ok(None);
    };
    if item.is_volume {
        return Ok(None);
    }
    let Some(mut body) = store_read::load_chapter(&opened.dir, item).await? else {
        // 书源书的章节文件只是抓取缓存/占位，解析不出正文时仍应回退到网络；
        // 只有本地合成章节（`local:<book>#<id>`）没有书源可回退，返回空正文让
        // 阅读端显示空白页（SVG 封面、空页），而不是落到「source not found」。
        if !chapter_url.starts_with("local:") {
            return Ok(None);
        }
        let _ = crate::library::touch_book(pool, book_url).await;
        return Ok(Some(BookContent {
            book_url: book_url.to_string(),
            url: chapter_url.to_string(),
            title: Some(item.title.clone()),
            content: Some(String::new()),
            chapter_index: pos as i64,
            content_type: "text".into(),
            ..Default::default()
        }));
    };
    // 章内插图存的是 `images/<hash>.jpg` 这类相对路径（EPUB 需要相对地址）；
    // 阅读端统一改成 `oread-img://` 本地 URL，走同一个图片代理（含墨水屏转码）。
    let book_id = Store::book_id(book_url);
    localize_images(
        &mut body.blocks,
        &format!("OEBPS/{}", item.href),
        &book_id,
        opened.meta.revision,
    );
    let html = store_read::blocks_to_html(&body.blocks);
    let _ = crate::library::touch_book(pool, book_url).await;
    Ok(Some(BookContent {
        book_url: book_url.to_string(),
        url: chapter_url.to_string(),
        title: Some(item.title.clone()),
        content: Some(html),
        chapter_index: pos as i64,
        content_type: "text".into(),
        ..Default::default()
    }))
}

/// 把章内相对图片路径改成带修订号的本地图片 URL；远程地址原样保留。
///
/// 相对地址必须相对**章节文件所在目录**解析。导入的 EPUB 章节不一定在
/// `OEBPS/chapters/`（可能嵌套多层），所以不能再简单剥一个 `../` 后拼
/// `OEBPS/`，否则会得到 `OEBPS/../Images/x.jpg` 这类被安全校验拒绝的路径。
fn localize_images(
    blocks: &mut [boranlib_ebook::html::Block],
    chapter_href: &str,
    book_id: &str,
    revision: u64,
) {
    use boranlib_ebook::html::Block;
    let from_dir = chapter_href
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("");
    for block in blocks.iter_mut() {
        let Block::Image { src, .. } = block else {
            continue;
        };
        let raw = src.trim();
        if raw.is_empty() || raw.contains("://") || raw.starts_with("data:") {
            continue;
        }
        let path = crate::library_store::href_to_path(raw.split(['?', '#']).next().unwrap_or(raw));
        let Some(href) = resolve_relative_path(from_dir, &path) else {
            continue;
        };
        *src = comic::local_image_url_v(book_id, &href, revision);
    }
}

/// 把 `from_dir` 下的相对地址折叠成书根目录下的路径（`OEBPS/...`）。
/// 越出书根或含绝对路径时返回 `None`，交给后续安全校验拒绝。
fn resolve_relative_path(from_dir: &str, href: &str) -> Option<String> {
    if href.starts_with('/') || href.contains(':') || href.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = from_dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s if s.starts_with('.') => return None,
            s => parts.push(s),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// 漫画导出视图：书目录是唯一来源，按 `oread.json` 页表逐页 `stat`。
pub async fn load_comic_input(
    store: &crate::library_store::Store,
    pool: &SqlitePool,
    book_url: &str,
) -> AppResult<ComicInput> {
    let book_url = book_url.trim();
    let Some(comic) = store.open_comic(book_url).await? else {
        return Err(AppError::Conflict(
            "这本书还没有本地漫画目录，无法导出".into(),
        ));
    };

    let source: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT origin, origin_name FROM library_books WHERE book_url = ?1")
            .bind(book_url)
            .fetch_optional(pool)
            .await?;
    let (origin, origin_name) = source.unwrap_or_default();

    let mut chapters = Vec::with_capacity(comic.meta.chapters.len());
    let mut seq = 0i64;
    let mut total_pages = 0i64;
    let mut available_pages = 0i64;
    let mut available_bytes = 0i64;
    let mut total_chapters = 0i64;
    let mut cached_chapters = 0i64;

    for chapter in &comic.meta.chapters {
        if chapter.is_volume {
            chapters.push(ChapterRef {
                url: chapter
                    .url
                    .clone()
                    .unwrap_or_else(|| local_chapter_url(book_url, &chapter.id)),
                title: chapter.title.clone(),
                seq: 0,
                is_volume: true,
                kind: ChapterKind::Image,
            });
            continue;
        }
        seq += 1;
        total_chapters += 1;
        chapters.push(ChapterRef {
            url: chapter
                .url
                .clone()
                .unwrap_or_else(|| local_chapter_url(book_url, &chapter.id)),
            title: chapter.title.clone(),
            seq,
            is_volume: false,
            kind: ChapterKind::Image,
        });
        total_pages += chapter.pages.len() as i64;
        let mut chapter_complete = !chapter.pages.is_empty();
        for page in &chapter.pages {
            let Ok(path) = crate::library_store::safe_join(&comic.dir, &page.href) else {
                chapter_complete = false;
                continue;
            };
            match tokio::fs::metadata(&path).await {
                Ok(meta) if meta.is_file() => {
                    available_pages += 1;
                    available_bytes += meta.len() as i64;
                }
                _ => chapter_complete = false,
            }
        }
        if chapter_complete {
            cached_chapters += 1;
        }
    }

    let meta = BookMeta {
        book_url: book_url.to_string(),
        name: comic.meta.title.clone(),
        author: comic.meta.author.clone().unwrap_or_default(),
        language: comic.meta.language.clone().filter(|s| !s.trim().is_empty()),
        publisher: comic.meta.publisher.clone(),
        pubdate: comic.meta.pubdate.clone(),
        series_name: comic.meta.series.clone(),
        series_index: comic.meta.series_index,
        intro: comic.meta.description.clone(),
        source: origin_name.filter(|s| !s.trim().is_empty()).or(origin),
        identifiers: comic
            .meta
            .isbn
            .clone()
            .map(|isbn| vec![("isbn".to_string(), isbn)])
            .unwrap_or_default(),
        tags: comic.meta.subjects.clone(),
    };

    Ok(ComicInput {
        book: BookInput {
            meta,
            chapters,
            total_chapters,
            cached_chapters,
            missing_chapters: (total_chapters - cached_chapters).max(0),
            cached_bytes: 0,
            text_chapters: 0,
            cached_text_chapters: 0,
        },
        total_pages,
        available_pages,
        available_bytes,
    })
}

/// 馆藏正文读取器：writer 只看到「按 URL 取一章」，正文直接读已打开的书目录。
pub struct LibraryReader {
    book: store_read::OpenedBook,
}

impl LibraryReader {
    pub fn new(book: store_read::OpenedBook) -> Self {
        Self { book }
    }
}

#[axum::async_trait]
impl ChapterReader for LibraryReader {
    async fn chapter_text(&self, url: &str) -> anyhow::Result<Option<String>> {
        let Some(item) = self.book.meta.chapter_by_url(url) else {
            return Ok(None);
        };
        let Some(body) = store_read::load_chapter(&self.book.dir, item).await? else {
            return Ok(None);
        };
        Ok(Some(body.html))
    }

    /// 文字书目录没有页表；漫画导出走原样打包，不需要逐页读取。
    async fn chapter_pages(&self, _url: &str) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn page_data(&self, _url: &str) -> anyhow::Result<Option<PageData>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::localize_images;
    use boranlib_ebook::html::Block;

    #[test]
    fn relative_images_become_versioned_local_urls() {
        let mut blocks = vec![
            Block::Paragraph("正文".into()),
            Block::Image {
                src: "../images/a.jpg".into(),
                alt: String::new(),
            },
            Block::Image {
                src: "https://remote.test/b.jpg".into(),
                alt: String::new(),
            },
        ];
        localize_images(&mut blocks, "OEBPS/chapters/ch-0001.xhtml", "abc123", 7);
        assert_eq!(
            blocks[1],
            Block::Image {
                src: "oread-img://abc123/OEBPS/images/a.jpg?v=7".into(),
                alt: String::new(),
            }
        );
        assert_eq!(
            blocks[2],
            Block::Image {
                src: "https://remote.test/b.jpg".into(),
                alt: String::new(),
            }
        );
    }
}
