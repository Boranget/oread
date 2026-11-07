//! web_book 的图片章节抽取（move-only 从 web_book.rs 拆出）。

use super::*;

/// 单章最多抽取的图片数（防止异常书源把一次请求放大成数百张）
pub const MAX_CHAPTER_IMAGES: usize = 300;

/// 图片章节抽取结果
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChapterImages {
    pub images: Vec<BookImage>,
    pub truncated: bool,
}

/// `img` 标签与属性解析用的静态正则，避免每张图重复编译。
pub(super) fn img_tag_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap())
}

pub(super) fn attr_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#,
        )
        .unwrap()
    })
}

pub(super) fn parse_attrs(tag: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for c in attr_re().captures_iter(tag) {
        let name = c
            .get(1)
            .map(|m| m.as_str().to_ascii_lowercase())
            .unwrap_or_default();
        let val = c
            .get(2)
            .or_else(|| c.get(3))
            .or_else(|| c.get(4))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        if !name.is_empty() {
            map.insert(name, val);
        }
    }
    map
}

pub(super) fn parse_dim(v: Option<&String>) -> Option<u32> {
    let s = v?.trim().trim_end_matches("px").trim();
    s.parse::<u32>().ok()
}

/// 是否 1px（或更小）的占位图：宽/高属性或内联样式里出现 1px。
pub(super) fn is_placeholder_size(attrs: &HashMap<String, String>) -> bool {
    let w = parse_dim(attrs.get("width"));
    let h = parse_dim(attrs.get("height"));
    if w.map(|n| n <= 1).unwrap_or(false) || h.map(|n| n <= 1).unwrap_or(false) {
        return true;
    }
    if let Some(style) = attrs.get("style") {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            regex::Regex::new(r"(?i)(?:^|;)\s*(?:width|height)\s*:\s*([0-9.]+)\s*px").unwrap()
        });
        for c in re.captures_iter(style) {
            if c.get(1)
                .and_then(|m| m.as_str().parse::<f32>().ok())
                .map(|n| n <= 1.0)
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// 一张图片的“原始”信息（尚未去重/编号）
#[derive(Debug, Clone)]
pub(super) struct RawImage {
    pub(super) url: String,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
}

/// 归一图片地址：部分书源（如 1kkk/酸奶漫画）把请求头 JSON 直接拼在 `src` 里，
/// 产出 `url,{"headers":...}`；HTML 解析只能在第一个引号处截断，会留下 `,{` 尾巴。
/// 这里按 `,{` 切掉后缀，避免代理把无效后缀带到源站导致 404。
pub(super) fn clean_image_url(raw: &str) -> String {
    let u = raw.trim();
    let head = match u.find(",{") {
        Some(pos) => u[..pos].trim(),
        None => u,
    };
    head.trim_end_matches(',').trim().to_string()
}

pub(super) fn push_raw(
    out: &mut Vec<RawImage>,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
) {
    let u = clean_image_url(url);
    if u.is_empty() || u.starts_with("data:") {
        return;
    }
    out.push(RawImage {
        url: u,
        width,
        height,
    });
}

pub(super) fn is_usable_src(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && !t.starts_with("data:")
        && !t.starts_with("javascript:")
        && !t.starts_with('#')
        && !t.eq_ignore_ascii_case("about:blank")
}

/// 选图片地址：懒加载属性优先（src 常是 1px 占位图），再退回 src / srcset。
pub(super) fn pick_img_src(attrs: &HashMap<String, String>) -> &str {
    for key in [
        "data-src",
        "data-original",
        "data-lazy-src",
        "data-original-src",
        "data-echo",
        "data-url",
    ] {
        if let Some(v) = attrs.get(key).filter(|s| is_usable_src(s)) {
            return v;
        }
    }
    if let Some(v) = attrs.get("src").filter(|s| is_usable_src(s)) {
        return v;
    }
    for key in ["srcset", "data-srcset"] {
        if let Some(v) = attrs.get(key) {
            let first = v.split(',').next().unwrap_or("").trim();
            let url = first.split_whitespace().next().unwrap_or("");
            if is_usable_src(url) {
                return url;
            }
        }
    }
    ""
}

/// 从一段 HTML 里按页面顺序提取 `<img src>`（含懒加载 `data-src`/`data-original` 兜底）。
pub(super) fn images_from_html(html: &str) -> Vec<RawImage> {
    let mut out = Vec::new();
    for tag in img_tag_re().find_iter(html) {
        let attrs = parse_attrs(tag.as_str());
        let src = pick_img_src(&attrs);
        if is_placeholder_size(&attrs) {
            continue;
        }
        push_raw(
            &mut out,
            src,
            parse_dim(attrs.get("width")),
            parse_dim(attrs.get("height")),
        );
    }
    out
}

/// JS 数组/JSON 结果 -> 图片项；支持 `["url"]`、`[{url,width,height}]`、
/// `{images:[...]}` 以及 `{url:...}` 单对象。
pub(super) fn images_from_json(text: &str) -> Option<Vec<RawImage>> {
    let t = text.trim();
    if !(t.starts_with('[') || t.starts_with('{')) {
        return None;
    }
    let v: Value = serde_json::from_str(t).ok()?;
    let mut out = Vec::new();
    collect_json_images(&v, &mut out);
    Some(out)
}

pub(super) fn collect_json_images(v: &Value, out: &mut Vec<RawImage>) {
    match v {
        Value::String(s) => push_raw(out, s, None, None),
        Value::Array(items) => {
            for it in items {
                collect_json_images(it, out);
            }
        }
        Value::Object(obj) => {
            for key in ["images", "list", "data"] {
                if let Some(inner) = obj.get(key) {
                    collect_json_images(inner, out);
                    return;
                }
            }
            let url = obj
                .get("url")
                .or_else(|| obj.get("src"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let width = obj
                .get("width")
                .and_then(|x| {
                    x.as_u64()
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .map(|n| n as u32);
            let height = obj
                .get("height")
                .and_then(|x| {
                    x.as_u64()
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .map(|n| n as u32);
            push_raw(out, url, width, height);
        }
        _ => {}
    }
}

/// 单条规则结果 -> 图片项。兼容 HTML 片段、纯 URL、JSON 数组三种形态。
pub(super) fn images_from_value(value: &str) -> Vec<RawImage> {
    let t = value.trim();
    if t.is_empty() {
        return vec![];
    }
    if let Some(v) = images_from_json(t) {
        return v;
    }
    let from_html = images_from_html(t);
    if !from_html.is_empty() {
        return from_html;
    }
    // 纯 URL（无标签、无空白）：content 规则常直接写 `@css:img@src` 或 `@js:` 返回单链
    if !t.contains('<') && !t.chars().any(char::is_whitespace) {
        let mut out = Vec::new();
        push_raw(&mut out, t, None, None);
        return out;
    }
    from_html
}

/// 图片章节正文：所有 `<img src>` 按页面顺序去重、过滤占位图，`index` 从 1 开始。
pub async fn chapter_images(
    source: &BookSource,
    chapter: &BookChapter,
) -> anyhow::Result<ChapterImages> {
    chapter_images_book(source, chapter, None).await
}

/// 带 book 作用域的图片章节（正文规则里的变量/cache 按同书预载）。
pub async fn chapter_images_book(
    source: &BookSource,
    chapter: &BookChapter,
    store: Option<&BookStore>,
) -> anyhow::Result<ChapterImages> {
    let rule: &ContentRule = match &source.rule_content {
        Some(r) => r,
        None => return Ok(ChapterImages::default()),
    };
    // 没有 content 规则时不要退化成“整页图片”（会混入广告/图标）
    let content_rule = rule.content.as_deref().unwrap_or("").trim();
    if content_rule.is_empty() {
        return Ok(ChapterImages::default());
    }
    let pages = fetch_html_pages(source, chapter, rule).await?;
    let book_url = chapter.book_url.clone().unwrap_or_default();
    let mut per_page: Vec<(String, Vec<RawImage>)> = Vec::new();
    for (page_url, html) in &pages {
        // 规则本身可能直接产出多条（CSS 列表 / JS 数组），逐条归一
        let ar = make_ar_book(source, store, &book_url);
        let values = ar.get_string_list_all(html, content_rule, page_url);
        record_ar(&ar, store);

        let mut raws: Vec<RawImage> = if values.is_empty() {
            Vec::new()
        } else {
            values.iter().flat_map(|v| images_from_value(v)).collect()
        };
        tracing::debug!(page_url = %page_url, values = values.len(), raws = raws.len(), "image page extracted");
        // 兼容 `class.ComicPics@img@html`：`@html` 在图片节点上是空的内层 HTML，
        // 换成 `@all`（outer HTML）再解析 `<img src>`；只在原规则零结果时兜底。
        if raws.is_empty() {
            if let Some(head) = content_rule.strip_suffix("@html") {
                let alt_rule = format!("{}@all", head.trim());
                let alt_values = ar.get_string_list_all(html, &alt_rule, page_url);
                raws = alt_values
                    .iter()
                    .flat_map(|v| images_from_value(v))
                    .collect();
            }
        }
        per_page.push((page_url.clone(), raws));
    }
    Ok(build_images(&per_page, MAX_CHAPTER_IMAGES))
}

/// 把“页面 URL + 原始图片”归一为最终列表：相对地址补全、去重、编号从 1 开始、超限截断。
pub(super) fn build_images(pages: &[(String, Vec<RawImage>)], limit: usize) -> ChapterImages {
    let mut images: Vec<BookImage> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut truncated = false;
    for (page_url, raws) in pages {
        for raw in raws {
            let absolute = abs(page_url, &raw.url);
            if !seen.insert(absolute.clone()) {
                continue;
            }
            if images.len() >= limit {
                truncated = true;
                break;
            }
            let index = images.len() as i64 + 1;
            images.push(BookImage {
                index,
                url: absolute,
                width: raw.width,
                height: raw.height,
                bytes: None,
                priority: Some(if index <= 3 { "high" } else { "normal" }.to_string()),
            });
        }
        if truncated {
            break;
        }
    }
    ChapterImages { images, truncated }
}
