//! 漫画目录：**解压后的 CBZ + 一份 `oread.json` 目录边车**。
//!
//! 漫画本身保持 CBZ 的扁平布局（`0001-001.jpg`、`0001-002.jpg` …），这样
//! 「导出 = 原样打包」。但 CBZ 标准没有章节标题、章节 URL 与卷标记，因此我们在
//! 书根目录额外写一份 `oread.json`，只保存**目录语义**，不复制任何图片字节。
//!
//! `oread.json` 缺失时（例如早期迁移的目录、或用户手工放进来的 CBZ），按文件名
//! 前缀 `NNNN-MMM.ext` 反推章节，保证目录仍然可读；`ComicInfo.xml` 继续作为
//! 标准元数据文件存在，二者不互相替代。

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{atomic_write, safe_join};

/// 目录边车文件名。
pub const META_FILE: &str = "oread.json";
/// 标准漫画元数据文件。
pub const COMIC_INFO_FILE: &str = "ComicInfo.xml";

/// 本地图片 URL 的 scheme。图片代理见到它就直接读书目录，不发起任何网络请求。
pub const LOCAL_IMAGE_SCHEME: &str = "oread-img://";

/// 漫画书目录的语义投影（`oread.json`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComicMeta {
    #[serde(default = "meta_version")]
    pub version: u32,
    #[serde(default)]
    pub book_url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub pubdate: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub series_index: Option<f64>,
    #[serde(default)]
    pub isbn: Option<String>,
    /// 与文字书 OPF 的 `oread:revision` 同义：保存时递增，索引可据此判脏。
    #[serde(default)]
    pub revision: u64,
    /// 秒级 UTC 时间戳（与 OPF 的 `dcterms:modified` 同格式）。
    #[serde(default)]
    pub modified: String,
    #[serde(default)]
    pub chapters: Vec<ComicChapter>,
}

/// 漫画的一章。`id` 与文件名序号无关，只用于稳定引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComicChapter {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// 书源章节 URL；本地新建/无源章节为空。
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub is_volume: bool,
    #[serde(default)]
    pub pages: Vec<ComicPage>,
}

/// 漫画的一页。`href` 相对书根目录（CBZ 扁平布局）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComicPage {
    pub href: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub bytes: Option<u64>,
    /// 原始图片 URL，仅用于后续补图/换源排查，不影响阅读。
    #[serde(default)]
    pub source_url: Option<String>,
}

impl ComicMeta {
    pub fn new(book_url: &str, title: &str) -> Self {
        Self {
            version: meta_version(),
            book_url: book_url.to_string(),
            title: title.to_string(),
            author: None,
            language: None,
            publisher: None,
            pubdate: None,
            description: None,
            subjects: Vec::new(),
            series: None,
            series_index: None,
            isbn: None,
            revision: 0,
            modified: super::opf::now_modified(),
            chapters: Vec::new(),
        }
    }

    /// 保存前推进修订号与修改时间。
    pub fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.modified = super::opf::now_modified();
    }

    pub fn chapter_by_url(&self, url: &str) -> Option<&ComicChapter> {
        let url = url.trim();
        self.chapters
            .iter()
            .find(|c| c.url.as_deref().map(str::trim) == Some(url))
    }

    pub fn chapter_by_id(&self, id: &str) -> Option<&ComicChapter> {
        self.chapters.iter().find(|c| c.id == id)
    }
}

fn meta_version() -> u32 {
    1
}

/// 目录是否是漫画书：有标准 ComicInfo.xml 或我们的边车即可。
pub fn is_comic_dir(dir: &Path) -> bool {
    dir.join(COMIC_INFO_FILE).is_file() || dir.join(META_FILE).is_file()
}

/// 写出 `oread.json`。调用方负责先 [`ComicMeta::bump`]。
pub async fn write_meta(dir: &Path, meta: &ComicMeta) -> io::Result<()> {
    let text = serde_json::to_string_pretty(meta)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write(&dir.join(META_FILE), text.as_bytes()).await
}

/// 图片 MIME → 文件扩展名；未知按 jpg 处理（与迁移口径一致）。
pub fn ext_for_mime(mime: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.contains("png") {
        "png"
    } else if m.contains("webp") {
        "webp"
    } else if m.contains("gif") {
        "gif"
    } else if m.contains("avif") {
        "avif"
    } else {
        "jpg"
    }
}

/// 生成标准 `ComicInfo.xml`。`PageCount` 按当前页表合计，只写我们真正拥有的字段。
pub fn to_comic_info(meta: &ComicMeta) -> String {
    use std::fmt::Write as _;
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <ComicInfo xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
    );
    let mut field = |name: &str, value: Option<&str>| {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            let _ = writeln!(
                s,
                "  <{name}>{}</{name}>",
                boranlib_ebook::xml::xml_escape(v)
            );
        }
    };
    field("Title", Some(meta.title.as_str()));
    field("Writer", meta.author.as_deref());
    field("Series", meta.series.as_deref());
    field("Summary", meta.description.as_deref());
    field("LanguageISO", meta.language.as_deref());
    let pages: usize = meta.chapters.iter().map(|c| c.pages.len()).sum();
    let _ = writeln!(s, "  <PageCount>{pages}</PageCount>");
    s.push_str("</ComicInfo>\n");
    s
}

/// 写出 `ComicInfo.xml`。标准阅读器只看这个文件，因此每次目录/页表变化都刷新。
pub async fn write_comic_info(dir: &Path, meta: &ComicMeta) -> io::Result<()> {
    atomic_write(&dir.join(COMIC_INFO_FILE), to_comic_info(meta).as_bytes()).await
}

/// 读 `oread.json`；不存在时按 CBZ 文件名与 ComicInfo.xml 反推一份。
pub async fn read_meta(dir: &Path, book_url_hint: &str) -> io::Result<ComicMeta> {
    match tokio::fs::read_to_string(dir.join(META_FILE)).await {
        Ok(text) => {
            let mut meta =
                parse_meta(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if meta.book_url.trim().is_empty() {
                meta.book_url = book_url_hint.trim().to_string();
            }
            if meta.title.trim().is_empty() {
                meta.title = book_url_hint.trim().to_string();
            }
            Ok(meta)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => synth_meta(dir, book_url_hint).await,
        Err(e) => Err(e),
    }
}

pub fn parse_meta(text: &str) -> Result<ComicMeta, String> {
    serde_json::from_str(text).map_err(|e| format!("oread.json 不良构: {e}"))
}

/// 没有边车时，从 ComicInfo.xml 与文件名反推。
async fn synth_meta(dir: &Path, book_url: &str) -> io::Result<ComicMeta> {
    let info = read_comic_info(dir).await;
    let title = info
        .title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| book_url.to_string());
    let mut meta = ComicMeta::new(book_url, &title);
    meta.author = info.writer;
    meta.series = info.series;
    meta.description = info.summary;
    meta.language = info.language;

    let mut files = list_image_files(dir).await?;
    files.sort();
    if files.is_empty() {
        return Ok(meta);
    }

    // 标准命名 `0001-001.jpg`：前 4 位是章节序号。没有这个形状时整本算一章。
    let grouped = files.iter().any(|f| chapter_key(f).is_some());
    if grouped {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files {
            let key = chapter_key(&file).unwrap_or_else(|| "0001".to_string());
            groups.entry(key).or_default().push(file);
        }
        for (key, pages) in groups {
            let n: u32 = key.parse().unwrap_or(1);
            meta.chapters.push(ComicChapter {
                id: format!("ch-{n:04}"),
                title: format!("第 {n} 章"),
                url: None,
                is_volume: false,
                pages: pages.into_iter().map(page_of).collect(),
            });
        }
    } else {
        meta.chapters.push(ComicChapter {
            id: "ch-0001".into(),
            title: "第 1 章".into(),
            url: None,
            is_volume: false,
            pages: files.into_iter().map(page_of).collect(),
        });
    }
    Ok(meta)
}

fn chapter_key(file: &str) -> Option<String> {
    let stem = Path::new(file).file_stem()?.to_str()?;
    let (head, tail) = stem.split_once('-')?;
    // 迁移命名固定 `{章:04}-{页:03}`；超过 9999 章时前缀会自然变长，这里放宽到 6 位。
    if (4..=6).contains(&head.len()) && head.chars().all(|c| c.is_ascii_digit()) && !tail.is_empty()
    {
        Some(head.to_string())
    } else {
        None
    }
}

fn page_of(href: String) -> ComicPage {
    ComicPage {
        mime: page_mime(&href).to_string(),
        href,
        width: None,
        height: None,
        bytes: None,
        source_url: None,
    }
}

async fn list_image_files(dir: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if page_mime(&name) != "application/octet-stream" {
            out.push(name);
        }
    }
    Ok(out)
}

#[derive(Default)]
struct ComicInfo {
    title: Option<String>,
    writer: Option<String>,
    series: Option<String>,
    summary: Option<String>,
    language: Option<String>,
}

async fn read_comic_info(dir: &Path) -> ComicInfo {
    let Ok(xml) = tokio::fs::read_to_string(dir.join(COMIC_INFO_FILE)).await else {
        return ComicInfo::default();
    };
    parse_comic_info(&xml)
}

fn parse_comic_info(xml: &str) -> ComicInfo {
    let Ok(package) = sxd_document::parser::parse(&boranlib_ebook::xml::strip_doctype_and_bom(xml))
    else {
        return ComicInfo::default();
    };
    let doc = package.as_document();
    let Some(root) = doc
        .root()
        .children()
        .into_iter()
        .filter_map(|c| c.element())
        .find(|e| e.name().local_part() == "ComicInfo")
    else {
        return ComicInfo::default();
    };
    let mut info = ComicInfo::default();
    for child in root.children().into_iter().filter_map(|c| c.element()) {
        let text = text_of(child);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match child.name().local_part() {
            "Title" => info.title = Some(text.to_string()),
            "Writer" => info.writer = Some(text.to_string()),
            "Series" => info.series = Some(text.to_string()),
            "Summary" => info.summary = Some(text.to_string()),
            "LanguageISO" => info.language = Some(text.to_string()),
            _ => {}
        }
    }
    info
}

fn text_of(el: sxd_document::dom::Element<'_>) -> String {
    use sxd_document::dom::ChildOfElement;
    let mut out = String::new();
    for child in el.children() {
        match child {
            ChildOfElement::Text(t) => out.push_str(t.text()),
            ChildOfElement::Element(e) => out.push_str(&text_of(e)),
            _ => {}
        }
    }
    out
}

/// 读一页原始字节；路径相对书根目录，越界/绝对路径一律拒绝。
pub async fn read_page(dir: &Path, href: &str) -> io::Result<Option<Vec<u8>>> {
    let path = safe_join(dir, href)?;
    match tokio::fs::read(&path).await {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 按扩展名给出 MIME；未知扩展返回 `application/octet-stream`。
pub fn page_mime(href: &str) -> &'static str {
    let ext = Path::new(href)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// 只读图片头解析宽高；失败返回 `None`（不阻塞阅读）。
pub fn page_dims(path: &Path) -> Option<(u32, u32)> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// 在阻塞线程里解析宽高，避免图片头读取卡住 async runtime。
pub async fn page_dims_async(path: PathBuf) -> Option<(u32, u32)> {
    tokio::task::spawn_blocking(move || page_dims(&path))
        .await
        .ok()
        .flatten()
}

/// 拼一个本地图片 URL。`book_id` 是书目录名（sha256 十六进制）。
pub fn local_image_url(book_id: &str, href: &str) -> String {
    format!(
        "{LOCAL_IMAGE_SCHEME}{book_id}/{}",
        super::path_to_href(href)
    )
}

/// 带修订号的本地图片 URL。修订号变化后 URL 变化，图片缓存与 ETag 自动失效，
/// 换页/换封面后不会继续拿到旧字节。
pub fn local_image_url_v(book_id: &str, href: &str, revision: u64) -> String {
    format!(
        "{LOCAL_IMAGE_SCHEME}{book_id}/{}?v={revision}",
        super::path_to_href(href)
    )
}

/// 解析本地图片 URL → `(book_id, href)`；不是本地 URL 返回 `None`。
/// 忽略 `?v=` 之类的查询串，href 永远是目录内、已解码的相对路径。
pub fn parse_local_image_url(url: &str) -> Option<(String, String)> {
    let rest = url.trim().strip_prefix(LOCAL_IMAGE_SCHEME)?;
    let rest = rest.split(['?', '#']).next()?;
    let (id, href) = rest.split_once('/')?;
    if id.is_empty()
        || href.is_empty()
        || !id.chars().all(|c| c.is_ascii_hexdigit())
        || href.contains("..")
        || href.starts_with('/')
    {
        return None;
    }
    Some((id.to_string(), super::href_to_path(href)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_url_round_trip() {
        let url = local_image_url("abc123", "0001-001.jpg");
        assert_eq!(
            parse_local_image_url(&url),
            Some(("abc123".into(), "0001-001.jpg".into()))
        );
        let versioned = local_image_url_v("abc123", "0001-001.jpg", 7);
        assert_eq!(
            parse_local_image_url(&versioned),
            Some(("abc123".into(), "0001-001.jpg".into())),
            "查询串只用于缓存失效，不进入文件路径"
        );
        let spaced = local_image_url_v("abc123", "OEBPS/Text/a b.png", 1);
        assert!(spaced.contains("a%20b.png"));
        assert_eq!(
            parse_local_image_url(&spaced),
            Some(("abc123".into(), "OEBPS/Text/a b.png".into()))
        );
        assert!(parse_local_image_url("https://x/a.jpg").is_none());
        assert!(parse_local_image_url("oread-img://abc/../x.jpg").is_none());
        assert!(parse_local_image_url("oread-img://abc").is_none());
    }

    #[test]
    fn parses_comic_info_fields() {
        let xml = r#"<?xml version="1.0"?><ComicInfo><Title>书名</Title><Writer>作者</Writer>
            <Series>系列</Series><Summary>简介</Summary><LanguageISO>zh</LanguageISO></ComicInfo>"#;
        let info = parse_comic_info(xml);
        assert_eq!(info.title.as_deref(), Some("书名"));
        assert_eq!(info.writer.as_deref(), Some("作者"));
        assert_eq!(info.series.as_deref(), Some("系列"));
        assert_eq!(info.summary.as_deref(), Some("简介"));
        assert_eq!(info.language.as_deref(), Some("zh"));
    }

    #[test]
    fn meta_json_round_trip() {
        let mut meta = ComicMeta::new("https://x/book", "漫画");
        meta.chapters.push(ComicChapter {
            id: "ch-0001".into(),
            title: "第一章".into(),
            url: Some("https://x/c1".into()),
            is_volume: false,
            pages: vec![ComicPage {
                href: "0001-001.jpg".into(),
                mime: "image/jpeg".into(),
                width: Some(1200),
                height: Some(1800),
                bytes: Some(123),
                source_url: Some("https://cdn/1.jpg".into()),
            }],
        });
        let text = serde_json::to_string(&meta).unwrap();
        let back = parse_meta(&text).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn chapter_key_requires_standard_prefix() {
        assert_eq!(chapter_key("0001-001.jpg"), Some("0001".into()));
        assert_eq!(chapter_key("001-001.jpg"), None);
        assert_eq!(chapter_key("cover.jpg"), None);
    }

    #[tokio::test]
    async fn synthesizes_toc_for_plain_cbz_dir() {
        let dir = std::env::temp_dir().join(format!(
            "oread-comic-synth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(COMIC_INFO_FILE),
            "<ComicInfo><Title>手工漫画</Title><Writer>作者</Writer></ComicInfo>",
        )
        .unwrap();
        for name in ["0001-001.jpg", "0001-002.png", "0002-001.webp"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let meta = read_meta(&dir, "https://x/book").await.unwrap();
        assert_eq!(meta.title, "手工漫画");
        assert_eq!(meta.author.as_deref(), Some("作者"));
        assert_eq!(meta.book_url, "https://x/book");
        assert_eq!(meta.chapters.len(), 2);
        assert_eq!(meta.chapters[0].title, "第 1 章");
        assert_eq!(meta.chapters[0].pages.len(), 2);
        assert_eq!(meta.chapters[1].pages[0].href, "0002-001.webp");
        assert_eq!(meta.chapters[1].pages[0].mime, "image/webp");

        let _ = std::fs::remove_dir_all(dir);
    }
}
