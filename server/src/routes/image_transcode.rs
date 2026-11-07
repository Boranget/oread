//! 图片代理的 transcode 部分（move-only 从 image.rs 拆出）。

use super::*;

pub(super) fn cache_lookup(keys: &[(String, String)]) -> Option<(Vec<u8>, String, String)> {
    for (key, etag) in keys {
        if let Some((data, ct)) = image_cache::get(key) {
            return Some((data, ct, etag.clone()));
        }
    }
    None
}

/// 找到客户端 `If-None-Match` 命中的那个变体 ETag（用于 304 响应）。
pub(super) fn matching_key_etag<'a>(
    headers: &HeaderMap,
    keys: &'a [(String, String)],
) -> Option<&'a String> {
    keys.iter()
        .find(|(_, etag)| matches_if_none_match(headers, etag))
        .map(|(_, etag)| etag)
}

/// 目标尺寸：宽度超过上限时等比缩小，绝不放大（1024 宽的手机源保持原尺寸）。
pub(super) fn transcode_target_size(w: u32, h: u32, max_width: u32) -> (u32, u32) {
    if w <= max_width || h == 0 {
        return (w.max(1), h.max(1));
    }
    let nh = ((h as u64 * max_width as u64) / w as u64).max(1) as u32;
    (max_width, nh)
}

pub(super) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub(super) fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        1.0
    } else {
        let px = std::f32::consts::PI * x;
        px.sin() / px
    }
}

pub(super) fn lanczos3(x: f32) -> f32 {
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// 分离式 Lanczos3，在线性光空间缩放（gamma 正确），避免暗部被过度压暗。
pub(super) fn resize_lanczos_linear(src: &image::GrayImage, tw: u32, th: u32) -> image::GrayImage {
    let sw = src.width() as usize;
    let sh = src.height() as usize;
    let twu = tw as usize;
    let thu = th as usize;
    let a = src.as_raw();
    let mut lin = vec![0f32; sw * sh];
    for i in 0..sw * sh {
        lin[i] = srgb_to_linear(a[i] as f32 / 255.0);
    }

    let mut tmp = vec![0f32; twu * sh];
    let scale_x = sw as f32 / twu as f32;
    let support_x = 3.0 * scale_x.max(1.0);
    for x in 0..twu {
        let center = (x as f32 + 0.5) * scale_x - 0.5;
        let start = ((center - support_x).floor() as isize).max(0);
        let end = ((center + support_x).ceil() as isize).min(sw as isize - 1);
        let mut weights = Vec::new();
        let mut sum = 0f32;
        for s in start..=end {
            let d = (s as f32 - center) / scale_x.max(1.0);
            let w = lanczos3(d);
            weights.push((s as usize, w));
            sum += w;
        }
        if sum.abs() < 1e-9 {
            weights.clear();
            let s = center.round().clamp(0.0, (sw - 1) as f32) as usize;
            weights.push((s, 1.0));
            sum = 1.0;
        }
        for y in 0..sh {
            let mut v = 0f32;
            for &(s, w) in &weights {
                v += lin[y * sw + s] * w;
            }
            tmp[y * twu + x] = (v / sum).clamp(0.0, 1.0);
        }
    }

    let mut out = vec![0u8; twu * thu];
    let scale_y = sh as f32 / thu as f32;
    let support_y = 3.0 * scale_y.max(1.0);
    for y in 0..thu {
        let center = (y as f32 + 0.5) * scale_y - 0.5;
        let start = ((center - support_y).floor() as isize).max(0);
        let end = ((center + support_y).ceil() as isize).min(sh as isize - 1);
        let mut weights = Vec::new();
        let mut sum = 0f32;
        for s in start..=end {
            let d = (s as f32 - center) / scale_y.max(1.0);
            let w = lanczos3(d);
            weights.push((s as usize, w));
            sum += w;
        }
        if sum.abs() < 1e-9 {
            weights.clear();
            let s = center.round().clamp(0.0, (sh - 1) as f32) as usize;
            weights.push((s, 1.0));
            sum = 1.0;
        }
        for x in 0..twu {
            let mut v = 0f32;
            for &(s, w) in &weights {
                v += tmp[s * twu + x] * w;
            }
            out[y * twu + x] = (linear_to_srgb((v / sum).clamp(0.0, 1.0)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    image::GrayImage::from_raw(tw, th, out).expect("尺寸与缓冲一致")
}

/// 轻量 USM：5×5 高斯近似（sigma≈0.8），量 0.3，满足「半径≤1、量≤0.3」。
pub(super) fn usm_luma(src: &image::GrayImage) -> image::GrayImage {
    let w = src.width() as usize;
    let h = src.height() as usize;
    let a = src.as_raw();
    let kernel = [0.0545f32, 0.2442, 0.4026, 0.2442, 0.0545];
    let mut tmp = vec![0f32; w * h];
    let mut blur = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = 0f32;
            for (k, kw) in kernel.iter().enumerate() {
                let sx = (x as isize + k as isize - 2).clamp(0, w as isize - 1) as usize;
                v += a[y * w + sx] as f32 * kw;
            }
            tmp[y * w + x] = v;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut v = 0f32;
            for (k, kw) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - 2).clamp(0, h as isize - 1) as usize;
                v += tmp[sy * w + x] * kw;
            }
            blur[y * w + x] = v;
        }
    }
    let mut out = vec![0u8; w * h];
    for i in 0..w * h {
        let v = a[i] as f32;
        out[i] = (v + 0.3 * (v - blur[i]).round()).round().clamp(0.0, 255.0) as u8;
    }
    image::GrayImage::from_raw(src.width(), src.height(), out).expect("尺寸与缓冲一致")
}

/// 自适应判定：中间调（80–200）像素占比 < 0.30 视为文字页，走 q85；否则画面页 q70。
/// 按 2 像素步长采样，避免整页 Sobel 的 CPU 开销。
pub(super) fn adaptive_quality(luma: &image::GrayImage) -> u8 {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    let a = luma.as_raw();
    let mut mid = 0u64;
    let mut n = 0u64;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let v = a[y * w + x];
            if (80..200).contains(&v) {
                mid += 1;
            }
            n += 1;
        }
    }
    if (mid as f64 / n.max(1) as f64) < EINK_ADAPTIVE_MID_THRESHOLD {
        EINK_ADAPTIVE_TEXT_Q
    } else {
        EINK_ADAPTIVE_ART_Q
    }
}

/// 通用「解码 → 灰度/彩色 → 缩放 → JPEG 重编码」，WebP 源与 JPEG 源共用。
/// 返回 (输出字节, 尺寸, 实际质量)。灰度档先转 Luma8 再缩放，缩放量与编码量
/// 都只有 RGB 的 1/3。
pub(super) fn transcode_to_jpeg(
    raw: &[u8],
    format: image::ImageFormat,
    profile: EncodeProfile,
    cfg: EinkConfig,
) -> Result<(Vec<u8>, Dims, u8), String> {
    let label = match format {
        image::ImageFormat::WebP => "WebP",
        image::ImageFormat::Jpeg => "JPEG",
        _ => "图片",
    };
    let reader = image::ImageReader::with_format(std::io::Cursor::new(raw), format);
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("{label} 头部解析失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err(format!("{label} 尺寸为 0"));
    }
    if w as u64 * h as u64 > MAX_TRANSCODE_PIXELS {
        return Err(format!(
            "源图 {w}x{h} 超过解码像素上限 {MAX_TRANSCODE_PIXELS}"
        ));
    }
    let img = image::load_from_memory_with_format(raw, format)
        .map_err(|e| format!("{label} 解码失败: {e}"))?;
    let (src_w, src_h) = (img.width(), img.height());
    let (tw, th) = transcode_target_size(src_w, src_h, cfg.width);
    let mut buf: Vec<u8> = Vec::new();
    if profile.grayscale {
        // to_luma8 使用 BT.709 亮度系数（0.2126R + 0.7152G + 0.0722B），不是通道平均
        let luma = img.to_luma8();
        let quality = if cfg.adaptive {
            adaptive_quality(&luma)
        } else {
            profile.quality
        };
        let resized = if (tw, th) == (luma.width(), luma.height()) {
            luma
        } else {
            resize_lanczos_linear(&luma, tw, th)
        };
        let out = if cfg.usm { usm_luma(&resized) } else { resized };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        Ok((buf, dims, quality))
    } else {
        let rgb = img.to_rgb8();
        let out = if (tw, th) == (rgb.width(), rgb.height()) {
            rgb
        } else {
            image::imageops::resize(&rgb, tw, th, image::imageops::FilterType::Triangle)
        };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, profile.quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        Ok((buf, dims, profile.quality))
    }
}

/// WebP → JPEG（内存内完成，不落盘）。仅测试用便捷封装；生产路径直接调用 `transcode_to_jpeg`。
#[cfg(test)]
pub(super) fn transcode_webp_to_jpeg(
    raw: &[u8],
    profile: EncodeProfile,
) -> Result<(Vec<u8>, Dims), String> {
    let (data, dims, _) = transcode_to_jpeg(
        raw,
        image::ImageFormat::WebP,
        profile,
        EinkConfig::default(),
    )?;
    Ok((data, dims))
}

/// JPEG 源 → 灰度 JPEG：仅墨水屏使用，强制 Luma8。
pub(super) fn transcode_jpeg_to_gray(
    raw: &[u8],
    profile: EncodeProfile,
    cfg: EinkConfig,
) -> Result<(Vec<u8>, Dims, u8), String> {
    let gray = EncodeProfile {
        grayscale: true,
        quality: profile.quality,
    };
    transcode_to_jpeg(raw, image::ImageFormat::Jpeg, gray, cfg)
}
