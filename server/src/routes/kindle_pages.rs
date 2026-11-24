//! /k 的 pages 部分（move-only 从 kindle.rs 拆出）。

use super::*;

// ---------------- HTML 工具 ----------------

/// HTML 转义（标题/正文等所有用户与书源数据都必须过这里）
pub(super) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// query 参数百分号编码（book_id / source 都是 URL，必须编码后拼 href）
pub(super) fn qenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// 给 /k 内部链接补上字号参数（Cookie 之外的第二重保险）
pub(super) fn with_fs(url: &str, fs: usize) -> String {
    if url.contains('?') {
        format!("{url}&fs={fs}")
    } else {
        format!("{url}?fs={fs}")
    }
}

/// 给链接补上视口参数：链接自传播，客户端翻页零测量、零额外请求。
pub(super) fn with_vp(url: &str, vp: Vp) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}vw={}&vh={}", vp.w, vp.h)
}

/// /k 内部链接的标配：字号 + 视口参数。
pub(super) fn qlink(url: &str, fs: usize, vp: Vp) -> String {
    with_vp(&with_fs(url, fs), vp)
}

/// 解析字号：显式 `?fs=1..4` 优先，其次 `k_fs` Cookie，缺省标准档（2）。
/// 档位 1/2/3 为既有小/标准/大，4 为新增超小档。
/// 返回 (档位, 是否需要回写 Cookie)。
pub(super) fn resolve_fs(headers: &HeaderMap, q: Option<i64>) -> (usize, bool) {
    if let Some(v) = q {
        if (1..=4).contains(&v) {
            return (v as usize, true);
        }
    }
    if let Some(cookie) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for kv in cookie.split(';') {
            if let Some(rest) = kv.trim().strip_prefix(&format!("{K_FS_COOKIE}=")) {
                if let Ok(v) = rest.trim().parse::<i64>() {
                    if (1..=4).contains(&v) {
                        return (v as usize, false);
                    }
                }
            }
        }
    }
    (2, false)
}

/// 字号轮换：按字号从小到大 4→1→2→3，避免“大→超小”以外的乱跳
pub(super) fn cycle_fs(fs: usize) -> usize {
    match fs {
        4 => 1,
        1 => 2,
        2 => 3,
        _ => 4,
    }
}

/// 显式切换字号时把档位写进 30 天 Cookie
pub(super) fn remember_fs(res: &mut Response, fs: usize) {
    let value = format!("{K_FS_COOKIE}={fs}; Path=/; Max-Age=2592000; SameSite=Lax");
    if let Ok(v) = HeaderValue::from_str(&value) {
        res.headers_mut().insert(SET_COOKIE, v);
    }
}

/// 统一页面骨架：固定版心、`-webkit-text-size-adjust:none` 防老引擎自动放大。
/// viewport 宽度随请求视口（`vw`），缺参时回落到默认 1072；不写 initial-scale=1
/// 时 Silk 会按 device-width 缩放。
/// 生产渲染走 `page_html_css`（需要页面级 CSS）；本函数为无额外 CSS 的便捷封装，仅单测使用。
#[allow(dead_code)]
pub(super) fn page_html(title: &str, fs: usize, body: &str, vp: Vp) -> String {
    page_html_css(title, fs, body, "", vp)
}

/// 与 `page_html` 相同，额外拼接一段页面级 CSS（阅读页用「一屏一页」锁高）。
/// viewport 只声明宽度与 scale：**不再输出 `height=`**（真机实测该 meta 整条不被认，
/// 且方向已定为「不靠骗浏览器」）。可用高由 `K_VP_JS` 现算并以 `vh` 参数传回；
/// `html/body` 高度统一锁成运行时 `H`，任何页的贴底元素都落在真实可见区内。
pub(super) fn page_html_css(title: &str, fs: usize, body: &str, extra_css: &str, vp: Vp) -> String {
    // 字号/行高按统一比例缩放（1072 → 原值，390 → 50%），见 `Vp::px`
    let spec = vp.fs(fs);
    // 显式 vw → 锁死版心宽（1:1 不缩放）；缺参 → device-width
    let width = if vp.fixed {
        format!("width={}", vp.w)
    } else {
        "width=device-width".to_string()
    };
    let viewport = format!("{width},initial-scale=1,maximum-scale=1,user-scalable=0");
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"{viewport}\"><title>{title}</title><style>\
html{{-webkit-text-size-adjust:none;height:{doc}px;overflow:hidden}}\
html,body{{margin:0;padding:0;width:100%;background:#fff;color:#000}}\
body{{position:relative;height:{doc}px;overflow:hidden;font-family:serif;font-size:{size}px;line-height:{line}px}}\
*{{box-sizing:border-box}}\
a{{color:#000;text-decoration:none}}\
h1,h2,h3,p{{margin:0}}\
table{{border-collapse:collapse}}\
.wrap{{width:{pw}px;margin-left:{pad}px}}\
{extra}\
</style><script>{guard}</script><script>{vpjs}</script></head><body><div class=\"wrap\">{body}</div><script>{probe}</script></body></html>",
        viewport = viewport,
        title = esc(title),
        doc = vp.layout_h(),
        size = spec.size,
        line = spec.line,
        pw = vp.page_w(),
        pad = vp.pad(),
        extra = extra_css,
        body = body,
        guard = K_GUARD_JS,
        vpjs = k_vp_js(),
        probe = K_PROBE_JS
    )
}

pub(super) fn page(title: &str, fs: usize, body: &str, vp: Vp) -> Response {
    page_css(title, fs, body, "", vp)
}

/// 带 `<meta http-equiv="refresh">` 的页面：`/k` 搜索中页用它做无 JS 轮询。
/// 只刷新当前 URL，不改变查询参数，因此不会重复创建任务。
pub(super) fn page_refresh(title: &str, fs: usize, body: &str, vp: Vp, secs: u64) -> Response {
    let html = page_html_css(title, fs, body, "", vp);
    let meta = format!("<meta http-equiv=\"refresh\" content=\"{secs}\">");
    let html = html.replacen("<head>", &format!("<head>{meta}"), 1);
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .expect("html response")
}

pub(super) fn page_css(title: &str, fs: usize, body: &str, extra_css: &str, vp: Vp) -> Response {
    let html = page_html_css(title, fs, body, extra_css, vp);
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/html; charset=utf-8")
        .body(axum::body::Body::from(html))
        .expect("html response")
}

/// 顶栏（73px = 72 内容 + 1px 下边线，白底，只有一条）：
/// 左：标题 28px 加粗（可带一段 20px 弱灰副标题）；右：动作链接 20px，间距 32px。
/// 全部尺寸按 `Vp` 统一比例缩放：390 宽 → 44px 高、14px 标题、13px 动作。
pub(super) fn top_bar(title: &str, sub: Option<&str>, actions: &str, vp: Vp) -> String {
    let sub = match sub {
        Some(s) => format!(
            " <span style=\"font-size:{f20}px;color:#555;font-weight:400\">{}</span>",
            esc(s),
            f20 = vp.px(20)
        ),
        None => String::new(),
    };
    let bar_h = vp.nav_bar_h();
    let cell_h = bar_h.saturating_sub(1);
    format!(
        "<div id=\"k-nav\" style=\"height:{bar_h}px;border-bottom:1px solid #ccc;background:#fff;white-space:nowrap;overflow:hidden\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td height=\"{cell_h}\" align=\"left\" style=\"font-size:{f28}px;line-height:{cell_h}px;font-weight:700;padding-left:{pad}px;overflow:hidden\">{title}{sub}</td>\
         <td height=\"{cell_h}\" align=\"right\" style=\"font-size:{f20}px;line-height:{cell_h}px;padding-right:{pad}px\">{actions}</td>\
         </tr></table></div>",
        bar_h = bar_h,
        cell_h = cell_h,
        pw = vp.page_w(),
        pad = vp.pad(),
        f28 = vp.px(28),
        f20 = vp.px(20),
        title = esc(title),
        sub = sub,
        actions = actions
    )
}

/// 顶栏动作链接：参考 20px 字号、32px 间距，按统一比例缩放（390 → 13px / 16px）。
pub(super) fn nav_link(href: &str, label: &str, vp: Vp) -> String {
    format!(
        "<a href=\"{href}\" style=\"padding-left:{pad}px;font-size:{size}px\">{label}</a>",
        href = href,
        pad = vp.px(32),
        size = vp.px(20),
        label = esc(label)
    )
}

/// 阅读页底栏：形如贴视口底，但用 `position:absolute;bottom:0`，它的包含块
/// （`body{position:relative}`）高度 = 运行时现算的版面高 `H`。
/// **不能用 `position:fixed`**：老 Silk 把 fixed 元素锚在布局视口（1268）而不是真实可见区。
/// 总高 `vp.bar_h()`（随运行时可用高派生，约 5.2%），内含 `SAFE_BOTTOM` 安全区。
/// 底栏只有 3 格等分（前 / 当前字号 / 后），整格都是 `click` 热区。
pub(super) fn fixed_bar(row: &str, vp: Vp) -> String {
    format!(
        "<div id=\"k-bar\" style=\"position:absolute;left:0;right:0;bottom:0;height:{h}px;padding-bottom:{safe}px;background:#fff;z-index:10\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-left:{pad}px;table-layout:fixed\"><tr>{row}</tr></table></div>",
        h = vp.bar_h(),
        safe = vp.len(SAFE_BOTTOM),
        pw = vp.page_w(),
        pad = vp.pad(),
        row = row
    )
}

/// 底栏格：等分 1/3、整格可点，只有一行主标签（无下方小字）。
pub(super) fn bar_cell(href: &str, label: &str, vp: Vp) -> String {
    let ch = vp.bar_content_h().saturating_sub(1);
    format!(
        "<td width=\"33.33%\" height=\"{ch}\" align=\"center\" valign=\"middle\" style=\"height:{ch}px;border-top:1px solid #ccc\">\
         <a href=\"{href}\" style=\"display:block;height:{ch}px;line-height:{ch}px;font-size:{fs}px;font-weight:700\">{label}</a></td>",
        href = href,
        ch = ch,
        fs = vp.px(34),
        label = esc(label)
    )
}

/// 底栏格：禁用态（弱灰、不可点），用于全书末页的「后」
pub(super) fn bar_cell_off(label: &str, vp: Vp) -> String {
    let ch = vp.bar_content_h().saturating_sub(1);
    format!(
        "<td width=\"33.33%\" height=\"{ch}\" align=\"center\" valign=\"middle\" style=\"height:{ch}px;border-top:1px solid #ccc\">\
         <span style=\"display:block;height:{ch}px;line-height:{ch}px;font-size:{fs}px;font-weight:700;color:#555\">{label}</span></td>",
        ch = ch,
        fs = vp.px(34),
        label = esc(label)
    )
}

/// 列表页翻页条（书架/搜索/目录）：64px 高、两格各占一半，贴 stage 底部（左右跟随 stage）。
/// 禁用态用 `#555` 弱灰且不可点。
pub(super) fn pager_bar(
    prev: Option<(String, &'static str)>,
    next: Option<(String, &'static str)>,
    vp: Vp,
) -> String {
    let h = vp.pager_h();
    let ch = h.saturating_sub(1);
    let cell = |item: &Option<(String, &'static str)>, fallback: &str| {
        match item {
        Some((href, label)) => format!(
            "<td width=\"50%\" height=\"{h}\" align=\"center\" valign=\"middle\" style=\"height:{ch}px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
             <a href=\"{href}\" style=\"display:block;height:{ch}px;line-height:{ch}px;font-size:{fs}px\">{label}</a></td>",
            h = h,
            ch = ch,
            fs = vp.px(24),
            href = href,
            label = esc(label)
        ),
        None => format!(
            "<td width=\"50%\" height=\"{h}\" align=\"center\" valign=\"middle\" style=\"height:{ch}px;border-top:1px solid #ccc;font-size:0;line-height:0\">\
             <span style=\"display:block;height:{ch}px;line-height:{ch}px;font-size:{fs}px;color:#555\">{label}</span></td>",
            h = h,
            ch = ch,
            fs = vp.px(24),
            label = fallback
        ),
    }
    };
    format!(
        "<div id=\"k-pager\" style=\"position:absolute;left:0;right:0;bottom:0;height:{h}px;background:#fff;z-index:8\">\
         <table width=\"{pw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"table-layout:fixed\"><tr>{p}{n}</tr></table></div>",
        pw = vp.page_w(),
        h = h,
        p = cell(&prev, "上一页"),
        n = cell(&next, "下一页")
    )
}

/// 左右透明翻页热区：各占屏宽 1/3，`top:{READ_NAV_H}`（从顶栏下沿开始）到底。
/// `position:absolute` 相对初始包含块（`.wrap` 不是定位元素，故不受其裁剪），
/// `z-index:5` 高于正文（0）低于底栏（10）与顶栏（12）；顶栏整格可点，
/// 热区又整整让开 56px，所以点书名/「书架」不会误翻页。不再写死 `height:vh−24`，
/// 否则视口比参数矮时热区自己会顶出第二屏。
pub(super) fn hot_zones(prev: Option<&str>, next: Option<&str>, vp: Vp) -> String {
    let css = format!(
        "position:absolute;top:{nav}px;bottom:0;width:33%;z-index:5;display:block",
        nav = vp.read_nav_h()
    );
    let mut out = String::new();
    if let Some(pv) = prev {
        out.push_str(&format!(
            "<a href=\"{prev}\" style=\"{css};left:0\"></a>",
            prev = pv,
            css = css
        ));
    }
    if let Some(nx) = next {
        out.push_str(&format!(
            "<a href=\"{nx}\" style=\"{css};right:0\"></a>",
            nx = nx,
            css = css
        ));
    }
    out
}

/// 按像素宽度截断文本：CJK/全角按 `size` 像素计，ASCII 按 `half` 计；超宽用 `…` 收尾。
/// 墨水屏不能用 `text-overflow`（老 Silk 不稳定），所以服务端直接把书名截到能放下的长度，
/// 再配 `white-space:nowrap` + `overflow:hidden` 双保险，保证顶栏高度不被撑开。
pub(super) fn fit_px(s: &str, max_px: usize, size: usize, half: usize) -> String {
    let mut w = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let cw = if (ch as u32) < 0x2E80 { half } else { size };
        if w + cw > max_px {
            out.push('…');
            return out;
        }
        w += cw;
        out.push(ch);
    }
    out
}

/// 阅读页顶栏（小说页/漫画页共用）：左「← 书架」、中书名（按像素截断）、右「目录」。
/// 高度 `READ_NAV_H`(56)，`position:fixed` 贴视口顶，白底 + 1px 黑分隔线，`z-index:12`
/// 高于翻页热区（5）与底栏（10）；三格全部 `display:block` 整格可点，
/// 书架/目录链接都带当前 `fs/vw/vh`，点一次就能带着同一档字号与视口回去。
pub(super) fn read_nav_html(title: &str, back_href: &str, toc_href: &str, vp: Vp) -> String {
    // 左右两格按统一比例缩放（1072 → 160/120，390 → 80/60），标题仍居中
    let (bw, tw) = (vp.len(READ_NAV_BACK_W), vp.len(READ_NAV_TOC_W));
    let title_px = vp
        .content_w()
        .saturating_sub(bw + tw)
        .saturating_sub(vp.len(24));
    let title = fit_px(title, title_px, vp.px(20), vp.len(10));
    format!(
        "<div id=\"k-read-nav\"><table cellpadding=\"0\" cellspacing=\"0\"><tr>\
<td width=\"{bw}\" align=\"center\" style=\"border-right:1px solid #ccc\"><a href=\"{back}\">&larr; 书架</a></td>\
<td align=\"center\">{title}</td>\
<td width=\"{tw}\" align=\"center\" style=\"border-left:1px solid #ccc\"><a href=\"{toc}\">目录</a></td>\
</tr></table></div>",
        back = back_href,
        toc = toc_href,
        title = esc(&title)
    )
}

pub(super) fn read_only_css(vp: Vp) -> String {
    let nav_h = vp.read_nav_h();
    format!(
        // 整屏分页（v0.3.22）：文档高 = 运行时实测可用高（`layout_h()`，来自 `vh` 参数），
        // 顶栏/正文/底栏全部从它派生；正文区写死高度（= 文档高 − 顶栏 − 底栏）且
        // `overflow:hidden`，页面结构上就没有可滚内容。**不做任何隐藏滚动条的补偿**：
        // 若 Silk 仍自绘视口竖条，就让它留着。也绝不用 `position:fixed`（老 Silk 会把它
        // 锚到布局视口 1268 而不是真实可见高），`body` 作 absolute 子元素的包含块。
        "html,body{{position:relative;top:0;left:0;width:100%;height:{doc}px;overflow:hidden}}\
         #k-read-nav{{position:absolute;top:0;left:0;right:0;height:{nav}px;padding:0 {pad}px;background:#fff;border-bottom:1px solid #000;z-index:12}}\
         #k-read-nav table{{width:100%;table-layout:fixed;border-collapse:collapse}}\
         #k-read-nav td{{height:{tdh}px;padding:0;font-size:{f20}px;line-height:{l28}px;vertical-align:middle;overflow:hidden;white-space:nowrap;color:#000}}\
         #k-read-nav td a{{display:block;height:{tdh}px;line-height:{tdh}px;color:#000;text-decoration:none}}\
         #k-stage{{position:absolute;top:{nav}px;left:{pad}px;width:{sw}px;height:{sh}px;padding:{pt}px {pad}px 0;overflow:hidden}}\
         #k-page-no{{position:absolute;right:{pad}px;bottom:8px;width:{pnw}px;font-size:{f20}px;line-height:{l28}px;color:#555;text-align:right}}",
        doc = vp.layout_h(),
        pad = vp.pad(),
        nav = nav_h,
        tdh = nav_h.saturating_sub(1),
        sw = vp.page_w(),
        sh = vp.layout_h().saturating_sub(nav_h + vp.bar_h()),
        pt = vp.read_pad_top(),
        pnw = vp.len(200),
        f20 = vp.px(20),
        l28 = vp.px(28)
    )
}

/// 阅读页内部链接：始终带目标档位的 `fs`，切档后不会被当前档位拼成重复参数。
pub(super) fn read_link(q: &ReadQ, fs: usize, ch: i64, p: i64, vp: Vp) -> String {
    qlink(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            ch,
            p
        ),
        fs,
        vp,
    )
}

/// 目录链接（按当前视口的每页章节数定位到当前章所在目录页，并带上当前章用于高亮）
pub(super) fn toc_link(q: &ReadQ, fs: usize, ch: i64, p: i64, vp: Vp) -> String {
    let per = toc_page_size(vp).max(1) as i64;
    qlink(
        &format!(
            "/k/toc?book={}&source={}&tp={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            (ch / per) + 1,
            ch,
            p
        ),
        fs,
        vp,
    )
}

/// 翻页导航：翻完本章接上一章/下一章；全书首页/末页对应项为 `None`（底栏禁用 + 热区不渲染）。
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // 导航参数均为标量；返回类型是 (上一项, 下一项) 二元组，本身语义清晰。
pub(super) fn read_nav(
    q: &ReadQ,
    fs: usize,
    ch: i64,
    p: i64,
    n: i64,
    total: i64,
    _toc: &str,
    vp: Vp,
) -> (
    Option<(String, &'static str)>,
    Option<(String, &'static str)>,
) {
    let prev = if p > 1 {
        Some((read_link(q, fs, ch, p - 1, vp), "上一页"))
    } else if ch > 0 {
        Some((read_link(q, fs, ch - 1, 1, vp), "上一章"))
    } else {
        None
    };
    let next = if p < n {
        Some((read_link(q, fs, ch, p + 1, vp), "下一页"))
    } else if ch + 1 < total {
        Some((read_link(q, fs, ch + 1, 1, vp), "下一章"))
    } else {
        None
    };
    (prev, next)
}

/// 底栏「上一页」格：章首变「上一章」，全书首页为禁用态
pub(super) fn bar_cell_prev(prev: Option<&(String, &'static str)>, vp: Vp) -> String {
    match prev {
        Some((href, _)) => bar_cell(href, "前", vp),
        None => bar_cell_off("前", vp),
    }
}

/// 底栏「下一页」格：有下一页/下一章则给链接，全书末页给弱灰不可点的禁用态。
pub(super) fn bar_cell_next(next: Option<&(String, &'static str)>, vp: Vp) -> String {
    match next {
        Some((href, _)) => bar_cell(href, "后", vp),
        None => bar_cell_off("后", vp),
    }
}

/// 描边按钮（只描边不填充、圆角 0、无阴影/渐变/过渡）：参考 240×72，按统一比例缩放
pub(super) fn outline_button(href: &str, label: &str, vp: Vp) -> String {
    let h = vp.tap(72);
    format!(
        "<a href=\"{href}\" style=\"display:block;width:{w}px;height:{h}px;line-height:{lh}px;border:1px solid #000;text-align:center;font-size:{fs}px\">{label}</a>",
        href = href,
        w = vp.len(240),
        h = h,
        lh = h.saturating_sub(2),
        fs = vp.px(24),
        label = esc(label)
    )
}

/// 空书架：2px 描边块，整块在可用区内垂直居中（`table` 100% + `valign="middle"`），
/// 含标题 26 加粗 + 说明 20 `#555` + 240×72 描边按钮。
pub(super) fn empty_shelf(fs: usize, vp: Vp) -> String {
    // 卡片最宽 600（参考视口），窄屏收进版心，不横向溢出
    let card_w = vp.len(600).min(vp.page_w());
    format!(
        "<table width=\"{pw}\" height=\"{h}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
         <td align=\"center\" valign=\"middle\">\
         <table width=\"{card_w}\" cellpadding=\"0\" cellspacing=\"0\" style=\"border:1px solid #000\"><tr>\
         <td align=\"left\" style=\"padding:{pad}px\">\
         <div style=\"font-size:{f26}px;line-height:{l40}px;font-weight:700\">书架还是空的</div>\
         <div style=\"font-size:{f20}px;line-height:{l32}px;color:#555;margin-top:{mt}px\">在电脑浏览器打开主站，把想看的书加进书架。</div>\
         <div style=\"margin-top:{mt2}px\">{btn}</div></td></tr></table></td></tr></table>",
        pw = vp.page_w(),
        h = vp.nav_h(),
        card_w = card_w,
        pad = vp.px(32),
        f26 = vp.px(26),
        l40 = vp.px(40),
        f20 = vp.px(20),
        l32 = vp.px(32),
        mt = vp.len(12),
        mt2 = vp.len(24),
        btn = outline_button(&qlink("/k/search", fs, vp), "去搜索一本书", vp)
    )
}

/// 带顶栏页面的标题行（页内标题，用于错误/分卷等没有顶栏动作的页面）
pub(super) fn page_head(title: &str, vp: Vp) -> String {
    let h = vp.nav_bar_h();
    format!(
        "<div style=\"height:{h}px;border-bottom:1px solid #ccc;padding:0 {pad}px;font-size:{f28}px;line-height:{cell}px;font-weight:700\">{title}</div>",
        h = h,
        pad = vp.pad(),
        f28 = vp.px(28),
        cell = h.saturating_sub(1),
        title = esc(title)
    )
}

/// 提示页（错误 / 分卷）：页内标题 + 说明 + 描边按钮，单屏内
pub(super) fn notice_page(title: &str, msg: &str, href: &str, label: &str, vp: Vp) -> String {
    format!(
        "{head}<div style=\"padding:{pad}px\"><p style=\"font-size:{f24}px;line-height:{l36}px;color:#333\">{msg}</p>\
         <p style=\"margin-top:{mt}px\">{btn}</p></div>",
        head = page_head(title, vp),
        pad = vp.pad(),
        f24 = vp.px(24),
        l36 = vp.px(36),
        mt = vp.len(24),
        msg = esc(msg),
        btn = outline_button(href, label, vp)
    )
}

pub(super) fn err_page(msg: &str, vp: Vp) -> Response {
    page(
        "错误",
        2,
        &notice_page("出错了", msg, "/k", "返回书架", vp),
        vp,
    )
}

pub(super) fn err_to_page(e: AppError, vp: Vp) -> Response {
    err_page(&format!("{e}"), vp)
}

/// 分卷行：`is_volume` 标记，或抓取层合成的 `volume:<book_url>#<index>` URL。
pub(super) fn volume_entry(c: &BookChapter) -> bool {
    c.is_volume || c.url.starts_with("volume:")
}

/// /k 目录可见章节（过滤分卷标题），保序且保留原始 `index`。
pub(super) fn visible_chapters(chapters: &[BookChapter]) -> Vec<&BookChapter> {
    chapters.iter().filter(|c| !volume_entry(c)).collect()
}

/// 直接访问分卷行时返回的友好提示页（沿用现有版心，不新增版式）。
pub(super) fn volume_notice(q: &ReadQ, fs: usize, explicit: bool, vp: Vp) -> Response {
    let back = qlink(
        &format!("/k/toc?book={}&source={}", qenc(&q.book), qenc(&q.source)),
        fs,
        vp,
    );
    let body = notice_page(
        "分卷标题",
        "该条为分卷标题，请从目录选择章节",
        &back,
        "返回目录",
        vp,
    );
    let mut res = page("分卷标题", fs, &body, vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 正文分页 ----------------

/// 单字宽度（em）：ASCII/数字/半角标点 0.5，其余（汉字、全角标点）1.0
pub(super) fn char_em(c: char) -> f64 {
    if c.is_ascii() {
        0.5
    } else {
        1.0
    }
}

/// 每行可用 em 数（正文可用宽 ÷ 字号）；宽度取 `Vp::content_w()`，与 `#k-stage` 的实际文本宽一致
pub(super) fn line_em(spec: &FsSpec, text_w: usize) -> f64 {
    text_w as f64 / spec.size.max(1) as f64
}

/// 段落占用行数：首行缩进 2em，续行不缩进（与渲染的 text-indent:2em 一致）
pub(super) fn para_lines(s: &str, em: f64) -> usize {
    let mut lines = 1usize;
    let mut used = 2.0f64;
    for c in s.chars() {
        let w = char_em(c);
        if used + w > em {
            lines += 1;
            used = w;
        } else {
            used += w;
        }
    }
    lines
}

/// 从段落头部取不超过 cap 行的一段，返回 (该段字节长度, 实际行数)
pub(super) fn split_para(s: &str, cap: usize, em: f64) -> (usize, usize) {
    let mut lines = 1usize;
    let mut used = 2.0f64;
    for (i, c) in s.char_indices() {
        let w = char_em(c);
        if used + w > em {
            if lines == cap {
                return (i, lines);
            }
            lines += 1;
            used = w;
        } else {
            used += w;
        }
    }
    (s.len(), lines)
}

/// 按行数预算分页：优先在段落边界断页，单段超预算时按行边界切分。
/// 行预算按「行当量」计：段与段之间另计 0.6 行段距（与渲染的 `margin-top` 一致），
/// 保证页面实际高度 ≤ `lines × line`，不把最后一行挤出可视区。
/// 返回每页的字节区间（相对 `text`），页边界只落在字符边界上。
pub(super) fn paginate_lines(
    text: &str,
    spec: &FsSpec,
    budget: usize,
    text_w: usize,
) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }
    let em = line_em(spec, text_w);
    let budget = budget.max(1) as f64;
    let gap = para_gap_px(spec.line) as f64 / spec.line as f64;
    let mut pages: Vec<(usize, usize)> = Vec::new();
    let mut page_start: Option<usize> = None;
    let mut used = 0.0f64;
    let mut has_para = false;
    let mut page_end = 0usize;
    let mut offset = 0usize;
    for raw in text.split('\n') {
        let p_start = offset;
        let p_end = offset + raw.len();
        offset = p_end + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let trim_start = p_start + (raw.len() - raw.trim_start().len());
        let trim_end = trim_start + trimmed.len();
        let mut rem_start = trim_start;
        while rem_start < trim_end {
            let mut g = if has_para { gap } else { 0.0 };
            if budget - used - g < 1.0 {
                if let Some(ps) = page_start.take() {
                    pages.push((ps, page_end));
                }
                used = 0.0;
                has_para = false;
                g = 0.0;
            }
            let cap = (budget - used - g).floor() as usize;
            let remaining = &text[rem_start..trim_end];
            let need = para_lines(remaining, em);
            if need <= cap {
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                used += g + need as f64;
                has_para = true;
                page_end = trim_end;
                rem_start = trim_end;
            } else {
                let (head, _) = split_para(remaining, cap, em);
                debug_assert!(head > 0);
                if head == 0 {
                    break;
                }
                if page_start.is_none() {
                    page_start = Some(rem_start);
                }
                pages.push((page_start.take().unwrap(), rem_start + head));
                used = 0.0;
                has_para = false;
                page_end = rem_start + head;
                rem_start += head;
            }
        }
    }
    if let Some(ps) = page_start.take() {
        pages.push((ps, page_end));
    }
    if pages.is_empty() {
        pages.push((0, 0));
    }
    pages
}

/// `/k` 的一页：文字页或整页插图。
pub(super) enum KPage {
    Text(String),
    Image { src: String, alt: String },
}

/// 正文块 → `/k` 页表。文字按行预算分页，插图单独成页（一张一页），
/// 避免在老 Kindle 内核上做复杂的图文混排高度测量。
pub(super) fn plan_doc_pages(
    blocks: &[boranlib_ebook::html::Block],
    spec: &FsSpec,
    budget: usize,
    text_w: usize,
    max_chars: usize,
) -> (Vec<KPage>, bool) {
    use boranlib_ebook::html::Block;

    fn flush(
        text: &mut String,
        pages: &mut Vec<KPage>,
        spec: &FsSpec,
        budget: usize,
        text_w: usize,
    ) {
        if text.trim().is_empty() {
            text.clear();
            return;
        }
        for (s, e) in paginate_lines(text, spec, budget, text_w) {
            pages.push(KPage::Text(text[s..e].to_string()));
        }
        text.clear();
    }

    let mut pages = Vec::new();
    let mut text = String::new();
    let mut remaining = max_chars;
    let mut truncated = false;
    for block in blocks {
        match block {
            Block::Heading(_, t) | Block::Paragraph(t) => {
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                if remaining == 0 {
                    truncated = true;
                    continue;
                }
                let count = t.chars().count();
                if count > remaining {
                    let cut = t
                        .char_indices()
                        .nth(remaining)
                        .map(|(i, _)| i)
                        .unwrap_or(t.len());
                    text.push_str(&t[..cut]);
                    text.push('\n');
                    truncated = true;
                    remaining = 0;
                } else {
                    text.push_str(t);
                    text.push('\n');
                    remaining -= count;
                }
            }
            Block::Image { src, alt } => {
                flush(&mut text, &mut pages, spec, budget, text_w);
                pages.push(KPage::Image {
                    src: src.clone(),
                    alt: alt.clone(),
                });
            }
        }
    }
    flush(&mut text, &mut pages, spec, budget, text_w);
    if pages.is_empty() {
        pages.push(KPage::Text(String::new()));
    }
    (pages, truncated)
}
