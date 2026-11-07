//! 图片代理的 slice 部分（move-only 从 image.rs 拆出）。

use super::*;

// ---------------- 长条页切片（墨水屏） ----------------

/// 切片参数（CSS px / 亮度阈值）：全部进缓存 key，参数一变 key 必变。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceParams {
    pub dw: u32,
    pub vh: u32,
    pub ov: u32,
    /// 近白行亮度均值下限
    pub mu: u8,
    /// 近白行带内标准差上限
    pub sd: u8,
    /// 切点搜索半径（CSS px）
    pub r: u32,
}

impl Default for SliceParams {
    fn default() -> Self {
        Self {
            dw: SLICE_DW_DEFAULT,
            vh: SLICE_VH_DEFAULT,
            ov: SLICE_OV_DEFAULT,
            mu: SLICE_MU,
            sd: SLICE_SD_U8,
            r: SLICE_R_CSS,
        }
    }
}

impl SliceParams {
    /// 非法/越界值夹到安全区间：版心 320–2000、可用高 320–2000、overlap ≤ 1/4 窗高。
    #[cfg(test)]
    pub(super) fn new(dw: Option<u32>, vh: Option<u32>, ov: Option<u32>) -> Self {
        let d = Self::default();
        Self::resolve(d.dw, d.vh, d.ov, dw, vh, ov, d.mu, d.sd, d.r)
    }

    /// 生效值：环境变量给出的默认值（见 `Config`），查询参数可覆盖版心宽/可用高/重叠。
    pub(crate) fn from_config(
        cfg: &crate::config::Config,
        dw: Option<u32>,
        vh: Option<u32>,
        ov: Option<u32>,
    ) -> Self {
        Self::resolve(
            cfg.comic_slice_dw,
            cfg.comic_slice_vh,
            cfg.comic_slice_ov,
            dw,
            vh,
            ov,
            cfg.comic_slice_mu,
            cfg.comic_slice_sd,
            cfg.comic_slice_r,
        )
    }

    #[allow(clippy::too_many_arguments)] // 视口/切片/字号等参数均为纯数值，收拢结构体反而增加构造样板。
    pub(super) fn resolve(
        dw: u32,
        vh: u32,
        ov: u32,
        dw_q: Option<u32>,
        vh_q: Option<u32>,
        ov_q: Option<u32>,
        mu: u8,
        sd: u8,
        r: u32,
    ) -> Self {
        let dw = dw_q.unwrap_or(dw).clamp(320, 2000);
        let vh = vh_q.unwrap_or(vh).clamp(320, 2000);
        let ov = ov_q.unwrap_or(ov).clamp(0, vh / 4);
        Self {
            dw,
            vh,
            ov,
            mu,
            sd,
            r: r.clamp(8, 480),
        }
    }

    /// 缓存 key 里的参数段；参数一变 key 必变，旧档不会串味。
    /// `sl2` + `ik2-g10`：新增切点邻域墨量守卫后升版，旧 `sl1` 切片一律不再命中。
    pub(super) fn label(&self) -> String {
        format!(
            "sl2-dw{}-vh{}-ov{}-mu{}-sd{}-r{}-ik{}-g{}",
            self.dw, self.vh, self.ov, self.mu, self.sd, self.r, SLICE_INK_PCT, SLICE_G_CSS
        )
    }
}

/// 版心宽（CSS px）
pub(super) const SLICE_DW_DEFAULT: u32 = 928;
/// 可用高（CSS px）= 1268 舞台 − 72 底栏留白
pub(super) const SLICE_VH_DEFAULT: u32 = 1196;
/// 片间 overlap（CSS px）：硬切时保证上下片在切点附近不丢行
pub(super) const SLICE_OV_DEFAULT: u32 = 64;
/// 切点搜索半径（CSS px）
pub(super) const SLICE_R_CSS: u32 = 96;
/// 近白行判定：3 行带 × 64 列采样的亮度均值下限
pub(super) const SLICE_MU: u8 = 200;
/// 近白行判定：带内标准差上限（缓存 key 用整数标签，切片时转 f64）
pub(super) const SLICE_SD_U8: u8 = 30;
/// 采样列数（整行扫描 1200×2872 要 340 万次读取，采样后每候选行约 200 次）
pub(super) const SLICE_SAMPLE_COLS: usize = 64;
/// 切点邻域校验带宽（CSS px）：±10px 内出现暗像素即判「切在画面上」
pub(super) const SLICE_G_CSS: u32 = 10;
/// 切点邻域暗像素占比上限（%）：切点行自身 ≤2%，带内均值 ≤4×2%
pub(super) const SLICE_INK_PCT: u8 = 2;
/// 带内均值相对切点行阈值的倍数
pub(super) const SLICE_INK_BAND_MULT: f64 = 4.0;
/// 暗像素亮度阈值（0–255）：低于此值算「画面内容」
pub(super) const SLICE_DARK_LUMA: u8 = 140;
/// 末片最小高度（相对窗高的比例）：避免只剩一条 64–128px 的「薄片」
pub(super) const SLICE_TAIL_MIN: f64 = 0.3;
/// 相邻切点最小间距（相对窗高的比例）：防御性下限，防止裁剪出过短的片
pub(super) const SLICE_MIN_GAP: f64 = 0.35;
/// 超过 1.3 屏才切：1.1 屏页切两片字节反而 +6.7%，首片已是整图 92%
pub(super) const SLICE_MAX_SCREENS: f64 = 1.3;

/// `slice=1/true/on` 才启用切片；缺省与 `slice=0` 走整图路径，字节输出与旧版一致。
pub(super) fn slice_requested(raw: Option<&str>) -> bool {
    raw.map(|v| {
        let v = v.trim().to_ascii_lowercase();
        v == "1" || v == "true" || v == "on"
    })
    .unwrap_or(false)
}

/// 显示高度（CSS px）= 原图高 × 版心宽 / 原图宽。
pub(super) fn display_height(w: u32, h: u32, p: &SliceParams) -> f64 {
    if w == 0 {
        return 0.0;
    }
    h as f64 * p.dw as f64 / w as f64
}

/// 是否值得切：显示高度超过 1.3 屏才切。
pub fn worth_slicing(w: u32, h: u32, p: &SliceParams) -> bool {
    display_height(w, h, p) > p.vh as f64 * SLICE_MAX_SCREENS
}

/// 源像素空间的（窗高, overlap, 搜索半径, 守卫带宽）：按显示缩放比换算，各至少 1px。
pub(super) fn src_metrics(img_w: u32, p: &SliceParams) -> (usize, usize, usize, usize) {
    let css_to_src = img_w.max(1) as f64 / p.dw as f64;
    let win = ((p.vh as f64 * css_to_src).round() as usize).max(64);
    let ov = ((p.ov as f64 * css_to_src).round() as usize).min(win / 4);
    let r = ((p.r as f64 * css_to_src).round() as usize).max(1);
    let g = ((SLICE_G_CSS as f64 * css_to_src).round() as usize).max(1);
    (win, ov, r, g)
}

/// 3 行带 × 64 列采样的亮度（μ, σ）；行越界返回 `None`。
pub(super) fn band_stats(raw: &[u8], w: usize, h: usize, y: usize) -> Option<(u8, f64)> {
    if y >= h || w == 0 {
        return None;
    }
    let y0 = y.saturating_sub(1);
    let y1 = (y + 1).min(h - 1);
    let step = (w / SLICE_SAMPLE_COLS).max(1);
    let mut sum = 0f64;
    let mut sum2 = 0f64;
    let mut n = 0u64;
    for row in y0..=y1 {
        let base = row * w;
        let mut x = 0usize;
        while x < w {
            let v = raw[base + x] as f64;
            sum += v;
            sum2 += v * v;
            n += 1;
            x += step;
        }
    }
    if n == 0 {
        return None;
    }
    let mean = sum / n as f64;
    let var = (sum2 / n as f64 - mean * mean).max(0.0);
    Some((mean.round().clamp(0.0, 255.0) as u8, var.sqrt()))
}

/// 单行暗像素（`luma < 140`）占比，全宽隔列采样；越界返回 `None`。
pub(super) fn row_ink_ratio(raw: &[u8], w: usize, h: usize, y: usize) -> Option<f64> {
    if y >= h || w == 0 {
        return None;
    }
    let step = (w / 256).max(1);
    let base = y * w;
    let mut ink = 0u64;
    let mut n = 0u64;
    let mut x = 0usize;
    while x < w {
        if raw[base + x] < SLICE_DARK_LUMA {
            ink += 1;
        }
        n += 1;
        x += step;
    }
    if n == 0 {
        return None;
    }
    Some(ink as f64 / n as f64)
}

/// 切点邻域守卫：切点行自身墨量 ≤ `ink_pct`%，±`g` 带内均值 ≤ `ink_pct × 4`%。
///
/// 空白沟槽墨量 ≈ 0；脸/眼/发区域即便切点行恰好落在高光，带内也必有暗像素而被拒。
/// 只读已解码的灰度缓冲，不额外分配内存。
pub(super) fn cut_guard_ok(
    raw: &[u8],
    w: usize,
    h: usize,
    y: usize,
    g: usize,
    ink_pct: f64,
) -> bool {
    let Some(own) = row_ink_ratio(raw, w, h, y) else {
        return false;
    };
    if own * 100.0 > ink_pct {
        return false;
    }
    let y0 = y.saturating_sub(g);
    let y1 = (y + g).min(h - 1);
    let mut sum = 0.0;
    let mut n = 0usize;
    for row in y0..=y1 {
        if let Some(v) = row_ink_ratio(raw, w, h, row) {
            sum += v;
            n += 1;
        }
    }
    n > 0 && (sum / n as f64) * 100.0 <= ink_pct * SLICE_INK_BAND_MULT
}

/// 纯函数切点规划：返回升序切点，首尾必为 `0` 与 `h`。
///
/// 目标切点是本片最远端 `start + win`（再往下就超过屏高）；在 `[target − r, target]`
/// 内找近白行（3 行带 × 64 列采样，μ ≥ `mu` 且 σ ≤ `sd`），再按「切点邻域墨量」二次校验：
/// 切点行自身暗像素 ≤ `ink_pct`%，±`g` 带内均值 ≤ 4×`ink_pct`%。全部不过时回退到不校验的
/// 最近近白行（与旧行为一致），再无近白行则硬切在 `target`。
/// 切点不超过 `start + win` 且间距 ≥ 0.35×窗高，保证每片显示高 ≤ `vh`；末刀优先让末片高度
/// ≥ 0.3×窗高（免得出 64–128px 的薄片）。同一图同一参数结果恒定。
#[allow(clippy::too_many_arguments)] // 切图几何参数（窗/重叠/行数/目标）均为标量，签名直白优于打包。
pub fn plan_cuts(
    luma: &image::GrayImage,
    win_src: usize,
    ov_src: usize,
    r_src: usize,
    g_src: usize,
    mu: u8,
    sd: f64,
    ink_pct: f64,
) -> Vec<usize> {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    if h == 0 || w == 0 {
        return vec![0, h];
    }
    let win = win_src.max(64);
    let min_gap = ((win as f64 * SLICE_MIN_GAP).ceil() as usize).max(1);
    let min_tail = ((win as f64 * SLICE_TAIL_MIN).ceil() as usize).max(1);
    let ov = ov_src.min(win / 4);
    if h <= win {
        return vec![0, h];
    }
    let raw = luma.as_raw();
    let mut cuts = vec![0usize];
    let mut start = 0usize;
    let mut guard = 0usize;
    while start + win < h && guard < 512 {
        guard += 1;
        let target = start + win;
        // 间距下限：r 很大时也不许把切点贴到上一刀旁边
        let lo = target.saturating_sub(r_src).max(start + min_gap);
        let hi = (target + r_src).min(start + win).min(h - 1);
        // 落在 [h − win + ov, h) 的切点就是最后一刀，此时末片高度直接决定观感
        let last_from = h.saturating_sub(win.saturating_sub(ov));
        // (距离, y, 均值, 末片是否够高)：优先守卫通过 + 末片够高，再比距离/亮度
        let mut best_ok: Option<(usize, usize, u8, bool)> = None;
        // 守卫全不过时的回退：旧的「最近近白行」，保证不劣于现状
        let mut best_raw: Option<(usize, usize, u8)> = None;
        for y in lo..=hi {
            let Some((m, s)) = band_stats(raw, w, h, y) else {
                continue;
            };
            if m < mu || s > sd {
                continue;
            }
            let d = y.abs_diff(target);
            let better_raw = match best_raw {
                None => true,
                Some((bd, _, bm)) => d < bd || (d == bd && m > bm),
            };
            if better_raw {
                best_raw = Some((d, y, m));
            }
            if !cut_guard_ok(raw, w, h, y, g_src, ink_pct) {
                continue;
            }
            let tail_ok = if y >= last_from {
                h - y >= min_tail
            } else {
                true
            };
            let better = match best_ok {
                None => true,
                Some((bd, _, bm, bt)) => {
                    if tail_ok != bt {
                        tail_ok
                    } else {
                        d < bd || (d == bd && m > bm)
                    }
                }
            };
            if better {
                best_ok = Some((d, y, m, tail_ok));
            }
        }
        let cut = best_ok
            .map(|(_, y, _, _)| y)
            .or_else(|| best_raw.map(|(_, y, _)| y))
            .unwrap_or(target)
            .clamp(start + min_gap, h);
        debug_assert!(cut - start >= min_gap.min(h - start), "切点间距下限失效");
        cuts.push(cut);
        if cut + ov >= h {
            break;
        }
        start = cut.saturating_sub(ov);
    }
    if cuts.last().copied() != Some(h) {
        cuts.push(h);
    }
    cuts
}

/// 由切点展开成每片的源行区间：第 i>0 片向前多吃 `ov` 行，形成片间重叠。
pub fn slice_ranges(cuts: &[usize], ov_src: usize, h: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..cuts.len().saturating_sub(1) {
        let start = if i == 0 {
            0
        } else {
            cuts[i].saturating_sub(ov_src)
        };
        let end = cuts[i + 1].min(h);
        if end > start {
            out.push((start, end));
        }
    }
    out
}

/// 只按窗口算片数（不读像素）：`1 + ceil((h − win) / (win − ov))`，供 meta/目录预判。
pub fn slice_count_window(w: u32, h: u32, p: &SliceParams) -> usize {
    if !worth_slicing(w, h, p) {
        return 1;
    }
    let (win, ov, _, _) = src_metrics(w, p);
    let hh = h as usize;
    if hh <= win {
        return 1;
    }
    let step = win.saturating_sub(ov).max(1);
    let extra = (hh - win).div_ceil(step);
    1 + extra
}

/// 每片显示高度（CSS px）估算：非末片按窗高，末片按剩余。实际白切点会在此附近 ±r。
pub fn slice_display_heights(w: u32, h: u32, p: &SliceParams) -> Vec<u32> {
    let scale = p.dw as f64 / w.max(1) as f64;
    let n = slice_count_window(w, h, p);
    if n <= 1 {
        return vec![(h as f64 * scale).round().min(p.vh as f64) as u32];
    }
    let (win, ov, _, _) = src_metrics(w, p);
    let hh = h as usize;
    let mut out = Vec::with_capacity(n);
    let mut start = 0usize;
    while start + win < hh {
        out.push(p.vh);
        start += win - ov;
        if start >= hh {
            break;
        }
    }
    let remain = hh.saturating_sub(start);
    if remain > 0 {
        out.push(((remain as f64 * scale).round() as u32).min(p.vh));
    }
    out.truncate(n);
    out
}

/// 切片缓存 key：`i{i}of{n}` 必须与片数一起进 key，避免「缓存 3 片、请求要 2 片」错位。
pub(super) fn slice_variant_key(base: &str, p: &SliceParams, i: usize, n: usize) -> String {
    format!("{base}#{}-i{}of{}", p.label(), i, n)
}

/// 切点计划缓存 key：只依赖原图与切片参数（质量/自适应不影响切点）。
pub(super) fn slice_plan_key(url: &str, cfg: &EinkConfig, p: &SliceParams) -> String {
    format!(
        "{url}#{}-plan-w{}-a{}",
        p.label(),
        cfg.width,
        u8::from(cfg.adaptive)
    )
}

/// 切点计划：片数 + 源行切点，随原图确定性生成，可跨请求复用。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct SlicePlan {
    pub(super) n: usize,
    pub(super) cuts: Vec<usize>,
}

pub(super) fn plan_get(key: &str) -> Option<SlicePlan> {
    image_cache::get(key).and_then(|(b, _)| serde_json::from_slice(&b).ok())
}

pub(super) fn plan_put(key: &str, plan: &SlicePlan) {
    if let Ok(b) = serde_json::to_vec(plan) {
        image_cache::put(key, b, "application/json".to_string());
    }
}

/// 从灰度图裁出 `[y0, y1)` 行的一段（切片编码用）。
pub(super) fn crop_luma_rows(
    luma: &image::GrayImage,
    y0: usize,
    y1: usize,
) -> Option<image::GrayImage> {
    let w = luma.width() as usize;
    let h = luma.height() as usize;
    if y0 >= y1 || y1 > h {
        return None;
    }
    let buf = luma.as_raw()[y0 * w..y1 * w].to_vec();
    image::GrayImage::from_raw(luma.width(), (y1 - y0) as u32, buf)
}

/// 切片结果：整图退化时 `n == 1`，与 `slice=0` 的字节完全一致。
pub(super) struct SliceOutcome {
    pub(super) data: Vec<u8>,
    pub(super) dims: Dims,
    pub(super) quality: u8,
    pub(super) n: usize,
    pub(super) index: usize,
    pub(super) plan: Option<SlicePlan>,
}

/// 解码 → 灰度/缩放 → 切点规划 → 裁片 → JPEG q70 重编码。
/// 显示高度 ≤ 1.3 屏时直接退化成整图（不切），保证 1.1 屏页字节不变。
pub(super) fn transcode_slice(
    raw: &[u8],
    format: image::ImageFormat,
    profile: EncodeProfile,
    cfg: EinkConfig,
    p: &SliceParams,
    want: usize,
) -> Result<SliceOutcome, String> {
    let reader = image::ImageReader::with_format(std::io::Cursor::new(raw), format);
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("图片头部解析失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err("图片尺寸为 0".into());
    }
    if !worth_slicing(w, h, p) {
        let (data, dims, quality) = transcode_to_jpeg(raw, format, profile, cfg)?;
        return Ok(SliceOutcome {
            data,
            dims,
            quality,
            n: 1,
            index: 0,
            plan: None,
        });
    }
    let img = image::load_from_memory_with_format(raw, format)
        .map_err(|e| format!("图片解码失败: {e}"))?;
    let luma = img.to_luma8();
    let (tw, th) = transcode_target_size(luma.width(), luma.height(), cfg.width);
    let resized = if (tw, th) == (luma.width(), luma.height()) {
        luma
    } else {
        resize_lanczos_linear(&luma, tw, th)
    };
    let (win, ov, r, g) = src_metrics(resized.width(), p);
    let cuts = plan_cuts(
        &resized,
        win,
        ov,
        r,
        g,
        p.mu,
        p.sd as f64,
        SLICE_INK_PCT as f64,
    );
    let ranges = slice_ranges(&cuts, ov, resized.height() as usize);
    if ranges.len() <= 1 {
        let out = if cfg.usm { usm_luma(&resized) } else { resized };
        let dims = Dims {
            width: out.width(),
            height: out.height(),
        };
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, profile.quality)
            .encode_image(&out)
            .map_err(|e| format!("JPEG 编码失败: {e}"))?;
        return Ok(SliceOutcome {
            data: buf,
            dims,
            quality: profile.quality,
            n: 1,
            index: 0,
            plan: None,
        });
    }
    let n = ranges.len();
    let index = want.min(n - 1);
    let (y0, y1) = ranges[index];
    let slice = crop_luma_rows(&resized, y0, y1).ok_or_else(|| "切片越界".to_string())?;
    let out = if cfg.usm { usm_luma(&slice) } else { slice };
    let dims = Dims {
        width: out.width(),
        height: out.height(),
    };
    let quality = profile.quality;
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(&out)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(SliceOutcome {
        data: buf,
        dims,
        quality,
        n,
        index,
        plan: Some(SlicePlan { n, cuts }),
    })
}

pub(super) fn key_pairs(url: &str, jpeg_source: bool, cfg: &EinkConfig) -> Vec<(String, String)> {
    candidate_qualities(cfg)
        .into_iter()
        .map(|q| {
            let key = variant_key(url, jpeg_source, cfg, q);
            let etag = etag_of(&key);
            (key, etag)
        })
        .collect()
}
