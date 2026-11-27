//! 本地 CBZ 导入：把 ZIP 里的图片安全解压成「漫画目录」，再走与书源漫画相同的读路径。
//!
//! 与 EPUB 导入一样是两步：`stage` 只解压到 `.import/<token>/book`，`commit` 才搬进馆藏。
//! 漫画目录保持 CBZ 的扁平布局（`0001-001.jpg` …），并写一份 `oread.json` 与
//! `ComicInfo.xml`；图片本身不复制第二份，导出时原样打包即可。
//!
//! 安全边界复用 EPUB 导入：条目名、加密、压缩比、总大小都在解压前检查；图片还会
//! 验证文件头，避免把伪装成 `.jpg` 的可执行文件放进馆藏。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::import::{
    self, CommitMode, CommitResult, ImportError, StageInfo, MAX_ENTRIES, MAX_ENTRY_BYTES,
    MAX_RATIO, MAX_RATIO_MIN_BYTES, MAX_TOTAL_BYTES,
};
use super::{comic, index, usage, Store, Tier};
use crate::library;

fn bad(msg: impl Into<String>) -> ImportError {
    ImportError::BadRequest(msg.into())
}

fn zip_err(e: zip::result::ZipError) -> ImportError {
    bad(format!("CBZ 不是有效的 ZIP：{e}"))
}

/// 解压并校验上传的 CBZ，返回可供前端展示与 commit 的摘要。
pub async fn stage(
    store: &Store,
    token: &str,
    source: PathBuf,
    file_name: &str,
    sha256: String,
) -> Result<StageInfo, ImportError> {
    import::validate_token(token)?;
    let stage_dir = store.import_stage_dir(token);
    let book_dir = stage_dir.join("book");
    let book_url = import::import_book_url(&sha256);
    let name = import::sanitize_file_name_with(file_name, "import.cbz");

    let source_for_task = source.clone();
    let book_url_for_task = book_url.clone();
    let name_for_task = name.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        extract_cbz(
            &source_for_task,
            &book_dir,
            &book_url_for_task,
            &name_for_task,
        )
    })
    .await
    .map_err(|e| ImportError::Internal(format!("解压任务失败：{e}")))??;

    let page_count: usize = outcome.meta.chapters.iter().map(|c| c.pages.len()).sum();
    let info = StageInfo {
        kind: "comic".into(),
        book_url,
        sha256,
        content_fingerprint: Some(outcome.fingerprint),
        file_name: name,
        title: outcome.meta.title.clone(),
        author: outcome.meta.author.clone(),
        language: outcome.meta.language.clone().unwrap_or_default(),
        publisher: outcome.meta.publisher.clone(),
        pubdate: outcome.meta.pubdate.clone(),
        chapter_count: outcome
            .meta
            .chapters
            .iter()
            .filter(|c| !c.is_volume)
            .count(),
        image_count: page_count,
        page_count,
        // 漫画没有独立封面文件；第一页在阅读器里就是封面。
        has_cover: false,
        warnings: outcome.warnings,
    };
    let raw = serde_json::to_vec_pretty(&info)
        .map_err(|e| ImportError::Internal(format!("序列化暂存信息失败：{e}")))?;
    tokio::fs::write(stage_dir.join("info.json"), raw).await?;
    let _ = tokio::fs::remove_file(&source).await;
    Ok(info)
}

/// 把暂存区的漫画目录搬进馆藏，并写入 `library_books`。
pub async fn commit(
    store: &Store,
    pool: &sqlx::SqlitePool,
    token: &str,
    mode: CommitMode,
    target_book_url: Option<&str>,
) -> Result<CommitResult, ImportError> {
    import::validate_token(token)?;
    if mode == CommitMode::Cancel {
        import::discard_stage(store, token).await;
        return Ok(CommitResult {
            book_url: String::new(),
            created: false,
            overwritten: false,
            cancelled: true,
            check: None,
        });
    }
    let info = import::read_stage_info(store, token).await?;
    if info.kind != "comic" {
        return Err(bad("暂存区不是 CBZ 导入会话"));
    }

    let stage_dir = store.import_stage_dir(token);
    let book_dir = stage_dir.join("book");
    if !book_dir.join(comic::META_FILE).is_file() {
        return Err(bad("导入会话已过期，请重新上传 CBZ"));
    }
    let mut meta = comic::read_meta(&book_dir, &info.book_url)
        .await
        .map_err(|e| ImportError::Internal(format!("读取暂存漫画目录失败：{e}")))?;

    let final_url = match mode {
        CommitMode::New => import::unique_book_url(store, pool, &info.book_url).await?,
        CommitMode::Overwrite => {
            let target = target_book_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("覆盖导入需要指定 target_book_url"))?;
            let Some(existing) = library::load_book(pool, target).await? else {
                return Err(bad("要覆盖的书不存在"));
            };
            if existing.tier != Tier::Collection.as_str() {
                return Err(bad("只能覆盖馆藏书籍；临时缓存请先加入馆藏"));
            }
            if existing.book_source_type != 2 {
                return Err(bad("目标书不是漫画，不能用 CBZ 覆盖"));
            }
            target.to_string()
        }
        CommitMode::Cancel => unreachable!(),
    };

    let _guard = super::lock_book(&final_url).await;
    let dest = store.dir_for(Tier::Collection, &final_url);
    let old_revision = if dest.join(comic::META_FILE).is_file() {
        comic::read_meta(&dest, &final_url)
            .await
            .map(|m| m.revision)
            .unwrap_or(0)
    } else {
        0
    };

    meta.book_url = final_url.clone();
    meta.revision = old_revision.saturating_add(1);
    meta.modified = super::opf::now_modified();
    comic::write_meta(&book_dir, &meta)
        .await
        .map_err(|e| ImportError::Internal(format!("写入 oread.json 失败：{e}")))?;
    comic::write_comic_info(&book_dir, &meta)
        .await
        .map_err(|e| ImportError::Internal(format!("写入 ComicInfo.xml 失败：{e}")))?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let overwritten = dest.exists();
    let aside = overwritten.then(|| store.trash_target(&final_url, "import-overwrite"));
    if let Some(aside) = aside.as_deref() {
        tokio::fs::create_dir_all(store.root().join(super::TRASH_DIR)).await?;
        super::rename_retry(&dest, aside)
            .await
            .map_err(|e| ImportError::Internal(format!("旧漫画目录移入回收站失败：{e}")))?;
    }
    if let Err(e) = super::rename_retry(&book_dir, &dest).await {
        if let Some(aside) = aside.as_deref() {
            let _ = super::rename_retry(aside, &dest).await;
        }
        return Err(ImportError::Internal(format!("搬入馆藏失败：{e}")));
    }

    upsert_library_comic(pool, &meta, &info).await?;
    index::sync_book(store, &final_url).await;
    let _ = usage::refresh_rollup(pool, store, &final_url).await;
    import::discard_stage(store, token).await;
    Ok(CommitResult {
        book_url: final_url,
        created: !overwritten,
        overwritten,
        cancelled: false,
        check: None,
    })
}

async fn upsert_library_comic(
    pool: &sqlx::SqlitePool,
    meta: &comic::ComicMeta,
    info: &StageInfo,
) -> Result<(), ImportError> {
    let now = chrono::Utc::now().timestamp_millis();
    let chapter_count = meta.chapters.iter().filter(|c| !c.is_volume).count() as i64;
    sqlx::query(
        "INSERT INTO library_books (
            book_url, name, author, kind, cover_url, intro, language, publisher, pubdate,
            series_name, series_index, book_source_type, total_chapter_num, tier,
            collected_at, last_accessed_at, checked_at, file_sha256, content_fingerprint,
            import_filename, origin_name
         ) VALUES (?1,?2,?3,'comic',NULL,?4,?5,?6,?7,?8,?9,2,?10,'collection',?11,?11,?11,?12,?13,?14,?15)
         ON CONFLICT(book_url) DO UPDATE SET
            name = excluded.name,
            author = excluded.author,
            kind = 'comic',
            cover_url = excluded.cover_url,
            intro = excluded.intro,
            language = excluded.language,
            publisher = excluded.publisher,
            pubdate = excluded.pubdate,
            series_name = excluded.series_name,
            series_index = excluded.series_index,
            book_source_type = 2,
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
    .bind("本地导入")
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 解压（同步，跑在 spawn_blocking 里）
// ---------------------------------------------------------------------------

struct Entry {
    /// 归一化后的 ZIP 内路径，用于自然排序与分组。
    name: String,
    index: usize,
}

struct ExtractedComic {
    meta: comic::ComicMeta,
    warnings: Vec<String>,
    fingerprint: String,
}

fn extract_cbz(
    source: &Path,
    book_dir: &Path,
    book_url: &str,
    file_name: &str,
) -> Result<ExtractedComic, ImportError> {
    let file = fs::File::open(source)?;
    let mut zip = zip::ZipArchive::new(file).map_err(zip_err)?;
    if zip.len() > MAX_ENTRIES {
        return Err(bad(format!(
            "CBZ 条目过多（{}，上限 {MAX_ENTRIES}）",
            zip.len()
        )));
    }

    let mut entries: Vec<Entry> = Vec::with_capacity(zip.len());
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0u64;
    let mut warnings = Vec::new();
    let mut skipped = 0usize;
    let mut skipped_samples: Vec<String> = Vec::new();
    let mut comic_info: Option<String> = None;

    for i in 0..zip.len() {
        // 先取出元数据并释放 ZIP 条目的借用，后面读 ComicInfo / 图片时才能再次借 `zip`。
        let (raw_name, size, compressed) = {
            let e = zip.by_index(i).map_err(zip_err)?;
            if e.is_dir() {
                continue;
            }
            let raw_name = e
                .name()
                .map_err(|err| bad(format!("ZIP 条目名无法解码：{err}")))?
                .to_string();
            if e.encrypted() {
                return Err(bad(format!("CBZ 含加密条目，暂不支持：{raw_name}")));
            }
            (raw_name, e.size(), e.compressed_size())
        };
        import::validate_entry_name(&raw_name)?;
        let name = import::normalize_zip_path(&raw_name);
        if name.is_empty() || import::is_junk_entry(&name) || is_hidden_junk(&name) {
            continue;
        }
        let key = name.to_ascii_lowercase();
        if !seen.insert(key) {
            return Err(bad(format!("CBZ 含重复条目（大小写不敏感）：{name}")));
        }

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
            return Err(bad("CBZ 解压后总大小超过 2 GiB 上限"));
        }

        let base = name.rsplit('/').next().unwrap_or(name.as_str());
        if base.eq_ignore_ascii_case(comic::COMIC_INFO_FILE) {
            if comic_info.is_some() {
                warnings.push(format!("CBZ 含多份 ComicInfo.xml，忽略：{name}"));
                continue;
            }
            let data = read_entry(&mut zip, i)?;
            comic_info = Some(
                decode_xml(&data)
                    .ok_or_else(|| bad(format!("ComicInfo.xml 不是有效文本：{name}")))?,
            );
            continue;
        }
        if !is_supported_image_name(base) {
            skipped += 1;
            if skipped_samples.len() < 5 {
                skipped_samples.push(name);
            }
            continue;
        }
        entries.push(Entry { name, index: i });
    }

    if entries.is_empty() {
        return Err(bad("CBZ 中没有 JPEG / PNG / WebP / GIF 图片"));
    }
    entries.sort_by(|a, b| natural_cmp(&a.name, &b.name).then_with(|| a.name.cmp(&b.name)));

    // 同一目录的图片算一章：单层 CBZ 通常只有一章；带 `Ch01/`、`Ch02/` 的按目录分章。
    let mut groups: Vec<(String, Vec<&Entry>)> = Vec::new();
    for entry in &entries {
        let dir = parent_dir(&entry.name);
        match groups.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, list)) => list.push(entry),
            None => groups.push((dir, vec![entry])),
        }
    }
    // 根目录固定算第 1 章，其余目录按自然序排；否则根目录的封面图可能被排到章节后面。
    groups.sort_by(|(a, _), (b, _)| match (a.is_empty(), b.is_empty()) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => natural_cmp(a, b).then_with(|| a.cmp(b)),
    });

    fs::create_dir_all(book_dir)?;
    let title = title_from_file_name(file_name);
    let mut meta = comic::ComicMeta::new(book_url, &title);
    if let Some(xml) = comic_info.as_deref() {
        let parsed = comic::parse_comic_info(xml);
        if let Some(title) = parsed.title.filter(|v| !v.trim().is_empty()) {
            meta.title = title;
        }
        meta.author = parsed.writer;
        meta.series = parsed.series;
        meta.description = parsed.summary;
        meta.language = parsed.language;
        meta.publisher = parsed.publisher;
        meta.pubdate = parsed.pubdate;
    } else {
        warnings.push("缺少 ComicInfo.xml，标题按文件名生成".into());
    }
    if skipped > 0 {
        let mut msg = format!("忽略 {skipped} 个非图片文件");
        if !skipped_samples.is_empty() {
            msg.push_str("（如 ");
            msg.push_str(&skipped_samples.join("、"));
            msg.push('）');
        }
        warnings.push(msg);
    }

    let mut hasher = Sha256::new();
    let mut dim_failures = 0usize;
    let mut skipped_images = 0usize;
    let mut skipped_image_samples: Vec<String> = Vec::new();
    for (gi, (dir, pages)) in groups.iter().enumerate() {
        let chapter_no = gi + 1;
        let title = if groups.len() == 1 {
            "第 1 章".to_string()
        } else if dir.is_empty() {
            format!("第 {chapter_no} 章")
        } else {
            dir.rsplit('/').next().unwrap_or(dir).to_string()
        };
        let mut chapter = comic::ComicChapter {
            id: format!("ch-{chapter_no:04}"),
            title,
            url: None,
            is_volume: false,
            pages: Vec::with_capacity(pages.len()),
        };
        for entry in pages.iter() {
            let data = read_entry(&mut zip, entry.index)?;
            let ext = match image::guess_format(&data).ok().and_then(image_ext) {
                Some(ext) => ext,
                None => {
                    skipped_images += 1;
                    if skipped_image_samples.len() < 5 {
                        skipped_image_samples.push(entry.name.clone());
                    }
                    continue;
                }
            };
            let (width, height) = match image_dims(&data) {
                Some(v) => (Some(v.0), Some(v.1)),
                None => {
                    dim_failures += 1;
                    (None, None)
                }
            };
            // 页码按「实际写入的页」递增，跳过坏图后不会留下空洞。
            let href = format!("{chapter_no:04}-{:03}.{ext}", chapter.pages.len() + 1);
            fs::write(book_dir.join(&href), &data)?;
            hasher.update(ext.as_bytes());
            hasher.update(&data);
            chapter.pages.push(comic::ComicPage {
                mime: comic::page_mime(&href).to_string(),
                href,
                width,
                height,
                bytes: Some(data.len() as u64),
                source_url: None,
            });
        }
        if chapter.pages.is_empty() {
            continue;
        }
        meta.chapters.push(chapter);
    }
    if meta.chapters.is_empty() {
        return Err(bad("CBZ 中没有可读取的 JPEG / PNG / WebP / GIF 图片"));
    }
    if skipped_images > 0 {
        let mut msg = format!("跳过 {skipped_images} 个无法识别或格式不支持的图片");
        if !skipped_image_samples.is_empty() {
            msg.push_str("（如 ");
            msg.push_str(&skipped_image_samples.join("、"));
            msg.push('）');
        }
        warnings.push(msg);
    }
    if dim_failures > 0 {
        warnings.push(format!(
            "{dim_failures} 张图片无法解析宽高，阅读时再尝试补齐"
        ));
    }

    let json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| ImportError::Internal(format!("序列化 oread.json 失败：{e}")))?;
    fs::write(book_dir.join(comic::META_FILE), json)?;
    fs::write(
        book_dir.join(comic::COMIC_INFO_FILE),
        comic::to_comic_info(&meta),
    )?;

    Ok(ExtractedComic {
        meta,
        warnings,
        fingerprint: format!("{:x}", hasher.finalize()),
    })
}

fn read_entry<R: Read + io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    index: usize,
) -> Result<Vec<u8>, ImportError> {
    let e = zip.by_index(index).map_err(zip_err)?;
    import::read_limited(e, MAX_ENTRY_BYTES)
}

fn is_hidden_junk(name: &str) -> bool {
    name.split('/').any(|seg| seg.starts_with('.'))
}

fn is_supported_image_name(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif")
}

fn image_ext(fmt: image::ImageFormat) -> Option<&'static str> {
    match fmt {
        image::ImageFormat::Jpeg => Some("jpg"),
        image::ImageFormat::Png => Some("png"),
        image::ImageFormat::WebP => Some("webp"),
        image::ImageFormat::Gif => Some("gif"),
        _ => None,
    }
}

fn image_dims(data: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

fn title_from_file_name(file_name: &str) -> String {
    let base = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .trim();
    // 用 `get` 避免在非字符边界切片：中文文件名没有 `.cbz` 后缀时不应 panic。
    let stem = base
        .get(base.len().saturating_sub(4)..)
        .filter(|tail| tail.eq_ignore_ascii_case(".cbz"))
        .map(|_| &base[..base.len() - 4])
        .unwrap_or(base)
        .trim();
    if stem.is_empty() {
        "未命名漫画".into()
    } else {
        stem.to_string()
    }
}

/// 自然排序：`2.jpg` 排在 `10.jpg` 前，避免把页序按字典序打乱。
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let va = na.trim_start_matches('0');
                let vb = nb.trim_start_matches('0');
                let ord = va.len().cmp(&vb.len()).then_with(|| va.cmp(vb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let lx = x.to_ascii_lowercase();
                let ly = y.to_ascii_lowercase();
                if lx != ly {
                    return lx.cmp(&ly);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// ComicInfo.xml 可能是 UTF-16（Windows 工具常见）；UTF-8 失败时按 BOM 解一次。
fn decode_xml(data: &[u8]) -> Option<String> {
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    match String::from_utf8(data.to_vec()) {
        Ok(text) => Some(text),
        Err(e) => Some(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oread-cbz-import-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn write_cbz(path: &Path, files: &[(&str, Vec<u8>)]) {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn natural_sort_keeps_page_order() {
        let mut names = vec!["10.jpg", "2.jpg", "1.jpg", "001.jpg"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["1.jpg", "001.jpg", "2.jpg", "10.jpg"]);
        assert_eq!(natural_cmp("Ch2/p1.jpg", "Ch10/p1.jpg"), Ordering::Less);
    }

    #[test]
    fn titles_fall_back_to_file_name() {
        assert_eq!(title_from_file_name("My Comic.cbz"), "My Comic");
        assert_eq!(title_from_file_name("a/b/CBZ.CBZ"), "CBZ");
        assert_eq!(title_from_file_name(".cbz"), "未命名漫画");
        // 多字节文件名没有扩展名时不能按字节切片 panic。
        assert_eq!(title_from_file_name("漫画"), "漫画");
    }

    #[tokio::test]
    async fn extracts_flat_cbz_into_comic_directory() {
        let root = temp_dir("flat");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let stage_dir = store.import_stage_dir(token);
        fs::create_dir_all(&stage_dir).unwrap();
        let source = stage_dir.join("source.cbz");
        write_cbz(
            &source,
            &[
                ("cover.png", png_bytes(7, 11)),
                ("Ch1/01.png", png_bytes(8, 12)),
                ("Ch1/02.png", png_bytes(9, 13)),
                ("Ch2/01.png", png_bytes(10, 14)),
                ("Ch2/broken.jpg", b"not an image".to_vec()),
                (
                    "ComicInfo.xml",
                    r#"<ComicInfo><Title>测试漫画</Title><Writer>画师</Writer>
                        <Publisher>出版社</Publisher><Year>2024</Year><Month>3</Month></ComicInfo>"#
                        .as_bytes()
                        .to_vec(),
                ),
                ("notes.txt", b"skip me".to_vec()),
            ],
        );

        let info = stage(
            &store,
            token,
            source.clone(),
            "My Comic.cbz",
            "a".repeat(64),
        )
        .await
        .unwrap();
        assert_eq!(info.kind, "comic");
        assert_eq!(info.title, "测试漫画");
        assert_eq!(info.author.as_deref(), Some("画师"));
        assert_eq!(info.publisher.as_deref(), Some("出版社"));
        assert_eq!(info.pubdate.as_deref(), Some("2024-3"));
        assert_eq!(info.chapter_count, 3);
        assert_eq!(info.page_count, 4);
        assert_eq!(info.image_count, 4);
        assert!(info.warnings.iter().any(|w| w.contains("notes.txt")));
        assert!(info.warnings.iter().any(|w| w.contains("broken.jpg")));

        let book_dir = stage_dir.join("book");
        for name in [
            "0001-001.png",
            "0002-001.png",
            "0002-002.png",
            "0003-001.png",
        ] {
            assert!(book_dir.join(name).is_file(), "缺少 {name}");
        }
        let meta = comic::read_meta(&book_dir, &info.book_url).await.unwrap();
        assert_eq!(meta.title, "测试漫画");
        assert_eq!(meta.chapters.len(), 3);
        // 根目录固定第 1 章，章节目录按自然序排在后面。
        assert_eq!(meta.chapters[0].title, "第 1 章");
        assert_eq!(meta.chapters[0].pages[0].width, Some(7));
        assert_eq!(meta.chapters[1].title, "Ch1");
        assert_eq!(meta.chapters[1].pages[0].width, Some(8));
        assert_eq!(meta.chapters[2].title, "Ch2");
        assert_eq!(meta.chapters[2].pages[0].width, Some(10));
        assert!(book_dir.join(comic::COMIC_INFO_FILE).is_file());
        assert!(!source.exists(), "解压后应删掉上传包");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rejects_cbz_without_images() {
        let root = temp_dir("empty");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let token = "fedcba9876543210fedcba9876543210";
        let stage_dir = store.import_stage_dir(token);
        fs::create_dir_all(&stage_dir).unwrap();
        let source = stage_dir.join("source.cbz");
        write_cbz(&source, &[("readme.txt", b"no images".to_vec())]);
        let err = stage(&store, token, source, "empty.cbz", "b".repeat(64))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("没有 JPEG"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let root = temp_dir("traversal");
        let store = Store::at(root.join("library"));
        store.create_layout().unwrap();
        let token = "00112233445566778899aabbccddeeff";
        let stage_dir = store.import_stage_dir(token);
        fs::create_dir_all(&stage_dir).unwrap();
        let source = stage_dir.join("source.cbz");
        write_cbz(&source, &[("../evil.png", png_bytes(2, 2))]);
        let err = stage(&store, token, source, "evil.cbz", "c".repeat(64))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("路径穿越"), "{err}");
        let _ = fs::remove_dir_all(root);
    }
}
