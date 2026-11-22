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

pub mod comic;
pub mod edit;
pub mod index;
pub mod migrate;
pub mod opf;
pub mod pack;
pub mod read;
pub mod usage;
pub mod write;

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
/// 入馆 rename 失败时留在临时目录里的标记文件，内容是 `book_url`。
pub const PROMOTE_MARKER: &str = ".oread-promote";

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
    ///
    /// 有内容的目录优先：入馆 rename 失败时会出现「空馆藏骨架 + 完整临时目录」，
    /// 读路径不能被空骨架挡住。两边都没内容时仍返回先找到的那个。
    pub fn find(&self, book_url: &str) -> Option<(Tier, PathBuf)> {
        self.find_by_id(&Self::book_id(book_url))
    }

    /// 按目录名（sha256 十六进制）查找。本地图片 URL 只带目录名，不带书源 URL。
    pub fn find_by_id(&self, id: &str) -> Option<(Tier, PathBuf)> {
        let id = id.trim();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let mut fallback = None;
        for tier in [Tier::Collection, Tier::Temp] {
            let dir = self.root.join(tier.as_str()).join(id);
            if !dir.is_dir() {
                continue;
            }
            if has_content(&dir) {
                return Some((tier, dir));
            }
            fallback.get_or_insert((tier, dir));
        }
        fallback
    }

    pub fn create_layout(&self) -> io::Result<()> {
        for sub in [COLLECTION_DIR, TEMP_DIR, TRASH_DIR] {
            std::fs::create_dir_all(self.root.join(sub))?;
        }
        Ok(())
    }

    /// 只建目录骨架（`mimetype` / `META-INF` / `OEBPS` / 样式），不写 OPF。
    ///
    /// 写路径自己控制 OPF 的落盘时机：先写章节文件，再 `nav.xhtml`，最后 `content.opf`，
    /// 这样任何时刻目录里都有一份可打开的旧 OPF，而不是空 spine。
    pub async fn ensure_dirs(&self, tier: Tier, book_url: &str) -> io::Result<PathBuf> {
        let dir = self.dir_for(tier, book_url);
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
        Ok(dir)
    }

    /// 建一本最小但合法的 EPUB 目录。已存在时只补齐缺失文件，不覆盖正文。
    pub async fn ensure_skeleton(&self, tier: Tier, meta: &OpfMeta) -> io::Result<PathBuf> {
        let dir = self.ensure_dirs(tier, &meta.book_url).await?;
        let pending = dir.join("OEBPS").join(PENDING_HREF);
        if !pending.exists() && meta.chapters.is_empty() {
            atomic_write(
                &pending,
                pending_chapter_xhtml(&meta.language, "待缓存", &["本书内容尚未缓存".into()])
                    .as_bytes(),
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
        self.write_chapter_marked(dir, language, item, paragraphs, false)
            .await
    }

    /// 写「尚未抓取」的占位章：正文带 [`PENDING_CLASS`] 标记，读路径据此回源，
    /// 不会被当成真实内容返回。
    pub async fn write_pending_chapter(
        &self,
        dir: &Path,
        language: &str,
        item: &ChapterItem,
        paragraphs: &[String],
    ) -> io::Result<()> {
        self.write_chapter_marked(dir, language, item, paragraphs, true)
            .await
    }

    async fn write_chapter_marked(
        &self,
        dir: &Path,
        language: &str,
        item: &ChapterItem,
        paragraphs: &[String],
        pending: bool,
    ) -> io::Result<()> {
        let path = dir.join("OEBPS").join(&item.href);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let html = if pending {
            pending_chapter_xhtml(language, &item.title, paragraphs)
        } else {
            chapter_xhtml(language, &item.title, paragraphs)
        };
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
    ///
    /// 不看 [`Self::find`] 的层：入馆失败后可能已经建了空馆藏骨架，
    /// 这里必须以「临时目录是否存在」为准，否则会把真实内容留在临时区。
    pub async fn promote(&self, book_url: &str) -> io::Result<Option<(Tier, PathBuf)>> {
        let temp = self.dir_for(Tier::Temp, book_url);
        let to = self.dir_for(Tier::Collection, book_url);
        if !temp.is_dir() {
            if to.is_dir() {
                index::sync_book(self, book_url).await;
                return Ok(Some((Tier::Collection, to)));
            }
            return Ok(None);
        }
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 目标已存在（空骨架，或上次 rename 成功但 DB 没来得及更新）时先清走，
        // 否则 Windows 上的目录 rename 会失败。搬进 trash 而不是删除，数据可救。
        if to.exists() {
            let aside = self.trash_target(book_url, "promote-dup");
            tokio::fs::create_dir_all(self.root.join(TRASH_DIR)).await?;
            rename_retry(&to, &aside).await?;
        }
        rename_retry(&temp, &to).await?;
        // 入馆成功：顺手清掉「待重试」标记（标记随目录一起搬过来）。
        let _ = tokio::fs::remove_file(to.join(PROMOTE_MARKER)).await;
        index::sync_book(self, book_url).await;
        Ok(Some((Tier::Collection, to)))
    }

    /// 移出馆藏：整目录从 `collection/` 搬回 `temp/`。目录不存在时返回 `None`。
    pub async fn demote(&self, book_url: &str) -> io::Result<Option<(Tier, PathBuf)>> {
        let temp = self.dir_for(Tier::Temp, book_url);
        let from = self.dir_for(Tier::Collection, book_url);
        let c_ok = from.is_dir();
        let t_ok = temp.is_dir();
        let out = match (c_ok, t_ok) {
            (false, false) => None,
            (false, true) => Some((Tier::Temp, temp)),
            // 馆藏目录只是入馆失败时留下的空骨架：保留临时目录里的正文。
            (true, true) if !has_content(&from) && has_content(&temp) => {
                let aside = self.trash_target(book_url, "demote-empty");
                tokio::fs::create_dir_all(self.root.join(TRASH_DIR)).await?;
                rename_retry(&from, &aside).await?;
                Some((Tier::Temp, temp))
            }
            (true, true) => {
                let aside = self.trash_target(book_url, "demote-dup");
                tokio::fs::create_dir_all(self.root.join(TRASH_DIR)).await?;
                rename_retry(&temp, &aside).await?;
                rename_retry(&from, &temp).await?;
                Some((Tier::Temp, temp))
            }
            (true, false) => {
                if let Some(parent) = temp.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                rename_retry(&from, &temp).await?;
                Some((Tier::Temp, temp))
            }
        };
        if out.is_some() {
            index::sync_book(self, book_url).await;
        }
        Ok(out)
    }

    /// 删除一本：把 `collection/` 与 `temp/` 里同 id 的目录都移到
    /// `trash/<hash>-<ts>-<tier>/`，保留一段时间再清理。
    ///
    /// 不按 [`Self::find`] 只搬一个：入馆失败时可能同时存在空骨架与真实目录。
    pub async fn move_to_trash(&self, book_url: &str) -> io::Result<bool> {
        let mut moved = false;
        for tier in [Tier::Collection, Tier::Temp] {
            let from = self.dir_for(tier, book_url);
            if !from.is_dir() {
                continue;
            }
            let to = self.trash_target(book_url, tier.as_str());
            tokio::fs::create_dir_all(self.root.join(TRASH_DIR)).await?;
            rename_retry(&from, &to).await?;
            moved = true;
        }
        if moved {
            index::forget_book(self, book_url).await;
        }
        Ok(moved)
    }

    fn trash_target(&self, book_url: &str, tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.root
            .join(TRASH_DIR)
            .join(format!("{}-{stamp}-{tag}", Self::book_id(book_url)))
    }

    /// 记下「这本书要入馆但目录还没搬过去」。标记文件放在临时目录里，
    /// 后台维护循环看到就会重试；目录本身仍是唯一事实，不需要 DB 列。
    pub async fn mark_promote_pending(&self, book_url: &str) -> io::Result<()> {
        let dir = self.dir_for(Tier::Temp, book_url);
        tokio::fs::create_dir_all(&dir).await?;
        atomic_write(&dir.join(PROMOTE_MARKER), book_url.trim().as_bytes()).await
    }

    /// 重试所有待入馆目录，返回成功搬走的数量。
    ///
    /// 只在目录里做：标记文件携带 `book_url`（目录名是 sha256，反推不回来）。
    pub async fn retry_pending_promotions(&self) -> io::Result<usize> {
        let temp = self.root.join(TEMP_DIR);
        let mut rd = match tokio::fs::read_dir(&temp).await {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut moved = 0;
        while let Some(entry) = rd.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let book_url = match tokio::fs::read_to_string(dir.join(PROMOTE_MARKER)).await {
                Ok(v) => v.trim().to_string(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "read promote marker failed");
                    continue;
                }
            };
            if book_url.is_empty() {
                continue;
            }
            // 标记内容和目录名必须对得上；对不上就跳过，避免误搬别的书。
            let id = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if Store::book_id(&book_url) != id {
                tracing::warn!(
                    dir = %dir.display(),
                    book = %book_url,
                    "promote marker does not match dir; skipped"
                );
                continue;
            }
            let _guard = lock_book(&book_url).await;
            match self.promote(&book_url).await {
                Ok(Some((_, to))) => {
                    let _ = tokio::fs::remove_file(dir.join(PROMOTE_MARKER)).await;
                    let _ = tokio::fs::remove_file(to.join(PROMOTE_MARKER)).await;
                    moved += 1;
                }
                Ok(None) => {
                    let _ = tokio::fs::remove_file(dir.join(PROMOTE_MARKER)).await;
                }
                Err(e) => {
                    tracing::warn!(book = %book_url, error = %e, "promote retry failed");
                }
            }
        }
        Ok(moved)
    }
}

/// 目录里是否已经有一本可打开的书：文字书看 OPF，漫画看标准/自有元数据。
fn has_content(dir: &Path) -> bool {
    dir.join("OEBPS").join("content.opf").is_file() || comic::is_comic_dir(dir)
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

/// 占位章段落上的 class：读路径见到它就知道「这一章还没抓」。
pub const PENDING_CLASS: &str = "oread-pending";

/// 判断一份章节 XHTML 是否是占位章。
pub fn is_pending_html(html: &str) -> bool {
    html.contains(PENDING_CLASS)
}

/// 生成一章规范 XHTML。段落一律是纯文本，序列化时统一转义，
/// 因此段落编辑「想写出非法文件」在结构上就不可能。
pub fn chapter_xhtml(language: &str, title: &str, paragraphs: &[String]) -> String {
    chapter_xhtml_marked(language, title, paragraphs, false)
}

/// 占位章：结构与正文一致，只是段落带 [`PENDING_CLASS`]。
pub fn pending_chapter_xhtml(language: &str, title: &str, paragraphs: &[String]) -> String {
    chapter_xhtml_marked(language, title, paragraphs, true)
}

fn chapter_xhtml_marked(
    language: &str,
    title: &str,
    paragraphs: &[String],
    pending: bool,
) -> String {
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
        if pending {
            let _ = writeln!(
                s,
                "<p class=\"{PENDING_CLASS}\">{}</p>",
                boranlib_ebook::xml::xml_escape(text)
            );
        } else {
            let _ = writeln!(s, "<p>{}</p>", boranlib_ebook::xml::xml_escape(text));
        }
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

/// 下一个章节序号。`ch-0001` / `vol-0003` 共用同一序列，只增不减、永不复用；
/// 插入新章不改任何旧文件名。
pub fn next_chapter_seq<'a>(ids: impl Iterator<Item = &'a str>) -> u32 {
    ids.filter_map(|id| id.rsplit_once('-').map(|(_, n)| n))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

/// 下一个章节 ID。序号只增不减、永不复用；插入新章不改任何旧文件名。
pub fn next_chapter_number(chapters: &[ChapterItem]) -> u32 {
    next_chapter_seq(chapters.iter().map(|c| c.id.as_str()))
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
