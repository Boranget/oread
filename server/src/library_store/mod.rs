//! 馆藏落盘层：**一本书记录 = 一个解压后的书目录**。
//!
//! 布局（`data_dir/library/`）：
//!
//! ```text
//! index.db                       派生索引，可删可重建
//! collection/<sha256(book_url)>/ 馆藏，永久保存
//! temp/<sha256(book_url)>/       临时缓存，按 TTL 回收
//! trash/<hash>-<ts>/             删除/回收站
//! ```
//!
//! 文字书目录就是一本合法的 EPUB 3（`mimetype` + `META-INF/container.xml` +
//! `OEBPS/`）；漫画目录就是解压后的 CBZ（图片 + `ComicInfo.xml`）。
//! 因此「导出 = 原样打包」，不需要任何再生成步骤。
//!
//! 写入纪律：
//! - 单文件同目录临时文件 → `fsync` → 原子 `rename`（Windows 上退避重试）；
//! - 同一本书的保存串行化（[`lock_book`]）；
//! - 先写正文/图片，最后写 `content.opf`，任何时刻目录都是一本合法的书。

pub mod opf;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub use opf::{ChapterItem, CoverItem, OpfMeta};

pub const COLLECTION_DIR: &str = "collection";
pub const TEMP_DIR: &str = "temp";
pub const TRASH_DIR: &str = "trash";
/// 占位章：书刚建目录、还没有任何正文时放进 spine，保证它从第一秒起就是合法 EPUB。
pub const PENDING_ID: &str = "ch-0000";
pub const PENDING_HREF: &str = "chapters/ch-0000-pending.xhtml";

/// 书所在的层。层由**目录位置**表达，不写进数据库字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Collection,
    Temp,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Collection => COLLECTION_DIR,
            Tier::Temp => TEMP_DIR,
        }
    }
}

/// `data_dir/library` 的句柄。克隆便宜（只含路径），可放进 `AppState`。
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            root: data_dir.as_ref().join("library"),
        }
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 目录名：完整 sha256 十六进制，避免不同书源的同名书互相覆盖。
    pub fn book_id(book_url: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(book_url.trim().as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn dir_for(&self, tier: Tier, book_url: &str) -> PathBuf {
        self.root.join(tier.as_str()).join(Self::book_id(book_url))
    }

    /// 先找馆藏、再找临时区。返回实际层（层是路径事实，不是字段）。
    pub fn find(&self, book_url: &str) -> Option<(Tier, PathBuf)> {
        for tier in [Tier::Collection, Tier::Temp] {
            let dir = self.dir_for(tier, book_url);
            if dir.is_dir() {
                return Some((tier, dir));
            }
        }
        None
    }

    pub fn create_layout(&self) -> io::Result<()> {
        for sub in [COLLECTION_DIR, TEMP_DIR, TRASH_DIR] {
            std::fs::create_dir_all(self.root.join(sub))?;
        }
        Ok(())
    }

    /// 建一本最小但合法的 EPUB 目录。已存在时只补齐缺失文件，不覆盖正文。
    pub async fn ensure_skeleton(&self, tier: Tier, meta: &OpfMeta) -> io::Result<PathBuf> {
        let dir = self.dir_for(tier, &meta.book_url);
        let oebps = dir.join("OEBPS");
        let chapters = oebps.join("chapters");
        let images = oebps.join("images");
        let meta_inf = dir.join("META-INF");
        for d in [&oebps, &chapters, &images, &meta_inf] {
            tokio::fs::create_dir_all(d).await?;
        }
        let mimetype = dir.join("mimetype");
        if !mimetype.exists() {
            atomic_write(&mimetype, b"application/epub+zip").await?;
        }
        let container = meta_inf.join("container.xml");
        if !container.exists() {
            atomic_write(&container, CONTAINER_XML.as_bytes()).await?;
        }
        let style = oebps.join("style.css");
        if !style.exists() {
            atomic_write(&style, STYLE_CSS.as_bytes()).await?;
        }
        let pending = oebps.join(PENDING_HREF);
        if !pending.exists() && meta.chapters.is_empty() {
            atomic_write(
                &pending,
                chapter_xhtml(&meta.language, "待缓存", &["本书内容尚未缓存".into()]).as_bytes(),
            )
            .await?;
        }
        self.write_opf(&dir, meta).await?;
        self.write_nav(&dir, meta).await?;
        Ok(dir)
    }

    pub async fn read_opf(&self, dir: &Path) -> io::Result<OpfMeta> {
        let path = dir.join("OEBPS").join("content.opf");
        let xml = tokio::fs::read_to_string(&path).await?;
        opf::parse_opf(&xml).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// 写 `content.opf`。调用方负责先 [`OpfMeta::bump`]。
    pub async fn write_opf(&self, dir: &Path, meta: &OpfMeta) -> io::Result<()> {
        let path = dir.join("OEBPS").join("content.opf");
        atomic_write(&path, opf::to_opf_xml(meta).as_bytes()).await
    }

    pub async fn write_nav(&self, dir: &Path, meta: &OpfMeta) -> io::Result<()> {
        let path = dir.join("OEBPS").join("nav.xhtml");
        atomic_write(&path, opf::to_nav_xhtml(meta).as_bytes()).await
    }

    /// 写一章正文（归一化后的段落）。`id`/`href` 来自 [`ChapterItem`]。
    pub async fn write_chapter(
        &self,
        dir: &Path,
        language: &str,
        item: &ChapterItem,
        paragraphs: &[String],
    ) -> io::Result<()> {
        let path = dir.join("OEBPS").join(&item.href);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let html = chapter_xhtml(language, &item.title, paragraphs);
        atomic_write(&path, html.as_bytes()).await
    }

    /// 读一章正文；文件不存在返回 `Ok(None)`（未缓存）。
    pub async fn read_chapter(&self, dir: &Path, href: &str) -> io::Result<Option<String>> {
        let path = safe_join(&dir.join("OEBPS"), href)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 读取并删除 orphan 文件不在本期范围；这里只负责把章节文件删掉。
    pub async fn remove_file(&self, dir: &Path, href: &str) -> io::Result<()> {
        let path = safe_join(&dir.join("OEBPS"), href)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 临时书入馆：整目录搬到 `collection/`。
    ///
    /// Windows 上可能因为阅读器还开着里面的图片而失败；调用方应记录
    /// `promote_pending` 并后台重试，期间书仍算临时书（层由路径决定）。
    pub async fn promote(&self, book_url: &str) -> io::Result<Option<(Tier, PathBuf)>> {
        let Some((tier, from)) = self.find(book_url) else {
            return Ok(None);
        };
        if tier == Tier::Collection {
            return Ok(Some((tier, from)));
        }
        let to = self.dir_for(Tier::Collection, book_url);
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        rename_retry(&from, &to).await?;
        Ok(Some((Tier::Collection, to)))
    }

    /// 删除一本：移到 `trash/<hash>-<ts>/`，保留一段时间再清理。
    pub async fn move_to_trash(&self, book_url: &str) -> io::Result<bool> {
        let Some((_tier, from)) = self.find(book_url) else {
            return Ok(false);
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let to = self
            .root
            .join(TRASH_DIR)
            .join(format!("{}-{stamp}", Self::book_id(book_url)));
        tokio::fs::create_dir_all(self.root.join(TRASH_DIR)).await?;
        rename_retry(&from, &to).await?;
        Ok(true)
    }
}

/// 每本书一把写锁：正文落盘、元数据保存、入馆 rename 都走它。
pub async fn lock_book(book_url: &str) -> OwnedMutexGuard<()> {
    static LOCKS: Lazy<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    let key = Store::book_id(book_url);
    let lock = {
        let mut map = LOCKS.lock().expect("book lock map poisoned");
        map.entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };
    lock.lock_owned().await
}

// ---------------------------------------------------------------------------
// 原子写
// ---------------------------------------------------------------------------

/// 同目录临时文件 → 写入 → `fsync` → 原子替换。
///
/// 临时文件必须和目标同目录：跨卷 `rename` 不是原子操作。Windows 上杀软/索引
/// 服务会短暂占用目标文件，`rename` 会报 `ACCESS_DENIED`/`SHARING_VIOLATION`，
/// 所以退避重试几次（calibre 的 `atomic_rename` 也这么做）。
pub async fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || write_atomic_sync(&path, &bytes))
        .await
        .map_err(|e| io::Error::other(format!("atomic write task failed: {e}")))?
}

fn write_atomic_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标文件没有父目录"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "oread".into()),
        std::process::id()
    ));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    let mut last = None;
    for attempt in 0..5u32 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(20 * (attempt as u64 + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

async fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let from = from.to_path_buf();
    let to = to.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut last = None;
        for attempt in 0..5u32 {
            match std::fs::rename(&from, &to) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(30 * (attempt as u64 + 1)));
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
    })
    .await
    .map_err(|e| io::Error::other(format!("rename task failed: {e}")))?
}

// ---------------------------------------------------------------------------
// 正文 XHTML
// ---------------------------------------------------------------------------

/// 生成一章规范 XHTML。段落一律是纯文本，序列化时统一转义，
/// 因此段落编辑「想写出非法文件」在结构上就不可能。
pub fn chapter_xhtml(language: &str, title: &str, paragraphs: &[String]) -> String {
    use std::fmt::Write as _;
    let lang = if language.trim().is_empty() {
        opf::DEFAULT_LANGUAGE
    } else {
        language.trim()
    };
    let mut s = String::with_capacity(256 + paragraphs.len() * 128);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        s,
        "<html xmlns=\"http://www.w3.org/1999/xhtml\" lang=\"{}\" xml:lang=\"{}\">",
        boranlib_ebook::xml::xml_escape(lang),
        boranlib_ebook::xml::xml_escape(lang)
    );
    let _ = writeln!(
        s,
        "<head><title>{}</title>\
         <link rel=\"stylesheet\" type=\"text/css\" href=\"../style.css\"/></head>",
        boranlib_ebook::xml::xml_escape(title)
    );
    s.push_str("<body>\n");
    if !title.trim().is_empty() {
        let _ = writeln!(
            s,
            "<h2>{}</h2>",
            boranlib_ebook::xml::xml_escape(title.trim())
        );
    }
    for p in paragraphs {
        let text = p.trim();
        if text.is_empty() {
            continue;
        }
        let _ = writeln!(s, "<p>{}</p>", boranlib_ebook::xml::xml_escape(text));
    }
    s.push_str("</body>\n</html>\n");
    s
}

/// 文件名 slug：只保留 ASCII 字母数字与 `-`，其余丢弃；中文标题自然退化为空。
pub fn slug(title: &str, max_len: usize) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == '-' || ch == ' ') && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= max_len {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// 下一个章节 ID。序号只增不减、永不复用；插入新章不改任何旧文件名。
pub fn next_chapter_number(chapters: &[ChapterItem]) -> u32 {
    chapters
        .iter()
        .filter_map(|c| c.id.strip_prefix("ch-"))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

/// 拼接并校验相对 href，拒绝绝对路径与 `..`。
pub fn safe_join(base: &Path, href: &str) -> io::Result<PathBuf> {
    let rel = Path::new(href);
    let bad = rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
    if bad {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("非法相对路径: {href}"),
        ));
    }
    Ok(base.join(rel))
}

pub const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#;

pub const STYLE_CSS: &str = r#"html { line-height: 1.6; }
body { margin: 0 1em; }
h2 { font-size: 1.2em; margin: 1.2em 0 0.8em; }
p { margin: 0 0 0.8em; text-indent: 2em; }
"#;

#[cfg(test)]
mod tests;
