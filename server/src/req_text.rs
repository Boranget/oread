//! req 的字符集嗅探与响应解码（move-only 从 req.rs 拆出）。

use super::*;

// ---------------- 响应解码 ----------------

/// 从 Content-Type 里取 charset（大小写不敏感，允许引号）。
pub(super) fn charset_from_content_type(ct: &str) -> Option<String> {
    let lower = ct.to_ascii_lowercase();
    let pos = lower.find("charset=")?;
    let rest = ct[pos + "charset=".len()..].trim_start();
    let rest = rest.trim_start_matches('"').trim_start_matches('\'');
    let end = rest
        .find(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let label = rest[..end].trim();
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

/// 从 HTML 头部（前 4KB）嗅探 `<meta charset=...>`；覆盖无 Content-Type charset 的站点。
pub(super) fn meta_charset(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(&buf[..buf.len().min(4096)]);
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("charset=") {
        let pos = from + rel + "charset=".len();
        let rest = &text[pos..];
        let rest = rest.trim_start_matches('"').trim_start_matches('\'');
        let end = rest.find([';', '"', '\'', '>', '/']).unwrap_or(rest.len());
        let label = rest[..end].trim();
        if !label.is_empty() {
            return Some(label.to_string());
        }
        from = pos;
    }
    None
}

pub(super) fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().trim_matches('"').trim_matches('\'').as_bytes())
}

pub(super) fn decode_with(enc: &'static Encoding, buf: &[u8]) -> (String, bool) {
    let (cow, _, had_errors) = enc.decode(buf);
    (cow.into_owned(), had_errors)
}

/// 常见汉字（简/繁）集合：无 charset 时用“常用字命中率”区分 GB18030 与 Big5。
/// 正确解码得到的正文里高频字占比明显更高，错配编码只会得到零散生僻字。
pub(super) const COMMON_HAN: &str = "的一是在不了有和人这中大为上个国我以要他时来用们生到作地于出就分对成会可主发年动同工也能下过子说产种面而方后多定行学法所民得经十三之进着等部度家电力里如水化高自二理起小物现实加量都两体制机当使点从业本去把性好应开它合还因由其些然前外天政四日那社义事平形相全表间样与关各重新线内数正心反你明看原又么利比或但质气第向道命此变条只没结解问意建月公无系军很情者最立代想已通并提直题程展果料象员位入常文总次品式活设及管特件长求老头基资边流路级少图山统接知较将组见计别手角期根论运农指几九区强放决西被干做必战先回则任取据处理世车清器美收千称德精板精這們來個國為時會說產種對開關聯實體機發現應該還從業東動過學機關電點錢書語運進連樣見處禮樂龍馬鳥魚車長門問間陽風飛養習寫讀聽覺氣華萬歲廣";

/// 文本可信度：ASCII / 常用汉字权重更高；替换字符（U+FFFD）直接拉低分数。
pub(super) fn plausibility(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut good = 0f64;
    let mut total = 0usize;
    for ch in text.chars() {
        total += 1;
        if ch == '\u{FFFD}' {
            continue;
        }
        if ch.is_ascii() && !ch.is_control() {
            good += 1.0;
        } else if ('\u{4E00}'..='\u{9FFF}').contains(&ch) {
            good += if COMMON_HAN.contains(ch) { 3.0 } else { 1.0 };
        } else if ('\u{3000}'..='\u{303F}').contains(&ch) || ('\u{FF00}'..='\u{FFEF}').contains(&ch)
        {
            good += 2.0;
        }
    }
    good / total as f64
}

/// 解码响应体：Content-Type charset 优先，其次 meta charset，最后 UTF-8/GB18030/Big5 嗅探。
/// 解码失败一律具名报错，绝不 `from_utf8_lossy` 静默出乱码。
pub(super) fn decode_body(buf: &[u8], content_type: Option<&str>) -> anyhow::Result<String> {
    if buf.is_empty() {
        return Ok(String::new());
    }
    let declared = content_type
        .and_then(charset_from_content_type)
        .or_else(|| meta_charset(buf));
    if let Some(label) = declared {
        let Some(enc) = encoding_for_label(&label) else {
            return Err(anyhow!("响应解码失败：不支持的字符集 {label}"));
        };
        let (text, had_errors) = decode_with(enc, buf);
        if had_errors {
            return Err(anyhow!(
                "响应解码失败：按 charset={label} 解码出现非法字节（内容长度 {} 字节）",
                buf.len()
            ));
        }
        return Ok(text);
    }
    // 无 charset：先做严格 UTF-8（最可靠），失败再在 GB18030 / Big5 之间择优
    if let Ok(s) = std::str::from_utf8(buf) {
        return Ok(s.to_string());
    }
    let mut best: Option<(f64, &'static Encoding, String)> = None;
    for enc in [GB18030, BIG5] {
        let (text, had_errors) = decode_with(enc, buf);
        let score = plausibility(&text) - if had_errors { 1.0 } else { 0.0 };
        if best.as_ref().map(|(b, _, _)| score > *b).unwrap_or(true) {
            best = Some((score, enc, text));
        }
    }
    match best {
        Some((score, _, text)) if score > 0.0 => Ok(text),
        _ => Err(anyhow!(
            "响应解码失败：未声明 charset，且字节流不符合 UTF-8/GB18030/Big5"
        )),
    }
}
