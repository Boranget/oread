//! calibre 式书目录编辑：文件树、源码读写、跨文件替换、预览与 Check book。
//!
//! 只处理文字书（合法 EPUB 3 目录）；漫画仍走 [`super::edit`] 的页面编辑。
//! 所有写入都限制在书目录内、走 [`lock_book`] 与原子写，并在成功后推进
//! `content.opf` 的 `oread:revision`，让图片缓存与索引一起失效。
//!
//! 源码编辑不重建 OPF：只改 `oread:revision` / `dcterms:modified` 两个文本节点，
//! 因此管理员手工加进 manifest 的字体、样式等条目不会被丢掉。

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};
use sxd_document::dom::Element;

use super::edit::{detect_image, require_revision, EditError};
use super::opf;
use super::read::{body_blocks, OpenedBook};
use super::{atomic_write, comic, lock_book, safe_join, OpfMeta, Store};
use boranlib_ebook::html::Block;

pub const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_FILES: usize = 5000;
const MAX_PATH_CHARS: usize = 512;
const MAX_REPLACE_MATCHES: usize = 10_000;
const MAX_PREVIEW_BLOCKS: usize = 5000;
const MAX_PREVIEW_HITS: usize = 100;

/// 文件树里一项的类别。`editable` 是「这个文件可以用源码编辑器保存」，
/// 图片只在 JPEG / WebP 时可替换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Image,
    Binary,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Text => "text",
            FileKind::Image => "image",
            FileKind::Binary => "binary",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BookFile {
    pub path: String,
    pub size: u64,
    pub media_type: String,
    pub kind: FileKind,
    pub editable: bool,
}

pub enum FileContent {
    Text(String),
    Image(String),
}

pub struct ReadFile {
    pub path: String,
    pub size: u64,
    pub media_type: String,
    pub content: FileContent,
}

#[derive(Debug)]
pub struct WriteOutcome {
    pub revision: u64,
    pub size: u64,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReplaceHit {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub matched: String,
    pub replacement: String,
}

#[derive(Debug, Clone)]
pub struct ReplaceOutcome {
    pub total: usize,
    pub changed_files: Vec<String>,
    pub hits: Vec<ReplaceHit>,
    pub truncated: bool,
    pub applied: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplaceOptions<'a> {
    pub query: &'a str,
    pub replacement: &'a str,
    pub regex: bool,
    pub case_sensitive: bool,
    pub paths: Option<&'a [String]>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct CheckIssue {
    pub severity: &'static str,
    pub code: String,
    pub path: Option<String>,
    pub message: String,
}

pub struct CheckReport {
    pub revision: u64,
    pub issues: Vec<CheckIssue>,
    pub checked_files: usize,
}

pub struct PreviewOutcome {
    pub revision: u64,
    pub blocks: Vec<Block>,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// 路径与文件类型
// ---------------------------------------------------------------------------

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn media_type(path: &str) -> &'static str {
    if path == "mimetype" {
        return "text/plain";
    }
    match ext_of(path).as_str() {
        "xhtml" | "html" => "application/xhtml+xml",
        "opf" => "application/oebps-package+xml",
        "css" => "text/css",
        "xml" => "application/xml",
        "ncx" => "application/x-dtbncx+xml",
        "txt" => "text/plain",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "png" => "image/png",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn is_text_path(path: &str) -> bool {
    path == "mimetype"
        || matches!(
            ext_of(path).as_str(),
            "xhtml" | "html" | "opf" | "css" | "xml" | "ncx" | "txt"
        )
}

fn is_xhtml_path(path: &str) -> bool {
    matches!(ext_of(path).as_str(), "xhtml" | "html")
}

fn is_image_path(path: &str) -> bool {
    media_type(path).starts_with("image/")
}

fn is_editable_image(path: &str) -> bool {
    matches!(media_type(path), "image/jpeg" | "image/webp")
}

/// 把用户输入规范成书目录内的相对路径；拒绝绝对路径、`..`、盘符与隐藏文件。
fn normalize_path(raw: &str) -> Result<String, EditError> {
    let raw = raw.trim().replace('\\', "/");
    if raw.is_empty() {
        return Err(EditError::BadRequest("文件路径不能为空".into()));
    }
    if raw.chars().count() > MAX_PATH_CHARS {
        return Err(EditError::BadRequest("文件路径过长".into()));
    }
    if raw.starts_with('/') {
        return Err(EditError::BadRequest(
            "文件路径必须是书目录内的相对路径".into(),
        ));
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(EditError::BadRequest("文件路径不能包含 ..".into()));
        }
        if part.contains(':') || part.contains('\0') || part.starts_with('.') {
            return Err(EditError::BadRequest(format!("非法文件名: {part}")));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(EditError::BadRequest("文件路径不能为空".into()));
    }
    Ok(parts.join("/"))
}

fn rel_path(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn file_rank(path: &str) -> (u8, &str) {
    let rank = if path == "mimetype" {
        0
    } else if path == "META-INF/container.xml" {
        1
    } else if path == "OEBPS/content.opf" {
        2
    } else if path == "OEBPS/nav.xhtml" {
        3
    } else if path == "OEBPS/style.css" {
        4
    } else if path.starts_with("OEBPS/chapters/") {
        5
    } else if path.starts_with("OEBPS/images/") {
        6
    } else {
        7
    };
    (rank, path)
}

// ---------------------------------------------------------------------------
// 文件树与读写
// ---------------------------------------------------------------------------

async fn open_text(store: &Store, book_url: &str) -> Result<OpenedBook, EditError> {
    let book_url = book_url.trim();
    if book_url.is_empty() {
        return Err(EditError::BadRequest("book_url 不能为空".into()));
    }
    store
        .open_book(book_url)
        .await?
        .ok_or_else(|| EditError::NotFound("书目录不存在或不是文字书".into()))
}

async fn walk_files(dir: &Path) -> io::Result<Vec<BookFile>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&current).await {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.contains(".tmp-") {
                continue;
            }
            let ft = entry.file_type().await?;
            if ft.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let Some(path) = rel_path(dir, &entry.path()) else {
                continue;
            };
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            let media = media_type(&path);
            let kind = if is_text_path(&path) {
                FileKind::Text
            } else if media.starts_with("image/") {
                FileKind::Image
            } else {
                FileKind::Binary
            };
            let editable = match kind {
                FileKind::Text => path != "mimetype",
                FileKind::Image => is_editable_image(&path),
                FileKind::Binary => false,
            };
            out.push(BookFile {
                path,
                size,
                media_type: media.to_string(),
                kind,
                editable,
            });
            if out.len() >= MAX_FILES {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// 文件树：返回 OPF 投影与按「mimetype → OPF → nav/style → chapters → images」排序的文件。
pub async fn list_files(
    store: &Store,
    book_url: &str,
) -> Result<(OpfMeta, Vec<BookFile>), EditError> {
    let opened = open_text(store, book_url).await?;
    let mut files = walk_files(&opened.dir).await?;
    files.sort_by(|a, b| {
        file_rank(&a.path)
            .cmp(&file_rank(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok((opened.meta, files))
}

pub async fn read_file(
    store: &Store,
    book_url: &str,
    raw_path: &str,
) -> Result<(OpfMeta, ReadFile), EditError> {
    let opened = open_text(store, book_url).await?;
    let rel = normalize_path(raw_path)?;
    let path = resolve_path(&opened.dir, &rel)?;
    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            EditError::NotFound(format!("文件不存在: {rel}"))
        } else {
            EditError::Io(e)
        }
    })?;
    if !meta.is_file() {
        return Err(EditError::NotFound(format!("文件不存在: {rel}")));
    }
    let media = media_type(&rel);
    if media.starts_with("image/") {
        let image_url =
            comic::local_image_url_v(&Store::book_id(book_url.trim()), &rel, opened.meta.revision);
        return Ok((
            opened.meta,
            ReadFile {
                path: rel,
                size: meta.len(),
                media_type: media.to_string(),
                content: FileContent::Image(image_url),
            },
        ));
    }
    if !is_text_path(&rel) {
        return Err(EditError::BadRequest("该文件不是文本，无法源码编辑".into()));
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(EditError::BadRequest(format!(
            "文件超过上限（最多 {} MB）",
            MAX_TEXT_BYTES / (1024 * 1024)
        )));
    }
    let bytes = tokio::fs::read(&path).await?;
    let text = String::from_utf8(bytes)
        .map_err(|_| EditError::BadRequest("文件不是 UTF-8 文本".into()))?;
    Ok((
        opened.meta,
        ReadFile {
            path: rel,
            size: meta.len(),
            media_type: media.to_string(),
            content: FileContent::Text(text),
        },
    ))
}

fn validate_text(rel: &str, text: &str, book_url: &str) -> Result<(), EditError> {
    if rel == "mimetype" {
        if text != "application/epub+zip" {
            return Err(EditError::BadRequest(
                "mimetype 必须正好是 application/epub+zip（不能有换行）".into(),
            ));
        }
        return Ok(());
    }
    let ext = ext_of(rel);
    if matches!(ext.as_str(), "xhtml" | "html" | "xml" | "opf" | "ncx") {
        sxd_document::parser::parse(text)
            .map_err(|e| EditError::BadRequest(format!("{rel} 不是良构 XML: {e:?}")))?;
    }
    if ext == "opf" {
        let meta = opf::parse_opf(text)
            .map_err(|e| EditError::BadRequest(format!("content.opf 不合法: {e}")))?;
        if meta.book_url.trim() != book_url.trim() {
            return Err(EditError::BadRequest(
                "content.opf 的 book-url 与当前书不一致".into(),
            ));
        }
    }
    Ok(())
}

static RE_OPF_REV: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)(<meta\s+property="oread:revision"[^>]*>).*?(</meta>)"#)
        .expect("valid revision regex")
});
static RE_OPF_MOD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)(<meta\s+property="dcterms:modified"[^>]*>).*?(</meta>)"#)
        .expect("valid modified regex")
});

/// 只替换 OPF 里的修订号与修改时间，保留管理员手写的其余内容。
fn patch_opf_revision(raw: &str, new_rev: u64) -> Result<String, EditError> {
    if !RE_OPF_REV.is_match(raw) {
        return Err(EditError::BadRequest(
            "content.opf 缺少 <meta property=\"oread:revision\">，无法推进修订号".into(),
        ));
    }
    let out = RE_OPF_REV
        .replace(raw, |caps: &regex::Captures<'_>| {
            format!("{}{new_rev}{}", &caps[1], &caps[2])
        })
        .to_string();
    let out = RE_OPF_MOD
        .replace(&out, |caps: &regex::Captures<'_>| {
            format!("{}{}{}", &caps[1], opf::now_modified(), &caps[2])
        })
        .to_string();
    Ok(out)
}

async fn bump_opf_file(dir: &Path, new_rev: u64) -> Result<(), EditError> {
    let path = dir.join("OEBPS").join("content.opf");
    let raw = tokio::fs::read_to_string(&path).await?;
    let patched = patch_opf_revision(&raw, new_rev)?;
    atomic_write(&path, patched.as_bytes()).await?;
    Ok(())
}

pub async fn write_text_file(
    store: &Store,
    book_url: &str,
    raw_path: &str,
    revision: u64,
    text: &str,
) -> Result<WriteOutcome, EditError> {
    let book_url = book_url.trim();
    let rel = normalize_path(raw_path)?;
    if rel == "mimetype" {
        return Err(EditError::BadRequest("mimetype 只读".into()));
    }
    if !is_text_path(&rel) {
        return Err(EditError::BadRequest("只有文本文件可以源码保存".into()));
    }
    if text.len() as u64 > MAX_TEXT_BYTES {
        return Err(EditError::BadRequest(format!(
            "文件超过上限（最多 {} MB）",
            MAX_TEXT_BYTES / (1024 * 1024)
        )));
    }
    let _guard = lock_book(book_url).await;
    let opened = open_text(store, book_url).await?;
    require_revision(opened.meta.revision, revision)?;
    let new_rev = opened.meta.revision.saturating_add(1);
    let path = resolve_path(&opened.dir, &rel)?;

    let final_text = if rel == "OEBPS/content.opf" {
        validate_text(&rel, text, book_url)?;
        let patched = patch_opf_revision(text, new_rev)?;
        validate_text(&rel, &patched, book_url)?;
        patched
    } else {
        validate_text(&rel, text, book_url)?;
        text.to_string()
    };
    atomic_write(&path, final_text.as_bytes()).await?;
    if rel != "OEBPS/content.opf" {
        bump_opf_file(&opened.dir, new_rev).await?;
    }
    super::index::sync_book(store, book_url).await;
    Ok(WriteOutcome {
        revision: new_rev,
        size: final_text.len() as u64,
        image_url: None,
    })
}

pub async fn write_image_file(
    store: &Store,
    book_url: &str,
    raw_path: &str,
    revision: u64,
    data: &[u8],
) -> Result<WriteOutcome, EditError> {
    let book_url = book_url.trim();
    let rel = normalize_path(raw_path)?;
    if !is_image_path(&rel) {
        return Err(EditError::BadRequest("只有图片文件可以替换".into()));
    }
    let (mime, ext) = detect_image(data)?;
    let file_ext = ext_of(&rel);
    let ext_ok = match ext {
        "jpg" => matches!(file_ext.as_str(), "jpg" | "jpeg"),
        "webp" => file_ext == "webp",
        _ => false,
    };
    if !ext_ok {
        return Err(EditError::BadRequest(format!(
            "文件扩展名与图片内容不符（内容是 {mime}）"
        )));
    }
    if data.len() > MAX_IMAGE_BYTES {
        return Err(EditError::BadRequest(format!(
            "图片超过上限（最多 {} MB）",
            MAX_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    let _guard = lock_book(book_url).await;
    let opened = open_text(store, book_url).await?;
    require_revision(opened.meta.revision, revision)?;
    let new_rev = opened.meta.revision.saturating_add(1);
    let path = resolve_path(&opened.dir, &rel)?;
    atomic_write(&path, data).await?;
    bump_opf_file(&opened.dir, new_rev).await?;
    super::index::sync_book(store, book_url).await;
    let image_url = comic::local_image_url_v(&Store::book_id(book_url), &rel, new_rev);
    Ok(WriteOutcome {
        revision: new_rev,
        size: data.len() as u64,
        image_url: Some(image_url),
    })
}

pub async fn delete_file(
    store: &Store,
    book_url: &str,
    raw_path: &str,
    revision: u64,
) -> Result<u64, EditError> {
    let book_url = book_url.trim();
    let rel = normalize_path(raw_path)?;
    if matches!(
        rel.as_str(),
        "mimetype" | "META-INF/container.xml" | "OEBPS/content.opf" | "OEBPS/nav.xhtml"
    ) {
        return Err(EditError::BadRequest(
            "该文件是 EPUB 必需文件，不能删除".into(),
        ));
    }
    let _guard = lock_book(book_url).await;
    let opened = open_text(store, book_url).await?;
    require_revision(opened.meta.revision, revision)?;
    let new_rev = opened.meta.revision.saturating_add(1);
    let path = resolve_path(&opened.dir, &rel)?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(EditError::NotFound(format!("文件不存在: {rel}")))
        }
        Err(e) => return Err(EditError::Io(e)),
    }
    bump_opf_file(&opened.dir, new_rev).await?;
    super::index::sync_book(store, book_url).await;
    Ok(new_rev)
}

// ---------------------------------------------------------------------------
// 跨文件搜索替换
// ---------------------------------------------------------------------------

fn line_at(text: &str, idx: usize) -> String {
    let start = text[..idx].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[idx..]
        .find('\n')
        .map(|i| idx + i)
        .unwrap_or(text.len());
    let line = &text[start..end];
    let mut out: String = line.chars().take(240).collect();
    if line.chars().count() > 240 {
        out.push('…');
    }
    out
}

fn replacement_preview(re: &Regex, matched: &str, replacement: &str, literal: bool) -> String {
    if literal {
        return replacement.to_string();
    }
    re.replace(matched, replacement).to_string()
}

pub async fn replace_all(
    store: &Store,
    book_url: &str,
    revision: u64,
    opts: &ReplaceOptions<'_>,
) -> Result<ReplaceOutcome, EditError> {
    let book_url = book_url.trim();
    if opts.query.trim().is_empty() {
        return Err(EditError::BadRequest("查找内容不能为空".into()));
    }
    if opts.query.chars().count() > 500 {
        return Err(EditError::BadRequest("查找内容最多 500 字".into()));
    }
    if opts.replacement.chars().count() > 10_000 {
        return Err(EditError::BadRequest("替换内容最多 10000 字".into()));
    }
    let pattern = if opts.regex {
        opts.query.to_string()
    } else {
        regex::escape(opts.query)
    };
    let re = RegexBuilder::new(&pattern)
        .case_insensitive(!opts.case_sensitive)
        .build()
        .map_err(|e| EditError::BadRequest(format!("正则不合法: {e}")))?;

    let _guard = if opts.dry_run {
        None
    } else {
        Some(lock_book(book_url).await)
    };
    let opened = open_text(store, book_url).await?;
    if !opts.dry_run {
        require_revision(opened.meta.revision, revision)?;
    }
    let all = walk_files(&opened.dir).await?;
    let mut targets = Vec::new();
    if let Some(paths) = opts.paths {
        for p in paths {
            let rel = normalize_path(p)?;
            if rel == "mimetype" || !is_text_path(&rel) || targets.contains(&rel) {
                continue;
            }
            targets.push(rel);
        }
    } else {
        for f in &all {
            if f.kind == FileKind::Text && f.path != "mimetype" {
                targets.push(f.path.clone());
            }
        }
    }
    if targets.len() > MAX_FILES {
        return Err(EditError::BadRequest(format!(
            "文件过多（最多 {MAX_FILES} 个）"
        )));
    }

    let mut total = 0usize;
    let mut hits = Vec::new();
    let mut truncated = false;
    let mut changes: Vec<(String, String)> = Vec::new();
    for rel in targets {
        let path = resolve_path(&opened.dir, &rel)?;
        let bytes = tokio::fs::read(&path).await?;
        if bytes.len() as u64 > MAX_TEXT_BYTES {
            return Err(EditError::BadRequest(format!(
                "文件 {rel} 超过上限（最多 {} MB）",
                MAX_TEXT_BYTES / (1024 * 1024)
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| EditError::BadRequest(format!("文件 {rel} 不是 UTF-8 文本")))?;
        let count = re.find_iter(&text).count();
        if count == 0 {
            continue;
        }
        total = total.saturating_add(count);
        if total > MAX_REPLACE_MATCHES {
            return Err(EditError::BadRequest(format!(
                "匹配超过 {MAX_REPLACE_MATCHES} 处，请缩小范围"
            )));
        }
        for m in re.find_iter(&text).take(5) {
            if hits.len() >= MAX_PREVIEW_HITS {
                truncated = true;
                break;
            }
            let line = text[..m.start()].matches('\n').count() + 1;
            hits.push(ReplaceHit {
                path: rel.clone(),
                line,
                text: line_at(&text, m.start()),
                matched: m.as_str().to_string(),
                replacement: replacement_preview(&re, m.as_str(), opts.replacement, !opts.regex),
            });
        }
        let new_text = if opts.regex {
            re.replace_all(&text, opts.replacement).to_string()
        } else {
            re.replace_all(&text, regex::NoExpand(opts.replacement))
                .to_string()
        };
        if new_text == text {
            continue;
        }
        validate_text(&rel, &new_text, book_url)?;
        changes.push((rel, new_text));
    }

    let mut changed_files: Vec<String> = changes.iter().map(|(p, _)| p.clone()).collect();
    changed_files.sort();
    changed_files.dedup();

    if opts.dry_run || changes.is_empty() {
        return Ok(ReplaceOutcome {
            total,
            changed_files,
            hits,
            truncated,
            applied: false,
            revision: opened.meta.revision,
        });
    }

    let new_rev = opened.meta.revision.saturating_add(1);
    // 先把 OPF 的最终文本算好并校验，再写其它文件；避免写到一半才发现修订号补不上。
    let opf_patched = changes
        .iter()
        .find(|(p, _)| p == "OEBPS/content.opf")
        .map(|(rel, new_text)| {
            let patched = patch_opf_revision(new_text, new_rev)?;
            validate_text(rel, &patched, book_url)?;
            Ok::<(String, String), EditError>((rel.clone(), patched))
        })
        .transpose()?;
    // 先写章节 / 样式 / nav，最后写 OPF；OPF 改了也只补修订号，不重建。
    for (rel, new_text) in changes.iter() {
        if rel == "OEBPS/content.opf" {
            continue;
        }
        let path = resolve_path(&opened.dir, rel)?;
        atomic_write(&path, new_text.as_bytes()).await?;
    }
    if let Some((rel, patched)) = opf_patched {
        let path = resolve_path(&opened.dir, &rel)?;
        atomic_write(&path, patched.as_bytes()).await?;
    } else {
        bump_opf_file(&opened.dir, new_rev).await?;
    }
    super::index::sync_book(store, book_url).await;
    Ok(ReplaceOutcome {
        total,
        changed_files,
        hits,
        truncated,
        applied: true,
        revision: new_rev,
    })
}

// ---------------------------------------------------------------------------
// Check book
// ---------------------------------------------------------------------------

fn push_issue(
    issues: &mut Vec<CheckIssue>,
    severity: &'static str,
    code: &str,
    path: Option<String>,
    message: impl Into<String>,
) {
    issues.push(CheckIssue {
        severity,
        code: code.to_string(),
        path,
        message: message.into(),
    });
}

fn element_children<'a>(el: Element<'a>) -> Vec<Element<'a>> {
    el.children()
        .into_iter()
        .filter_map(|c| c.element())
        .collect()
}

fn find_element<'a>(el: Element<'a>, local: &str) -> Option<Element<'a>> {
    if el.name().local_part() == local {
        return Some(el);
    }
    for child in element_children(el) {
        if let Some(found) = find_element(child, local) {
            return Some(found);
        }
    }
    None
}

fn find_all<'a>(el: Element<'a>, local: &str, out: &mut Vec<Element<'a>>) {
    if el.name().local_part() == local {
        out.push(el);
    }
    for child in element_children(el) {
        find_all(child, local, out);
    }
}

fn attr(el: Element<'_>, name: &str) -> String {
    el.attribute_value(name).unwrap_or("").to_string()
}

struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
    properties: String,
}

fn parse_package(xml: &str) -> Result<(Vec<ManifestItem>, Vec<String>), String> {
    let package = sxd_document::parser::parse(xml).map_err(|e| format!("XML 不良构: {e:?}"))?;
    let doc = package.as_document();
    let root = doc
        .root()
        .children()
        .into_iter()
        .filter_map(|c| c.element())
        .find(|e| e.name().local_part() == "package")
        .ok_or_else(|| "OPF 缺少 <package> 根元素".to_string())?;
    let mut items = Vec::new();
    let mut spine = Vec::new();
    for child in element_children(root) {
        match child.name().local_part() {
            "manifest" => {
                for item in element_children(child) {
                    if item.name().local_part() != "item" {
                        continue;
                    }
                    items.push(ManifestItem {
                        id: attr(item, "id"),
                        href: attr(item, "href"),
                        media_type: attr(item, "media-type"),
                        properties: attr(item, "properties"),
                    });
                }
            }
            "spine" => {
                for itemref in element_children(child) {
                    if itemref.name().local_part() == "itemref" {
                        spine.push(attr(itemref, "idref"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok((items, spine))
}

fn parse_container_rootfile(xml: &str) -> Result<String, String> {
    let package = sxd_document::parser::parse(xml).map_err(|e| format!("XML 不良构: {e:?}"))?;
    let doc = package.as_document();
    for child in doc.root().children() {
        if let Some(el) = child.element() {
            if let Some(rootfile) = find_element(el, "rootfile") {
                let path = attr(rootfile, "full-path");
                if !path.trim().is_empty() {
                    return Ok(path);
                }
            }
        }
    }
    Err("container.xml 缺少 rootfile full-path".into())
}

fn strip_query_fragment(raw: &str) -> &str {
    raw.split(['?', '#']).next().unwrap_or(raw)
}

/// 把书内相对引用解析成绝对路径，允许 `..` 在书目录内回退，但不允许越界。
///
/// 章节里的图片地址是 `../images/x.jpg`，不能用 [`safe_join`]（它直接拒绝 `..`）。
fn resolve_inside(root: &Path, from_dir: &str, href: &str) -> Option<PathBuf> {
    let href = strip_query_fragment(href.trim());
    if href.is_empty() || href.starts_with('/') || href.contains(':') {
        return None;
    }
    let mut parts: Vec<String> = from_dir
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s if s.starts_with('.') => return None,
            s => parts.push(s.to_string()),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().fold(root.to_path_buf(), |p, s| p.join(s)))
}

/// 把用户路径限制在书目录内；越界按参数错误处理，避免落到 500。
fn resolve_path(dir: &Path, rel: &str) -> Result<PathBuf, EditError> {
    safe_join(dir, rel).map_err(|_| EditError::BadRequest(format!("非法文件路径: {rel}")))
}

fn check_image_src(issues: &mut Vec<CheckIssue>, root: &Path, chapter: &str, src: &str) {
    let raw = src.trim();
    if raw.is_empty() || raw.contains("://") || raw.starts_with("data:") {
        return;
    }
    let from_dir = Path::new(chapter)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let from_dir = from_dir
        .strip_prefix("OEBPS/")
        .unwrap_or(&from_dir)
        .to_string();
    match resolve_inside(root, &from_dir, raw) {
        Some(path) if path.is_file() => {}
        Some(_) => push_issue(
            issues,
            "error",
            "image_missing",
            Some(chapter.to_string()),
            format!("图片不存在: {raw}"),
        ),
        None => push_issue(
            issues,
            "error",
            "image_path",
            Some(chapter.to_string()),
            format!("图片路径越界: {raw}"),
        ),
    }
}

pub async fn check_book(store: &Store, book_url: &str) -> Result<CheckReport, EditError> {
    let book_url = book_url.trim();
    let opened = open_text(store, book_url).await?;
    let files = walk_files(&opened.dir).await?;
    let checked_files = files.len();
    let mut issues = Vec::new();

    // mimetype
    match tokio::fs::read(opened.dir.join("mimetype")).await {
        Ok(bytes) if bytes == b"application/epub+zip" => {}
        Ok(_) => push_issue(
            &mut issues,
            "error",
            "mimetype_content",
            Some("mimetype".into()),
            "mimetype 必须正好是 application/epub+zip（不能有换行）",
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => push_issue(
            &mut issues,
            "error",
            "mimetype_missing",
            Some("mimetype".into()),
            "缺少 mimetype 文件",
        ),
        Err(e) => push_issue(
            &mut issues,
            "error",
            "mimetype_io",
            Some("mimetype".into()),
            format!("读取 mimetype 失败: {e}"),
        ),
    }

    // container.xml
    match tokio::fs::read_to_string(opened.dir.join("META-INF/container.xml")).await {
        Ok(xml) => match parse_container_rootfile(&xml) {
            Ok(rootfile) => match resolve_path(&opened.dir, &rootfile) {
                Ok(path) if path.is_file() => {}
                Ok(_) => push_issue(
                    &mut issues,
                    "error",
                    "container_rootfile_missing",
                    Some("META-INF/container.xml".into()),
                    format!("rootfile 指向的文件不存在: {rootfile}"),
                ),
                Err(_) => push_issue(
                    &mut issues,
                    "error",
                    "container_rootfile_path",
                    Some("META-INF/container.xml".into()),
                    format!("rootfile 路径越界: {rootfile}"),
                ),
            },
            Err(e) => push_issue(
                &mut issues,
                "error",
                "container_invalid",
                Some("META-INF/container.xml".into()),
                e,
            ),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => push_issue(
            &mut issues,
            "error",
            "container_missing",
            Some("META-INF/container.xml".into()),
            "缺少 META-INF/container.xml",
        ),
        Err(e) => push_issue(
            &mut issues,
            "error",
            "container_io",
            Some("META-INF/container.xml".into()),
            format!("读取 container.xml 失败: {e}"),
        ),
    }

    // OPF
    let opf_raw = match tokio::fs::read_to_string(opened.dir.join("OEBPS/content.opf")).await {
        Ok(v) => v,
        Err(e) => {
            push_issue(
                &mut issues,
                "error",
                "opf_missing",
                Some("OEBPS/content.opf".into()),
                format!("读取 content.opf 失败: {e}"),
            );
            return Ok(CheckReport {
                revision: opened.meta.revision,
                issues,
                checked_files,
            });
        }
    };
    let meta = match opf::parse_opf(&opf_raw) {
        Ok(v) => v,
        Err(e) => {
            push_issue(
                &mut issues,
                "error",
                "opf_invalid",
                Some("OEBPS/content.opf".into()),
                e,
            );
            return Ok(CheckReport {
                revision: opened.meta.revision,
                issues,
                checked_files,
            });
        }
    };
    if meta.book_url.trim() != book_url {
        push_issue(
            &mut issues,
            "error",
            "opf_book_url",
            Some("OEBPS/content.opf".into()),
            "content.opf 的 book-url 与当前书不一致",
        );
    }
    if !opf_raw.contains("property=\"dcterms:modified\"") {
        push_issue(
            &mut issues,
            "warning",
            "opf_modified_missing",
            Some("OEBPS/content.opf".into()),
            "缺少 dcterms:modified",
        );
    }
    if !opf_raw.contains("property=\"oread:revision\"") {
        push_issue(
            &mut issues,
            "warning",
            "opf_revision_missing",
            Some("OEBPS/content.opf".into()),
            "缺少 oread:revision，保存时无法推进修订号",
        );
    }
    let (items, spine) = match parse_package(&opf_raw) {
        Ok(v) => v,
        Err(e) => {
            push_issue(
                &mut issues,
                "error",
                "opf_manifest_invalid",
                Some("OEBPS/content.opf".into()),
                e,
            );
            return Ok(CheckReport {
                revision: opened.meta.revision,
                issues,
                checked_files,
            });
        }
    };

    let mut ids: HashMap<String, String> = HashMap::new();
    let mut hrefs: HashSet<String> = HashSet::new();
    for item in &items {
        let item_path = if item.href.is_empty() {
            None
        } else {
            Some(format!("OEBPS/{}", item.href))
        };
        if item.id.is_empty() {
            push_issue(
                &mut issues,
                "error",
                "manifest_id_missing",
                item_path.clone(),
                "manifest item 缺少 id",
            );
        } else if ids.insert(item.id.clone(), item.href.clone()).is_some() {
            push_issue(
                &mut issues,
                "error",
                "manifest_id_duplicate",
                item_path.clone(),
                format!("manifest id 重复: {}", item.id),
            );
        }
        if item.href.is_empty() {
            push_issue(
                &mut issues,
                "error",
                "manifest_href_missing",
                item_path.clone(),
                format!("manifest item 缺少 href: {}", item.id),
            );
            continue;
        }
        if !hrefs.insert(item.href.clone()) {
            push_issue(
                &mut issues,
                "warning",
                "manifest_href_duplicate",
                item_path.clone(),
                format!("manifest href 重复: {}", item.href),
            );
        }
        let path = match resolve_path(&opened.dir.join("OEBPS"), &item.href) {
            Ok(p) => p,
            Err(_) => {
                push_issue(
                    &mut issues,
                    "error",
                    "manifest_href_path",
                    item_path.clone(),
                    format!("manifest href 越界: {}", item.href),
                );
                continue;
            }
        };
        if !path.is_file() {
            push_issue(
                &mut issues,
                "error",
                "manifest_href_missing_file",
                item_path.clone(),
                format!("manifest 指向的文件不存在: {}", item.href),
            );
            continue;
        }
        if item.media_type == "application/xhtml+xml" || item.media_type == "text/html" {
            match tokio::fs::read_to_string(&path).await {
                Ok(xhtml) => {
                    if sxd_document::parser::parse(&xhtml).is_err() {
                        push_issue(
                            &mut issues,
                            "error",
                            "xhtml_invalid",
                            item_path.clone(),
                            "XHTML 不是良构 XML",
                        );
                    } else {
                        for block in body_blocks(&xhtml, "") {
                            if let Block::Image { src, .. } = block {
                                check_image_src(
                                    &mut issues,
                                    &opened.dir,
                                    item_path.as_deref().unwrap_or(""),
                                    &src,
                                );
                            }
                        }
                    }
                }
                Err(e) => push_issue(
                    &mut issues,
                    "error",
                    "xhtml_read",
                    item_path.clone(),
                    format!("读取 XHTML 失败: {e}"),
                ),
            }
        }
    }

    for idref in &spine {
        if idref.is_empty() {
            push_issue(
                &mut issues,
                "error",
                "spine_idref_missing",
                Some("OEBPS/content.opf".into()),
                "spine itemref 缺少 idref",
            );
        } else if !ids.contains_key(idref) {
            push_issue(
                &mut issues,
                "error",
                "spine_idref_unknown",
                Some("OEBPS/content.opf".into()),
                format!("spine 引用了 manifest 里不存在的 id: {idref}"),
            );
        }
    }
    if spine.is_empty() {
        push_issue(
            &mut issues,
            "warning",
            "spine_empty",
            Some("OEBPS/content.opf".into()),
            "spine 为空",
        );
    } else if meta.chapters.is_empty() {
        push_issue(
            &mut issues,
            "warning",
            "spine_no_chapters",
            Some("OEBPS/content.opf".into()),
            "spine 只有占位章，没有正文章节",
        );
    }

    // nav.xhtml
    let nav_item = items
        .iter()
        .find(|i| i.properties.split_whitespace().any(|p| p == "nav"));
    let Some(nav_item) = nav_item else {
        push_issue(
            &mut issues,
            "error",
            "nav_missing",
            Some("OEBPS/content.opf".into()),
            "manifest 缺少 properties=\"nav\" 的导航文件",
        );
        return finish_check(opened.meta.revision, issues, checked_files);
    };
    let nav_path = resolve_path(&opened.dir.join("OEBPS"), &nav_item.href)?;
    match tokio::fs::read_to_string(&nav_path).await {
        Ok(nav) => match sxd_document::parser::parse(&nav) {
            Ok(package) => {
                let doc = package.as_document();
                let mut anchors = Vec::new();
                for child in doc.root().children() {
                    if let Some(el) = child.element() {
                        find_all(el, "a", &mut anchors);
                    }
                }
                for a in anchors {
                    let href = attr(a, "href");
                    if href.trim().is_empty() {
                        push_issue(
                            &mut issues,
                            "warning",
                            "nav_link_empty",
                            Some(format!("OEBPS/{}", nav_item.href)),
                            "导航链接缺少 href",
                        );
                        continue;
                    }
                    let target = strip_query_fragment(href.trim());
                    if target.is_empty() {
                        continue;
                    }
                    match resolve_path(&opened.dir.join("OEBPS"), target) {
                        Ok(p) if p.is_file() => {}
                        Ok(_) => push_issue(
                            &mut issues,
                            "error",
                            "nav_link_missing",
                            Some(format!("OEBPS/{}", nav_item.href)),
                            format!("导航链接指向的文件不存在: {href}"),
                        ),
                        Err(_) => push_issue(
                            &mut issues,
                            "error",
                            "nav_link_path",
                            Some(format!("OEBPS/{}", nav_item.href)),
                            format!("导航链接路径越界: {href}"),
                        ),
                    }
                }
            }
            Err(e) => push_issue(
                &mut issues,
                "error",
                "nav_invalid",
                Some(format!("OEBPS/{}", nav_item.href)),
                format!("nav 不是良构 XML: {e:?}"),
            ),
        },
        Err(e) => push_issue(
            &mut issues,
            "error",
            "nav_read",
            Some(format!("OEBPS/{}", nav_item.href)),
            format!("读取 nav 失败: {e}"),
        ),
    }

    if let Some(cover) = &meta.cover {
        if let Ok(path) = safe_join(&opened.dir.join("OEBPS"), &cover.href) {
            if !path.is_file() {
                push_issue(
                    &mut issues,
                    "error",
                    "cover_missing",
                    Some(format!("OEBPS/{}", cover.href)),
                    "封面文件不存在",
                );
            }
        }
    }
    for ch in &meta.chapters {
        if !ch.is_volume && ch.title.trim().is_empty() {
            push_issue(
                &mut issues,
                "warning",
                "chapter_title_empty",
                Some(format!("OEBPS/{}", ch.href)),
                "章节标题为空",
            );
        }
    }

    // 未登记文件：允许 META-INF 下的其它标准文件，其余给出提示。
    let mut referenced: HashSet<String> = HashSet::new();
    for item in &items {
        referenced.insert(format!("OEBPS/{}", item.href));
    }
    referenced.insert("mimetype".into());
    referenced.insert("META-INF/container.xml".into());
    referenced.insert("OEBPS/content.opf".into());
    for f in &files {
        if f.path.starts_with("META-INF/") || referenced.contains(&f.path) {
            continue;
        }
        push_issue(
            &mut issues,
            "warning",
            "orphan_file",
            Some(f.path.clone()),
            "文件未登记在 OPF manifest",
        );
    }

    finish_check(opened.meta.revision, issues, checked_files)
}

fn finish_check(
    revision: u64,
    mut issues: Vec<CheckIssue>,
    checked_files: usize,
) -> Result<CheckReport, EditError> {
    issues.sort_by(|a, b| {
        (a.severity != "error")
            .cmp(&(b.severity != "error"))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.code.cmp(&b.code))
    });
    Ok(CheckReport {
        revision,
        issues,
        checked_files,
    })
}

// ---------------------------------------------------------------------------
// 实时预览
// ---------------------------------------------------------------------------

pub async fn preview_file(
    store: &Store,
    book_url: &str,
    raw_path: &str,
    text: Option<&str>,
) -> Result<PreviewOutcome, EditError> {
    let book_url = book_url.trim();
    let opened = open_text(store, book_url).await?;
    let rel = normalize_path(raw_path)?;
    if !is_xhtml_path(&rel) {
        return Err(EditError::BadRequest("只有 XHTML 章节可以预览".into()));
    }
    let source = match text {
        Some(t) => t.to_string(),
        None => {
            let path = resolve_path(&opened.dir, &rel)?;
            let bytes = tokio::fs::read(&path).await.map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    EditError::NotFound(format!("文件不存在: {rel}"))
                } else {
                    EditError::Io(e)
                }
            })?;
            if bytes.len() as u64 > MAX_TEXT_BYTES {
                return Err(EditError::BadRequest("文件超过上限".into()));
            }
            String::from_utf8(bytes)
                .map_err(|_| EditError::BadRequest("文件不是 UTF-8 文本".into()))?
        }
    };
    if source.len() as u64 > MAX_TEXT_BYTES {
        return Err(EditError::BadRequest("文件超过上限".into()));
    }
    let mut blocks = body_blocks(&source, "");
    let truncated = blocks.len() > MAX_PREVIEW_BLOCKS;
    blocks.truncate(MAX_PREVIEW_BLOCKS);
    let book_id = Store::book_id(book_url);
    let from_dir = Path::new(&rel)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    for block in &mut blocks {
        let Block::Image { src, .. } = block else {
            continue;
        };
        let raw = src.trim();
        if raw.is_empty() || raw.contains("://") || raw.starts_with("data:") {
            continue;
        }
        if let Some(full) = resolve_inside(&opened.dir, &from_dir, raw) {
            if let Some(href) = rel_path(&opened.dir, &full) {
                *src = comic::local_image_url_v(&book_id, &href, opened.meta.revision);
            }
        }
    }
    Ok(PreviewOutcome {
        revision: opened.meta.revision,
        blocks,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_store::{ChapterItem, OpfMeta, Tier};
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oread-files-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn text_book(root: &Path) -> (Store, String, PathBuf) {
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let book_url = "https://example.com/book/files".to_string();
        let mut meta = OpfMeta::new(&book_url, "文件书");
        let item = ChapterItem {
            id: "ch-0001".into(),
            href: "chapters/ch-0001-kai-duan.xhtml".into(),
            title: "开端".into(),
            url: Some("https://example.com/book/files/1".into()),
            is_volume: false,
        };
        meta.chapters.push(item.clone());
        let dir = store.ensure_skeleton(Tier::Temp, &meta).await.unwrap();
        store
            .write_chapter(
                &dir,
                "zh",
                &item,
                &crate::library_store::text_blocks(&["第一段".into(), "第二段".into()]),
            )
            .await
            .unwrap();
        meta.bump();
        store.write_opf(&dir, &meta).await.unwrap();
        store.write_nav(&dir, &meta).await.unwrap();
        (store, book_url, dir)
    }

    fn text_of(read: ReadFile) -> String {
        match read.content {
            FileContent::Text(t) => t,
            FileContent::Image(_) => panic!("expected text file"),
        }
    }

    #[tokio::test]
    async fn files_round_trip_edit_preview_and_check() {
        let root = temp_root("round");
        let (store, book_url, _dir) = text_book(&root).await;
        let (meta, files) = list_files(&store, &book_url).await.unwrap();
        assert!(files.iter().any(|f| f.path == "OEBPS/content.opf"));
        assert!(files
            .iter()
            .any(|f| f.path == "OEBPS/chapters/ch-0001-kai-duan.xhtml"));

        let chapter = "OEBPS/chapters/ch-0001-kai-duan.xhtml";
        let (_, read) = read_file(&store, &book_url, chapter).await.unwrap();
        let text = text_of(read);
        assert!(text.contains("第一段"));

        let edited = text.replace("第一段", "改过的第一段");
        let out = write_text_file(&store, &book_url, chapter, meta.revision, &edited)
            .await
            .unwrap();
        assert_eq!(out.revision, meta.revision + 1);
        let (meta2, read) = read_file(&store, &book_url, chapter).await.unwrap();
        assert!(text_of(read).contains("改过的第一段"));
        assert_eq!(meta2.revision, out.revision);

        let preview = preview_file(
            &store,
            &book_url,
            chapter,
            Some("<html><body><p>预览</p><img src=\"../images/a.jpg\"/></body></html>"),
        )
        .await
        .unwrap();
        assert!(preview
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(t) if t == "预览")));
        assert!(preview
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Image { src, .. } if src.starts_with("oread-img://"))));

        let report = check_book(&store, &book_url).await.unwrap();
        assert!(
            report.issues.iter().all(|i| i.severity != "error"),
            "{:?}",
            report.issues
        );

        let err = write_text_file(&store, &book_url, "../x.xhtml", meta2.revision, "<x/>")
            .await
            .unwrap_err();
        assert!(matches!(err, EditError::BadRequest(_)));
        let err = write_text_file(&store, &book_url, chapter, meta2.revision, "<html>")
            .await
            .unwrap_err();
        assert!(matches!(err, EditError::BadRequest(_)));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn replace_dry_run_then_apply() {
        let root = temp_root("replace");
        let (store, book_url, _dir) = text_book(&root).await;
        let meta = store.open_book(&book_url).await.unwrap().unwrap().meta;
        let chapter = "OEBPS/chapters/ch-0001-kai-duan.xhtml";
        let opts = ReplaceOptions {
            query: "第一段",
            replacement: "新段落",
            regex: false,
            case_sensitive: true,
            paths: None,
            dry_run: true,
        };
        let dry = replace_all(&store, &book_url, meta.revision, &opts)
            .await
            .unwrap();
        assert!(!dry.applied);
        assert!(dry.total >= 1);
        assert!(dry.changed_files.iter().any(|p| p == chapter));
        let (_, read) = read_file(&store, &book_url, chapter).await.unwrap();
        assert!(text_of(read).contains("第一段"));

        let apply = ReplaceOptions {
            dry_run: false,
            ..opts
        };
        let applied = replace_all(&store, &book_url, meta.revision, &apply)
            .await
            .unwrap();
        assert!(applied.applied);
        assert_eq!(applied.revision, meta.revision + 1);
        let (_, read) = read_file(&store, &book_url, chapter).await.unwrap();
        assert!(text_of(read).contains("新段落"));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn check_detects_missing_chapter_file() {
        let root = temp_root("check");
        let (store, book_url, dir) = text_book(&root).await;
        tokio::fs::remove_file(dir.join("OEBPS/chapters/ch-0001-kai-duan.xhtml"))
            .await
            .unwrap();
        let report = check_book(&store, &book_url).await.unwrap();
        assert!(report.issues.iter().any(|i| {
            i.severity == "error"
                && i.path.as_deref() == Some("OEBPS/chapters/ch-0001-kai-duan.xhtml")
        }));
        std::fs::remove_dir_all(root).ok();
    }
}
