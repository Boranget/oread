//! 正文 HTML → 语义段落。
//!
//! 书源正文常混着手写 HTML、`<br>` 换行与纯文本。EPUB 要求良构 XHTML，
//! 直接把原始 HTML 塞进去会产出坏包，所以这里先归一化成块：
//!
//! - 标题（`h1`–`h6`）保留层级；
//! - 块级元素（`p`/`div`/`li`/`blockquote`/表格单元）各自成段；
//! - `<br>` 与文本里的换行断段；
//! - 行内标签只取文字（不强求保留粗体/颜色，阅读器可自行排版）；
//! - `<img>` 成为独立图片块（兼容懒加载属性，过滤非法地址与跟踪像素）；
//! - `script`/`style`/`svg`/视频等直接丢弃，避免 EPUB 里出现脚本与外链资源。

use ego_tree::NodeRef;
use scraper::node::Element;
use scraper::{Html, Node};

/// 归一化后的正文块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(u8, String),
    Paragraph(String),
    /// `src` 原样保留（相对地址由调用方按章节 URL 归一化），`alt` 可为空。
    Image {
        src: String,
        alt: String,
    },
}

impl Block {
    pub fn is_image(&self) -> bool {
        matches!(self, Block::Image { .. })
    }
}

/// 把一章正文转成块序列。纯文本输入同样适用（按换行断段）。
pub fn to_blocks(html: &str) -> Vec<Block> {
    let doc = Html::parse_fragment(html);
    let root = doc.root_element();
    let mut b = Builder::default();
    for child in root.children() {
        walk(child, &mut b);
    }
    b.flush();
    b.blocks
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    cur: String,
}

impl Builder {
    fn push(&mut self, s: &str) {
        self.cur.push_str(s);
    }

    fn flush(&mut self) {
        let text = collapse(&self.cur);
        self.cur.clear();
        if !text.is_empty() {
            self.blocks.push(Block::Paragraph(text));
        }
    }
}

fn walk(node: NodeRef<'_, Node>, b: &mut Builder) {
    match node.value() {
        Node::Text(t) => {
            let mut first = true;
            for part in t.text.split('\n') {
                if !first {
                    b.flush();
                }
                b.push(part.trim_end_matches('\r'));
                first = false;
            }
        }
        Node::Element(el) => {
            let name = el.name();
            if is_skip(name) {
                return;
            }
            if name == "img" {
                b.flush();
                if let Some((src, alt)) = image_of(el) {
                    b.blocks.push(Block::Image { src, alt });
                }
                return;
            }
            if let Some(level) = heading_level(name) {
                b.flush();
                let text = collapse(&inline_text(&node));
                if !text.is_empty() {
                    b.blocks.push(Block::Heading(level, text));
                }
                return;
            }
            if name == "br" {
                b.flush();
                return;
            }
            let block = is_block(name);
            if block {
                b.flush();
            }
            if name == "li" {
                b.push("• ");
            }
            for child in node.children() {
                walk(child, b);
            }
            if block {
                b.flush();
            }
        }
        _ => {}
    }
}

/// 标题里的纯文字（跳过脚本/图片等）。
fn inline_text<'a>(node: &NodeRef<'a, Node>) -> String {
    let mut out = String::new();
    let mut stack: Vec<NodeRef<'a, Node>> = node.children().collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        match n.value() {
            Node::Text(t) => out.push_str(t.text.as_ref()),
            Node::Element(el) => {
                let name = el.name();
                if is_skip(name) {
                    continue;
                }
                if name == "br" {
                    out.push(' ');
                    continue;
                }
                let children: Vec<_> = n.children().collect();
                stack.extend(children.into_iter().rev());
            }
            _ => {}
        }
    }
    out
}

fn is_skip(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "iframe" | "video" | "audio" | "source" | "svg"
    )
}

/// 图片地址候选：懒加载属性优先于 `src`（`src` 常是 1px 占位图），
/// 最后才看 `srcset` 的第一项。顺序与前端旧解析保持一致。
const IMG_ATTRS: &[&str] = &[
    "data-src",
    "data-original",
    "data-lazy-src",
    "data-original-src",
    "data-echo",
    "data-url",
];

fn attr(el: &Element, name: &str) -> Option<String> {
    el.attr(name)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn image_src(el: &Element) -> Option<String> {
    IMG_ATTRS
        .iter()
        .find_map(|name| attr(el, name))
        .or_else(|| attr(el, "src"))
        .or_else(|| {
            let set = attr(el, "srcset").or_else(|| attr(el, "data-srcset"))?;
            let first = set.split(',').next()?.split_whitespace().next()?;
            Some(first.to_string())
        })
        .filter(|src| valid_img_src(src))
}

/// 只保留能安全交给前端的地址：拒绝 `javascript:`/`data:` 等伪协议。
fn valid_img_src(src: &str) -> bool {
    let lower = src.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    ![
        "data:",
        "javascript:",
        "vbscript:",
        "blob:",
        "file:",
        "about:",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
}

fn dim_attr(el: &Element, name: &str) -> Option<u32> {
    el.attr(name)?
        .trim()
        .trim_end_matches("px")
        .parse::<u32>()
        .ok()
}

/// 提取图片块；`width`/`height` 都很小（≤2px）时视为跟踪像素丢弃。
fn image_of(el: &Element) -> Option<(String, String)> {
    if let (Some(w), Some(h)) = (dim_attr(el, "width"), dim_attr(el, "height")) {
        if w <= 2 && h <= 2 {
            return None;
        }
    }
    let src = image_src(el)?;
    let alt = attr(el, "alt").unwrap_or_default();
    let alt = alt.chars().take(200).collect();
    Some((src, alt))
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "blockquote"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "pre"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
    )
}

/// 折叠 HTML 空白：连续空白（含 `&nbsp;`、全角空格）变一个空格。
fn collapse(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c == '\u{a0}' || c == '\u{3000}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{to_blocks, Block};

    #[test]
    fn plain_text_splits_on_newlines() {
        let blocks = to_blocks("第一段\n\n第二段\n  续行  \n");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("第一段".into()),
                Block::Paragraph("第二段".into()),
                Block::Paragraph("续行".into()),
            ]
        );
    }

    #[test]
    fn html_blocks_headings_and_br() {
        let blocks = to_blocks(
            "<div>开头<br>换行</div><h2>小标题</h2><p>正文 <b>加粗</b> 结束</p><ul><li>一</li><li>二</li></ul>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("开头".into()),
                Block::Paragraph("换行".into()),
                Block::Heading(2, "小标题".into()),
                Block::Paragraph("正文 加粗 结束".into()),
                Block::Paragraph("• 一".into()),
                Block::Paragraph("• 二".into()),
            ]
        );
    }

    #[test]
    fn skips_scripts_and_collapses_spaces() {
        let blocks = to_blocks("<p>甲&nbsp;&nbsp;乙<script>alert(1)</script>丙</p>");
        assert_eq!(blocks, vec![Block::Paragraph("甲 乙丙".into())]);
    }

    #[test]
    fn images_become_blocks_with_lazy_src() {
        let blocks = to_blocks(
            "<p>前<img src=\"placeholder.gif\" data-src=\"real.jpg\" alt=\"图\">后</p>\
             <img src=\"https://a.test/b.jpg\">",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("前".into()),
                Block::Image {
                    src: "real.jpg".into(),
                    alt: "图".into()
                },
                Block::Paragraph("后".into()),
                Block::Image {
                    src: "https://a.test/b.jpg".into(),
                    alt: "".into()
                },
            ]
        );
    }

    #[test]
    fn images_filter_bad_sources_and_tracking_pixels() {
        let blocks = to_blocks(
            "<img src=\"javascript:alert(1)\">\
             <img src=\"data:image/gif;base64,AAA\">\
             <img src=\"https://a.test/p.gif\" width=\"1\" height=\"1\">\
             <img srcset=\"https://a.test/x.jpg 1x, https://a.test/y.jpg 2x\">",
        );
        assert_eq!(
            blocks,
            vec![Block::Image {
                src: "https://a.test/x.jpg".into(),
                alt: "".into()
            }]
        );
    }

    #[test]
    fn empty_and_whitespace_only_produce_nothing() {
        assert!(to_blocks("").is_empty());
        assert!(to_blocks(" \n\t \n").is_empty());
        assert!(to_blocks("<div>   </div>").is_empty());
    }
}
