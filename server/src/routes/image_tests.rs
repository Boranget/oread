//! 图片代理单测（move-only 从 image.rs 拆出）。

use super::*;

#[test]
fn image_content_type_filter() {
    assert!(allowed_content_type("image/jpeg"));
    assert!(allowed_content_type("IMAGE/PNG; charset=binary"));
    assert!(allowed_content_type("image/webp"));
    assert!(!allowed_content_type("text/html"));
    assert!(!allowed_content_type("application/json"));
    assert!(!allowed_content_type(""));
}

#[test]
fn image_budget_uses_source_timeout_or_image_default() {
    let mut s = BookSource::default();
    assert_eq!(
        image_budget(&s).0,
        IMAGE_TIMEOUT,
        "未设置时保持图片 5s 预算"
    );
    assert_eq!(image_budget(&s).1, None);
    s.timeout_millisecond = 2000;
    assert_eq!(
        image_budget(&s),
        (Duration::from_secs(2), Some(Duration::from_secs(2)))
    );
    s.timeout_millisecond = 50;
    assert_eq!(
        image_budget(&s).0,
        Duration::from_millis(100),
        "低于极小值保护按 100ms 夹取"
    );
}

#[test]
fn private_hosts_rejected() {
    assert!(req::is_private_host("127.0.0.1"));
    assert!(req::is_private_host("localhost"));
    assert!(req::is_private_host("10.0.0.1"));
    assert!(req::is_private_host("192.168.1.2"));
    assert!(req::is_private_host("169.254.169.254"));
    assert!(!req::is_private_host("example.com"));
}

fn make_webp(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8]);
    }
    let mut buf: Vec<u8> = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::webp::WebPEncoder::new_lossless(&mut buf),
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )
    .expect("encode webp");
    buf
}

#[test]
fn webp_dims_parsed_from_vp8l_and_vp8x() {
    // 真实编码器产出的 VP8L
    let webp = make_webp(1500, 2573);
    assert_eq!(
        parse_dims(&webp),
        Some(Dims {
            width: 1500,
            height: 2573
        })
    );

    // 合成 VP8L 头
    let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
    vp8l.resize(20, 0);
    vp8l.push(0x2F);
    let bits: u32 = (799 - 1) | ((1199 - 1) << 14);
    vp8l.extend_from_slice(&bits.to_le_bytes());
    vp8l.resize(30, 0);
    assert_eq!(
        parse_dims(&vp8l),
        Some(Dims {
            width: 799,
            height: 1199
        })
    );

    // 合成 VP8X 头（宽高以 24 位小端存 width-1 / height-1）
    let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
    vp8x.resize(24, 0);
    vp8x.extend_from_slice(&(799u32).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(1199u32).to_le_bytes()[..3]);
    assert_eq!(
        parse_dims(&vp8x),
        Some(Dims {
            width: 800,
            height: 1200
        })
    );
}

fn gray_profile(quality: u8) -> EncodeProfile {
    EncodeProfile {
        grayscale: true,
        quality,
    }
}

fn color_profile(quality: u8) -> EncodeProfile {
    EncodeProfile {
        grayscale: false,
        quality,
    }
}

/// 统计 JPEG SOF 段里的分量数：1 = 灰度，3 = RGB。
fn jpeg_components(jpeg: &[u8]) -> Option<u8> {
    let mut pos = 2usize;
    while pos + 4 <= jpeg.len() {
        if jpeg[pos] != 0xFF {
            return None;
        }
        let marker = jpeg[pos + 1];
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return jpeg.get(pos + 9).copied();
        }
        pos += 2 + len;
    }
    None
}

#[test]
fn webp_transcodes_to_width_capped_jpeg() {
    let webp = make_webp(1500, 2400);
    let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
    assert_eq!(
        dims,
        Dims {
            width: 1200,
            height: 1920
        }
    );
    assert!(jpeg.starts_with(&[0xFF, 0xD8]), "输出应为 JPEG");
    // JPEG 头部里的 SOF 宽高也应与转码结果一致
    assert_eq!(
        parse_dims(&jpeg),
        Some(Dims {
            width: 1200,
            height: 1920
        })
    );
    // 墨水屏档必须是单分量灰度 JPEG
    assert_eq!(jpeg_components(&jpeg), Some(1));
}

#[test]
fn small_webp_keeps_dimensions_and_corrupt_is_rejected() {
    let webp = make_webp(600, 900);
    let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
    assert_eq!(
        dims,
        Dims {
            width: 600,
            height: 900
        }
    );
    assert!(!jpeg.is_empty());
    assert!(transcode_webp_to_jpeg(b"not a webp", gray_profile(78)).is_err());
}

#[test]
fn eink_grayscale_keeps_reasonable_tones() {
    let webp = make_webp(800, 1200);
    let (jpeg, dims) = transcode_webp_to_jpeg(&webp, gray_profile(78)).expect("transcode");
    assert_eq!(
        dims,
        Dims {
            width: 800,
            height: 1200
        }
    );
    assert_eq!(jpeg_components(&jpeg), Some(1));

    // 解码回来统计亮度：既不能是全黑/全白，也不能反相
    let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
    let luma = decoded.to_luma8();
    let (mut min, mut max, mut sum) = (255u8, 0u8, 0u64);
    for p in luma.pixels() {
        let v = p.0[0];
        min = min.min(v);
        max = max.max(v);
        sum += v as u64;
    }
    let n = u64::from(luma.width()) * u64::from(luma.height());
    let mean = sum / n;
    assert!(min < 60, "最小值 {min} 说明可能全黑");
    assert!(max > 200, "最大值 {max} 说明可能全黑或被压扁");
    assert!(
        (60..=200).contains(&mean),
        "平均亮度 {mean} 应居中，反相/全黑都不满足"
    );
}

#[test]
fn color_profile_still_three_components() {
    let (jpeg, _) =
        transcode_webp_to_jpeg(&make_webp(300, 400), color_profile(82)).expect("transcode");
    assert_eq!(jpeg_components(&jpeg), Some(3));
}

#[test]
fn eink_quality_env_parsing_clamps_to_60_90() {
    assert_eq!(parse_eink_jpeg_quality("78"), Some(78));
    assert_eq!(parse_eink_jpeg_quality(" 85 "), Some(85));
    assert_eq!(parse_eink_jpeg_quality("40"), Some(60));
    assert_eq!(parse_eink_jpeg_quality("95"), Some(90));
    assert_eq!(parse_eink_jpeg_quality("abc"), None);
}

#[test]
fn transcode_decision_follows_client_capability() {
    fn headers(ua: Option<&str>, accept: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(ua) = ua {
            h.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
        }
        if let Some(a) = accept {
            h.insert(header::ACCEPT, HeaderValue::from_str(a).unwrap());
        }
        h
    }
    let kindle = "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+";
    let silk = "Mozilla/5.0 (Linux; Android 4.4; KFAPWI Build/KTU84P) AppleWebKit/537.36 Silk/3.68 like Chrome/34.0";
    let desktop =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";

    // Kindle/Silk 一律转，即使 Accept 声明支持 webp
    assert!(wants_webp_transcode(&headers(
        Some(kindle),
        Some("image/webp,image/*")
    )));
    assert!(wants_webp_transcode(&headers(
        Some(silk),
        Some("image/webp")
    )));
    // 桌面 UA + Accept 含 webp：原样透传
    assert!(!wants_webp_transcode(&headers(
        Some(desktop),
        Some("image/avif,image/webp,*/*")
    )));
    // 现代 UA 但 Accept 明确不含 webp：转
    assert!(wants_webp_transcode(&headers(
        Some(desktop),
        Some("image/jpeg,image/png")
    )));
    // 无 Accept 头按现代客户端处理
    assert!(!wants_webp_transcode(&headers(Some(desktop), None)));
}

#[test]
fn cache_variants_do_not_collide() {
    let u = "https://img.example/a.jpg";
    let base = EinkConfig::default();
    let webp_key = variant_key(u, false, &base, 78);
    let jpeg_key = variant_key(u, true, &base, 78);
    let orig = format!("{u}#orig");
    assert_ne!(webp_key, jpeg_key, "WebP 转码与 JPEG 源灰度不能共用 key");
    assert_ne!(jpeg_key, orig, "转码档与原图不能共用 key");
    assert_ne!(etag_of(&webp_key), etag_of(&jpeg_key));

    let wide = EinkConfig {
        width: 1440,
        ..base
    };
    let adaptive = EinkConfig {
        adaptive: true,
        ..base
    };
    assert_ne!(
        variant_key(u, true, &base, 70),
        variant_key(u, true, &wide, 70),
        "宽度必须进 key"
    );
    assert_ne!(
        variant_key(u, true, &base, 70),
        variant_key(u, true, &base, 85),
        "质量必须进 key"
    );
    assert_ne!(
        variant_key(u, true, &base, 70),
        variant_key(u, true, &adaptive, 70),
        "自适应标记必须进 key"
    );
}

#[test]
fn adaptive_lookup_covers_both_qualities() {
    let cfg = EinkConfig {
        adaptive: true,
        ..EinkConfig::default()
    };
    let pairs = key_pairs("https://img.example/a.jpg", true, &cfg);
    assert_eq!(pairs.len(), 2, "自适应要同时查文字档与画面档");
    assert!(pairs.iter().any(|(k, _)| k.contains("-q85-")));
    assert!(pairs.iter().any(|(k, _)| k.contains("-q70-")));
    assert!(pairs
        .iter()
        .all(|(k, _)| k.starts_with("https://img.example/a.jpg#jpegsrc-w1200-")));
}

fn make_jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x % 251) as u8, (y % 251) as u8, ((x + y) % 251) as u8]);
    }
    let mut buf: Vec<u8> = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92)
        .encode_image(&img)
        .expect("encode jpeg");
    buf
}

#[test]
fn jpeg_source_transcodes_to_width_capped_gray() {
    let jpeg = make_jpeg(1500, 2400);
    assert_eq!(jpeg_components(&jpeg), Some(3));
    let cfg = EinkConfig::default();
    let (out, dims, quality) =
        transcode_jpeg_to_gray(&jpeg, gray_profile(EINK_JPEG_QUALITY_DEFAULT), cfg)
            .expect("transcode");
    assert_eq!(
        dims,
        Dims {
            width: 1200,
            height: 1920
        }
    );
    assert_eq!(quality, EINK_JPEG_QUALITY_DEFAULT);
    assert_eq!(jpeg_components(&out), Some(1), "墨水屏输出必须是单分量灰度");
    assert_eq!(parse_dims(&out), Some(dims));
    assert!(transcode_jpeg_to_gray(b"not a jpeg", gray_profile(70), cfg).is_err());
}

#[test]
fn jpeg_source_with_env_width_is_honored() {
    let cfg = EinkConfig {
        width: 1440,
        ..EinkConfig::default()
    };
    let (out, dims, _) =
        transcode_jpeg_to_gray(&make_jpeg(2000, 3000), gray_profile(70), cfg).expect("transcode");
    assert_eq!(
        dims,
        Dims {
            width: 1440,
            height: 2160
        }
    );
    assert_eq!(parse_dims(&out), Some(dims));
}

#[test]
fn eink_detection_covers_ua_header_and_query() {
    fn h(ua: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(ua) = ua {
            h.insert(header::USER_AGENT, HeaderValue::from_str(ua).unwrap());
        }
        h
    }
    let kindle_ua = "Mozilla/5.0 (Linux; U; Android 2.3.4) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/531.2 Kindle/3.0+";
    let desktop_ua =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";

    let desktop = h(Some(desktop_ua));
    assert!(!is_eink_client(&desktop, None), "普通浏览器默认不转灰度");
    assert!(!is_eink_client(&desktop, Some("0")));
    assert!(is_eink_client(&desktop, Some("1")));
    assert!(is_eink_client(&desktop, Some("eink")));

    let mut mode = h(Some(desktop_ua));
    mode.insert(
        HeaderName::from_static("x-device-mode"),
        HeaderValue::from_static("eink"),
    );
    assert!(
        is_eink_client(&mode, None),
        "X-Device-Mode: eink 等价于 ?eink=1"
    );

    assert!(
        is_eink_client(&h(Some(kindle_ua)), None),
        "Kindle UA 无需额外声明"
    );
}

#[test]
fn dims_parsed_from_png_gif_jpeg_bmp() {
    let png = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x02, 0x80, 0x00, 0x00, 0x01, 0xE0,
    ];
    assert_eq!(
        parse_dims(&png),
        Some(Dims {
            width: 640,
            height: 480
        })
    );

    let gif = [b'G', b'I', b'F', b'8', b'9', b'a', 0x40, 0x01, 0xF0, 0x00];
    assert_eq!(
        parse_dims(&gif),
        Some(Dims {
            width: 320,
            height: 240
        })
    );

    // SOI + 一个 SOF0 段（长度 0x0011 = 17，精度 8，高 480，宽 640）
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03, 0x01, 0x22, 0x00,
        0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
    ];
    assert_eq!(
        parse_dims(&jpeg),
        Some(Dims {
            width: 640,
            height: 480
        })
    );

    let mut bmp = vec![b'B', b'M'];
    bmp.resize(18, 0);
    bmp.extend_from_slice(&640i32.to_le_bytes());
    bmp.extend_from_slice(&480i32.to_le_bytes());
    assert_eq!(
        parse_dims(&bmp),
        Some(Dims {
            width: 640,
            height: 480
        })
    );

    assert_eq!(parse_dims(b"not an image"), None);
    assert_eq!(parse_dims(&[]), None);
}

#[test]
fn etag_is_stable_and_quoted() {
    let a = etag_of("https://img.example/a.jpg");
    let b = etag_of("https://img.example/a.jpg");
    assert_eq!(a, b);
    assert!(a.starts_with('"') && a.ends_with('"'));
    assert_ne!(a, etag_of("https://img.example/b.jpg"));
}

// ---------------- 长条页切片 ----------------

/// 合成一张「暗底 + 指定行白带」的灰度图，用来验证近白行切点。
fn luma_with_white_band(w: u32, h: u32, band: std::ops::Range<usize>) -> image::GrayImage {
    image::GrayImage::from_fn(w, h, |x, y| {
        let _ = x;
        let v = if band.contains(&(y as usize)) {
            255u8
        } else {
            40u8
        };
        image::Luma([v])
    })
}

#[test]
fn plan_cuts_snaps_to_white_row_within_radius() {
    // 1200×2266 的源像素空间：win≈1547、ov≈83、r≈124，target≈1547
    let luma = luma_with_white_band(1200, 2266, 1480..1491);
    let cuts = plan_cuts(&luma, 1547, 83, 124, 13, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
    assert_eq!(cuts.len(), 3, "两片 + 首尾切点");
    assert_eq!(cuts[0], 0);
    // 白带内离 target 最近的近白行（带的下沿）就是切点
    assert!(
        cuts[1] >= 1480 && cuts[1] <= 1498,
        "切点应落在白带内: {cuts:?}"
    );
    assert_eq!(cuts[2], 2266);
    // 上界保证：每片显示高 ≤ vh
    assert!(cuts[1] <= 1547, "切点不得超过窗口上界");
}

#[test]
fn plan_cuts_guard_rejects_cut_inside_face_band() {
    // 100×400：整图近白，但 156..164 行是「脸/眼」暗带（全宽暗像素）。
    // target=150 正好落在暗带上方的白高光行：μ/σ 能过，但 ±10 带内有暗像素 → 必须拒绝。
    let luma = image::GrayImage::from_fn(100, 400, |_x, y| {
        let v = if (156..164).contains(&(y as usize)) {
            30u8
        } else {
            255u8
        };
        image::Luma([v])
    });
    let cuts = plan_cuts(&luma, 150, 30, 30, 10, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
    assert_ne!(
        cuts[1], 150,
        "切在暗带旁边的候选必须被墨量守卫拒掉: {cuts:?}"
    );
    // 退到带外最近的一行（146：±10 带内已不含 156 行）
    assert_eq!(cuts[1], 146, "应退到墨量达标的最近行: {cuts:?}");
    assert!(
        cuts.windows(2).all(|w| w[1] > w[0]),
        "切点必须升序: {cuts:?}"
    );
    assert_eq!(cuts.last().copied(), Some(400));
}

#[test]
fn plan_cuts_prefers_tail_high_enough_on_last_cut() {
    // 100×860 整图纯白、win=400、ov=50：最后一刀 target=750 会剩 110px 薄片
    //（< 0.3×400=120），必须前移到 740，让末片正好 120px。
    let luma = image::GrayImage::from_pixel(100, 860, image::Luma([255]));
    let cuts = plan_cuts(&luma, 400, 50, 30, 10, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
    assert_eq!(cuts[1], 400, "非末刀不受末片规则影响");
    assert_eq!(cuts[2], 740, "末刀前移保证末片 ≥ 0.3 窗高: {cuts:?}");
    assert!(860 - cuts[2] >= 120);
    // 间距下限也在：相邻切点 ≥ 0.35×win
    assert!(cuts[2] - cuts[1] >= 140);
}

#[test]
fn slice_key_uses_sl2_and_ink_params() {
    let p = SliceParams::default();
    let label = p.label();
    assert!(
        label.starts_with("sl2-dw928-vh1196-ov64-mu200-sd30-r96"),
        "{label}"
    );
    assert!(
        label.ends_with("-ik2-g10"),
        "墨量守卫参数必须进 key: {label}"
    );
    // 守卫带宽/阈值是常量，key 里必须能看出用的是哪一组
    assert_eq!(SLICE_INK_PCT, 2);
    assert_eq!(SLICE_G_CSS, 10);
}

#[test]
fn plan_cuts_hard_cuts_when_no_white_row() {
    // 全暗图：窗口内没有近白行，硬切在本片最远端 target = win = 1547
    let luma = luma_with_white_band(1200, 2266, 0..0);
    let cuts = plan_cuts(&luma, 1547, 83, 124, 13, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
    assert_eq!(cuts, vec![0, 1547, 2266]);
    // 第二片从 1547 − 83 开始，与上片重叠 83 行
    let ranges = slice_ranges(&cuts, 83, 2266);
    assert_eq!(ranges[1].0, 1464);
}

#[test]
fn plan_cuts_ranges_cover_height_and_overlap() {
    let luma = luma_with_white_band(1200, 2872, 0..0);
    let (win, ov, r, g) = src_metrics(1200, &SliceParams::default());
    let cuts = plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
    let ranges = slice_ranges(&cuts, ov, 2872);
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].0, 0);
    assert_eq!(ranges.last().unwrap().1, 2872, "必须覆盖到底");
    // 相邻片在 overlap 区重叠：第二片起点 = 切点 − ov
    assert_eq!(ranges[1].0, cuts[1] - ov);
    for (s, e) in &ranges {
        assert!(e - s <= win, "单片源高不得超过窗高: {s}..{e}");
    }
    // 确定性：同一图同一参数必须得到同一 cuts
    assert_eq!(
        cuts,
        plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0)
    );
}

#[test]
fn slice_count_matches_measured_samples() {
    let p = SliceParams::default();
    // 三条长条页 → 2 片；1.1 屏页与横版 → 不切
    assert_eq!(slice_count_window(1200, 2266, &p), 2);
    assert_eq!(slice_count_window(1200, 2296, &p), 2);
    assert_eq!(slice_count_window(1200, 2872, &p), 2);
    assert_eq!(slice_count_window(1024, 1471, &p), 1);
    assert_eq!(slice_count_window(1024, 736, &p), 1);
    assert_eq!(slice_count_window(900, 1300, &p), 1);
    assert!(!worth_slicing(1024, 1471, &p));
    assert!(worth_slicing(1200, 2266, &p));
    // 估算的片高数组长度与片数一致，且都不超过 vh
    for (w, h) in [(1200u32, 2266u32), (1200, 2872), (1024, 1471)] {
        let hs = slice_display_heights(w, h, &p);
        assert_eq!(hs.len(), slice_count_window(w, h, &p));
        assert!(hs.iter().all(|v| *v <= p.vh), "{w}x{h} 片高越界: {hs:?}");
    }
}

#[test]
fn slice_cache_keys_are_unique_and_deterministic() {
    let u = "https://img.example/long.jpg";
    let cfg = EinkConfig::default();
    let p = SliceParams::default();
    let base = variant_key(u, true, &cfg, 70);
    let k0 = slice_variant_key(&base, &p, 0, 2);
    let k1 = slice_variant_key(&base, &p, 1, 2);
    let k3 = slice_variant_key(&base, &p, 2, 3);
    assert_ne!(k0, k1);
    assert_ne!(k1, k3, "同一片在不同总片数下不能共用 key");
    assert!(
        k0.contains("sl2-dw928-vh1196-ov64-mu200-sd30-r96-ik2-g10-i0of2"),
        "{k0}"
    );
    assert_eq!(k0, slice_variant_key(&base, &p, 0, 2), "key 必须可复现");
    let small = SliceParams::new(Some(1072), None, None);
    assert_ne!(
        slice_variant_key(&base, &p, 0, 2),
        slice_variant_key(&base, &small, 0, 2),
        "版心宽必须进 key"
    );
    // 计划 key 与片 key 不撞
    assert_ne!(slice_plan_key(u, &cfg, &p), k0);
}

#[test]
fn transcode_slice_degrades_to_whole_below_1_3_screens() {
    // 1024×1471 = 1.29 屏（928 版心）：必须不切，且字节与整图路径完全一致
    let jpeg = make_jpeg(1024, 1471);
    let cfg = EinkConfig::default();
    let p = SliceParams::default();
    let (whole, whole_dims, whole_q) =
        transcode_jpeg_to_gray(&jpeg, gray_profile(70), cfg).expect("whole");
    let sliced = transcode_slice(
        &jpeg,
        image::ImageFormat::Jpeg,
        gray_profile(70),
        cfg,
        &p,
        0,
    )
    .expect("slice");
    assert_eq!(sliced.n, 1, "1.1 屏页不许切");
    assert_eq!(sliced.index, 0);
    assert_eq!(sliced.dims, whole_dims);
    assert_eq!(sliced.quality, whole_q);
    assert_eq!(sliced.data, whole, "不切时必须与整图字节完全相同");
}

#[test]
fn transcode_slice_splits_long_page_and_keeps_overlap() {
    // 1200×2266 + 白带 → 2 片；第 2 片起点比切点提前 overlap 行
    let w = 1200u32;
    let h = 2266u32;
    let luma = luma_with_white_band(w, h, 1480..1491);
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92)
        .encode_image(&luma)
        .expect("encode");
    let cfg = EinkConfig::default();
    let p = SliceParams::default();
    let first = transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 0)
        .expect("slice 0");
    assert_eq!(first.n, 2);
    assert_eq!(first.index, 0);
    let plan = first.plan.as_ref().expect("plan");
    let ov = {
        let (win, ov, r, g) = src_metrics(w, &p);
        let expect = plan_cuts(&luma, win, ov, r, g, SLICE_MU, SLICE_SD_U8 as f64, 2.0);
        assert_eq!(expect.len(), 3);
        // JPEG 有损：白带边界可能被振铃污染，允许切点在带内 ±2 行漂移
        assert!(
            plan.cuts[1].abs_diff(expect[1]) <= 2,
            "切点 {:?} 应接近 {:?}",
            plan.cuts,
            expect
        );
        ov
    };
    let ranges = slice_ranges(&plan.cuts, ov, h as usize);
    assert_eq!(ranges.len(), 2);
    assert_eq!(first.dims.height as usize, ranges[0].1 - ranges[0].0);
    let second = transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 1)
        .expect("slice 1");
    assert_eq!(second.index, 1);
    assert_eq!(second.dims.height as usize, ranges[1].1 - ranges[1].0);
    // 越界 i 被夹到末片
    let clamped = transcode_slice(&buf, image::ImageFormat::Jpeg, gray_profile(70), cfg, &p, 9)
        .expect("clamp");
    assert_eq!(clamped.index, 1, "i 越界夹到 n−1");
}
