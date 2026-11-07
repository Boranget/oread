//! rule_engine HTML 辅助的jsoup 文本归一化（move-only 从 rule_engine_html.rs 拆出）。

pub(in crate::rule_engine) fn is_jsoup_block_tag(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "frameset"
            | "script"
            | "noscript"
            | "style"
            | "meta"
            | "link"
            | "title"
            | "frame"
            | "noframes"
            | "section"
            | "nav"
            | "aside"
            | "hgroup"
            | "header"
            | "footer"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "div"
            | "blockquote"
            | "hr"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "ins"
            | "del"
            | "dl"
            | "dt"
            | "dd"
            | "li"
            | "table"
            | "caption"
            | "thead"
            | "tfoot"
            | "tbody"
            | "colgroup"
            | "col"
            | "tr"
            | "th"
            | "td"
            | "video"
            | "audio"
            | "canvas"
            | "details"
            | "menu"
            | "plaintext"
            | "template"
            | "article"
            | "main"
            | "svg"
            | "math"
            | "center"
            | "dir"
            | "applet"
            | "marquee"
            | "listing"
    )
}

/// jsoup `Tag.preserveWhitespaceTags`（1.16.2）。
pub(in crate::rule_engine) fn is_jsoup_preserve_ws_tag(name: &str) -> bool {
    matches!(name, "pre" | "plaintext" | "title" | "textarea")
}

/// jsoup `Tag.formatAsInlineTags`（1.16.2）：块级标签但按行内方式排版，
/// `Element.text()` 的 tail 阶段不会在这些元素前补空格。
pub(in crate::rule_engine) fn is_jsoup_format_inline_tag(name: &str) -> bool {
    matches!(
        name,
        "title"
            | "a"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "address"
            | "li"
            | "th"
            | "td"
            | "script"
            | "style"
            | "ins"
            | "del"
            | "s"
    )
}

/// jsoup 的 `isActuallyWhitespace`（只认这几个 ASCII 空白，不把 NBSP/全角空格算进去）。
pub(in crate::rule_engine) fn is_jsoup_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C')
}

pub(in crate::rule_engine) fn jsoup_last_char_is_ws(accum: &str) -> bool {
    accum.chars().last().map(is_jsoup_ws).unwrap_or(false)
}

/// jsoup `TextNode.appendNormalisedWhitespace`：连续空白折叠成一个空格，
/// `accum` 已以空白结尾时跳过前导空白。
pub(in crate::rule_engine) fn append_jsoup_normalised(accum: &mut String, text: &str) {
    let strip_leading = jsoup_last_char_is_ws(accum);
    let mut last_white = false;
    let mut reached_non_white = false;
    for c in text.chars() {
        if is_jsoup_ws(c) {
            if strip_leading && !reached_non_white {
                continue;
            }
            if last_white {
                continue;
            }
            accum.push(' ');
            last_white = true;
        } else {
            accum.push(c);
            reached_non_white = true;
            last_white = false;
        }
    }
}

/// jsoup `Element.text()`（1.16.2）的等价实现：块级元素/`br` 前后补空格、
/// 空白按 `appendNormalisedWhitespace` 折叠、`pre/plaintext/title/textarea` 保留原样。
pub(in crate::rule_engine) fn jsoup_text(el: scraper::ElementRef<'_>) -> String {
    let mut accum = String::new();
    // (节点, 是否出栈, 是否处于 preserveWhitespace 子树)
    let mut stack = vec![(*el, false, is_jsoup_preserve_ws_tag(el.value().name()))];
    while let Some((node, exiting, preserve)) = stack.pop() {
        if exiting {
            // jsoup tail：块级元素结束且下一个兄弟是文本或行内元素时补空格
            if let Some(e) = node.value().as_element() {
                if is_jsoup_block_tag(e.name())
                    && !accum.is_empty()
                    && !jsoup_last_char_is_ws(&accum)
                {
                    let need_space = match node.next_sibling() {
                        Some(n) => match n.value() {
                            scraper::node::Node::Text(_) => true,
                            scraper::node::Node::Element(ne) => {
                                !is_jsoup_format_inline_tag(ne.name())
                            }
                            _ => false,
                        },
                        None => false,
                    };
                    if need_space {
                        accum.push(' ');
                    }
                }
            }
            continue;
        }
        match node.value() {
            scraper::node::Node::Text(t) => {
                let s = t.text.to_string();
                if preserve {
                    accum.push_str(&s);
                } else {
                    append_jsoup_normalised(&mut accum, &s);
                }
            }
            scraper::node::Node::Element(e) => {
                let name = e.name();
                // jsoup head：进入块级元素（或 br）时，若已有内容且不以空白结尾则补空格
                if !accum.is_empty()
                    && (is_jsoup_block_tag(name) || name == "br")
                    && !jsoup_last_char_is_ws(&accum)
                {
                    accum.push(' ');
                }
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                let child_preserve = preserve || is_jsoup_preserve_ws_tag(name);
                for child in kids.into_iter().rev() {
                    stack.push((child, false, child_preserve));
                }
            }
            _ => {
                stack.push((node, true, preserve));
                let kids: Vec<_> = node.children().collect();
                for child in kids.into_iter().rev() {
                    stack.push((child, false, preserve));
                }
            }
        }
    }
    accum.trim().to_string()
}
