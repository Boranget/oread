//! 正文 HTML → 语义段落。
//!
//! 书源正文常混着手写 HTML、`<br>` 换行与纯文本。EPUB 要求良构 XHTML，
//! 直接把原始 HTML 塞进去会产出坏包，所以这里先归一化成块：
//!
//! - 标题（`h1`–`h6`）保留层级；
//! - 块级元素（`p`/`div`/`li`/`blockquote`/表格单元）各自成段；
//! - `<br>` 断段；纯文本块（无行内元素）里的换行也断段，含行内元素的块按排版空白折叠；
//! - 行内标签只取文字（不强求保留粗体/颜色，阅读器可自行排版）；
//! - `<img>` 成为独立图片块（兼容懒加载属性，过滤非法地址与跟踪像素）；
//! - 真正的数据表保留行列、表头与 `colspan` / `rowspan`，版式表仍按普通内容降级；
//! - 独立公式保留原始 MathML 给浏览器，另带一份纯文本给 `/k`、导出与搜索；
//! - SVG 里的 `<image>` 提取为图片块，`<text>` / `<tspan>` 按行还原为段落；
//! - `<hr>` 成为分隔块；`ruby` 只保留底字，`rt` / `rp` 注音丢弃，避免正文重复；
//! - `script`/`style`/视频等直接丢弃，避免 EPUB 里出现脚本与外链资源。

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

/// 表格单元格。`colspan` / `rowspan` 只保留布局信息，不在这里展开网格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub text: String,
    pub colspan: u8,
    pub rowspan: u8,
    pub header: bool,
}

/// 表格行。`header` 表示整行都是表头（`thead` 或全部 `th`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub header: bool,
    pub cells: Vec<TableCell>,
}

/// 数据表。只收多行多列、且不含图片的“真表格”；单格公式包装表和版式表
/// 继续走原来的段落/图片路径，避免把版式当数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub caption: Option<String>,
    pub rows: Vec<TableRow>,
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
    /// 分隔线（`<hr>`）：场景切换等，本身没有文字。
    Divider,
    /// `src` 原样保留（相对地址由调用方按章节 URL 归一化），`alt` 可为空。
    Image {
        src: String,
        alt: String,
    },
    /// 数据表；行列与合并单元格信息保留，渲染时自行决定边框和分页。
    Table(Table),
    /// 独立公式：`mathml` 是清理后的原始标记，`text` 是给纯文本出口的回退。
    Math {
        mathml: String,
        text: String,
    },
}

impl Block {
    pub fn is_image(&self) -> bool {
        matches!(self, Block::Image { .. })
    }

    /// 这一块有没有可见内容（图片、表格、公式都算）。
    pub fn has_content(&self) -> bool {
        match self {
            Block::Heading(_, t) | Block::Paragraph(t) | Block::Code(t) => !t.trim().is_empty(),
            Block::List(items) => items.iter().any(|i| !i.text.trim().is_empty()),
            Block::Image { .. } => true,
            Block::Divider => true,
            Block::Table(table) => table.rows.iter().any(|r| !r.cells.is_empty()),
            Block::Math { text, mathml } => !text.trim().is_empty() || !mathml.trim().is_empty(),
        }
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

/// 表格 → XHTML 片段。样式内联，导出 EPUB 与阅读器都不依赖外部 CSS。
pub fn table_to_html(table: &Table) -> String {
    use crate::xml::xml_escape;

    let mut out = String::from(
        "<table style=\"border-collapse:collapse;width:100%;margin:0.8em 0;table-layout:auto\">\n",
    );
    if let Some(caption) = table.caption.as_deref().filter(|c| !c.trim().is_empty()) {
        out.push_str(&format!("<caption>{}</caption>\n", xml_escape(caption)));
    }
    for row in &table.rows {
        out.push_str("<tr>\n");
        for cell in &row.cells {
            let tag = if cell.header || row.header {
                "th"
            } else {
                "td"
            };
            out.push_str(&format!(
                "<{tag} colspan=\"{}\" rowspan=\"{}\" style=\"border:1px solid #999;padding:0.3em 0.5em;text-align:left;vertical-align:top\">{}</{tag}>\n",
                cell.colspan.max(1),
                cell.rowspan.max(1),
                xml_escape(&cell.text)
            ));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
    out
}

/// 公式 → XHTML 片段。MathML 为空时退回纯文本，保证任何出口都不出现空白。
pub fn math_to_html(mathml: &str, text: &str) -> String {
    use crate::xml::xml_escape;

    if mathml.trim().is_empty() {
        let text = text.trim();
        if text.is_empty() {
            String::new()
        } else {
            format!("<p>{}</p>\n", xml_escape(text))
        }
    } else {
        format!("<div class=\"oread-math\">{mathml}</div>\n")
    }
}

/// 表格 → 纯文本：一行一条，单元格用 ` | ` 分隔，表头在前。
pub fn table_to_text(table: &Table) -> String {
    let mut lines = Vec::new();
    if let Some(caption) = table.caption.as_deref().filter(|c| !c.trim().is_empty()) {
        lines.push(caption.trim().to_string());
    }
    for row in &table.rows {
        let cells: Vec<&str> = row
            .cells
            .iter()
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if !cells.is_empty() {
            lines.push(cells.join(" | "));
        }
    }
    lines.join("\n")
}

/// 块序列 → 纯文本。TXT 导出、搜索与 `/k` 共用同一套降级规则。
pub fn blocks_to_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut push = |text: &str| {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    };
    for block in blocks {
        match block {
            Block::Heading(_, text) | Block::Paragraph(text) | Block::Code(text) => push(text),
            Block::List(items) => {
                for item in items {
                    push(&item.text);
                }
            }
            Block::Image { .. } => {}
            Block::Divider => push("———"),
            Block::Table(table) => push(&table_to_text(table)),
            Block::Math { text, .. } => push(text),
        }
    }
    out
}

/// 正文 HTML → 纯文本。保留段落换行，表格按行、公式按回退文本输出。
pub fn to_text(html: &str) -> String {
    blocks_to_text(&to_blocks(html))
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
            // 换行只在“块级文本”里当分段：纯文本正文、`<div>一行\n两行</div>`。
            // 行内元素里的换行（排版过的 XHTML、MathML 的 `<mi>`/`<mo>` 之间）只是
            // 空白，必须折叠成空格；否则每个符号都会变成独立段落。
            if !text_breaks_paragraphs(&node) {
                b.push(&t.text);
                return;
            }
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
            if name == "svg" {
                b.flush();
                walk_svg(node, b);
                b.flush();
                return;
            }
            if name == "table" {
                if let Some(table) = table_of(&node) {
                    b.flush();
                    b.blocks.push(Block::Table(table));
                    b.flush();
                    return;
                }
                // 版式表（1 行 2 图、单格公式等）继续按普通内容走。
            }
            if name == "math" {
                let (mathml, text) = math_of(&node);
                if is_block_math(&node) && (!mathml.is_empty() || !text.is_empty()) {
                    b.flush();
                    b.blocks.push(Block::Math { mathml, text });
                    b.flush();
                } else if !text.is_empty() {
                    b.push(&text);
                }
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
            if name == "hr" {
                b.flush();
                b.blocks.push(Block::Divider);
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
            if name == "sub" || name == "sup" {
                b.push(&shift_script(&inline_text(&node), name == "sub"));
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
                if name == "math" {
                    out.push_str(&math_text(n));
                    continue;
                }
                if name == "sub" || name == "sup" {
                    out.push_str(&shift_script(&inline_text(&n), name == "sub"));
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
                if name == "math" {
                    out.push_str(&math_text(n));
                    continue;
                }
                if name == "sub" || name == "sup" {
                    out.push_str(&shift_script(&inline_text(&n), name == "sub"));
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
        "script" | "style" | "noscript" | "iframe" | "video" | "audio" | "source" | "rt" | "rp"
    )
}

/// `<sub>` / `<sup>` 的纯文本降级：数字与常见运算符映射成 Unicode 上下标。
/// 字母保持原样，避免破坏搜索（`1st` 不会变成 `1ˢᵗ`）。
fn shift_script(text: &str, sub: bool) -> String {
    text.chars()
        .map(|c| match (sub, c) {
            (false, '0') => '⁰',
            (false, '1') => '¹',
            (false, '2') => '²',
            (false, '3') => '³',
            (false, '4') => '⁴',
            (false, '5') => '⁵',
            (false, '6') => '⁶',
            (false, '7') => '⁷',
            (false, '8') => '⁸',
            (false, '9') => '⁹',
            (false, '+') => '⁺',
            (false, '-') => '⁻',
            (false, '=') => '⁼',
            (false, '(') => '⁽',
            (false, ')') => '⁾',
            (true, '0') => '₀',
            (true, '1') => '₁',
            (true, '2') => '₂',
            (true, '3') => '₃',
            (true, '4') => '₄',
            (true, '5') => '₅',
            (true, '6') => '₆',
            (true, '7') => '₇',
            (true, '8') => '₈',
            (true, '9') => '₉',
            (true, '+') => '₊',
            (true, '-') => '₋',
            (true, '=') => '₌',
            (true, '(') => '₍',
            (true, ')') => '₎',
            _ => c,
        })
        .collect()
}

/// SVG 里只关心 `<image>` 与 `<text>`；`defs` 等定义树不参与正文。
fn is_svg_skip(name: &str) -> bool {
    matches!(
        name,
        "defs"
            | "style"
            | "script"
            | "title"
            | "desc"
            | "metadata"
            | "clippath"
            | "mask"
            | "pattern"
            | "lineargradient"
            | "radialgradient"
            | "filter"
            | "marker"
            | "symbol"
            | "use"
    )
}

// ---------------------------------------------------------------------------
// SVG：固定版式书把文字画在 <text>/<tspan>，封面/扫描页放在 <image>
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SvgLine {
    x: f64,
    y: f64,
    text: String,
}

fn walk_svg(node: NodeRef<'_, Node>, b: &mut Builder) {
    for child in node.children() {
        let Node::Element(el) = child.value() else {
            continue;
        };
        let name = el.name();
        if is_svg_skip(name) {
            continue;
        }
        if name == "image" {
            if let Some((src, alt)) = svg_image_of(el) {
                b.blocks.push(Block::Image { src, alt });
            }
            continue;
        }
        if name == "text" {
            let (tx, ty) = transform_origin(el);
            let base_x = tx + first_number(el, "x").unwrap_or(0.0);
            let base_y = ty + first_number(el, "y").unwrap_or(0.0);
            let mut lines = Vec::new();
            collect_svg_text(child, base_x, base_y, &mut lines);
            for text in svg_lines_to_paragraphs(lines) {
                b.blocks.push(Block::Paragraph(text));
            }
            continue;
        }
        walk_svg(child, b);
    }
}

/// 跨命名空间取属性：SVG 的 `xlink:href` 在 scraper 里是带命名空间的 `href`，
/// `Element::attr("href")` 只找无命名空间属性，会漏掉封面图。
fn attr_any_ns(el: &Element, name: &str) -> Option<String> {
    let suffix = format!(":{name}");
    el.attrs()
        .find(|(k, _)| {
            let key = k.to_string();
            key == name || key.ends_with(&suffix)
        })
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn svg_image_of(el: &Element) -> Option<(String, String)> {
    let src = attr_any_ns(el, "href").or_else(|| attr_any_ns(el, "src"))?;
    if !valid_img_src(&src) {
        return None;
    }
    if let (Some(w), Some(h)) = (dim_attr(el, "width"), dim_attr(el, "height")) {
        if w <= 2 && h <= 2 {
            return None;
        }
    }
    let alt = attr(el, "alt")
        .or_else(|| attr(el, "aria-label"))
        .unwrap_or_default();
    Some((src, alt.chars().take(200).collect()))
}

fn collect_svg_text(node: NodeRef<'_, Node>, x: f64, y: f64, out: &mut Vec<SvgLine>) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => {
                let text = collapse(&t.text);
                if !text.is_empty() {
                    out.push(SvgLine { x, y, text });
                }
            }
            Node::Element(el) => {
                let name = el.name();
                if is_svg_skip(name) {
                    continue;
                }
                if name == "tspan" {
                    let tx = first_number(el, "x").unwrap_or(0.0);
                    let ty = first_number(el, "y").unwrap_or(0.0);
                    let nx = x + tx;
                    let ny = y + ty;
                    let has_elements = child
                        .children()
                        .any(|c| matches!(c.value(), Node::Element(_)));
                    if has_elements {
                        collect_svg_text(child, nx, ny, out);
                    } else {
                        let text = collapse(&inline_text(&child));
                        if !text.is_empty() {
                            out.push(SvgLine { x: nx, y: ny, text });
                        }
                    }
                    continue;
                }
                collect_svg_text(child, x, y, out);
            }
            _ => {}
        }
    }
}

/// 同一 `<text>` 里的多行按行距分组：行距突然变大或缩进变化就另起一段。
fn svg_lines_to_paragraphs(lines: Vec<SvgLine>) -> Vec<String> {
    let mut lines: Vec<SvgLine> = lines
        .into_iter()
        .filter(|l| !l.text.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut gaps: Vec<f64> = lines
        .windows(2)
        .map(|w| w[1].y - w[0].y)
        .filter(|g| *g > 0.0 && *g < 1000.0)
        .collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 最小行距比中位数稳：段落之间的大间距不会把基准抬高到看不出断段。
    let base_gap = gaps.first().copied().unwrap_or(15.0).max(1.0);

    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<SvgLine> = None;
    for line in lines.drain(..) {
        let brk = prev.as_ref().is_some_and(|p| {
            let gap = line.y - p.y;
            gap < -1.0 || gap > base_gap * 1.75 || (line.x - p.x).abs() > 4.0
        });
        if brk && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            join_svg_text(&mut cur, &line.text);
        } else {
            cur = line.text.trim().to_string();
        }
        prev = Some(line);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn join_svg_text(out: &mut String, next: &str) {
    let next = next.trim();
    if next.is_empty() {
        return;
    }
    let last = out.chars().last();
    let first = next.chars().next();
    if out.ends_with('-') && first.is_some_and(|c| c.is_ascii_lowercase()) {
        out.pop();
    } else if last.is_some_and(is_cjk) || first.is_some_and(is_cjk) {
        // 中日韩文字换行不加空格
    } else {
        out.push(' ');
    }
    out.push_str(next);
}

fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    ) || matches!(
        c,
        '\u{3001}'
            | '\u{3002}'
            | '\u{FF0C}'
            | '\u{FF1A}'
            | '\u{FF1B}'
            | '\u{FF1F}'
            | '\u{FF01}'
            | '\u{300A}'
            | '\u{300B}'
            | '\u{3008}'
            | '\u{3009}'
    )
}

fn first_number(el: &Element, name: &str) -> Option<f64> {
    let raw = el.attr(name)?;
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .find_map(|s| s.trim().parse::<f64>().ok())
}

/// `<text transform="matrix(a b c d e f)">` / `translate(x y)` 的平移量。
fn transform_origin(el: &Element) -> (f64, f64) {
    let Some(raw) = el.attr("transform") else {
        return (0.0, 0.0);
    };
    let lower = raw.to_ascii_lowercase();
    for (head, count) in [("matrix(", 6usize), ("translate(", 2)] {
        if let Some(start) = lower.find(head) {
            let rest = &raw[start + head.len()..];
            let end = rest.find(')').unwrap_or(rest.len());
            let nums: Vec<f64> = rest[..end]
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|s| s.trim().parse::<f64>().ok())
                .collect();
            if nums.len() >= count {
                return if count == 6 {
                    (nums[4], nums[5])
                } else {
                    (nums[0], nums[1])
                };
            }
        }
    }
    (0.0, 0.0)
}

// ---------------------------------------------------------------------------
// 表格：只收多行多列、无图片的数据表
// ---------------------------------------------------------------------------

fn table_of(node: &NodeRef<'_, Node>) -> Option<Table> {
    if has_image_descendant(*node) {
        return None;
    }
    let mut rows = Vec::new();
    collect_table_rows(*node, false, &mut rows);
    if rows.len() < 2 {
        return None;
    }
    let cols = rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.colspan.max(1) as usize)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    if cols < 2 {
        return None;
    }
    let caption = first_descendant_text(*node, "caption").filter(|s| !s.is_empty());
    Some(Table { caption, rows })
}

fn collect_table_rows(node: NodeRef<'_, Node>, in_thead: bool, out: &mut Vec<TableRow>) {
    for child in node.children() {
        let Node::Element(el) = child.value() else {
            continue;
        };
        match el.name() {
            "thead" => collect_table_rows(child, true, out),
            "tbody" | "tfoot" => collect_table_rows(child, false, out),
            "tr" => {
                if let Some(row) = table_row_of(child, in_thead) {
                    out.push(row);
                }
            }
            "table" => {}
            _ => collect_table_rows(child, in_thead, out),
        }
    }
}

fn table_row_of(node: NodeRef<'_, Node>, in_thead: bool) -> Option<TableRow> {
    let mut cells = Vec::new();
    for child in node.children() {
        let Node::Element(el) = child.value() else {
            continue;
        };
        let name = el.name();
        if name != "td" && name != "th" {
            continue;
        }
        let text = collapse(&inline_text(&child));
        cells.push(TableCell {
            text,
            colspan: span_attr(el, "colspan"),
            rowspan: span_attr(el, "rowspan"),
            header: name == "th" || in_thead,
        });
    }
    if cells.is_empty() {
        return None;
    }
    let header = in_thead || cells.iter().all(|c| c.header);
    Some(TableRow { header, cells })
}

fn span_attr(el: &Element, name: &str) -> u8 {
    el.attr(name)
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(1)
        .clamp(1, 64) as u8
}

fn first_descendant_text(node: NodeRef<'_, Node>, tag: &str) -> Option<String> {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if el.name() == tag {
                let text = collapse(&inline_text(&child));
                return Some(text);
            }
            if let Some(found) = first_descendant_text(child, tag) {
                return Some(found);
            }
        }
    }
    None
}

fn has_image_descendant(node: NodeRef<'_, Node>) -> bool {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if matches!(el.name(), "img" | "image") || has_image_descendant(child) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// MathML：原始标记给浏览器，纯文本给 /k、导出与搜索
// ---------------------------------------------------------------------------

fn is_block_math(node: &NodeRef<'_, Node>) -> bool {
    let Node::Element(node_el) = node.value() else {
        return false;
    };
    let display = node_el
        .attr("display")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if display == "block" {
        return true;
    }
    if display == "inline" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return true;
    };
    let Node::Element(el) = parent.value() else {
        return true;
    };
    let name = el.name();
    if !(is_block(name) || matches!(name, "td" | "th" | "li")) {
        return false;
    }
    let mut significant = 0usize;
    for child in parent.children() {
        match child.value() {
            Node::Element(_) => significant += 1,
            Node::Text(t) if !t.text.trim().is_empty() => significant += 1,
            _ => {}
        }
    }
    significant <= 1
}

fn math_of(node: &NodeRef<'_, Node>) -> (String, String) {
    let text = collapse(&math_text(*node));
    let mut mathml = String::new();
    mathml_node(*node, &mut mathml);
    if mathml.len() > 32 * 1024 {
        mathml.clear();
    }
    (mathml, text)
}

fn math_text(node: NodeRef<'_, Node>) -> String {
    let Node::Element(el) = node.value() else {
        return collapse(&inline_text(&node));
    };
    let name = el.name();
    let children = || -> Vec<String> {
        node.children()
            .filter_map(|c| {
                let s = math_text(c);
                (!s.trim().is_empty()).then_some(s)
            })
            .collect()
    };
    match name {
        "annotation" | "annotation-xml" | "none" | "mspace" => {
            if name == "mspace" {
                " ".to_string()
            } else {
                String::new()
            }
        }
        "mi" | "mn" | "mo" | "mtext" | "ms" | "mglyph" => collapse(&inline_text(&node)),
        "semantics" => node
            .children()
            .find(|c| {
                c.value()
                    .as_element()
                    .is_some_and(|el| !matches!(el.name(), "annotation" | "annotation-xml"))
            })
            .map(math_text)
            .unwrap_or_default(),
        "mfrac" => {
            let c = children();
            format!(
                "({})/({})",
                c.first().map(String::as_str).unwrap_or(""),
                c.get(1).map(String::as_str).unwrap_or("")
            )
        }
        "msup" | "msub" | "mover" | "munder" => {
            let c = children();
            let base = c.first().map(String::as_str).unwrap_or("");
            let script = c.get(1).map(String::as_str).unwrap_or("");
            let op = if name == "msup" || name == "mover" {
                "^"
            } else {
                "_"
            };
            format!("{base}{op}{}", script_paren(script))
        }
        "msubsup" | "munderover" => {
            let c = children();
            let base = c.first().map(String::as_str).unwrap_or("");
            let sub = c.get(1).map(String::as_str).unwrap_or("");
            let sup = c.get(2).map(String::as_str).unwrap_or("");
            format!("{base}_{}{}", script_paren(sub), script_paren(sup))
        }
        "msqrt" => format!("√({})", children().join(" ")),
        "mroot" => {
            let c = children();
            format!(
                "√[{}]({})",
                c.get(1).map(String::as_str).unwrap_or(""),
                c.first().map(String::as_str).unwrap_or("")
            )
        }
        "mfenced" => {
            let open = el.attr("open").unwrap_or("(");
            let close = el.attr("close").unwrap_or(")");
            let sep = el.attr("separators").unwrap_or(",");
            format!("{open}{}{close}", children().join(sep))
        }
        "mtable" => {
            let mut rows = Vec::new();
            for c in node.children() {
                if c.value().as_element().is_some_and(|el| el.name() == "mtr") {
                    rows.push(math_text(c));
                }
            }
            rows.join("; ")
        }
        "mtr" => {
            let mut cells = Vec::new();
            for c in node.children() {
                if c.value().as_element().is_some_and(|el| el.name() == "mtd") {
                    cells.push(math_text(c));
                }
            }
            cells.join(" | ")
        }
        _ => children().join(" "),
    }
}

fn script_paren(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 1 {
        s.to_string()
    } else {
        format!("({s})")
    }
}

fn mathml_node(node: NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => out.push_str(&crate::xml::xml_escape(t.text.as_ref())),
        Node::Element(el) => {
            let name = el.name();
            if !matches!(
                name,
                "math"
                    | "mrow"
                    | "mi"
                    | "mn"
                    | "mo"
                    | "mtext"
                    | "ms"
                    | "mspace"
                    | "mfrac"
                    | "msqrt"
                    | "mroot"
                    | "mstyle"
                    | "merror"
                    | "mpadded"
                    | "mphantom"
                    | "mtable"
                    | "mtr"
                    | "mtd"
                    | "munder"
                    | "mover"
                    | "munderover"
                    | "msub"
                    | "msup"
                    | "msubsup"
                    | "mfenced"
                    | "menclose"
                    | "semantics"
                    | "annotation"
                    | "mmultiscripts"
                    | "mprescripts"
                    | "none"
                    | "maligngroup"
                    | "malignmark"
                    | "maction"
                    | "mglyph"
            ) {
                return;
            }
            out.push('<');
            out.push_str(name);
            if name == "math" && el.attr("xmlns").is_none() {
                out.push_str(" xmlns=\"http://www.w3.org/1998/Math/MathML\"");
            }
            for (attr_name, value) in el.attrs() {
                if mathml_attr_allowed(attr_name) {
                    out.push(' ');
                    out.push_str(attr_name);
                    out.push_str("=\"");
                    out.push_str(&crate::xml::xml_escape(value));
                    out.push('"');
                }
            }
            out.push('>');
            for child in node.children() {
                mathml_node(child, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        _ => {}
    }
}

fn mathml_attr_allowed(name: &str) -> bool {
    if name.starts_with("on") || name.contains("href") {
        return false;
    }
    matches!(
        name,
        "xmlns"
            | "display"
            | "mathvariant"
            | "displaystyle"
            | "scriptlevel"
            | "columnalign"
            | "rowalign"
            | "columnlines"
            | "rowlines"
            | "columnspacing"
            | "rowspacing"
            | "frame"
            | "fence"
            | "separators"
            | "open"
            | "close"
            | "stretchy"
            | "largeop"
            | "movablelimits"
            | "accent"
            | "accentunder"
            | "linethickness"
            | "bevelled"
            | "notation"
            | "encoding"
            | "class"
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
            | "figure"
            | "figcaption"
            | "details"
            | "summary"
            | "address"
            | "hgroup"
            | "center"
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

/// 文本节点里的换行是否应当分段。
///
/// 只有直接挂在块级容器（`p`/`div`/`td`…）或文档根上、且该容器里没有任何
/// 子元素的文本才按换行分段（纯文本正文、`<div>一行\n两行</div>`）。
/// 一旦块里有行内元素（`<b>`、MathML、`<img>`…），换行就只是源码排版，
/// 必须折叠成空格；否则排版过的 XHTML 会把一句话拆成多段，公式会一个符号一段。
fn text_breaks_paragraphs(node: &NodeRef<'_, Node>) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    match parent.value() {
        Node::Element(el) => {
            let name = el.name();
            let block = is_block(name) || name == "body" || name == "html";
            block
                && !parent
                    .children()
                    .any(|c| matches!(c.value(), Node::Element(_)))
        }
        Node::Document | Node::Fragment => true,
        _ => true,
    }
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

    #[test]
    fn newlines_inside_inline_markup_are_spaces() {
        let blocks = to_blocks("<p>Hello <b>world\nagain</b> end</p>");
        assert_eq!(
            blocks,
            vec![Block::Paragraph("Hello world again end".into())]
        );
    }

    #[test]
    fn newlines_in_block_text_still_split_paragraphs() {
        let blocks = to_blocks("<div>第一行\n第二行</div>");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("第一行".into()),
                Block::Paragraph("第二行".into()),
            ]
        );
    }

    // 真实 EPUB 的 MathML 是逐元素排版的；换行不能被当成段落分隔，否则矩阵会
    // 变成一个符号一段。公式独占一个块时保留为 Math，符号之间仍保留空格。
    #[test]
    fn pretty_printed_mathml_stays_in_one_block() {
        let blocks = to_blocks(
            "<p><math>\n  <mrow>\n    <mi>A</mi>\n    <mo>=</mo>\n    <mn>1</mn>\n  </mrow>\n</math></p>",
        );
        assert!(matches!(
            &blocks[0],
            Block::Math { text, .. } if text == "A = 1"
        ));
    }

    // 含行内元素的块里，源码换行只是排版，不能把一句话拆成多段。
    #[test]
    fn pretty_printed_prose_with_inline_math_stays_one_paragraph() {
        let blocks = to_blocks(
            "<p>Suppose\n<math><mi>A</mi></math>\nis a square matrix\nof size <math><mi>n</mi></math>.</p>",
        );
        assert_eq!(
            blocks,
            vec![Block::Paragraph(
                "Suppose A is a square matrix of size n.".into()
            )]
        );
    }

    #[test]
    fn real_tables_keep_header_cells_and_spans() {
        let blocks = to_blocks(
            "<table><caption>参数</caption>\
             <thead><tr><th>名称</th><th colspan=\"2\">值</th></tr></thead>\
             <tbody><tr><td rowspan=\"2\">A</td><td>1</td><td>2</td></tr>\
             <tr><td>3</td><td>4</td></tr></tbody></table>",
        );
        let Block::Table(table) = &blocks[0] else {
            panic!("应识别为数据表: {blocks:?}");
        };
        assert_eq!(table.caption.as_deref(), Some("参数"));
        assert_eq!(table.rows.len(), 3);
        assert!(table.rows[0].header);
        assert_eq!(table.rows[0].cells[1].colspan, 2);
        assert_eq!(table.rows[1].cells[0].rowspan, 2);
        assert_eq!(table.rows[1].cells[1].text, "1");
    }

    #[test]
    fn layout_tables_and_single_cell_wrappers_are_not_data_tables() {
        let blocks = to_blocks(
            "<table><tr><td><img src=\"a.jpg\"></td><td><img src=\"b.jpg\"></td></tr></table>\
             <table><tr><td><math><mi>x</mi></math></td></tr></table>",
        );
        assert!(!blocks.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Image { src, .. } if src == "a.jpg")));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, Block::Math { text, .. } if text == "x")));
    }

    #[test]
    fn block_and_inline_math_are_distinguished() {
        let blocks = to_blocks(
            "<div><math display=\"block\"><mrow><mi>A</mi><mo>=</mo><mn>1</mn></mrow></math></div>\
             <p>当 <math><mi>n</mi><mo>&gt;</mo><mn>0</mn></math> 时成立。</p>",
        );
        assert!(matches!(
            &blocks[0],
            Block::Math { text, mathml }
                if text == "A = 1" && mathml.contains("<math") && mathml.contains("<mi>A</mi>")
        ));
        assert_eq!(blocks[1], Block::Paragraph("当 n > 0 时成立。".into()));
    }

    #[test]
    fn svg_text_lines_and_images_are_recovered() {
        let blocks = to_blocks(
            "<div class=\"page\"><svg xmlns=\"http://www.w3.org/2000/svg\" \
             xmlns:xlink=\"http://www.w3.org/1999/xlink\">\
             <image xlink:href=\"../Images/cover.jpeg\" width=\"600\" height=\"800\"/>\
             <text transform=\"matrix(1 0 0 1 56.6929 104.567)\">\
               <tspan x=\"0\" y=\"0\">le vent serait</tspan>\
               <tspan x=\"0\" y=\"15.249\">chaud.</tspan>\
               <tspan x=\"12.99\" y=\"45.747\">Nouveau paragraphe</tspan>\
             </text></svg></div>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Image {
                    src: "../Images/cover.jpeg".into(),
                    alt: "".into()
                },
                Block::Paragraph("le vent serait chaud.".into()),
                Block::Paragraph("Nouveau paragraphe".into()),
            ]
        );
    }

    #[test]
    fn table_and_math_serialize_to_xhtml() {
        let blocks = to_blocks(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>\
             <div><math display=\"block\"><mfrac><mn>1</mn><mn>2</mn></mfrac></math></div>",
        );
        let html = blocks
            .iter()
            .map(|b| match b {
                Block::Table(t) => super::table_to_html(t),
                Block::Math { mathml, text } => super::math_to_html(mathml, text),
                _ => String::new(),
            })
            .collect::<String>();
        assert!(html.contains("<table"));
        assert!(html.contains("<th colspan=\"1\""));
        assert!(html.contains("<math"));
        assert_eq!(super::to_blocks(&html).len(), 2);
    }

    #[test]
    fn ruby_annotation_text_is_not_duplicated() {
        let blocks = to_blocks("<p>漢<ruby>字<rt>じ</rt><rp>(</rp></ruby>です。</p>");
        assert_eq!(blocks, vec![Block::Paragraph("漢字です。".into())]);
    }

    #[test]
    fn figure_caption_and_details_stay_separate_blocks() {
        let blocks = to_blocks(
            "<div>前<figure><img src=\"a.jpg\"/><figcaption>图 1</figcaption></figure>后</div>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("前".into()),
                Block::Image {
                    src: "a.jpg".into(),
                    alt: "".into()
                },
                Block::Paragraph("图 1".into()),
                Block::Paragraph("后".into()),
            ]
        );
        let blocks = to_blocks("<details><summary>展开</summary><p>内容</p></details>");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("展开".into()),
                Block::Paragraph("内容".into()),
            ]
        );
    }

    #[test]
    fn hr_becomes_divider() {
        let blocks = to_blocks("<p>上</p><hr/><p>下</p>");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("上".into()),
                Block::Divider,
                Block::Paragraph("下".into()),
            ]
        );
        assert_eq!(super::blocks_to_text(&blocks), "上\n———\n下");
    }

    #[test]
    fn sub_and_sup_degrade_to_unicode_digits() {
        let blocks = to_blocks(
            "<p>H<sub>2</sub>O、x<sup>2</sup>、10<sup>-3</sup>，1<sup>st</sup> 保持原样。</p>\
             <h2>E=mc<sup>2</sup></h2>\
             <ul><li>CO<sub>2</sub></li></ul>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph("H₂O、x²、10⁻³，1st 保持原样。".into()),
                Block::Heading(2, "E=mc²".into()),
                Block::List(vec![super::ListItem {
                    level: 0,
                    ordered: false,
                    text: "CO₂".into(),
                }]),
            ]
        );
    }

    #[test]
    fn self_closing_pagebreak_does_not_swallow_following_text() {
        // HTML 解析器不认非空元素的 `/>`：页码标记会把后续文字吞进自己的子树。
        // 因此不能按 `epub:type` 整块跳过，只能保留内容。
        let blocks = to_blocks("<p>甲<span role=\"doc-pagebreak\" id=\"p2\"/>乙</p>");
        assert_eq!(blocks, vec![Block::Paragraph("甲乙".into())]);
    }
}
