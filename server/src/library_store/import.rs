//! DRM-free EPUB 导入：原结构保留，只做必要的安全清理与 OREAD 元数据注入。
//!
//! 设计要点（见 `docs/TODO.md`）：
//!
//! - **不重排章节、不改文件名**：解压后以压缩包内所有内容文件的公共父目录为
//!   基准，整棵树平移到 `OEBPS/`，rootfile 改名为 `content.opf`。这样 OPF 里的
//!   相对 href、章节里的 `../Images/x.jpg`、CSS 的字体路径全部保持原样。
//! - **OPF 仍是权威**：只补 `book-url` / `oread:revision` / `dcterms:modified` /
//!   章节标题，并原样登记 CSS、字体、NCX 等 manifest 项。
//! - **安全优先**：拒绝路径穿越、加密、压缩炸弹、外网资源、SVG、脚本与内联事件。
//! - **两步导入**：`stage` 解压并校验到 `.import/<token>/book`，`commit` 才搬进
//!   馆藏；查重弹窗发生在两步之间，避免用户确认时重复上传大文件。
//!
//! 读取端仍然把章节 XHTML 解析成语义块；原文保留是为了以后支持表格、列表等
//! 结构时，已导入的书不用重新导入。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{comic, files, index, opf, usage, Store, CONTAINER_XML, STYLE_CSS};
use crate::library;

/// 上传的 EPUB 上限（压缩包本身）。
pub const MAX_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 解压比可疑阈值：大文件才检查，避免误伤高度可压缩的纯文本。
const MAX_RATIO: u64 = 200;
const MAX_RATIO_MIN_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum ImportError {
    BadRequest(String),
    TooLarge(String),
    Io(io::Error),
    Internal(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::BadRequest(m) | ImportError::TooLarge(m) | ImportError::Internal(m) => {
                write!(f, "{m}")
            }
            ImportError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        ImportError::Io(e)
    }
}

impl From<sqlx::Error> for ImportError {
    fn from(e: sqlx::Error) -> Self {
        ImportError::Internal(e.to_string())
    }
}

impl From<anyhow::Error> for ImportError {
    fn from(e: anyhow::Error) -> Self {
        ImportError::Internal(e.to_string())
    }
}

fn bad(msg: impl Into<String>) -> ImportError {
    ImportError::BadRequest(msg.into())
}

fn zip_err(e: zip::result::ZipError) -> ImportError {
    bad(format!("EPUB 不是有效的 ZIP：{e}"))
}

// ---------------------------------------------------------------------------
// 暂存信息
// ---------------------------------------------------------------------------

/// 暂存区里的 sidecar：commit 时不需要重新解析整本书。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageInfo {
    pub book_url: String,
    pub sha256: String,
    pub content_fingerprint: Option<String>,
    pub file_name: String,
    pub title: String,
    pub author: Option<String>,
    pub language: String,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub chapter_count: usize,
    pub image_count: usize,
    pub has_cover: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct ExtractOutcome {
    pub info: StageInfo,
}

struct ExtractedBook {
    meta: opf::OpfMeta,
    warnings: Vec<String>,
    fingerprint: Option<String>,
}

// ---------------------------------------------------------------------------
// stage
// ---------------------------------------------------------------------------

/// 解压并校验上传的 EPUB，返回可供前端展示与 commit 的摘要。
pub async fn stage(
    store: &Store,
    token: &str,
    source: PathBuf,
    file_name: &str,
    sha256: String,
) -> Result<StageInfo, ImportError> {
    validate_token(token)?;
    let stage_dir = store.import_stage_dir(token);
    let book_dir = stage_dir.join("book");
    let book_url = import_book_url(&sha256);
    let name = sanitize_file_name(file_name);

    let source_for_task = source.clone();
    let book_url_for_task = book_url.clone();
    let name_for_task = name.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        extract_epub(
            &source_for_task,
            &book_dir,
            &book_url_for_task,
            &name_for_task,
        )
    })
    .await
    .map_err(|e| ImportError::Internal(format!("解压任务失败：{e}")))??;

    let info = StageInfo {
        book_url,
        sha256,
        content_fingerprint: outcome.fingerprint,
        file_name: name,
        title: outcome.meta.title.clone(),
        author: outcome.meta.author.clone(),
        language: outcome.meta.language.clone(),
        publisher: outcome.meta.publisher.clone(),
        pubdate: outcome.meta.pubdate.clone(),
        chapter_count: outcome
            .meta
            .chapters
            .iter()
            .filter(|c| !c.is_volume)
            .count(),
        image_count: outcome.meta.images.len(),
        has_cover: outcome.meta.cover.is_some(),
        warnings: outcome.warnings,
    };
    let raw = serde_json::to_vec_pretty(&info)
        .map_err(|e| ImportError::Internal(format!("序列化暂存信息失败：{e}")))?;
    tokio::fs::write(stage_dir.join("info.json"), raw).await?;
    // 原始上传包已解压，留着只会占空间。
    let _ = tokio::fs::remove_file(&source).await;
    Ok(info)
}

pub async fn read_stage_info(store: &Store, token: &str) -> Result<StageInfo, ImportError> {
    validate_token(token)?;
    let path = store.import_stage_dir(token).join("info.json");
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|_| bad("导入会话已过期，请重新上传 EPUB"))?;
    serde_json::from_slice(&raw)
        .map_err(|e| ImportError::Internal(format!("读取暂存信息失败：{e}")))
}

pub async fn discard_stage(store: &Store, token: &str) {
    if validate_token(token).is_ok() {
        let _ = tokio::fs::remove_dir_all(store.import_stage_dir(token)).await;
    }
}

/// 清理超过 `max_age` 的残留暂存目录（进程被中断时留下的）。
pub async fn prune_stages(store: &Store, max_age: std::time::Duration) {
    let root = store.root().join(super::IMPORT_DIR);
    let mut rd = match tokio::fs::read_dir(&root).await {
        Ok(v) => v,
        Err(_) => return,
    };
    let now = std::time::SystemTime::now();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age > max_age)
            .unwrap_or(false);
        if stale {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

pub fn import_book_url(sha256: &str) -> String {
    format!("import:{}", sha256.trim().to_ascii_lowercase())
}

fn validate_token(token: &str) -> Result<(), ImportError> {
    if token.len() == 32 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(bad("导入令牌无效"))
    }
}

pub fn new_token() -> Result<String, ImportError> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf)
        .map_err(|e| ImportError::Internal(format!("生成导入令牌失败：{e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn sanitize_file_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_matches('.');
    let mut out: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    if out.is_empty() {
        out = "import.epub".into();
    }
    out
}

// ---------------------------------------------------------------------------
// 解压与校验（同步，跑在 spawn_blocking 里）
// ---------------------------------------------------------------------------

struct ZipEntryMeta {
    name: String,
    index: usize,
}

fn extract_epub(
    source: &Path,
    book_dir: &Path,
    book_url: &str,
    file_name: &str,
) -> Result<ExtractedBook, ImportError> {
    let file = fs::File::open(source)?;
    let mut zip = zip::ZipArchive::new(file).map_err(zip_err)?;
    if zip.len() > MAX_ENTRIES {
        return Err(bad(format!(
            "EPUB 条目过多（{}，上限 {MAX_ENTRIES}）",
            zip.len()
        )));
    }

    let mut entries: Vec<ZipEntryMeta> = Vec::with_capacity(zip.len());
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let e = zip.by_index(i).map_err(zip_err)?;
        let raw_name = e
            .name()
            .map_err(|err| bad(format!("ZIP 条目名无法解码：{err}")))?
            .to_string();
        if e.is_dir() {
            continue;
        }
        if e.encrypted() {
            return Err(bad(format!("EPUB 含加密条目，暂不支持：{raw_name}")));
        }
        validate_entry_name(&raw_name)?;
        // `./`、重复斜杠等写法归一化后再落盘，避免同一文件被写成两份。
        let name = normalize_zip_path(&raw_name);
        if name.is_empty() {
            continue;
        }
        let key = name.to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(bad(format!("EPUB 含重复条目（大小写不敏感）：{name}")));
        }
        let size = e.size();
        let compressed = e.compressed_size();
        if size > MAX_ENTRY_BYTES {
            return Err(bad(format!("单文件过大：{name}（{size} 字节）")));
        }
        if size > MAX_RATIO_MIN_BYTES && (compressed == 0 || size / compressed > MAX_RATIO) {
            return Err(bad(format!(
                "压缩比异常，疑似压缩炸弹：{name}（{compressed} → {size} 字节）"
            )));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err(bad("EPUB 解压后总大小超过 2 GiB 上限"));
        }
        entries.push(ZipEntryMeta { name, index: i });
    }

    if entries
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case("META-INF/encryption.xml"))
    {
        return Err(bad(
            "EPUB 含 META-INF/encryption.xml（DRM 或字体混淆），暂不支持",
        ));
    }
    if entries
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case("META-INF/rights.xml"))
    {
        return Err(bad("EPUB 含 META-INF/rights.xml（DRM），暂不支持"));
    }

    let mut warnings = Vec::new();
    match find_entry(&entries, "mimetype") {
        Some(idx) => {
            let data = read_entry(&mut zip, idx)?;
            let text = String::from_utf8_lossy(&data);
            if text.trim() != "application/epub+zip" {
                warnings.push(format!(
                    "mimetype 内容不是 application/epub+zip（已按 EPUB 处理）：{}",
                    text.trim()
                ));
            }
        }
        None => warnings.push("缺少 mimetype，已补写".into()),
    }

    let container_idx = find_entry(&entries, "META-INF/container.xml")
        .ok_or_else(|| bad("不是 EPUB：缺少 META-INF/container.xml"))?;
    let container = read_entry(&mut zip, container_idx)?;
    let container =
        String::from_utf8(container).map_err(|_| bad("META-INF/container.xml 不是 UTF-8"))?;
    let rootfile_raw = files::parse_container_rootfile(&container)
        .map_err(|e| bad(format!("container.xml 无效：{e}")))?;
    let opf_path = normalize_zip_path(&super::href_to_path(rootfile_raw.trim()));
    if opf_path.is_empty() {
        return Err(bad("container.xml 的 rootfile 路径为空"));
    }
    let opf_idx = find_entry(&entries, &opf_path)
        .ok_or_else(|| bad(format!("container.xml 指向的 OPF 不存在：{opf_path}")))?;
    let opf_bytes = read_entry(&mut zip, opf_idx)?;
    let opf_xml = decode_text(&opf_bytes).ok_or_else(|| bad("content.opf 不是 UTF-8 文本"))?;

    let mut meta =
        opf::parse_opf_import(&opf_xml, book_url).map_err(|e| bad(format!("OPF 解析失败：{e}")))?;

    // 整棵树平移到 OEBPS/：以压缩包里实际文件的公共父目录为基准，
    // 这样章节、图片、CSS 之间的相对链接保持原样，只需重写 OPF 的 href。
    let content_root = content_root(&entries);
    let plan = rewrite_manifest_for_layout(&mut meta, &opf_xml, &opf_path, &content_root)?;

    let opf_dest = "OEBPS/content.opf".to_string();
    let mut written: HashSet<String> = HashSet::new();
    fs::create_dir_all(book_dir)?;

    for entry in &entries {
        if entry.name.eq_ignore_ascii_case("mimetype")
            || entry.name.to_ascii_lowercase().starts_with("meta-inf/")
            || is_junk_entry(&entry.name)
        {
            continue;
        }
        if entry.index == opf_idx {
            continue;
        }
        let Some(rel) = rel_to_root(&entry.name, &content_root) else {
            warnings.push(format!("忽略书根之外的文件：{}", entry.name));
            continue;
        };
        let dest = normalize_zip_path(&format!("OEBPS/{rel}"));
        if dest == opf_dest {
            return Err(bad("EPUB 含与 content.opf 同名的文件，无法安全解压"));
        }
        validate_dest_path(&dest)?;
        if !written.insert(dest.to_ascii_lowercase()) {
            return Err(bad(format!("文件重名，无法安全解压：{dest}")));
        }
        let data = read_entry(&mut zip, entry.index)?;
        let out = book_dir.join(Path::new(&dest));
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, data)?;
    }

    // 先清理活动内容，再提取标题与正文指纹；这样指纹看到的是最终会留在馆藏里的文本。
    sanitize_tree(book_dir)?;

    let nav_titles = plan
        .nav_href
        .as_deref()
        .and_then(|nav| nav_titles(book_dir, nav))
        .unwrap_or_default();
    let ncx_titles = ncx_titles(book_dir, &meta).unwrap_or_default();
    let fingerprint = fill_titles_and_fingerprint(book_dir, &mut meta, &nav_titles, &ncx_titles)?;

    if meta.language.trim().is_empty() {
        meta.language = opf::DEFAULT_LANGUAGE.to_string();
    }
    meta.book_url = book_url.to_string();
    meta.revision = 0;
    meta.modified = opf::now_modified();
    if meta.title.trim().is_empty() {
        meta.title = file_name.trim_end_matches(".epub").to_string();
    }
    dedupe_manifest_ids(&mut meta);

    write_final_layout(book_dir, &meta)?;
    Ok(ExtractedBook {
        meta,
        warnings,
        fingerprint,
    })
}

fn write_final_layout(book_dir: &Path, meta: &opf::OpfMeta) -> Result<(), ImportError> {
    let oebps = book_dir.join("OEBPS");
    fs::create_dir_all(&oebps)?;
    fs::write(book_dir.join("mimetype"), b"application/epub+zip")?;
    fs::create_dir_all(book_dir.join("META-INF"))?;
    fs::write(book_dir.join("META-INF/container.xml"), CONTAINER_XML)?;
    if !oebps.join("style.css").exists() {
        fs::write(oebps.join("style.css"), STYLE_CSS)?;
    }
    fs::write(oebps.join("content.opf"), opf::to_opf_xml(meta))?;
    fs::write(oebps.join("nav.xhtml"), opf::to_nav_xhtml(meta))?;
    Ok(())
}

fn read_entry<R: Read + io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    index: usize,
) -> Result<Vec<u8>, ImportError> {
    let e = zip.by_index(index).map_err(zip_err)?;
    read_limited(e, MAX_ENTRY_BYTES)
}

fn read_limited<R: Read>(mut reader: R, max: u64) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if out.len() as u64 + n as u64 > max {
            return Err(ImportError::TooLarge("解压后的文件超过上限".into()));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

fn find_entry(entries: &[ZipEntryMeta], name: &str) -> Option<usize> {
    entries
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .map(|e| e.index)
}

fn validate_entry_name(name: &str) -> Result<(), ImportError> {
    if name.is_empty() {
        return Err(bad("ZIP 含空文件名"));
    }
    if name.contains('\\') || name.contains('\0') {
        return Err(bad(format!("ZIP 条目含非法字符：{name}")));
    }
    if name.starts_with('/') {
        return Err(bad(format!("ZIP 条目是绝对路径：{name}")));
    }
    for seg in name.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(bad(format!("ZIP 条目含路径穿越：{name}"))),
            s => {
                if s.contains(':') {
                    return Err(bad(format!("ZIP 条目含盘符或冒号：{name}")));
                }
                if is_reserved_windows_name(s) {
                    return Err(bad(format!("ZIP 条目使用 Windows 保留名：{name}")));
                }
                if s.ends_with('.') || s.ends_with(' ') {
                    return Err(bad(format!("ZIP 条目名以点或空格结尾：{name}")));
                }
            }
        }
    }
    Ok(())
}

fn validate_dest_path(dest: &str) -> Result<(), ImportError> {
    validate_entry_name(dest)
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn is_junk_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("__macosx/")
        || lower.ends_with("/.ds_store")
        || lower == ".ds_store"
        || lower.ends_with("/thumbs.db")
        || lower.ends_with("/desktop.ini")
}

fn normalize_zip_path(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

/// `path` 是否等于 `root` 或位于其下。
fn path_under(path: &str, root: &str) -> bool {
    root.is_empty()
        || path == root
        || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

/// 压缩包里所有内容文件的公共父目录。章节、图片、CSS 相对它平移，
/// 相互之间的相对链接因此保持原样。
fn content_root(entries: &[ZipEntryMeta]) -> String {
    let mut root: Option<String> = None;
    for e in entries {
        let name = e.name.as_str();
        if name.eq_ignore_ascii_case("mimetype")
            || name.to_ascii_lowercase().starts_with("meta-inf/")
            || is_junk_entry(name)
        {
            continue;
        }
        match root.as_mut() {
            None => root = Some(parent_dir(name)),
            Some(dir) => {
                while !path_under(name, dir) {
                    if dir.is_empty() {
                        return String::new();
                    }
                    *dir = parent_dir(dir);
                }
            }
        }
    }
    root.unwrap_or_default()
}

fn rel_to_root(path: &str, root: &str) -> Option<String> {
    if root.is_empty() {
        return Some(path.to_string());
    }
    path.strip_prefix(root)?
        .strip_prefix('/')
        .map(str::to_string)
}

/// 把 OPF 里的 href（相对 OPF 目录、URI 百分号编码）解析成压缩包内的真实路径。
fn resolve_zip_path(opf_dir: &str, href: &str) -> Result<String, ImportError> {
    let raw = href.split(['?', '#']).next().unwrap_or("").trim();
    let decoded = super::href_to_path(raw);
    if decoded.is_empty()
        || decoded.starts_with('/')
        || decoded.contains('\\')
        || decoded.contains(':')
    {
        return Err(bad(format!("manifest href 非法：{href}")));
    }
    let mut parts: Vec<&str> = opf_dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(bad(format!("manifest href 越出书根：{href}")));
                }
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return Err(bad(format!("manifest href 指向书根目录：{href}")));
    }
    Ok(parts.join("/"))
}

struct LayoutPlan {
    /// 原 nav 文件在新布局里的路径（相对 `OEBPS/`）。
    nav_href: Option<String>,
}

/// 重写 manifest href，使其相对新的 `OEBPS/content.opf`。
fn rewrite_manifest_for_layout(
    meta: &mut opf::OpfMeta,
    opf_xml: &str,
    opf_path: &str,
    root: &str,
) -> Result<LayoutPlan, ImportError> {
    let opf_dir = parent_dir(opf_path);

    for ch in &mut meta.chapters {
        ch.href = new_href_for_layout(&ch.id, &ch.href, "application/xhtml+xml", &opf_dir, root)?;
    }
    for img in &mut meta.images {
        let id = img.id.clone();
        let media = img.media_type.clone();
        img.href = new_href_for_layout(&id, &img.href, &media, &opf_dir, root)?;
    }
    if let Some(cover) = &mut meta.cover {
        cover.href = new_href_for_layout(
            "cover-image",
            &cover.href,
            &cover.media_type,
            &opf_dir,
            root,
        )?;
    }
    for res in &mut meta.resources {
        let id = res.id.clone();
        let media = res.media_type.clone();
        res.href = new_href_for_layout(&id, &res.href, &media, &opf_dir, root)?;
    }

    // nav 不在 `OpfMeta` 里（我们每次重新生成），但原来的 nav 仍是章节标题的来源。
    let nav_href = manifest_items(opf_xml)
        .into_iter()
        .find(|(_, _, _, props)| props.split_whitespace().any(|p| p == "nav"))
        .map(|(id, href, media, _)| new_href_for_layout(&id, &href, &media, &opf_dir, root))
        .transpose()?;

    Ok(LayoutPlan { nav_href })
}

fn new_href_for_layout(
    id: &str,
    href: &str,
    media_type: &str,
    opf_dir: &str,
    root: &str,
) -> Result<String, ImportError> {
    if is_svg_media(media_type) {
        return Err(bad(format!("EPUB 含 SVG 图片，暂不支持：{id}")));
    }
    let old = resolve_zip_path(opf_dir, href)?;
    rel_to_root(&old, root).ok_or_else(|| bad(format!("manifest 项 {id} 指向书根之外：{href}")))
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Some(s.trim_start_matches('\u{feff}').to_string());
    }
    // 有些 EPUB 2 是 UTF-8 带 BOM 或声明了错误编码；只接受 UTF-8，避免引入编码猜测。
    String::from_utf8(bytes.to_vec())
        .ok()
        .map(|s| s.trim_start_matches('\u{feff}').to_string())
}

fn is_svg_media(media: &str) -> bool {
    media
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("image/svg+xml")
}

// ---------------------------------------------------------------------------
// 安全清理
// ---------------------------------------------------------------------------

static SCRIPT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap());
static SCRIPT_SELF: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<script\b[^>]*/\s*>").unwrap());
static IFRAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<iframe\b[^>]*>.*?</iframe\s*>").unwrap());
static OBJECT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<object\b[^>]*>.*?</object\s*>").unwrap());
static APPLET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<applet\b[^>]*>.*?</applet\s*>").unwrap());
static FRAMESET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<frameset\b[^>]*>.*?</frameset\s*>").unwrap());
static VOID_ACTIVE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<(?:embed|frame|base)\b[^>]*>").unwrap());
static META_REFRESH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<meta\b[^>]*http-equiv\s*=\s*["']?refresh[^>]*>"#).unwrap());
static EVENT_ATTR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)\s+on[a-z0-9_-]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap());
static JS_URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?is)\b(href|src|xlink:href)\s*=\s*("[^"]*javascript:[^"]*"|'[^']*javascript:[^']*')"#,
    )
    .unwrap()
});
static REMOTE_CSS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)(?:@import\s+(?:url\()?\s*|url\(\s*)["']?\s*(?:https?:)?//"#).unwrap()
});
static RESOURCE_TAGS: Lazy<Selector> = Lazy::new(|| {
    Selector::parse("img, image, script, iframe, object, embed, audio, video, source, link")
        .unwrap()
});
static ANCHORS: Lazy<Selector> = Lazy::new(|| Selector::parse("a[href]").unwrap());

fn sanitize_xhtml(text: &str) -> String {
    let mut out = SCRIPT.replace_all(text, "").into_owned();
    out = SCRIPT_SELF.replace_all(&out, "").into_owned();
    out = IFRAME.replace_all(&out, "").into_owned();
    out = OBJECT.replace_all(&out, "").into_owned();
    out = APPLET.replace_all(&out, "").into_owned();
    out = FRAMESET.replace_all(&out, "").into_owned();
    out = VOID_ACTIVE.replace_all(&out, "").into_owned();
    out = META_REFRESH.replace_all(&out, "").into_owned();
    out = EVENT_ATTR.replace_all(&out, "").into_owned();
    out = JS_URL
        .replace_all(&out, r#"$1="about:blank#""#)
        .into_owned();
    out
}

fn sanitize_tree(book_dir: &Path) -> Result<(), ImportError> {
    let oebps = book_dir.join("OEBPS");
    let mut stack = vec![oebps.clone()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in rd {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let rel = path
                .strip_prefix(&oebps)
                .map_err(|_| bad("文件路径越界"))?
                .to_string_lossy()
                .replace('\\', "/");
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "xhtml" | "html" | "htm") {
                let raw = fs::read_to_string(&path)?;
                validate_xhtml_resources(&raw, &rel)?;
                let clean = sanitize_xhtml(&raw);
                if clean != raw {
                    fs::write(&path, clean)?;
                }
                validate_xhtml_resources(&fs::read_to_string(&path)?, &rel)?;
            } else if ext == "css" {
                let raw = fs::read_to_string(&path)?;
                validate_css(&raw, &rel)?;
            }
        }
    }
    Ok(())
}

fn validate_css(css: &str, file: &str) -> Result<(), ImportError> {
    if REMOTE_CSS.is_match(css) {
        return Err(bad(format!("CSS 引用了外网资源（暂不支持）：{file}")));
    }
    Ok(())
}

fn validate_xhtml_resources(xhtml: &str, file: &str) -> Result<(), ImportError> {
    let doc = Html::parse_document(xhtml);
    for el in doc.select(&RESOURCE_TAGS) {
        let tag = el.value().name();
        for attr in ["src", "href", "data", "xlink:href", "poster"] {
            if let Some(raw) = el.value().attr(attr) {
                check_resource_url(raw, file, tag)?;
            }
        }
        if let Some(srcset) = el.value().attr("srcset") {
            for part in srcset.split(',') {
                if let Some(url) = part.split_whitespace().next() {
                    check_resource_url(url, file, tag)?;
                }
            }
        }
        if let Some(style) = el.value().attr("style") {
            validate_css(style, file)?;
        }
    }
    if REMOTE_CSS.is_match(xhtml) {
        return Err(bad(format!("XHTML 的样式引用了外网资源：{file}")));
    }
    Ok(())
}

fn check_resource_url(raw: &str, file: &str, tag: &str) -> Result<(), ImportError> {
    let url = raw.trim();
    if url.is_empty() || url.starts_with('#') {
        return Ok(());
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return Ok(());
    }
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("//")
        || lower.starts_with("ftp://")
    {
        return Err(bad(format!(
            "XHTML 引用了外网资源（暂不支持）：{file} 的 <{tag}>"
        )));
    }
    if url.starts_with('/') || url.contains('\\') {
        return Err(bad(format!(
            "XHTML 使用了绝对资源路径（暂不支持）：{file} 的 <{tag}>"
        )));
    }
    // 相对路径允许 `..`，但必须仍在 OEBPS 内。以章节文件所在目录为基准折叠。
    let path = super::href_to_path(url.split(['?', '#']).next().unwrap_or(url));
    let parent = Path::new(file)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let mut parts: Vec<&str> = parent
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(bad(format!(
                        "XHTML 资源路径越出书目录（暂不支持）：{file} 的 <{tag}>"
                    )));
                }
            }
            s => parts.push(s),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 标题与指纹
// ---------------------------------------------------------------------------

fn manifest_items(xml: &str) -> Vec<(String, String, String, String)> {
    let Ok(package) = sxd_document::parser::parse(xml) else {
        return Vec::new();
    };
    let doc = package.as_document();
    let mut out = Vec::new();
    for child in doc.root().children() {
        let Some(el) = child.element() else {
            continue;
        };
        if el.name().local_part() != "package" {
            continue;
        }
        for node in el.children() {
            let Some(manifest) = node.element() else {
                continue;
            };
            if manifest.name().local_part() != "manifest" {
                continue;
            }
            for item in manifest.children() {
                let Some(item) = item.element() else {
                    continue;
                };
                if item.name().local_part() != "item" {
                    continue;
                }
                out.push((
                    item.attribute_value("id").unwrap_or("").to_string(),
                    // 原样保留 URI 写法；调用方按需解码，避免重复解码。
                    item.attribute_value("href").unwrap_or("").to_string(),
                    item.attribute_value("media-type").unwrap_or("").to_string(),
                    item.attribute_value("properties").unwrap_or("").to_string(),
                ));
            }
        }
    }
    out
}

fn nav_titles(book_dir: &Path, nav_href: &str) -> Option<HashMap<String, String>> {
    let nav = fs::read_to_string(book_dir.join("OEBPS").join(nav_href)).ok()?;
    let doc = Html::parse_document(&nav);
    let dir = parent_dir(nav_href);
    let mut out = HashMap::new();
    for a in doc.select(&ANCHORS) {
        let Some(href) = a.value().attr("href") else {
            continue;
        };
        let target = href.split(['?', '#']).next().unwrap_or("").trim();
        if target.is_empty() {
            continue;
        }
        let Some(rel) = resolve_rel(&dir, &super::href_to_path(target)) else {
            continue;
        };
        let title = collapse(&a.text().collect::<Vec<_>>().join(" "));
        if !title.is_empty() {
            out.entry(rel).or_insert(title);
        }
    }
    Some(out)
}

fn ncx_titles(book_dir: &Path, meta: &opf::OpfMeta) -> Option<HashMap<String, String>> {
    let ncx_href = meta
        .resources
        .iter()
        .find(|r| {
            r.media_type
                .eq_ignore_ascii_case("application/x-dtbncx+xml")
        })
        .map(|r| r.href.clone())?;
    let xml = fs::read_to_string(book_dir.join("OEBPS").join(&ncx_href)).ok()?;
    let package = sxd_document::parser::parse(&xml).ok()?;
    let doc = package.as_document();
    let dir = parent_dir(&ncx_href);
    let mut out = HashMap::new();
    for child in doc.root().children() {
        if let Some(el) = child.element() {
            walk_ncx(el, &dir, &mut out);
        }
    }
    Some(out)
}

fn walk_ncx(el: sxd_document::dom::Element<'_>, dir: &str, out: &mut HashMap<String, String>) {
    if el.name().local_part() == "navPoint" {
        let label = direct_child(el, "navLabel")
            .and_then(|n| direct_child(n, "text"))
            .map(text_of)
            .map(|t| collapse(&t));
        let src = direct_child(el, "content")
            .and_then(|c| c.attribute_value("src"))
            .map(str::to_string);
        if let (Some(label), Some(src)) = (label, src) {
            let target = src.split(['?', '#']).next().unwrap_or("").trim();
            if let Some(rel) = resolve_rel(dir, &super::href_to_path(target)) {
                if !label.is_empty() {
                    out.entry(rel).or_insert(label);
                }
            }
        }
    }
    for child in el.children() {
        if let Some(child) = child.element() {
            walk_ncx(child, dir, out);
        }
    }
}

fn direct_child<'a>(
    el: sxd_document::dom::Element<'a>,
    name: &str,
) -> Option<sxd_document::dom::Element<'a>> {
    el.children()
        .into_iter()
        .filter_map(|c| c.element())
        .find(|c| c.name().local_part() == name)
}

fn text_of(el: sxd_document::dom::Element<'_>) -> String {
    let mut out = String::new();
    for child in el.children() {
        match child {
            sxd_document::dom::ChildOfElement::Text(t) => out.push_str(t.text()),
            sxd_document::dom::ChildOfElement::Element(e) => out.push_str(&text_of(e)),
            _ => {}
        }
    }
    out
}

fn resolve_rel(from_dir: &str, href: &str) -> Option<String> {
    if href.is_empty() || href.starts_with('/') || href.contains(':') || href.contains('\0') {
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

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fill_titles_and_fingerprint(
    book_dir: &Path,
    meta: &mut opf::OpfMeta,
    nav_titles: &HashMap<String, String>,
    ncx_titles: &HashMap<String, String>,
) -> Result<Option<String>, ImportError> {
    let oebps = book_dir.join("OEBPS");
    let mut hasher = Sha256::new();
    let mut text_chars = 0usize;
    let mut title_sel = None;

    for ch in &mut meta.chapters {
        if ch.title.trim().is_empty() {
            ch.title = nav_titles
                .get(&ch.href)
                .or_else(|| ncx_titles.get(&ch.href))
                .cloned()
                .unwrap_or_default();
        }
        let path = oebps.join(&ch.href);
        let Ok(raw) = fs::read_to_string(&path) else {
            if ch.title.trim().is_empty() {
                ch.title = file_stem(&ch.href);
            }
            continue;
        };
        let doc = Html::parse_document(&raw);
        if ch.title.trim().is_empty() {
            let sel = title_sel.get_or_insert_with(|| Selector::parse("title").unwrap());
            if let Some(t) = doc.select(sel).next() {
                ch.title = collapse(&t.text().collect::<Vec<_>>().join(" "));
            }
        }
        if ch.title.trim().is_empty() {
            let sel = heading_selector(&mut title_sel);
            if let Some(t) = doc.select(sel).next() {
                ch.title = collapse(&t.text().collect::<Vec<_>>().join(" "));
            }
        }
        if ch.title.trim().is_empty() {
            ch.title = file_stem(&ch.href);
        }
        let sel = Selector::parse("body").unwrap();
        let body = doc
            .select(&sel)
            .next()
            .map(|b| collapse(&b.text().collect::<Vec<_>>().join(" ")))
            .unwrap_or_default();
        text_chars += body.chars().count();
        hasher.update(body.as_bytes());
        hasher.update(b"\n");
    }

    // 正文太短的图像书/宣传册不参与内容查重，避免所有图册指纹相同。
    let fingerprint = if text_chars >= 64 {
        Some(format!("{:x}", hasher.finalize()))
    } else {
        None
    };
    Ok(fingerprint)
}

// 单独拎出来避免借用两个 Selector 变量时冲突。
fn heading_selector(_slot: &mut Option<Selector>) -> &Selector {
    static HEADING: Lazy<Selector> = Lazy::new(|| Selector::parse("h1, h2, h3").unwrap());
    &HEADING
}

fn file_stem(href: &str) -> String {
    let name = href.rsplit('/').next().unwrap_or(href);
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if stem.trim().is_empty() {
        href.to_string()
    } else {
        stem.to_string()
    }
}

fn dedupe_manifest_ids(meta: &mut opf::OpfMeta) {
    let mut used: HashSet<String> = ["nav", "style", "cover-image", "ch-0000"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut seq = 0usize;
    for ch in &mut meta.chapters {
        if !valid_xml_id(&ch.id) || !used.insert(ch.id.clone()) {
            ch.id = next_manifest_id(&mut used, &mut seq, "ch-import");
        }
    }
    for img in &mut meta.images {
        if !valid_xml_id(&img.id) || !used.insert(img.id.clone()) {
            img.id = next_manifest_id(&mut used, &mut seq, "img-import");
        }
    }
    for res in &mut meta.resources {
        if !valid_xml_id(&res.id) || !used.insert(res.id.clone()) {
            res.id = next_manifest_id(&mut used, &mut seq, "res-import");
        }
    }
}

fn next_manifest_id(used: &mut HashSet<String>, seq: &mut usize, prefix: &str) -> String {
    loop {
        *seq += 1;
        let id = format!("{prefix}-{seq}");
        if used.insert(id.clone()) {
            return id;
        }
    }
}

fn valid_xml_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

// ---------------------------------------------------------------------------
// 查重与 commit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateHit {
    /// `file` / `content` / `title_author`
    pub kind: &'static str,
    pub book_url: String,
    pub name: Option<String>,
    pub author: Option<String>,
}

/// 三层查重：文件 SHA-256 → 正文指纹 → 书名 + 作者。
pub async fn find_duplicates(
    pool: &sqlx::SqlitePool,
    info: &StageInfo,
) -> Result<Vec<DuplicateHit>, ImportError> {
    let mut out = Vec::new();
    let file: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT book_url, name, author FROM library_books WHERE file_sha256 = ?1 LIMIT 1",
    )
    .bind(&info.sha256)
    .fetch_optional(pool)
    .await?;
    if let Some((book_url, name, author)) = file {
        out.push(DuplicateHit {
            kind: "file",
            book_url,
            name,
            author,
        });
    }
    if let Some(fp) = info.content_fingerprint.as_deref() {
        let content: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT book_url, name, author FROM library_books \
             WHERE content_fingerprint = ?1 LIMIT 1",
        )
        .bind(fp)
        .fetch_optional(pool)
        .await?;
        if let Some((book_url, name, author)) = content {
            if !out.iter().any(|h| h.book_url == book_url) {
                out.push(DuplicateHit {
                    kind: "content",
                    book_url,
                    name,
                    author,
                });
            }
        }
    }
    if !info.title.trim().is_empty() {
        let title_author: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT book_url, name, author FROM library_books \
             WHERE lower(trim(name)) = lower(trim(?1)) \
               AND lower(trim(coalesce(author, ''))) = lower(trim(coalesce(?2, ''))) \
             LIMIT 1",
        )
        .bind(&info.title)
        .bind(info.author.as_deref().unwrap_or(""))
        .fetch_optional(pool)
        .await?;
        if let Some((book_url, name, author)) = title_author {
            if !out.iter().any(|h| h.book_url == book_url) {
                out.push(DuplicateHit {
                    kind: "title_author",
                    book_url,
                    name,
                    author,
                });
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    New,
    Overwrite,
    Cancel,
}

impl CommitMode {
    pub fn parse(raw: &str) -> Result<Self, ImportError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "new" | "add" => Ok(CommitMode::New),
            "overwrite" | "replace" => Ok(CommitMode::Overwrite),
            "cancel" => Ok(CommitMode::Cancel),
            other => Err(bad(format!("未知的导入方式：{other}"))),
        }
    }
}

#[derive(Debug)]
pub struct CommitResult {
    pub book_url: String,
    pub created: bool,
    pub overwritten: bool,
    pub cancelled: bool,
    pub check: Option<files::CheckReport>,
}

pub async fn commit(
    store: &Store,
    pool: &sqlx::SqlitePool,
    token: &str,
    mode: CommitMode,
    target_book_url: Option<&str>,
) -> Result<CommitResult, ImportError> {
    validate_token(token)?;
    if mode == CommitMode::Cancel {
        // 取消时暂存区可能已被清理，不应因此报错。
        discard_stage(store, token).await;
        return Ok(CommitResult {
            book_url: String::new(),
            created: false,
            overwritten: false,
            cancelled: true,
            check: None,
        });
    }
    let info = read_stage_info(store, token).await?;

    let stage_dir = store.import_stage_dir(token);
    let book_dir = stage_dir.join("book");
    if !book_dir.join("OEBPS/content.opf").is_file() {
        return Err(bad("导入会话已过期，请重新上传 EPUB"));
    }
    let staged = store
        .read_opf(&book_dir)
        .await
        .map_err(|e| ImportError::Internal(format!("读取暂存 OPF 失败：{e}")))?;

    let final_url = match mode {
        CommitMode::New => unique_book_url(store, pool, &info.book_url).await?,
        CommitMode::Overwrite => {
            let target = target_book_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("覆盖导入需要指定 target_book_url"))?;
            let Some(existing) = library::load_book(pool, target).await? else {
                return Err(bad("要覆盖的书不存在"));
            };
            if existing.tier != super::Tier::Collection.as_str() {
                return Err(bad("只能覆盖馆藏书籍；临时缓存请先加入馆藏"));
            }
            target.to_string()
        }
        CommitMode::Cancel => unreachable!(),
    };

    let _guard = super::lock_book(&final_url).await;
    let dest = store.dir_for(super::Tier::Collection, &final_url);
    let old_revision = if dest.join("OEBPS/content.opf").is_file() {
        store.read_opf(&dest).await.map(|m| m.revision).unwrap_or(0)
    } else {
        0
    };

    let mut meta = staged;
    meta.book_url = final_url.clone();
    meta.revision = old_revision.saturating_add(1);
    meta.modified = opf::now_modified();
    store
        .write_opf(&book_dir, &meta)
        .await
        .map_err(|e| ImportError::Internal(format!("写入 OPF 失败：{e}")))?;
    store
        .write_nav(&book_dir, &meta)
        .await
        .map_err(|e| ImportError::Internal(format!("写入 nav 失败：{e}")))?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let overwritten = dest.exists();
    if overwritten {
        let aside = store.trash_target(&final_url, "import-overwrite");
        tokio::fs::create_dir_all(store.root().join(super::TRASH_DIR)).await?;
        super::rename_retry(&dest, &aside)
            .await
            .map_err(|e| ImportError::Internal(format!("旧书目录移入回收站失败：{e}")))?;
    }
    if let Err(e) = super::rename_retry(&book_dir, &dest).await {
        // rename 失败时尽量把旧书搬回来，别留下「馆藏里没有这本书」的状态。
        if overwritten {
            let aside = store.trash_target(&final_url, "import-overwrite");
            let _ = super::rename_retry(&aside, &dest).await;
        }
        return Err(ImportError::Internal(format!("搬入馆藏失败：{e}")));
    }

    // DB 写失败时目录已经是新的：保留现场并报错，管理员可重建索引/重试。
    upsert_library_book(pool, store, &meta, &info).await?;

    index::sync_book(store, &final_url).await;
    let _ = usage::refresh_rollup(pool, store, &final_url).await;
    let check = files::check_book(store, &final_url).await.ok();
    discard_stage(store, token).await;
    Ok(CommitResult {
        book_url: final_url,
        created: !overwritten,
        overwritten,
        cancelled: false,
        check,
    })
}

async fn unique_book_url(
    store: &Store,
    pool: &sqlx::SqlitePool,
    base: &str,
) -> Result<String, ImportError> {
    if !book_url_taken(store, pool, base).await? {
        return Ok(base.to_string());
    }
    for n in 2..10_000 {
        let candidate = format!("{base}-{n}");
        if !book_url_taken(store, pool, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(bad("同名导入书籍过多，无法生成新书号"))
}

async fn book_url_taken(
    store: &Store,
    pool: &sqlx::SqlitePool,
    book_url: &str,
) -> Result<bool, ImportError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM library_books WHERE book_url = ?1")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    if exists.is_some() {
        return Ok(true);
    }
    Ok(store.dir_for(super::Tier::Collection, book_url).exists()
        || store.dir_for(super::Tier::Temp, book_url).exists())
}

async fn upsert_library_book(
    pool: &sqlx::SqlitePool,
    store: &Store,
    meta: &opf::OpfMeta,
    info: &StageInfo,
) -> Result<(), ImportError> {
    let now = chrono::Utc::now().timestamp_millis();
    let book_id = Store::book_id(&meta.book_url);
    let cover_url = meta
        .cover
        .as_ref()
        .map(|c| comic::local_image_url_v(&book_id, &format!("OEBPS/{}", c.href), meta.revision));
    let cover_url = if meta.cover.is_some() {
        cover_url
    } else {
        None
    };
    let chapter_count = meta.chapters.iter().filter(|c| !c.is_volume).count() as i64;
    let origin_name = "本地导入";
    let _ = store;

    sqlx::query(
        "INSERT INTO library_books (
            book_url, name, author, kind, cover_url, intro, language, publisher, pubdate,
            series_name, series_index, book_source_type, total_chapter_num, tier,
            collected_at, last_accessed_at, checked_at, file_sha256, content_fingerprint,
            import_filename, origin_name
         ) VALUES (?1,?2,?3,'text',?4,?5,?6,?7,?8,?9,?10,1,?11,'collection',?12,?12,?12,?13,?14,?15,?16)
         ON CONFLICT(book_url) DO UPDATE SET
            name = excluded.name,
            author = excluded.author,
            kind = excluded.kind,
            cover_url = excluded.cover_url,
            intro = excluded.intro,
            language = excluded.language,
            publisher = excluded.publisher,
            pubdate = excluded.pubdate,
            series_name = excluded.series_name,
            series_index = excluded.series_index,
            book_source_type = excluded.book_source_type,
            total_chapter_num = excluded.total_chapter_num,
            tier = 'collection',
            collected_at = COALESCE(library_books.collected_at, excluded.collected_at),
            last_accessed_at = excluded.last_accessed_at,
            checked_at = excluded.checked_at,
            file_sha256 = excluded.file_sha256,
            content_fingerprint = excluded.content_fingerprint,
            import_filename = excluded.import_filename,
            origin_name = excluded.origin_name",
    )
    .bind(&meta.book_url)
    .bind(&meta.title)
    .bind(&meta.author)
    .bind(&cover_url)
    .bind(&meta.description)
    .bind(&meta.language)
    .bind(&meta.publisher)
    .bind(&meta.pubdate)
    .bind(&meta.series)
    .bind(meta.series_index)
    .bind(chapter_count)
    .bind(now)
    .bind(&info.sha256)
    .bind(&info.content_fingerprint)
    .bind(&info.file_name)
    .bind(origin_name)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_removes_active_content() {
        let raw = r#"<html><head><script src="x.js"></script></head>
            <body onload="evil()"><iframe src="https://x">a</iframe>
            <p onclick='x()'>正文</p><a href="javascript:alert(1)">bad</a>
            <embed src="x.swf"></body></html>"#;
        let clean = sanitize_xhtml(raw);
        assert!(!clean.contains("<script"));
        assert!(!clean.contains("<iframe"));
        assert!(!clean.contains("onload"));
        assert!(!clean.contains("onclick"));
        assert!(!clean.contains("javascript:"));
        assert!(clean.contains("正文"));
    }

    #[test]
    fn rejects_remote_resources() {
        let err = validate_xhtml_resources(
            r#"<html><body><img src="https://x/a.jpg"/></body></html>"#,
            "Text/ch.xhtml",
        );
        assert!(err.is_err());
        assert!(validate_xhtml_resources(
            r#"<html><body><a href="https://x">外链</a></body></html>"#,
            "Text/ch.xhtml"
        )
        .is_ok());
    }

    #[test]
    fn rejects_escaping_relative_image() {
        let err = validate_xhtml_resources(
            r#"<html><body><img src="../../outside.jpg"/></body></html>"#,
            "Text/ch.xhtml",
        );
        assert!(err.is_err());
    }

    #[test]
    fn resolves_nested_relative_paths() {
        assert_eq!(
            resolve_rel("Text/part", "../Images/a.jpg"),
            Some("Text/Images/a.jpg".into())
        );
        assert_eq!(resolve_rel("Text", "../../x.jpg"), None);
        assert_eq!(resolve_rel("", "a.jpg"), Some("a.jpg".into()));
    }

    #[test]
    fn validates_zip_names() {
        assert!(validate_entry_name("OEBPS/Text/ch.xhtml").is_ok());
        assert!(validate_entry_name("../evil.xhtml").is_err());
        assert!(validate_entry_name("/etc/passwd").is_err());
        assert!(validate_entry_name("C:/x.xhtml").is_err());
        assert!(validate_entry_name("OEBPS/CON.txt").is_err());
    }

    fn entry(name: &str) -> ZipEntryMeta {
        ZipEntryMeta {
            name: name.into(),
            index: 0,
        }
    }

    #[test]
    fn content_root_handles_common_layouts() {
        // OPF 与内容都在压缩包根目录。
        let flat = vec![
            entry("mimetype"),
            entry("META-INF/container.xml"),
            entry("content.opf"),
            entry("page_styles.css"),
            entry("mathjax.html"),
            entry("toc.ncx"),
        ];
        assert_eq!(content_root(&flat), "");

        // 标准 EPUB：所有内容在 OEBPS/ 下。
        let oebps = vec![
            entry("mimetype"),
            entry("META-INF/container.xml"),
            entry("OEBPS/content.opf"),
            entry("OEBPS/Text/ch1.xhtml"),
            entry("OEBPS/Images/a.jpg"),
        ];
        assert_eq!(content_root(&oebps), "OEBPS");

        // OPF 在 OEBPS/、资源在根：公共根是压缩包根，两边都相对它平移。
        let mixed = vec![
            entry("META-INF/container.xml"),
            entry("OEBPS/content.opf"),
            entry("Text/ch1.xhtml"),
            entry("Images/a.jpg"),
        ];
        assert_eq!(content_root(&mixed), "");

        // 整本书在一个子目录里：公共根保留该目录，平移后目录结构不变。
        let nested = vec![
            entry("mimetype"),
            entry("META-INF/container.xml"),
            entry("book/content.opf"),
            entry("book/Text/ch1.xhtml"),
        ];
        assert_eq!(content_root(&nested), "book");
    }

    #[test]
    fn content_root_ignores_meta_and_junk() {
        let entries = vec![
            entry("mimetype"),
            entry("META-INF/container.xml"),
            entry("__MACOSX/book/._content.opf"),
            entry("book/content.opf"),
            entry("book/Text/ch1.xhtml"),
        ];
        assert_eq!(content_root(&entries), "book");

        // 只有 mimetype / META-INF 时按书根处理，不 panic。
        let only_meta = vec![entry("mimetype"), entry("META-INF/container.xml")];
        assert_eq!(content_root(&only_meta), "");
    }

    #[test]
    fn resolves_manifest_hrefs() {
        assert_eq!(
            resolve_zip_path("OEBPS", "Text/ch1.xhtml").unwrap(),
            "OEBPS/Text/ch1.xhtml"
        );
        assert_eq!(resolve_zip_path("", "ch1.xhtml").unwrap(), "ch1.xhtml");
        assert_eq!(
            resolve_zip_path("OEBPS/Text", "../Images/a.jpg").unwrap(),
            "OEBPS/Images/a.jpg"
        );
        // URI 百分号编码按真实路径解码。
        assert_eq!(
            resolve_zip_path("OEBPS", "Images/a%20b.png").unwrap(),
            "OEBPS/Images/a b.png"
        );
        // 片段与查询串不参与文件定位。
        assert_eq!(
            resolve_zip_path("OEBPS", "Text/ch1.xhtml#frag").unwrap(),
            "OEBPS/Text/ch1.xhtml"
        );
        assert_eq!(
            resolve_zip_path("OEBPS", "Text/ch1.xhtml?x=1").unwrap(),
            "OEBPS/Text/ch1.xhtml"
        );
    }

    #[test]
    fn rejects_manifest_hrefs_outside_root() {
        assert!(resolve_zip_path("", "../x.jpg").is_err());
        assert!(resolve_zip_path("OEBPS", "../../x.jpg").is_err());
        assert!(resolve_zip_path("OEBPS", "/etc/passwd").is_err());
        assert!(resolve_zip_path("OEBPS", "C:/x.xhtml").is_err());
        assert!(resolve_zip_path("OEBPS", "..\\x.xhtml").is_err());
        assert!(resolve_zip_path("OEBPS", "").is_err());
    }

    #[test]
    fn maps_hrefs_to_common_root() {
        assert_eq!(
            new_href_for_layout(
                "ch1",
                "Text/ch1.xhtml",
                "application/xhtml+xml",
                "OEBPS",
                "OEBPS"
            )
            .unwrap(),
            "Text/ch1.xhtml"
        );
        // OPF 在 OEBPS/、图片在书根：新 OPF 也放在 OEBPS/，href 指回书根下的原路径。
        assert_eq!(
            new_href_for_layout("img", "../Images/a.jpg", "image/jpeg", "OEBPS", "").unwrap(),
            "Images/a.jpg"
        );
        assert_eq!(
            new_href_for_layout("img", "Images/a.jpg", "image/jpeg", "book/OEBPS", "book").unwrap(),
            "OEBPS/Images/a.jpg"
        );
    }

    #[test]
    fn rejects_svg_manifest_items() {
        let err = new_href_for_layout("img", "a.svg", "image/svg+xml", "", "").unwrap_err();
        assert!(matches!(err, ImportError::BadRequest(_)));
    }

    #[test]
    fn sanitizer_removes_meta_refresh() {
        let raw = r#"<html><head><meta http-equiv="refresh" content="0;url=https://x"/></head>
            <body><p>正文</p></body></html>"#;
        let clean = sanitize_xhtml(raw);
        assert!(!clean.to_ascii_lowercase().contains("http-equiv"));
        assert!(clean.contains("正文"));
    }

    #[test]
    fn rejects_remote_css() {
        assert!(validate_css("@import url(https://x/a.css);", "a.css").is_err());
        assert!(validate_css("body { background: url(//cdn/x.png); }", "a.css").is_err());
        assert!(validate_css("body { background: url(../Images/a.png); }", "a.css").is_ok());
    }

    fn temp_book_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-import-{tag}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(dir.join("OEBPS/Text")).unwrap();
        dir
    }

    fn chapter_meta() -> opf::OpfMeta {
        let mut meta = opf::OpfMeta::new("test:1", "t");
        meta.chapters.push(opf::ChapterItem {
            id: "ch-0001".into(),
            href: "Text/ch1.xhtml".into(),
            title: String::new(),
            url: None,
            is_volume: false,
        });
        meta
    }

    #[test]
    fn fingerprint_ignores_whitespace_and_short_text() {
        let dir = temp_book_dir("fingerprint");
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let empty = HashMap::new();

        fs::write(
            dir.join("OEBPS/Text/ch1.xhtml"),
            format!("<html><body><p>{long}</p></body></html>"),
        )
        .unwrap();
        let mut meta = chapter_meta();
        let first = fill_titles_and_fingerprint(&dir, &mut meta, &empty, &empty)
            .unwrap()
            .expect("长正文应生成指纹");
        assert!(!meta.chapters[0].title.is_empty());

        // 换行与多余空白不影响指纹。
        fs::write(
            dir.join("OEBPS/Text/ch1.xhtml"),
            format!("<html><body>\n  <p>{long}</p>\n  <p>   </p>\n</body></html>"),
        )
        .unwrap();
        let mut meta = chapter_meta();
        let second = fill_titles_and_fingerprint(&dir, &mut meta, &empty, &empty).unwrap();
        assert_eq!(Some(first), second);

        // 图像书/宣传册正文过短，不参与内容查重。
        fs::write(
            dir.join("OEBPS/Text/ch1.xhtml"),
            "<html><body><p>短</p></body></html>",
        )
        .unwrap();
        let mut meta = chapter_meta();
        assert!(fill_titles_and_fingerprint(&dir, &mut meta, &empty, &empty)
            .unwrap()
            .is_none());
    }
}
