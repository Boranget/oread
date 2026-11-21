//! `migrate-store` 的端到端测试：旧库（正文/图片在 SQLite）→ 目录即书。

use std::path::{Path, PathBuf};
use std::str::FromStr;

use boranlib::library_store::{migrate, Store};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "oread-migrate-{tag}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn pool(dir: &Path) -> SqlitePool {
    let opts =
        SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("library.db").display()))
            .unwrap()
            .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::migrate!("./migrations/library")
        .run(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn migrates_text_book_into_valid_epub_dir() {
    let dir = temp_dir("text");
    let pool = pool(&dir).await;
    sqlx::query(
        "INSERT INTO library_books (book_url, name, author, intro, language, publisher, \
         pubdate, series_name, series_index, tier) \
         VALUES ('https://s/book/1', '测试书', '作者', '简介', 'zh', '出版社', '2024-01-01', '系列', 2.0, 'collection')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO library_tags (book_url, tag) VALUES ('https://s/book/1', '玄幻')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
         VALUES ('https://s/book/1', 'https://s/book/1/1', '开端', 0, 0), \
                ('https://s/book/1', 'https://s/book/1/2', '第一卷', 1, 1), \
                ('https://s/book/1', 'https://s/book/1/3', '未缓存', 0, 2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_content (url, origin, book_url, title, content, chapter_index, content_type) \
         VALUES ('https://s/book/1/1', 'src', 'https://s/book/1', '开端', \
                 '<p>第一段</p><p>第二段 &amp; 更多</p>', 0, 'text')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let store = Store::new(&dir);
    let report = migrate::migrate_from_db(&pool, &store).await.unwrap();
    assert_eq!(report.books, 1);
    assert_eq!(report.chapters, 3);
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let book = store.find("https://s/book/1").unwrap();
    assert_eq!(book.0, boranlib::library_store::Tier::Collection);
    let oebps = book.1.join("OEBPS");
    assert_eq!(
        std::fs::read(book.1.join("mimetype")).unwrap(),
        b"application/epub+zip"
    );

    let opf = std::fs::read_to_string(oebps.join("content.opf")).unwrap();
    assert!(opf.contains("<dc:title>测试书</dc:title>"));
    assert!(opf.contains("<dc:subject>玄幻</dc:subject>"));
    assert!(opf.contains("belongs-to-collection"));
    assert!(opf.contains("oread:chapter-url"));

    let reparse = store.read_opf(&book.1).await.unwrap();
    assert_eq!(reparse.title, "测试书");
    assert_eq!(reparse.series.as_deref(), Some("系列"));
    assert_eq!(reparse.series_index, Some(2.0));
    assert_eq!(reparse.chapters.len(), 3);
    assert_eq!(reparse.chapters[0].href, "chapters/ch-0001.xhtml");
    assert!(reparse.chapters[1].is_volume);
    assert_eq!(reparse.chapters[1].href, "chapters/vol-0002.xhtml");
    let pending = store
        .read_chapter(&book.1, &reparse.chapters[2].href)
        .await
        .unwrap()
        .unwrap();
    assert!(boranlib::library_store::is_pending_html(&pending));

    let html = store
        .read_chapter(&book.1, &reparse.chapters[0].href)
        .await
        .unwrap()
        .unwrap();
    assert!(html.contains("<p>第一段</p>"));
    assert!(html.contains("第二段 &amp; 更多"));

    // 读路径：目录优先，不再看 library_content。
    let input =
        boranlib::ebook_source::load_book_input_with_store(&store, &pool, "https://s/book/1")
            .await
            .unwrap();
    assert_eq!(input.meta.name, "测试书");
    assert_eq!(input.meta.author, "作者");
    assert_eq!(input.meta.tags, vec!["玄幻".to_string()]);
    assert_eq!(input.total_chapters, 2);
    assert_eq!(input.cached_chapters, 1);
    assert_eq!(input.chapters.len(), 3);
    assert!(input.chapters[1].is_volume);

    let opened = store.open_book("https://s/book/1").await.unwrap().unwrap();
    let reader =
        boranlib::ebook_source::LibraryReader::new(&pool, "https://s/book/1").with_dir(opened);
    let body =
        boranlib::ebook::pipeline::ChapterReader::chapter_text(&reader, "https://s/book/1/1")
            .await
            .unwrap()
            .unwrap();
    assert!(body.contains("<p>第一段</p>"));
    assert!(
        !body.contains("开端"),
        "OPF 标题不应重复出现在正文里: {body}"
    );

    let hit = boranlib::ebook_source::load_dir_content(
        &store,
        &pool,
        "https://s/book/1",
        "https://s/book/1/1",
    )
    .await
    .unwrap()
    .expect("已缓存章应命中目录");
    assert_eq!(hit.content_type, "text");
    assert_eq!(hit.book_url, "https://s/book/1");
    assert!(hit.content.unwrap().contains("第二段"));

    let miss = boranlib::ebook_source::load_dir_content(
        &store,
        &pool,
        "https://s/book/1",
        "https://s/book/1/3",
    )
    .await
    .unwrap();
    assert!(miss.is_none(), "占位章必须回退回源，而不是当正文返回");

    // 再跑一次：已存在的目录跳过，不覆盖。
    let again = migrate::migrate_from_db(&pool, &store).await.unwrap();
    assert_eq!(again.skipped, 1);
    assert_eq!(again.chapters, 0);

    pool.close().await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn migrates_comic_into_flat_cbz_dir() {
    let dir = temp_dir("comic");
    let pool = pool(&dir).await;
    sqlx::query(
        "INSERT INTO library_books (book_url, name, author, tier) \
         VALUES ('https://s/comic/1', '漫画', '漫画家', 'temp')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_chapters (book_url, url, title, is_volume, \"index\") \
         VALUES ('https://s/comic/1', 'https://s/comic/1/1', '第1话', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO library_content (url, origin, book_url, title, content_type, images, images_ready) \
         VALUES ('https://s/comic/1/1', 'src', 'https://s/comic/1', '第1话', 'image', \
                 '[{\"index\":0,\"url\":\"https://img/1.jpg\"},{\"index\":1,\"url\":\"https://img/2.png\"}]', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (url, mime) in [
        ("https://img/1.jpg", "image/jpeg"),
        ("https://img/2.png", "image/png"),
    ] {
        sqlx::query(
            "INSERT INTO library_images (url, origin, book_url, mime, data, size_bytes) \
             VALUES (?1, 'src', 'https://s/comic/1', ?2, ?3, ?4)",
        )
        .bind(url)
        .bind(mime)
        .bind(vec![1u8, 2, 3, 4])
        .bind(4i64)
        .execute(&pool)
        .await
        .unwrap();
    }

    let store = Store::new(&dir);
    let report = migrate::migrate_from_db(&pool, &store).await.unwrap();
    assert_eq!(report.comics, 1);
    assert_eq!(report.images, 2);
    assert!(report.failures.is_empty(), "{:?}", report.failures);

    let (_tier, book) = store.find("https://s/comic/1").unwrap();
    assert!(book.join("ComicInfo.xml").is_file());
    assert!(book.join("0001-001.jpg").is_file());
    assert!(book.join("0001-002.png").is_file());
    assert!(migrate::is_comic_dir(&book));

    let info = std::fs::read_to_string(book.join("ComicInfo.xml")).unwrap();
    assert!(info.contains("<PageCount>2</PageCount>"));
    assert!(info.contains("<Title>漫画</Title>"));

    // 目录边车：章节标题/URL/页范围都保留，读路径不依赖旧库。
    let opened = store
        .open_comic("https://s/comic/1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opened.meta.title, "漫画");
    assert_eq!(opened.meta.author.as_deref(), Some("漫画家"));
    assert_eq!(opened.meta.chapters.len(), 1);
    let ch = &opened.meta.chapters[0];
    assert_eq!(ch.title, "第1话");
    assert_eq!(ch.url.as_deref(), Some("https://s/comic/1/1"));
    assert_eq!(ch.pages.len(), 2);
    assert_eq!(ch.pages[0].href, "0001-001.jpg");
    assert_eq!(ch.pages[0].mime, "image/jpeg");
    assert_eq!(ch.pages[1].href, "0001-002.png");
    assert_eq!(
        boranlib::library_store::comic::read_page(&book, &ch.pages[0].href)
            .await
            .unwrap()
            .unwrap(),
        vec![1u8, 2, 3, 4]
    );

    let local = boranlib::library_store::comic::local_image_url(
        &Store::book_id("https://s/comic/1"),
        &ch.pages[0].href,
    );
    let (id, href) = boranlib::library_store::comic::parse_local_image_url(&local).unwrap();
    assert_eq!(id, Store::book_id("https://s/comic/1"));
    assert_eq!(href, "0001-001.jpg");
    assert!(store.find_by_id(id).is_some());

    // 目录优先的 TOC：漫画章节从边车来，不再查 library_chapters。
    let toc = boranlib::ebook_source::load_dir_toc(&store, "https://s/comic/1")
        .await
        .unwrap()
        .expect("漫画目录应提供 TOC");
    assert_eq!(toc.len(), 1);
    assert_eq!(toc[0].title.as_deref(), Some("第1话"));
    assert_eq!(toc[0].url, "https://s/comic/1/1");

    pool.close().await;
    std::fs::remove_dir_all(dir).ok();
}
