//! `content.opf` / `nav.xhtml` 的读写。
//!
//! 规则：**OPF 是书的元数据权威**。索引表只是它的物化视图，删了能重建。
//! 因此这里必须满足两条硬约束，并由 round-trip 测试守着：
//!
//! 1. 我们认识的每个字段都 1:1 写进 OPF，重建时能原样读回；
//! 2. 只写 EPUB 3 标准字段 + `oread:*` 自有扩展，不用 calibre 私有元数据。
//!
//! 章节顺序只存在于 `spine`；章节文件本身永不改名，所以插入新章不需要
//! 重写任何 href——这是把「改名导致链接全断」从源头掐掉。

use std::fmt::Write as _;

use chrono::Utc;
use sxd_document::dom::{ChildOfElement, Element};
use sxd_document::parser;

use boranlib_ebook::xml::xml_escape;

use super::{href_to_path, path_to_href};

/// 我们写进 OPF 的正文语言缺省值。
pub const DEFAULT_LANGUAGE: &str = "zh";

/// 一章节在书里的登记信息。
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterItem {
    /// manifest id，同时也是文件名前缀（`ch-0001` / `vol-0003`）。
    pub id: String,
    /// 相对 `OEBPS/` 的路径，例如 `chapters/ch-0001-qi-dian.xhtml`。
    pub href: String,
    pub title: String,
    /// 书源章节 URL。写进 OPF 的 `oread:chapter-url`，用于重建索引。
    pub url: Option<String>,
    /// 卷标题：阅读器渲染为分组标题，不生成正文文件之外的东西。
    pub is_volume: bool,
}

/// 封面登记信息。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverItem {
    pub href: String,
    pub media_type: String,
}

/// 正文插图登记信息（`OEBPS/images/` 下的原图）。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageItem {
    /// manifest id，例如 `img-1a2b3c4d5e6f7788`。
    pub id: String,
    /// 相对 `OEBPS/` 的路径，例如 `images/<sha256>.jpg`。
    pub href: String,
    pub media_type: String,
}

/// 不在 spine、也不是正文插图的 manifest 项：样式表、字体、NCX 等。
///
/// 导入的外部 EPUB 会把这些文件原样保留在书目录里，OPF 也必须继续登记它们，
/// 否则管理员后续保存元数据时会把 CSS/字体从 manifest 里抹掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceItem {
    pub id: String,
    pub href: String,
    pub media_type: String,
    /// 原样保留的 `properties`（`nav` 除外，导航由我们重新生成）。
    pub properties: String,
}

/// 书的元数据（OPF 的内存投影）。
#[derive(Debug, Clone, PartialEq)]
pub struct OpfMeta {
    pub book_url: String,
    pub title: String,
    pub language: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    /// 出版日期原样保存（`YYYY-MM-DD` 或书源给的字符串）。
    pub pubdate: Option<String>,
    pub description: Option<String>,
    pub subjects: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub isbn: Option<String>,
    /// 自增修订号：判断索引缓存是否过期、以及乐观锁。
    pub revision: u64,
    /// EPUB 3 必填的 `dcterms:modified`（秒级 UTC）。
    pub modified: String,
    pub cover: Option<CoverItem>,
    pub chapters: Vec<ChapterItem>,
    /// 正文插图；顺序即首次写入顺序，不参与 spine。
    pub images: Vec<ImageItem>,
    /// 其余 manifest 项（CSS / 字体 / NCX 等），原样登记、原样保留。
    pub resources: Vec<ResourceItem>,
}

impl OpfMeta {
    pub fn new(book_url: &str, title: &str) -> Self {
        Self {
            book_url: book_url.to_string(),
            title: title.to_string(),
            language: DEFAULT_LANGUAGE.to_string(),
            author: None,
            publisher: None,
            pubdate: None,
            description: None,
            subjects: Vec::new(),
            series: None,
            series_index: None,
            isbn: None,
            revision: 0,
            modified: now_modified(),
            cover: None,
            chapters: Vec::new(),
            images: Vec::new(),
            resources: Vec::new(),
        }
    }

    /// 保存前推进修订号与修改时间；EPUB 3 要求每次保存刷新 `dcterms:modified`。
    pub fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.modified = now_modified();
    }
}

/// 秒级 UTC 时间戳，EPUB 3 规定的 `CCYY-MM-DDThh:mm:ssZ`。
pub fn now_modified() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ---------------------------------------------------------------------------
// 写出
// ---------------------------------------------------------------------------

/// 生成 `content.opf`。`chapters` 的顺序就是 spine 顺序。
pub fn to_opf_xml(meta: &OpfMeta) -> String {
    let mut s = String::with_capacity(2048);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(
        "<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" \
                unique-identifier=\"book-url\">\n",
    );
    s.push_str("  <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n");
    let lang = escape_attr(&meta.language);
    let _ = writeln!(s, "    <dc:language>{lang}</dc:language>");
    let _ = writeln!(
        s,
        "    <dc:identifier id=\"book-url\">{}</dc:identifier>",
        xml_escape(&meta.book_url)
    );
    let _ = writeln!(s, "    <dc:title>{}</dc:title>", xml_escape(&meta.title));
    if let Some(v) = non_empty_opt(&meta.author) {
        let _ = writeln!(s, "    <dc:creator>{}</dc:creator>", xml_escape(v));
    }
    if let Some(v) = non_empty_opt(&meta.publisher) {
        let _ = writeln!(s, "    <dc:publisher>{}</dc:publisher>", xml_escape(v));
    }
    if let Some(v) = non_empty_opt(&meta.pubdate) {
        let _ = writeln!(s, "    <dc:date>{}</dc:date>", xml_escape(v));
    }
    if let Some(v) = non_empty_opt(&meta.description) {
        let _ = writeln!(s, "    <dc:description>{}</dc:description>", xml_escape(v));
    }
    for subject in &meta.subjects {
        if !subject.trim().is_empty() {
            let _ = writeln!(
                s,
                "    <dc:subject>{}</dc:subject>",
                xml_escape(subject.trim())
            );
        }
    }
    if let Some(isbn) = non_empty_opt(&meta.isbn) {
        let _ = writeln!(
            s,
            "    <dc:identifier id=\"isbn\">{}</dc:identifier>",
            xml_escape(isbn)
        );
    }

    let _ = writeln!(
        s,
        "    <meta property=\"dcterms:modified\">{}</meta>",
        xml_escape(&meta.modified)
    );
    let _ = writeln!(
        s,
        "    <meta property=\"oread:revision\">{}</meta>",
        meta.revision
    );

    if let Some(series) = non_empty_opt(&meta.series) {
        let _ = writeln!(
            s,
            "    <meta property=\"belongs-to-collection\" id=\"series\">{}</meta>",
            xml_escape(series)
        );
        s.push_str("    <meta refines=\"#series\" property=\"collection-type\">series</meta>\n");
        if let Some(index) = meta.series_index {
            let _ = writeln!(
                s,
                "    <meta refines=\"#series\" property=\"group-position\">{}</meta>",
                fmt_number(index)
            );
        }
        // KOReader / Kindle 系工具只认这两个 calibre 扩展；标准字段仍以上面为准。
        let _ = writeln!(
            s,
            "    <meta name=\"calibre:series\" content=\"{}\"/>",
            escape_attr(series)
        );
        if let Some(index) = meta.series_index {
            let _ = writeln!(
                s,
                "    <meta name=\"calibre:series_index\" content=\"{}\"/>",
                escape_attr(&fmt_number(index))
            );
        }
    }

    if let Some(cover) = &meta.cover {
        let _ = cover;
        s.push_str("    <meta name=\"cover\" content=\"cover-image\"/>\n");
    }

    // 章节的扩展元数据必须放在 <metadata> 内：`refines` 引用 manifest 的 id 合法，
    // 但直接挂在 <package> 下不合法，解析时也会被忽略。
    for ch in &meta.chapters {
        let _ = writeln!(
            s,
            "    <meta property=\"oread:chapter-title\" refines=\"#{}\">{}</meta>",
            escape_attr(&ch.id),
            xml_escape(&ch.title)
        );
        if let Some(url) = non_empty_opt(&ch.url) {
            let _ = writeln!(
                s,
                "    <meta property=\"oread:chapter-url\" refines=\"#{}\">{}</meta>",
                escape_attr(&ch.id),
                xml_escape(url)
            );
        }
        if ch.is_volume {
            let _ = writeln!(
                s,
                "    <meta property=\"oread:volume\" refines=\"#{}\">1</meta>",
                escape_attr(&ch.id)
            );
        }
    }

    s.push_str("  </metadata>\n");
    s.push_str("  <manifest>\n");
    s.push_str(
        "    <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" \
                properties=\"nav\"/>\n",
    );
    if !meta.resources.iter().any(|r| r.href == "style.css") {
        s.push_str("    <item id=\"style\" href=\"style.css\" media-type=\"text/css\"/>\n");
    }
    if let Some(cover) = &meta.cover {
        let _ = writeln!(
            s,
            "    <item id=\"cover-image\" href=\"{}\" media-type=\"{}\" properties=\"cover-image\"/>",
            escape_attr(&path_to_href(&cover.href)),
            escape_attr(&cover.media_type)
        );
    }
    for ch in &meta.chapters {
        let _ = writeln!(
            s,
            "    <item id=\"{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>",
            escape_attr(&ch.id),
            escape_attr(&path_to_href(&ch.href))
        );
    }
    for img in &meta.images {
        let _ = writeln!(
            s,
            "    <item id=\"{}\" href=\"{}\" media-type=\"{}\"/>",
            escape_attr(&img.id),
            escape_attr(&path_to_href(&img.href)),
            escape_attr(&img.media_type)
        );
    }
    for res in &meta.resources {
        // nav 已经由上面重建；同 href 的 style.css 也由我们统一提供，避免重复登记。
        if res.properties.split_whitespace().any(|p| p == "nav") {
            continue;
        }
        if res.href == "style.css" && res.media_type == "text/css" {
            continue;
        }
        let props = if res.properties.trim().is_empty() {
            String::new()
        } else {
            format!(" properties=\"{}\"", escape_attr(&res.properties))
        };
        let _ = writeln!(
            s,
            "    <item id=\"{}\" href=\"{}\" media-type=\"{}\"{}/>",
            escape_attr(&res.id),
            escape_attr(&path_to_href(&res.href)),
            escape_attr(&res.media_type),
            props
        );
    }
    if meta.chapters.is_empty() {
        s.push_str(
            "    <item id=\"ch-0000\" href=\"chapters/ch-0000-pending.xhtml\" \
             media-type=\"application/xhtml+xml\"/>\n",
        );
    }
    s.push_str("  </manifest>\n");

    s.push_str("  <spine>\n");
    if meta.chapters.is_empty() {
        // EPUB 3 的 spine 至少要有一项；建目录时先放占位章，等真实正文落盘再摘掉。
        s.push_str("    <itemref idref=\"ch-0000\"/>\n");
    }
    for ch in &meta.chapters {
        let _ = writeln!(s, "    <itemref idref=\"{}\"/>", escape_attr(&ch.id));
    }
    s.push_str("  </spine>\n");
    s.push_str("</package>\n");
    s
}

/// 生成 `nav.xhtml`。章节顺序与 OPF spine 一致。
pub fn to_nav_xhtml(meta: &OpfMeta) -> String {
    let mut s = String::with_capacity(768);
    let lang = escape_attr(&meta.language);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        s,
        "<html xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"{lang}\" xml:lang=\"{lang}\">"
    );
    s.push_str("<head><title>目录</title></head>\n");
    s.push_str("<body>\n<nav epub:type=\"toc\" id=\"toc\">\n<h1>目录</h1>\n<ol>\n");
    if meta.chapters.is_empty() {
        s.push_str("<li><span>本书内容尚未缓存</span></li>\n");
    }
    for ch in &meta.chapters {
        let _ = writeln!(
            s,
            "<li><a href=\"{}\">{}</a></li>",
            escape_attr(&path_to_href(&ch.href)),
            xml_escape(&ch.title)
        );
    }
    s.push_str("</ol>\n</nav>\n</body>\n</html>\n");
    s
}

// ---------------------------------------------------------------------------
// 读入
// ---------------------------------------------------------------------------

/// 解析 `content.opf`。缺少 `<dc:title>` 视为坏包（目录不再是一本合法的书）。
pub fn parse_opf(xml: &str) -> Result<OpfMeta, String> {
    parse_opf_inner(xml, None)
}

/// 导入外部 EPUB 时使用：原包没有我们的 `book-url` 标识，由调用方给定。
pub fn parse_opf_import(xml: &str, book_url: &str) -> Result<OpfMeta, String> {
    parse_opf_inner(xml, Some(book_url))
}

fn parse_opf_inner(xml: &str, fallback_book_url: Option<&str>) -> Result<OpfMeta, String> {
    let package = parser::parse(xml).map_err(|e| format!("OPF XML 不良构: {e:?}"))?;
    let doc = package.as_document();
    let root = doc
        .root()
        .children()
        .into_iter()
        .filter_map(|c| c.element())
        .find(|e| e.name().local_part() == "package")
        .ok_or_else(|| "OPF 缺少 <package> 根元素".to_string())?;

    let mut meta = OpfMeta::new("", "");
    let mut has_title = false;
    let mut spine_refs: Vec<String> = Vec::new();
    let mut manifest: Vec<ResourceItem> = Vec::new();
    let mut tags = MetaTags::default();

    for child in element_children(root) {
        match child.name().local_part() {
            "metadata" => read_metadata(child, &mut meta, &mut has_title, &mut tags)?,
            "manifest" => manifest = read_manifest(child),
            "spine" => {
                for itemref in element_children(child) {
                    if itemref.name().local_part() == "itemref" {
                        if let Some(idref) = itemref.attribute_value("idref") {
                            spine_refs.push(idref.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !has_title {
        // 导入外部 EPUB 时允许没有 dc:title，之后用文件名兜底；
        // 馆藏目录里我们自己写出的 OPF 则必须有标题。
        if fallback_book_url.is_none() {
            return Err("OPF 缺少 <dc:title>".into());
        }
    }
    if meta.book_url.is_empty() {
        match fallback_book_url {
            Some(url) if !url.trim().is_empty() => meta.book_url = url.trim().to_string(),
            _ => return Err("OPF 缺少 book-url 标识".into()),
        }
    }

    // 按 spine 顺序组装章节；href 在 manifest 读取时已经解码成文件路径。
    for idref in &spine_refs {
        // 占位章 `ch-0000` 不写进章节清单
        if idref == "ch-0000" {
            continue;
        }
        let Some(item) = manifest.iter().find(|i| &i.id == idref) else {
            continue;
        };
        meta.chapters.push(ChapterItem {
            id: idref.clone(),
            href: item.href.clone(),
            title: tags.take(&tags.titles, idref).unwrap_or_default(),
            url: tags.take(&tags.urls, idref),
            is_volume: tags.volumes.iter().any(|id| id == idref),
        });
    }

    // 封面：EPUB 3 的 properties="cover-image" 优先；兼容 EPUB 2 的
    // <meta name="cover" content="cover-id"/> 与 calibre 常用的 id="cover-image"。
    let cover_item = manifest.iter().find(|i| {
        i.properties.split_whitespace().any(|p| p == "cover-image")
            || i.id == "cover-image"
            || tags.cover_id.as_deref() == Some(i.id.as_str())
    });
    if let Some(item) = cover_item {
        meta.cover = Some(CoverItem {
            href: item.href.clone(),
            media_type: item.media_type.clone(),
        });
    }

    let spine_ids: std::collections::HashSet<&str> =
        spine_refs.iter().map(String::as_str).collect();
    let cover_href = meta.cover.as_ref().map(|c| c.href.as_str());
    for item in manifest {
        let is_nav = item.properties.split_whitespace().any(|p| p == "nav");
        if is_nav || cover_href == Some(item.href.as_str()) {
            continue;
        }
        if spine_ids.contains(item.id.as_str()) {
            continue;
        }
        if item.id == "style" && item.href == "style.css" {
            continue;
        }
        if item.media_type.starts_with("image/") {
            meta.images.push(ImageItem {
                id: item.id,
                href: item.href,
                media_type: item.media_type,
            });
        } else {
            meta.resources.push(item);
        }
    }
    Ok(meta)
}

/// OPF 里以 `refines` 关联到章节 id 的扩展字段。
#[derive(Default)]
struct MetaTags {
    urls: Vec<(String, String)>,
    titles: Vec<(String, String)>,
    volumes: Vec<String>,
    cover_id: Option<String>,
}

impl MetaTags {
    /// 取走某个 id 对应的值（同一 id 只写一次）。
    fn take(&self, list: &[(String, String)], id: &str) -> Option<String> {
        list.iter()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value.clone())
    }
}

fn read_metadata(
    el: Element<'_>,
    meta: &mut OpfMeta,
    has_title: &mut bool,
    tags: &mut MetaTags,
) -> Result<(), String> {
    let mut revision_seen = false;
    for child in element_children(el) {
        let local = child.name().local_part();
        let text = text_of(child);
        match local {
            "title" => {
                // 多 dc:title 时第一个是主标题，其余（副标题等）不覆盖它。
                if !*has_title && !text.trim().is_empty() {
                    meta.title = text;
                    *has_title = true;
                }
            }
            "language" => meta.language = text,
            "creator" => {
                if let Some(v) = non_empty(&text) {
                    meta.author = Some(match meta.author.take() {
                        Some(prev) => format!("{prev}, {v}"),
                        None => v.to_string(),
                    });
                }
            }
            "publisher" => meta.publisher = non_empty(&text).map(str::to_string),
            "date" => meta.pubdate = non_empty(&text).map(str::to_string),
            "description" => meta.description = non_empty(&text).map(str::to_string),
            "subject" => {
                if !text.trim().is_empty() {
                    meta.subjects.push(text);
                }
            }
            "identifier" => match child.attribute_value("id") {
                Some("book-url") => meta.book_url = text,
                Some("isbn") => meta.isbn = non_empty(&text).map(str::to_string),
                _ => {}
            },
            "meta" => read_meta_tag(child, meta, tags, &mut revision_seen)?,
            _ => {}
        }
    }
    if !revision_seen {
        meta.revision = 0;
    }
    Ok(())
}

fn read_meta_tag(
    el: Element<'_>,
    meta: &mut OpfMeta,
    tags: &mut MetaTags,
    revision_seen: &mut bool,
) -> Result<(), String> {
    let property = el.attribute_value("property");
    let name = el.attribute_value("name");
    let content = el.attribute_value("content");
    let refines = el
        .attribute_value("refines")
        .map(|r| r.trim_start_matches('#').to_string());
    let text = text_of(el);

    match (property, name) {
        (Some("dcterms:modified"), _) => {
            if !text.trim().is_empty() {
                meta.modified = text;
            }
        }
        (Some("oread:revision"), _) => {
            meta.revision = text.trim().parse().unwrap_or(0);
            *revision_seen = true;
        }
        (Some("belongs-to-collection"), _) => {
            meta.series = non_empty(&text).map(str::to_string);
        }
        (Some("group-position"), _) => {
            meta.series_index = text.trim().parse().ok();
        }
        (Some("oread:chapter-url"), _) => {
            if let Some(id) = refines {
                tags.urls.push((id, text));
            }
        }
        (Some("oread:chapter-title"), _) => {
            if let Some(id) = refines {
                tags.titles.push((id, text));
            }
        }
        (Some("oread:volume"), _) => {
            if let Some(id) = refines {
                tags.volumes.push(id);
            }
        }
        (_, Some("calibre:series")) if meta.series.is_none() => {
            meta.series = content.and_then(|c| non_empty(c).map(str::to_string));
        }
        (_, Some("calibre:series_index")) if meta.series_index.is_none() => {
            meta.series_index = content.and_then(|c| c.trim().parse().ok());
        }
        (_, Some("cover")) => {
            if let Some(id) = content.and_then(non_empty) {
                tags.cover_id = Some(id.to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

fn read_manifest(el: Element<'_>) -> Vec<ResourceItem> {
    let mut out = Vec::new();
    for child in element_children(el) {
        if child.name().local_part() != "item" {
            continue;
        }
        let id = child.attribute_value("id").unwrap_or("").to_string();
        let href = child.attribute_value("href").unwrap_or("");
        let media_type = child.attribute_value("media-type").unwrap_or("");
        let properties = child.attribute_value("properties").unwrap_or("");
        if href.is_empty() || id.is_empty() {
            continue;
        }
        out.push(ResourceItem {
            id,
            href: href_to_path(href),
            media_type: media_type.to_string(),
            properties: properties.to_string(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn element_children<'a>(el: Element<'a>) -> Vec<Element<'a>> {
    el.children()
        .into_iter()
        .filter_map(|c| c.element())
        .collect()
}

fn text_of(el: Element<'_>) -> String {
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

fn non_empty_opt(v: &Option<String>) -> Option<&str> {
    v.as_deref().and_then(non_empty)
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 属性值转义（文本节点用 `xml_escape`，属性还要额外处理换行）。
fn escape_attr(s: &str) -> String {
    xml_escape(s).replace('\n', "&#10;").replace('\r', "&#13;")
}

/// 系列序号：整数不带小数点，保持与 calibre 输出接近。
fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OpfMeta {
        let mut meta = OpfMeta::new("https://example.com/book/1", "测试书");
        meta.author = Some("作者 & 名字".into());
        meta.publisher = Some("出版社".into());
        meta.pubdate = Some("2024-05-01".into());
        meta.description = Some("简介 <b>不要</b> 被解释".into());
        meta.subjects = vec!["玄幻".into(), "连载".into()];
        meta.series = Some("某某系列".into());
        meta.series_index = Some(3.0);
        meta.isbn = Some("9780000000001".into());
        meta.revision = 7;
        meta.cover = Some(CoverItem {
            href: "images/cover.jpg".into(),
            media_type: "image/jpeg".into(),
        });
        meta.chapters = vec![
            ChapterItem {
                id: "ch-0001".into(),
                href: "chapters/ch-0001-kai-duan.xhtml".into(),
                title: "开端".into(),
                url: Some("https://example.com/book/1/1".into()),
                is_volume: false,
            },
            ChapterItem {
                id: "vol-0002".into(),
                href: "chapters/vol-0002-di-yi-juan.xhtml".into(),
                title: "第一卷".into(),
                url: None,
                is_volume: true,
            },
        ];
        meta.images = vec![ImageItem {
            id: "img-0011223344556677".into(),
            href: "images/0011223344556677.jpg".into(),
            media_type: "image/jpeg".into(),
        }];
        meta
    }

    #[test]
    fn opf_round_trip_keeps_every_known_field() {
        let meta = sample();
        let xml = to_opf_xml(&meta);
        let back = parse_opf(&xml).expect("应当能解析自己写出的 OPF");

        assert_eq!(back.book_url, meta.book_url);
        assert_eq!(back.title, meta.title);
        assert_eq!(back.language, meta.language);
        assert_eq!(back.author, meta.author);
        assert_eq!(back.publisher, meta.publisher);
        assert_eq!(back.pubdate, meta.pubdate);
        assert_eq!(back.description, meta.description);
        assert_eq!(back.subjects, meta.subjects);
        assert_eq!(back.series, meta.series);
        assert_eq!(back.series_index, meta.series_index);
        assert_eq!(back.isbn, meta.isbn);
        assert_eq!(back.revision, meta.revision);
        assert_eq!(back.modified, meta.modified);
        assert_eq!(back.cover, meta.cover);
        assert_eq!(back.images, meta.images);
        assert_eq!(back.chapters.len(), meta.chapters.len());
        for (a, b) in back.chapters.iter().zip(meta.chapters.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.href, b.href);
            assert_eq!(a.url, b.url);
            assert_eq!(a.is_volume, b.is_volume);
        }
    }

    #[test]
    fn opf_escapes_metadata() {
        let xml = to_opf_xml(&sample());
        assert!(xml.contains("作者 &amp; 名字"));
        assert!(xml.contains("简介 &lt;b&gt;不要&lt;/b&gt; 被解释"));
        assert!(xml.contains("belongs-to-collection"));
        assert!(xml.contains("calibre:series_index"));
    }

    #[test]
    fn rejects_missing_title() {
        let xml = "<?xml version=\"1.0\"?><package xmlns=\"http://www.idpf.org/2007/opf\">\
                   <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
                   <dc:identifier id=\"book-url\">x</dc:identifier></metadata></package>";
        assert!(parse_opf(xml).is_err());
    }

    #[test]
    fn empty_spine_gets_placeholder() {
        let meta = OpfMeta::new("u", "t");
        let xml = to_opf_xml(&meta);
        assert!(xml.contains("<itemref idref=\"ch-0000\"/>"));
        let back = parse_opf(&xml).unwrap();
        assert!(back.chapters.is_empty(), "占位章不应出现在章节清单里");
    }

    #[test]
    fn nav_lists_chapters_in_spine_order() {
        let meta = sample();
        let nav = to_nav_xhtml(&meta);
        let first = nav.find("ch-0001-kai-duan.xhtml").unwrap();
        let second = nav.find("vol-0002-di-yi-juan.xhtml").unwrap();
        assert!(first < second);
        assert!(nav.contains(">开端<"));
    }
}
