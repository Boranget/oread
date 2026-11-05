//! 书源 JS 桥需要的对称加密实现。
//!
//! 对齐 Legado 的 `java.aesBase64DecodeToString` / `java.createSymmetricCrypto`：
//! - `transformation` 形如 `AES/CBC/PKCS5Padding`（大小写不敏感，PKCS5 与 PKCS7 等价）；
//! - key/iv 按字符串原始字节使用；
//! - 目前支持 AES-CBC（PKCS5/7、NoPadding）；ECB/OFB/CFB 等显式报未支持。

use aes::cipher::block_padding::{NoPadding, Pkcs7};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aes::{Aes128, Aes192, Aes256};
use base64::Engine as _;

fn parse_transformation(t: &str) -> Result<(String, String, String), String> {
    let upper = t.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return Err("transformation 为空（应形如 AES/CBC/PKCS5Padding）".to_string());
    }
    let parts: Vec<&str> = upper.split('/').collect();
    if parts.len() != 3 {
        return Err(format!(
            "transformation 不合法：{t}（应形如 AES/CBC/PKCS5Padding）"
        ));
    }
    if parts[0] != "AES" {
        return Err(format!("暂只支持 AES 算法，收到：{}", parts[0]));
    }
    let mode = parts[1].to_string();
    if mode != "CBC" {
        return Err(format!("暂只支持 CBC 模式，收到：{mode}"));
    }
    let padding = parts[2].to_string();
    if padding != "PKCS5PADDING" && padding != "PKCS7PADDING" && padding != "NOPADDING" {
        return Err(format!("暂只支持 PKCS5/PKCS7/NoPadding，收到：{padding}"));
    }
    Ok(("AES".to_string(), mode, padding))
}

macro_rules! cbc_decrypt {
    ($ty:ty, $key:expr, $iv:expr, $data:expr, $pkcs:expr) => {{
        let buf = $data.to_vec();
        let dec = cbc::Decryptor::<$ty>::new_from_slices($key, $iv)
            .map_err(|e| format!("AES key/iv 长度不合法：{e}"))?;
        let out = if $pkcs {
            dec.decrypt_padded_vec_mut::<Pkcs7>(&buf)
        } else {
            dec.decrypt_padded_vec_mut::<NoPadding>(&buf)
        };
        out.map_err(|e| format!("AES 解密失败：{e:?}"))
    }};
}

macro_rules! cbc_encrypt {
    ($ty:ty, $key:expr, $iv:expr, $data:expr, $pkcs:expr) => {{
        let enc = cbc::Encryptor::<$ty>::new_from_slices($key, $iv)
            .map_err(|e| format!("AES key/iv 长度不合法：{e}"))?;
        let out = if $pkcs {
            enc.encrypt_padded_vec_mut::<Pkcs7>($data)
        } else {
            if $data.len() % 16 != 0 {
                return Err("NoPadding 要求明文长度为 16 的整数倍".to_string());
            }
            enc.encrypt_padded_vec_mut::<NoPadding>($data)
        };
        Ok(out)
    }};
}

/// 按 key 长度选择 AES-128/192/256（CBC）。
pub fn decrypt_bytes(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
) -> Result<Vec<u8>, String> {
    let (_, _, padding) = parse_transformation(transformation)?;
    if iv.is_empty() {
        return Err("CBC 模式需要 iv".to_string());
    }
    let pkcs = padding != "NOPADDING";
    match key.len() {
        16 => cbc_decrypt!(Aes128, key, iv, data, pkcs),
        24 => cbc_decrypt!(Aes192, key, iv, data, pkcs),
        32 => cbc_decrypt!(Aes256, key, iv, data, pkcs),
        n => Err(format!("AES key 长度不支持：{n}（应为 16/24/32 字节）")),
    }
}

pub fn encrypt_bytes(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
) -> Result<Vec<u8>, String> {
    let (_, _, padding) = parse_transformation(transformation)?;
    if iv.is_empty() {
        return Err("CBC 模式需要 iv".to_string());
    }
    let pkcs = padding != "NOPADDING";
    match key.len() {
        16 => cbc_encrypt!(Aes128, key, iv, data, pkcs),
        24 => cbc_encrypt!(Aes192, key, iv, data, pkcs),
        32 => cbc_encrypt!(Aes256, key, iv, data, pkcs),
        n => Err(format!("AES key 长度不支持：{n}（应为 16/24/32 字节）")),
    }
}

/// Legado `java.aesBase64DecodeToString`：Base64 解码后 AES 解密，按 UTF-8 转字符串。
pub fn decrypt_base64_to_string(
    data_b64: &str,
    key: &[u8],
    transformation: &str,
    iv: &[u8],
) -> Result<String, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|e| format!("Base64 解码失败：{e}"))?;
    let plain = decrypt_bytes(&raw, key, transformation, iv)?;
    Ok(String::from_utf8_lossy(&plain).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 3602 / NIST 常用向量：AES-128-CBC，key=iv=0x00.. 0f
    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn aes_128_cbc_roundtrip() {
        let key = b"0123456789abcdef";
        let iv = b"fedcba9876543210";
        let plain = b"hello boranlib";
        let enc = encrypt_bytes(plain, key, "AES/CBC/PKCS5Padding", iv).unwrap();
        let dec = decrypt_bytes(&enc, key, "AES/CBC/PKCS5Padding", iv).unwrap();
        assert_eq!(dec, plain);
    }

    #[test]
    fn aes_256_cbc_base64_decode_to_string() {
        let key = b"0123456789abcdef0123456789abcdef";
        let iv = b"fedcba9876543210";
        let cipher = encrypt_bytes(b"BorN", key, "aes/cbc/pkcs7padding", iv).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&cipher);
        let out = decrypt_base64_to_string(&b64, key, "AES/CBC/PKCS5Padding", iv).unwrap();
        assert_eq!(out, "BorN");
    }

    #[test]
    fn no_padding_requires_block_aligned_input() {
        let key = b"0123456789abcdef";
        let iv = b"fedcba9876543210";
        let plain = hex("00112233445566778899aabbccddeeff");
        let enc = encrypt_bytes(&plain, key, "AES/CBC/NoPadding", iv).unwrap();
        let dec = decrypt_bytes(&enc, key, "AES/CBC/NoPadding", iv).unwrap();
        assert_eq!(dec, plain);
    }

    // 哔哩漫画解图链路：key/iv 来自 JS 字节数组（不是 ASCII 字符串）
    #[test]
    fn aes_key_and_iv_from_byte_arrays() {
        let key: Vec<u8> = (0u8..16).collect();
        let iv: Vec<u8> = (16u8..32).collect();
        let plain = b"image-head";
        let enc = encrypt_bytes(plain, &key, "AES/CBC/PKCS7Padding", &iv).unwrap();
        let dec = decrypt_bytes(&enc, &key, "AES/CBC/PKCS7Padding", &iv).unwrap();
        assert_eq!(dec, plain);
    }

    #[test]
    fn unsupported_mode_is_explicit() {
        let key = b"0123456789abcdef";
        let err = decrypt_bytes(b"xx", key, "AES/ECB/PKCS5Padding", b"").unwrap_err();
        assert!(err.contains("CBC"), "err={err}");
    }
}
