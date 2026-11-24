use super::*;

fn temp_root(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("oread-store-{tag}-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn book_id_is_stable_and_distinct() {
    let a = Store::book_id("https://example.com/book/1");
    let b = Store::book_id("https://example.com/book/1");
    let c = Store::book_id("https://example.com/book/2");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn slug_keeps_ascii_only() {
    assert_eq!(slug("Hello World!", 24), "hello-world");
    assert_eq!(slug("第一章 开端", 24), "");
    assert_eq!(slug("Chapter-01_final", 24), "chapter-01_final");
    assert_eq!(slug("aaaaaaaaaaaaaaaaaaaaaaaa", 8).len(), 8);
}

#[test]
fn chapter_number_increases_and_never_reuses() {
    let items = vec![
        ChapterItem {
            id: "ch-0001".into(),
            href: "chapters/ch-0001.xhtml".into(),
            title: "a".into(),
            url: None,
            is_volume: false,
        },
        ChapterItem {
            id: "vol-0002".into(),
            href: "chapters/vol-0002.xhtml".into(),
            title: "卷".into(),
            url: None,
            is_volume: true,
        },
        ChapterItem {
            id: "ch-0007".into(),
            href: "chapters/ch-0007.xhtml".into(),
            title: "b".into(),
            url: None,
            is_volume: false,
        },
    ];
    assert_eq!(next_chapter_number(&items), 8);
    assert_eq!(next_chapter_number(&[]), 1);
}

#[test]
fn safe_join_rejects_traversal() {
    let base = Path::new("/tmp/oebps");
    assert!(safe_join(base, "chapters/ch-0001.xhtml").is_ok());
    assert!(safe_join(base, "../content.opf").is_err());
    assert!(safe_join(base, "/etc/passwd").is_err());
}

#[test]
fn chapter_xhtml_escapes_and_keeps_order() {
    let html = chapter_xhtml(
        "zh",
        "标题 & <标签>",
        &text_blocks(&["甲 < 乙".into(), "  ".into(), "丙".into()]),
    );
    assert!(html.contains("<title>标题 &amp; &lt;标签&gt;</title>"));
    assert!(html.contains("<p>甲 &lt; 乙</p>"));
    assert!(!html.contains("<p></p>"), "空段落应被跳过");
    let first = html.find("甲 &lt; 乙").unwrap();
    let second = html.find("<p>丙</p>").unwrap();
    assert!(first < second);
}

#[test]
fn chapter_xhtml_keeps_image_blocks() {
    let blocks = vec![
        Block::Paragraph("前".into()),
        Block::Image {
            src: "https://a.test/x.jpg?a=1&b=2".into(),
            alt: "\"图\"".into(),
        },
    ];
    let html = chapter_xhtml("zh", "标题", &blocks);
    assert!(html.contains("<img src=\"https://a.test/x.jpg?a=1&amp;b=2\""));
    let back = super::read::body_blocks(&html, "标题");
    assert_eq!(back, blocks);
}

#[tokio::test]
async fn skeleton_is_a_valid_epub_tree() {
    let root = temp_root("skeleton");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();

    let mut meta = OpfMeta::new("https://example.com/book/1", "测试书");
    meta.author = Some("作者".into());
    let dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();

    assert!(dir.join("mimetype").is_file());
    assert!(dir.join("META-INF/container.xml").is_file());
    assert!(dir.join("OEBPS/content.opf").is_file());
    assert!(dir.join("OEBPS/nav.xhtml").is_file());
    assert!(dir.join("OEBPS/chapters/ch-0000-pending.xhtml").is_file());

    let mimetype = std::fs::read(dir.join("mimetype")).unwrap();
    assert_eq!(mimetype, b"application/epub+zip");

    let back = store.read_opf(&dir).await.unwrap();
    assert_eq!(back.title, "测试书");
    assert_eq!(back.book_url, "https://example.com/book/1");

    assert_eq!(
        store.find("https://example.com/book/1").unwrap().0,
        Tier::Temp
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn chapter_round_trip_and_opf_order() {
    let root = temp_root("chapter");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();

    let mut meta = OpfMeta::new("https://example.com/book/2", "书");
    let item = ChapterItem {
        id: "ch-0001".into(),
        href: "chapters/ch-0001-kai-duan.xhtml".into(),
        title: "开端".into(),
        url: Some("https://example.com/book/2/1".into()),
        is_volume: false,
    };
    meta.chapters.push(item.clone());
    let dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
    store
        .write_chapter(
            &dir,
            "zh",
            &item,
            &text_blocks(&["第一段".into(), "第二段".into()]),
        )
        .await
        .unwrap();
    meta.bump();
    store.write_opf(&dir, &meta).await.unwrap();
    store.write_nav(&dir, &meta).await.unwrap();

    let html = store
        .read_chapter(&dir, &item.href)
        .await
        .unwrap()
        .expect("章节应存在");
    assert!(html.contains("<p>第一段</p>"));
    assert!(html.contains("<p>第二段</p>"));

    let back = store.read_opf(&dir).await.unwrap();
    assert_eq!(back.chapters.len(), 1);
    assert_eq!(back.chapters[0].href, item.href);
    assert_eq!(back.chapters[0].title, "开端");
    assert_eq!(back.chapters[0].url, item.url);
    assert_eq!(back.revision, 1);

    let nav = std::fs::read_to_string(dir.join("OEBPS/nav.xhtml")).unwrap();
    assert!(nav.contains("ch-0001-kai-duan.xhtml"));
    assert!(nav.contains(">开端<"));

    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn promote_moves_temp_to_collection() {
    let root = temp_root("promote");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/3", "书");
    let temp_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();

    let (tier, dir) = store.promote(&meta.book_url).await.unwrap().unwrap();
    assert_eq!(tier, Tier::Collection);
    assert!(!temp_dir.exists());
    assert!(dir.join("OEBPS/content.opf").is_file());
    assert_eq!(store.find(&meta.book_url).unwrap().0, Tier::Collection);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn demote_moves_collection_back_to_temp() {
    let root = temp_root("demote");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/4", "书");
    store
        .ensure_skeleton(Tier::Collection, &meta)
        .await
        .unwrap();

    let (tier, dir) = store.demote(&meta.book_url).await.unwrap().unwrap();
    assert_eq!(tier, Tier::Temp);
    assert!(dir.join("OEBPS/content.opf").is_file());
    assert_eq!(store.find(&meta.book_url).unwrap().0, Tier::Temp);

    // 已经是临时书：幂等，不再搬。
    let again = store.demote(&meta.book_url).await.unwrap().unwrap();
    assert_eq!(again.0, Tier::Temp);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn move_to_trash_takes_dir_out_of_library() {
    let root = temp_root("trash");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/5", "书");
    let dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();

    assert!(store.move_to_trash(&meta.book_url).await.unwrap());
    assert!(!dir.exists());
    assert!(store.find(&meta.book_url).is_none());
    let trash: Vec<_> = std::fs::read_dir(root.join("library/trash"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(trash.len(), 1);
    assert!(trash[0].contains(&Store::book_id(&meta.book_url)));

    // 目录不在了：返回 false，而不是报错。
    assert!(!store.move_to_trash(&meta.book_url).await.unwrap());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn promote_marker_is_retried_by_maintenance() {
    let root = temp_root("promote-marker");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/6", "书");
    let temp_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();

    store.mark_promote_pending(&meta.book_url).await.unwrap();
    assert!(temp_dir.join(PROMOTE_MARKER).is_file());

    let moved = store.retry_pending_promotions().await.unwrap();
    assert_eq!(moved, 1);
    let (tier, dir) = store.find(&meta.book_url).unwrap();
    assert_eq!(tier, Tier::Collection);
    assert!(!dir.join(PROMOTE_MARKER).exists());
    assert!(!temp_dir.exists());

    // 没有标记时什么也不做。
    assert_eq!(store.retry_pending_promotions().await.unwrap(), 0);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn promote_prefers_real_temp_book_over_empty_collection_skeleton() {
    let root = temp_root("promote-empty");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/7", "书");
    let temp_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
    // 入馆失败后 TOC 又在 collection 建了空骨架：没有 OPF，不算一本书。
    let empty = store
        .ensure_dirs(Tier::Collection, &meta.book_url)
        .await
        .unwrap();
    assert!(!empty.join("OEBPS/content.opf").exists());

    // 读路径不能被空骨架挡住。
    assert_eq!(store.find(&meta.book_url).unwrap().0, Tier::Temp);

    let (tier, dir) = store.promote(&meta.book_url).await.unwrap().unwrap();
    assert_eq!(tier, Tier::Collection);
    assert_eq!(dir, empty);
    assert!(dir.join("OEBPS/content.opf").is_file());
    assert!(!temp_dir.exists());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn demote_keeps_real_temp_book_when_collection_is_empty_skeleton() {
    let root = temp_root("demote-empty");
    let store = Store::at(root.join("library"));
    store.create_layout().unwrap();
    let meta = OpfMeta::new("https://example.com/book/8", "书");
    let temp_dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
    let empty = store
        .ensure_dirs(Tier::Collection, &meta.book_url)
        .await
        .unwrap();

    let (tier, dir) = store.demote(&meta.book_url).await.unwrap().unwrap();
    assert_eq!(tier, Tier::Temp);
    assert_eq!(dir, temp_dir);
    assert!(dir.join("OEBPS/content.opf").is_file());
    assert!(!empty.exists());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn atomic_write_replaces_and_leaves_no_temp_files() {
    let root = temp_root("atomic");
    let path = root.join("OEBPS").join("content.opf");
    atomic_write(&path, b"v1").await.unwrap();
    atomic_write(&path, b"v2-longer").await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"v2-longer");
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["content.opf".to_string()]);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn lock_is_per_book() {
    use std::time::Duration;
    let guard = lock_book("https://example.com/book/lock-a").await;
    let other = tokio::time::timeout(
        Duration::from_millis(100),
        lock_book("https://example.com/book/lock-b"),
    )
    .await;
    assert!(other.is_ok(), "不同的书不应互相阻塞");
    let same = tokio::time::timeout(
        Duration::from_millis(100),
        lock_book("https://example.com/book/lock-a"),
    )
    .await;
    assert!(same.is_err(), "同一本书必须串行");
    drop(guard);
}
