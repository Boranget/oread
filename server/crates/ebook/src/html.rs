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

/// 列表项。列表按文档顺序拍平，`level` 从 0 开始，`ordered` 表示该项所在列表的序号样式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub level: u8,
    pub ordered: bool,
    pub text: String,
}

/// 归一化后的正文块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(u8, String),
    Paragraph(String),
    /// 列表；嵌套项用 `level` 表示层级，渲染时自行决定缩进与序号。
    List(Vec<ListItem>),
    /// 预格式化文本（`<pre>` / 代码块）：保留换行与缩进，不做空白折叠。
    Code(String),
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

/// 列表块 → XHTML 片段。输出真正的嵌套 `ul` / `ol`，并带内联缩进：
/// 旧客户端即使重置了列表样式也能看出层级；同时保证 HTML 重新解析后仍能
/// 还原成同样的 `Block::List`（`/api/book/content` 的 blocks 由 content 再解析）。
pub fn list_to_html(items: &[ListItem]) -> String {
    use crate::xml::xml_escape;

    /// 从 `start` 开始渲染同一层级、同样式的一段列表；返回下一项下标。
    fn render(items: &[ListItem], start: usize, level: u8, out: &mut String) -> usize {
        let ordered = items[start].ordered;
        let (open, close) = if ordered {
            ("<ol", "</ol>")
        } else {
            ("<ul", "</ul>")
        };
        out.push_str(open);
        out.push_str(" style=\"margin:0.4em 0 0.4em 1.2em;padding-left:1.2em\">\n");
        let mut i = start;
        while i < items.len() && items[i].level == level && items[i].ordered == ordered {
            out.push_str("<li>");
            out.push_str(&xml_escape(&items[i].text));
            i += 1;
            while i < items.len() && items[i].level > level {
                i = render(items, i, items[i].level, out);
            }
            out.push_str("</li>\n");
        }
        out.push_str(close);
        out.push('\n');
        i
    }

    let mut out = String::new();
    let mut i = 0;
    while i < items.len() {
        i = render(items, i, items[i].level, &mut out);
    }
    out
}

/// 代码块 → XHTML 片段。`<pre>` 保留换行与行首缩进。
pub fn code_to_html(text: &str) -> String {
    format!("<pre>{}</pre>\n", crate::xml::xml_escape(text))
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
            if name == "ul" || name == "ol" {
                b.flush();
                let mut items = Vec::new();
                collect_list(node, 0, name == "ol", &mut items);
                if !items.is_empty() {
                    b.blocks.push(Block::List(items));
                }
                return;
            }
            if name == "pre" {
                b.flush();
                let text = pre_text(&node);
                if !text.trim().is_empty() {
                    b.blocks.push(Block::Code(text));
                }
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

/// 列表里的纯文字（跳过嵌套列表、脚本与图片）。
fn list_item_text(node: &NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    let mut stack: Vec<NodeRef<'_, Node>> = node.children().collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        match n.value() {
            Node::Text(t) => out.push_str(t.text.as_ref()),
            Node::Element(el) => {
                let name = el.name();
                if name == "ul" || name == "ol" || is_skip(name) {
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
    collapse(&out)
}

/// 把一个 `ul` / `ol` 拍平成带层级的列表项。
fn collect_list(node: NodeRef<'_, Node>, level: u8, ordered: bool, out: &mut Vec<ListItem>) {
    for child in node.children() {
        let Node::Element(el) = child.value() else {
            continue;
        };
        if el.name() != "li" {
            continue;
        }
        let text = list_item_text(&child);
        if !text.is_empty() {
            out.push(ListItem {
                level,
                ordered,
                text,
            });
        }
        // 嵌套列表是 `li` 的直接子元素；层级 +1，序号样式由子列表自己决定。
        for sub in child.children() {
            let Node::Element(sub_el) = sub.value() else {
                continue;
            };
            let name = sub_el.name();
            if name == "ul" || name == "ol" {
                collect_list(sub, level.saturating_add(1), name == "ol", out);
            }
        }
    }
}

/// `<pre>` 的原始文字：保留换行与行首缩进，只统一换行符并去掉首尾空行。
fn pre_text(node: &NodeRef<'_, Node>) -> String {
    fn collect(node: NodeRef<'_, Node>, out: &mut String) {
        for child in node.children() {
            match child.value() {
                Node::Text(t) => out.push_str(t.text.as_ref()),
                Node::Element(el) => {
                    let name = el.name();
                    if name == "br" {
                        out.push('\n');
                    } else if !is_skip(name) {
                        collect(child, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = String::new();
    collect(*node, &mut out);
    out.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_matches('\n')
        .to_string()
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
    use super::{list_to_html, to_blocks, Block, ListItem};

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
                Block::List(vec![
                    ListItem {
                        level: 0,
                        ordered: false,
                        text: "一".into(),
                    },
                    ListItem {
                        level: 0,
                        ordered: false,
                        text: "二".into(),
                    },
                ]),
            ]
        );
    }

    #[test]
    fn lists_keep_order_and_nesting() {
        let blocks = to_blocks(
            "<ul><li>一<ul><li>甲</li><li>乙</li></ul></li><li>二</li></ul>\
             <ol><li>first</li><li>second</li></ol>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::List(vec![
                    ListItem {
                        level: 0,
                        ordered: false,
                        text: "一".into(),
                    },
                    ListItem {
                        level: 1,
                        ordered: false,
                        text: "甲".into(),
                    },
                    ListItem {
                        level: 1,
                        ordered: false,
                        text: "乙".into(),
                    },
                    ListItem {
                        level: 0,
                        ordered: false,
                        text: "二".into(),
                    },
                ]),
                Block::List(vec![
                    ListItem {
                        level: 0,
                        ordered: true,
                        text: "first".into(),
                    },
                    ListItem {
                        level: 0,
                        ordered: true,
                        text: "second".into(),
                    },
                ]),
            ]
        );
    }

    #[test]
    fn list_html_round_trips_through_parser() {
        let items = vec![
            ListItem {
                level: 0,
                ordered: false,
                text: "一 & <二>".into(),
            },
            ListItem {
                level: 1,
                ordered: true,
                text: "甲".into(),
            },
            ListItem {
                level: 1,
                ordered: true,
                text: "乙".into(),
            },
            ListItem {
                level: 0,
                ordered: false,
                text: "三".into(),
            },
        ];
        let html = list_to_html(&items);
        assert!(html.contains("<ul"));
        assert!(html.contains("<ol"));
        assert_eq!(to_blocks(&html), vec![Block::List(items)]);
    }

    #[test]
    fn pre_keeps_line_breaks_and_indent() {
        let blocks = to_blocks(
            "<p>前</p><pre><code>fn main() {\n    println!(\"hi\");\n}\n</code></pre><p>后</p>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("前".into()),
                Block::Code("fn main() {\n    println!(\"hi\");\n}".into()),
                Block::Paragraph("后".into()),
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
