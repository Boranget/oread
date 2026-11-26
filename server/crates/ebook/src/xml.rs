//! 最小 XML 工具：转义，以及解析前清理真实 EPUB 里常见的“合法但严格解析器不接受”的写法。

use std::borrow::Cow;

/// 转义文本节点与属性值里必须处理的五个字符。
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// 解析前去掉 UTF-8 BOM 与 `<!DOCTYPE ...>` 声明。
///
/// 真实 EPUB 里常见：
/// - `container.xml` 带 BOM（部分生成器默认写 BOM）；
/// - XHTML 1.1 章节带 `<!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.1//EN' ...>`，
///   而且 PUBLIC/SYSTEM 用单引号。
///
/// `sxd-document` 会把这两种写法当成语法错误；它们对内容没有影响，读入时清理即可，
/// 不改磁盘上的原文件。没有可清理内容时返回借用，避免热路径复制。
pub fn strip_doctype_and_bom(xml: &str) -> Cow<'_, str> {
    let text = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let Some(first) = find_doctype(text) else {
        return Cow::Borrowed(text);
    };

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut offset = first;
    loop {
        out.push_str(&rest[..offset]);
        let after = skip_doctype(&rest[offset..]);
        rest = &rest[offset + after..];
        match find_doctype(rest) {
            Some(next) => offset = next,
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    Cow::Owned(out)
}

fn find_doctype(text: &str) -> Option<usize> {
    [text.find("<!DOCTYPE"), text.find("<!doctype")]
        .into_iter()
        .flatten()
        .min()
}

/// 从 `<!DOCTYPE` 起点跳到声明结束（`>`），跳过引号与内部子集 `[...]` 里的 `>`。
fn skip_doctype(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut bracket = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                b'>' if bracket == 0 => break,
                _ => {}
            },
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::{strip_doctype_and_bom, xml_escape};
    use std::borrow::Cow;

    #[test]
    fn escapes_all_five() {
        assert_eq!(xml_escape("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&apos;");
    }

    #[test]
    fn strips_utf8_bom() {
        let out = strip_doctype_and_bom("\u{feff}<?xml version=\"1.0\"?><a/>");
        assert_eq!(out, "<?xml version=\"1.0\"?><a/>");
    }

    #[test]
    fn strips_doctype_with_single_quotes() {
        let xml = "<?xml version='1.0'?>\n<!DOCTYPE html PUBLIC '-//W3C//DTD XHTML 1.1//EN' \
                   'http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd'>\n<html/>";
        assert_eq!(
            strip_doctype_and_bom(xml),
            "<?xml version='1.0'?>\n\n<html/>"
        );
    }

    #[test]
    fn strips_doctype_with_internal_subset() {
        let xml = "<!DOCTYPE r [<!ENTITY x \">\">]><r/>";
        assert_eq!(strip_doctype_and_bom(xml), "<r/>");
    }

    #[test]
    fn leaves_clean_xml_borrowed() {
        let xml = "<?xml version=\"1.0\"?><package/>";
        assert!(matches!(strip_doctype_and_bom(xml), Cow::Borrowed(_)));
    }
}
