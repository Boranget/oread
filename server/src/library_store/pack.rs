//! 「导出 = 原样打包」：把书目录直接打成 EPUB / CBZ。
//!
//! 目录本身就是合法的 EPUB 3 / 解压 CBZ，因此导出不再逐章重新生成，而是按固定顺序
//! 把文件写进 store-only ZIP：
//!
//! - EPUB：`mimetype` 必须是第一条（OCF 要求），随后 `META-INF/container.xml`，其余按路径排序；
//! - CBZ：`ComicInfo.xml` 第一条，其余按文件名排序；`oread.json` 是内部边车，不导出。
//!
//! 不完整或不存在目录时返回 `None`，调用方回退到旧的「按章节重新生成」路径。

use std::io;
use std::path::{Path, PathBuf};

use boranlib_ebook::zip::ZipWriter;
use tokio::fs::File;

use super::{comic, Store};

/// 目标格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    Epub,
    Cbz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackReport {
    pub entries: usize,
    pub bytes: u64,
}

/// 目录是否具备打包成对应格式的最小文件集。
///
/// 只做一次目录列举，不解析 OPF；真正的完整性由阅读/导出路径自己保证。
pub fn is_complete(dir: &Path, kind: PackKind) -> bool {
    match kind {
        PackKind::Epub => ["mimetype", "META-INF/container.xml", "OEBPS/content.opf"]
            .iter()
            .all(|rel| dir.join(rel).is_file()),
        PackKind::Cbz => dir.join(comic::COMIC_INFO_FILE).is_file() && has_image(dir),
    }
}

fn has_image(dir: &Path) -> bool {
    const EXTS: [&str; 7] = ["jpg", "jpeg", "png", "webp", "gif", "avif", "bmp"];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|ent| {
        ent.file_type().map(|t| t.is_file()).unwrap_or(false)
            && ent
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
    })
}

/// 有完整目录时直接打包；否则 `None`，调用方走回退路径。
pub async fn pack_book(
    store: &Store,
    book_url: &str,
    kind: PackKind,
    out: &Path,
) -> io::Result<Option<PackReport>> {
    let Some((_tier, dir)) = store.find(book_url) else {
        return Ok(None);
    };
    if !is_complete(&dir, kind) {
        return Ok(None);
    }
    pack_dir(&dir, kind, out).await.map(Some)
}

/// 把目录打成 ZIP。`out` 由调用方创建父目录；已存在会被覆盖。
pub async fn pack_dir(dir: &Path, kind: PackKind, out: &Path) -> io::Result<PackReport> {
    if !is_complete(dir, kind) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "书目录不完整，无法直接打包",
        ));
    }
    let mut items = collect_files(dir)?
        .into_iter()
        .filter_map(|path| rel_name(dir, &path).map(|name| (name, path)))
        .filter(|(name, _)| kind == PackKind::Epub || name != comic::META_FILE)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        rank(kind, &a.0)
            .cmp(&rank(kind, &b.0))
            .then_with(|| a.0.cmp(&b.0))
    });

    let entries = items.len();
    let file = File::create(out).await?;
    let mut zip = ZipWriter::new(file);
    for (name, path) in items {
        let data = tokio::fs::read(&path).await?;
        zip.add_stored(&name, &data)
            .await
            .map_err(io::Error::other)?;
    }
    let bytes = zip.finish().await.map_err(io::Error::other)?;
    Ok(PackReport { entries, bytes })
}

/// 排序权重：EPUB 的 `mimetype`、CBZ 的 `ComicInfo.xml` 必须排在最前。
fn rank(kind: PackKind, name: &str) -> u8 {
    match kind {
        PackKind::Epub => match name {
            "mimetype" => 0,
            "META-INF/container.xml" => 1,
            _ => 2,
        },
        PackKind::Cbz => {
            if name == comic::COMIC_INFO_FILE {
                0
            } else {
                1
            }
        }
    }
}

fn rel_name(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// 递归收集普通文件，跳过点文件（`.oread-promote` 等内部标记）。
fn collect_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in std::fs::read_dir(&dir)? {
            let ent = ent?;
            let name = ent.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let ft = ent.file_type()?;
            if ft.is_dir() {
                stack.push(ent.path());
            } else if ft.is_file() {
                out.push(ent.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_store::opf::OpfMeta;
    use crate::library_store::{ChapterItem, Tier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-pack-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn packs_text_book_as_epub_with_mimetype_first() {
        let root = temp_root("epub");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let mut meta = OpfMeta::new("https://example.com/p/1", "打包测试");
        let item = ChapterItem {
            id: "ch-0001".into(),
            href: "chapters/ch-0001-a.xhtml".into(),
            title: "第一章".into(),
            url: Some("https://example.com/p/1/1".into()),
            is_volume: false,
        };
        meta.chapters.push(item.clone());
        let dir = store
            .ensure_skeleton(Tier::Collection, &meta)
            .await
            .unwrap();
        store
            .write_chapter(&dir, "zh", &item, &["正文".into()])
            .await
            .unwrap();

        let out = root.join("book.epub");
        let report = pack_book(&store, &meta.book_url, PackKind::Epub, &out)
            .await
            .unwrap()
            .expect("完整目录应直接打包");
        assert!(report.entries >= 5);
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"PK\x03\x04"));
        // 本地文件头：签名 4 字节 + 版本 2 + 标志 2 + 压缩 2 + 时间 2 + 日期 2 +
        // CRC 4 + 压缩大小 4 + 原始大小 4 + 名称长度 2 + 扩展长度 2 = 30。
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(report.bytes, bytes.len() as u64);
    }

    #[tokio::test]
    async fn packs_comic_as_cbz_without_oread_sidecar() {
        let root = temp_root("cbz");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://example.com/p/comic";
        let dir = store.dir_for(Tier::Temp, book_url);
        std::fs::create_dir_all(&dir).unwrap();
        let mut meta = comic::ComicMeta::new(book_url, "漫画");
        meta.chapters.push(comic::ComicChapter {
            id: "ch-1".into(),
            title: "第 1 话".into(),
            url: Some(format!("{book_url}/1")),
            is_volume: false,
            pages: vec![comic::ComicPage {
                href: "0001-001.jpg".into(),
                mime: "image/jpeg".into(),
                width: Some(800),
                height: Some(1200),
                bytes: Some(4),
                source_url: None,
            }],
        });
        comic::write_meta(&dir, &meta).await.unwrap();
        comic::write_comic_info(&dir, &meta).await.unwrap();
        std::fs::write(dir.join("0001-001.jpg"), b"jpeg").unwrap();

        let out = root.join("book.cbz");
        pack_book(&store, book_url, PackKind::Cbz, &out)
            .await
            .unwrap()
            .expect("完整漫画目录应直接打包");
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"PK\x03\x04"));
        assert_eq!(&bytes[30..43], b"ComicInfo.xml");
        assert!(
            !bytes
                .windows(comic::META_FILE.len())
                .any(|w| w == comic::META_FILE.as_bytes()),
            "内部边车不应进入导出的 CBZ"
        );
    }

    #[test]
    fn incomplete_dirs_are_rejected() {
        let root = temp_root("incomplete");
        assert!(!is_complete(&root, PackKind::Epub));
        assert!(!is_complete(&root, PackKind::Cbz));
    }
}
