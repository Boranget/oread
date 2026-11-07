//! 图片代理的 body 部分（move-only 从 image.rs 拆出）。

use super::*;

/// 把整段响应体读入内存（受单图上限约束），仅转码路径使用。
// 返回 Err 时直接复用 axum 的 `Response`（几百字节），调用点无需再包一层错误类型；
// 这条路径只在取图失败时走 `?`，装箱/自定义错误类型反而让调用点变复杂。
#[allow(clippy::result_large_err)]
pub(super) async fn collect_image_body(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, Response> {
    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(error_body(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                &format!("图片大小 {len} 字节，超过上限 {max_bytes} 字节"),
            ));
        }
    }
    let mut raw = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = raw.next().await {
        match chunk {
            Ok(c) => {
                if buf.len() + c.len() > max_bytes {
                    return Err(error_body(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "image_too_large",
                        &format!("图片超过上限 {max_bytes} 字节，已拒绝转发"),
                    ));
                }
                buf.extend_from_slice(&c);
            }
            Err(e) => {
                return Err(error_body(
                    StatusCode::BAD_GATEWAY,
                    "upstream_error",
                    &format!("读取上游图片失败: {e}"),
                ))
            }
        }
    }
    Ok(buf)
}

/// 图片像素尺寸（服务端解析后随响应头返回，前端无需 object-fit/transform 即可排版）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dims {
    pub width: u32,
    pub height: u32,
}

pub(super) fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

pub(super) fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

pub(super) fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

pub(super) fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    b.get(at..at + 4)
        .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(super) fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(super) fn le_u24(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 3)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]))
}

/// 从图片头部字节解析宽高：支持 PNG / GIF / JPEG / BMP / WebP；解析不出来返回 None（不报错）。
pub fn parse_dims(buf: &[u8]) -> Option<Dims> {
    // PNG: 89 50 4E 47 0D 0A 1A 0A + IHDR
    if buf.starts_with(b"\x89PNG\r\n\x1a\n") && buf.len() >= 24 && &buf[12..16] == b"IHDR" {
        let width = be_u32(buf, 16)?;
        let height = be_u32(buf, 20)?;
        return Some(Dims { width, height });
    }
    // GIF87a / GIF89a
    if (buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a")) && buf.len() >= 10 {
        let width = le_u16(buf, 6)? as u32;
        let height = le_u16(buf, 8)? as u32;
        return Some(Dims { width, height });
    }
    // BMP
    if buf.starts_with(b"BM") && buf.len() >= 26 {
        let width = le_i32(buf, 18)?.unsigned_abs();
        let height = le_i32(buf, 22)?.unsigned_abs();
        return Some(Dims { width, height });
    }
    // WebP: RIFF....WEBP，首个 chunk 可能是 VP8（有损）/ VP8L（无损）/ VP8X（扩展）
    if buf.len() >= 30 && buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        match &buf[12..16] {
            b"VP8 " => {
                // payload 偏移 20；3 字节帧标签后是起始码 9D 01 2A，再后面 2+2 字节为宽高
                if buf[23..26] == [0x9D, 0x01, 0x2A] {
                    let width = le_u16(buf, 26)? as u32 & 0x3FFF;
                    let height = le_u16(buf, 28)? as u32 & 0x3FFF;
                    if width > 0 && height > 0 {
                        return Some(Dims { width, height });
                    }
                }
            }
            b"VP8L" => {
                // payload 偏移 20：0x2F 签名 + 14 位宽-1 + 14 位高-1
                if buf[20] == 0x2F {
                    let bits = le_u32(buf, 21)?;
                    let width = (bits & 0x3FFF) + 1;
                    let height = ((bits >> 14) & 0x3FFF) + 1;
                    return Some(Dims { width, height });
                }
            }
            b"VP8X" => {
                // payload 偏移 20：1 字节标志 + 3 字节保留，随后 24 位宽-1、24 位高-1
                let width = le_u24(buf, 24)? + 1;
                let height = le_u24(buf, 27)? + 1;
                return Some(Dims { width, height });
            }
            _ => {}
        }
    }
    // JPEG: 扫描到 SOFn（C0-C3/C5-C7/C9-CB/CD-CF），段内为 精度(1)+高(2)+宽(2)
    if buf.starts_with(&[0xFF, 0xD8]) {
        let mut pos = 2usize;
        while pos + 4 <= buf.len() {
            if buf[pos] != 0xFF {
                pos += 1;
                continue;
            }
            let marker = buf[pos + 1];
            if marker == 0xFF {
                pos += 1;
                continue;
            }
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                pos += 2;
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break; // EOI 或已进入扫描数据，SOF 不会出现在后面
            }
            // JPEG 段长度与 SOF 里的宽高都是大端
            let seg_len = be_u16(buf, pos + 2)? as usize;
            if seg_len < 2 {
                break;
            }
            let is_sof = matches!(
                marker,
                0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
            );
            if is_sof {
                let h = be_u16(buf, pos + 5)? as u32;
                let w = be_u16(buf, pos + 7)? as u32;
                if w > 0 && h > 0 {
                    return Some(Dims {
                        width: w,
                        height: h,
                    });
                }
                return None;
            }
            pos = pos.saturating_add(2 + seg_len);
        }
    }
    None
}
