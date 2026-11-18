//! /k 的搜索页：任务入口、进度页与结果页。
//!
//! 搜索走后台任务：`/k/search?key=...` 只负责创建任务并 303 到
//! `/k/search?job=...`，之后进度页/结果页都从任务快照读取。这样翻页不再
//! 重新搜索，离开页面任务也会继续跑，进程重启后仍能读数据库快照。

use super::*;

use crate::entities::SearchBook;
use crate::routes::books::{
    advance_freeze, count, filtered_books, freeze, load_job_lookup, load_page_at,
    start_kindle_search_job, JobLookup, JobMeta,
};

/// 搜索中页的自动刷新间隔（秒）。
const SEARCH_REFRESH_SECS: u64 = 2;

/// 宽松布尔：`1`/`true`/`yes`/`on` 视为真（`include_cooled` / `more` 共用）。
fn flag_of(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim),
        Some("1" | "true" | "yes" | "on" | "TRUE" | "True")
    )
}

/// `include_cooled` 的链接后缀：任务页的翻页/字号/升级链接都要保留，
/// 否则 URL 丢失后「用全部源搜索」会退回排除冷却源的集合。
fn cooled_suffix(include_cooled: bool) -> &'static str {
    if include_cooled {
        "&include_cooled=1"
    } else {
        ""
    }
}

/// 搜索页导航：书架 + 字号切换。字号链接保留当前任务/关键词，切换字号不丢结果。
fn search_nav(key: &str, job: Option<&str>, include_cooled: bool, fs: usize, vp: Vp) -> String {
    let next_fs = cycle_fs(fs);
    let mut url = format!("/k/search?fs={next_fs}");
    if let Some(job) = job {
        url.push_str(&format!("&job={}", qenc(job)));
    }
    if !key.is_empty() {
        url.push_str(&format!("&key={}", qenc(key)));
    }
    url.push_str(cooled_suffix(include_cooled));
    format!(
        "{}{}",
        nav_link(&qlink("/k", fs, vp), "书架", vp),
        nav_link(&with_vp(&url, vp), FS_SPECS[next_fs - 1].label, vp),
    )
}

fn search_form(key: &str, fs: usize, vp: Vp) -> String {
    let field = format!(
        "font-size:{f}px;height:{h}px;border:1px solid #000;padding:0 {pad}px;font-family:serif",
        f = vp.px(24),
        h = vp.tap(56),
        pad = vp.px(12)
    );
    let btn = format!(
        "font-size:{f}px;height:{h}px;border:1px solid #000;background:#fff;color:#000;font-family:serif",
        f = vp.px(24),
        h = vp.tap(56)
    );
    // 窄屏：输入框与按钮各占一行、都吃满宽度；宽屏：一行排开（600 + 160）
    let controls = if vp.narrow() {
        format!(
            "<input type=\"text\" name=\"key\" value=\"{key}\" style=\"{field};width:100%\">\
             <input type=\"submit\" value=\"搜索\" style=\"{btn};width:100%;margin-top:{mt}px\">",
            key = esc(key),
            mt = vp.len(12)
        )
    } else {
        format!(
            "<input type=\"text\" name=\"key\" value=\"{key}\" style=\"{field};width:{fw}px\">\
             <input type=\"submit\" value=\"搜索\" style=\"{btn};width:{bw}px;margin-left:{ml}px\">",
            key = esc(key),
            fw = vp.len(600),
            bw = vp.len(160),
            ml = vp.len(12)
        )
    };
    format!(
        "<div style=\"padding:{pt}px {px}px {pb}px\"><form method=\"get\" action=\"/k/search\">{controls}\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\"></form></div>",
        pt = vp.len(20),
        px = vp.pad(),
        pb = vp.len(12)
    )
}

fn note_line(text: &str, vp: Vp) -> String {
    format!(
        "<p style=\"font-size:{f}px;line-height:{l}px;color:#555;padding:{pt}px {px}px 0\">{}</p>",
        esc(text),
        f = vp.px(20),
        l = vp.px(32),
        pt = vp.len(8),
        px = vp.pad()
    )
}

fn see_other(location: &str, fs: usize, explicit: bool) -> Response {
    let mut res = axum::http::Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(axum::http::header::LOCATION, location)
        .body(Body::empty())
        .expect("redirect response");
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

fn stage(top: &str, inner: &str, vp: Vp) -> String {
    format!(
        "{top}<div id=\"k-stage\" style=\"position:absolute;top:{nav}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{inner}</div>",
        nav = vp.nav_bar_h(),
        pad = vp.pad(),
        safe = vp.len(SAFE_BOTTOM),
        top = top,
        inner = inner
    )
}

// ---------------- 搜索入口 ----------------

pub(super) async fn search(
    State(s): State<AppState>,
    u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<SearchQ>,
) -> Response {
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let vp = vp_of(&s, &headers, q.vw, q.vh);
    // 已有任务：渲染进度/结果，绝不重新发起搜索。
    if let Some(job_id) = q.job.as_deref().map(str::trim).filter(|j| !j.is_empty()) {
        return job_page(&s.shared, u.id, job_id, &q, fs, explicit, vp).await;
    }
    let key = q.key.unwrap_or_default().trim().to_string();
    if key.is_empty() {
        return search_home(fs, explicit, vp);
    }
    let include_cooled = flag_of(q.include_cooled.as_deref());
    match start_kindle_search_job(
        &s,
        u.id,
        &db.0,
        key,
        k_scope(q.scope.as_deref()),
        include_cooled,
    )
    .await
    {
        Ok(job_id) => {
            let location = format!(
                "/k/search?job={}&fs={}{}",
                qenc(&job_id),
                fs,
                cooled_suffix(include_cooled)
            );
            see_other(&with_vp(&location, vp), fs, explicit)
        }
        Err(e) => err_to_page(e, vp),
    }
}

fn search_home(fs: usize, explicit: bool, vp: Vp) -> Response {
    let top = top_bar("搜索", None, &search_nav("", None, false, fs, vp), vp);
    let body = search_form("", fs, vp);
    let mut res = page("搜索", fs, &stage(&top, &body, vp), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 任务页 ----------------

async fn job_page(
    shared: &SqlitePool,
    uid: i64,
    job_id: &str,
    q: &SearchQ,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let include_cooled = flag_of(q.include_cooled.as_deref());
    match load_job_lookup(shared, uid, job_id).await {
        Ok(Some(lookup)) => {
            // 首屏：已经有结果就先出结果页，后台继续搜索（运行中带「显示更多结果」）。
            if lookup.counters.result_count > 0 || !lookup.running {
                result_page(shared, job_id, &lookup, include_cooled, q, fs, explicit, vp).await
            } else {
                progress_page(job_id, &lookup, include_cooled, fs, explicit, vp)
            }
        }
        Ok(None) => expired_page(fs, explicit, vp),
        Err(e) => err_to_page(e, vp),
    }
}

fn expired_page(fs: usize, explicit: bool, vp: Vp) -> Response {
    let body = notice_page(
        "任务已过期",
        "搜索任务不存在或已过期，请重新搜索。",
        &qlink("/k/search", fs, vp),
        "重新搜索",
        vp,
    );
    let mut res = page("搜索", fs, &body, vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

/// 页内块：上下/左右留白按统一比例缩放（参考 20/36/12）
fn block(vp: Vp, inner: &str) -> String {
    format!(
        "<div style=\"padding:{pt}px {pad}px {pb}px\">{inner}</div>",
        pt = vp.len(20),
        pad = vp.pad(),
        pb = vp.len(12)
    )
}

/// 行内描边按钮（「显示更多结果」「用全部源搜索」）：高度按触控下限缩放
fn inline_button(href: &str, label: &str, vp: Vp) -> String {
    let h = vp.tap(56);
    format!(
        "<a href=\"{href}\" style=\"display:inline-block;height:{h}px;line-height:{h}px;\
         padding:0 {pad}px;border:1px solid #000;font-size:{fs}px;color:#000\">{label}</a>",
        href = esc(href),
        h = h,
        pad = vp.len(24),
        fs = vp.px(24),
        label = esc(label)
    )
}

pub(super) fn progress_page(
    job_id: &str,
    lookup: &JobLookup,
    include_cooled: bool,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let meta = &lookup.meta;
    let c = &lookup.counters;
    let top = top_bar(
        "搜索",
        None,
        &search_nav(&meta.key, Some(job_id), include_cooled, fs, vp),
        vp,
    );
    let body = if meta.source_total == 0 {
        block(
            vp,
            &format!(
                "<p style=\"font-size:{f28}px;line-height:{l44}px\">正在搜索「{key}」…</p>\
                 <p style=\"font-size:{f24}px;line-height:{l36}px;color:#555;margin-top:{mt}px\">当前没有可搜索的源，请先在书源设置中导入并启用。</p>\
                 <p style=\"margin-top:{mt2}px\">{back}</p>",
                key = esc(&meta.key),
                back = outline_button(&qlink("/k/search", fs, vp), "重新搜索", vp),
                f28 = vp.px(28),
                l44 = vp.px(44),
                f24 = vp.px(24),
                l36 = vp.px(36),
                mt = vp.len(12),
                mt2 = vp.len(24)
            ),
        )
    } else {
        let done = c.finished.min(meta.source_total);
        let mut detail = format!("已完成 {done} / {total} 个源", total = meta.source_total);
        if c.pending > 0 {
            detail.push_str(&format!("，进行中 {} 个", c.pending));
        }
        if c.failed > 0 {
            detail.push_str(&format!("，失败 {} 个", c.failed));
        }
        if c.skipped > 0 {
            detail.push_str(&format!("，跳过 {} 个", c.skipped));
        }
        if c.cancelled > 0 {
            detail.push_str(&format!("，已取消 {} 个", c.cancelled));
        }
        block(
            vp,
            &format!(
                "<p style=\"font-size:{f28}px;line-height:{l44}px\">正在搜索「{key}」…</p>\
                 <p style=\"font-size:{f20}px;line-height:{l32}px;color:#555;margin-top:{mt}px\">{detail}</p>\
                 <p style=\"font-size:{f20}px;line-height:{l32}px;color:#555;margin-top:{mt}px\">页面每 {secs} 秒自动刷新，有结果后立即显示。</p>\
                 <p style=\"margin-top:{mt2}px\">{back}</p>",
                key = esc(&meta.key),
                detail = esc(&detail),
                secs = SEARCH_REFRESH_SECS,
                back = outline_button(&qlink("/k/search", fs, vp), "重新搜索", vp),
                f28 = vp.px(28),
                l44 = vp.px(44),
                f20 = vp.px(20),
                l32 = vp.px(32),
                mt = vp.len(12),
                mt2 = vp.len(24)
            ),
        )
    };
    let mut res = page_refresh("搜索", fs, &stage(&top, &body, vp), vp, SEARCH_REFRESH_SECS);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

/// 三层退化链的页面说明（沿用现有次级文字样式，不加新视觉 token）。
fn fallback_note(meta: &JobMeta) -> Option<String> {
    if meta.source_total == 0 {
        return None;
    }
    match meta.source_mode.as_str() {
        "curated_fallback" => Some(if meta.curated_total == 0 {
            format!(
                "尚未生成精选集，已改用常用源（{n} 个源）。可在书源设置里跑一次体检生成精选。",
                n = meta.source_total
            )
        } else {
            format!(
                "精选集暂无可用条目，已改用常用源（{n} 个源）。",
                n = meta.source_total
            )
        }),
        "all_fallback" => Some(if meta.curated_total == 0 {
            format!(
                "尚未生成精选集，已改用全部 {n} 个源。可在书源设置里跑一次体检生成精选。",
                n = meta.source_total
            )
        } else {
            format!(
                "精选集与常用源暂无可用条目，已改用全部 {n} 个源。",
                n = meta.source_total
            )
        }),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn result_page(
    shared: &SqlitePool,
    job_id: &str,
    lookup: &JobLookup,
    include_cooled: bool,
    q: &SearchQ,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let meta = &lookup.meta;
    let c = &lookup.counters;
    let key = meta.key.as_str();
    let top = top_bar(
        "搜索",
        None,
        &search_nav(key, Some(job_id), include_cooled, fs, vp),
        vp,
    );

    // 运行中只展示「首屏边界」内的结果：首次渲染冻结，`more=1` 主动推进；
    // 任务结束后改用最终全集（不再有边界）。
    let more = flag_of(q.more.as_deref());
    let frozen = if lookup.running && lookup.legacy_books.is_none() {
        let r = if more {
            advance_freeze(shared, job_id).await
        } else {
            freeze(shared, job_id).await
        };
        match r {
            Ok(v) => Some(v),
            Err(e) => return err_to_page(e.into(), vp),
        }
    } else {
        None
    };

    // 旧任务：快照里带完整书单；新任务：按边界从行表读取。
    let legacy_all = lookup
        .legacy_books
        .as_ref()
        .map(|books| filtered_books(meta, books.clone()));
    let total = match &legacy_all {
        Some(books) => books.len(),
        None => match count(shared, job_id, frozen).await {
            Ok(n) => n.max(0) as usize,
            Err(e) => return err_to_page(e.into(), vp),
        },
    };

    let mut notes: Vec<String> = Vec::new();
    if meta.source_total > 0 {
        if lookup.running {
            notes.push(format!(
                "仍在搜索（已完成 {} / {} 个源），以下是已找到的结果，后台会继续补充。",
                c.finished.min(meta.source_total),
                meta.source_total
            ));
        }
        match meta.status.as_str() {
            "cancelled" => notes.push("已取消搜索，以下为已得到的结果。".to_string()),
            "interrupted" => {
                notes.push("搜索被中断（服务重启或任务已过期），以下为已保存的结果。".to_string())
            }
            _ => {}
        }
        if c.skipped > 0 {
            if c.stop_reason == "max_bytes" {
                notes.push(format!("结果体积已达上限，有 {} 个源未执行。", c.skipped));
            } else {
                notes.push(format!("有 {} 个源未执行。", c.skipped));
            }
        }
        if let Some(note) = fallback_note(meta) {
            notes.push(note);
        }
    }
    // 结果不足（0 条或 < STRONG_MIN）时给「用全部源搜索」的主动升级入口，不是默认路径。
    let upgrade_link = meta.source_total > 0
        && meta.scope == crate::req::SearchScope::Curated
        && total < crate::search_rank::STRONG_MIN_RESULTS;
    // 状态提示 / 回退说明 / 升级按钮各占一行：按额外行数扣减每页条数，避免被贴底栏盖住。
    let per_page = search_page_size_extra(vp, notes.len() + usize::from(upgrade_link));

    let mut list = String::new();
    for note in &notes {
        list.push_str(&note_line(note, vp));
    }
    let mut pager = String::new();
    let empty_style = format!(
        "font-size:{}px;line-height:{}px;padding:0 {}px;color:#555",
        vp.px(24),
        vp.px(36),
        vp.pad()
    );
    if meta.source_total == 0 {
        list.push_str(&format!(
            "<p style=\"{empty_style}\">当前没有可搜索的源，请先在书源设置中导入并启用。</p>"
        ));
    } else if total == 0 {
        list.push_str(&format!("<p style=\"{empty_style}\">没有找到结果</p>"));
    } else {
        let t_pages = total.div_ceil(per_page).max(1);
        let sp = q.sp.unwrap_or(1).clamp(1, t_pages as i64) as usize;
        let start = (sp - 1) * per_page;
        let page_books: Vec<SearchBook> = match &legacy_all {
            Some(books) => books.iter().skip(start).take(per_page).cloned().collect(),
            None => match load_page_at(shared, job_id, frozen, start, per_page).await {
                Ok(v) => v,
                Err(e) => return err_to_page(e.into(), vp),
            },
        };
        list.push_str(&format!(
            "<table width=\"{iw}\" cellpadding=\"0\" cellspacing=\"0\">",
            iw = vp.page_w()
        ));
        for b in &page_books {
            let src = b.origin.clone().unwrap_or_default();
            let href = qlink(
                &format!("/k/toc?book={}&source={}", qenc(&b.book_url), qenc(&src)),
                fs,
                vp,
            );
            let name = b.name.as_deref().unwrap_or("未命名");
            let first = name.chars().next().unwrap_or('书');
            // 沿用现有副标题位置（字号/颜色不变）：多源共识显示「N 个源」。
            let mut sub = format!(
                "{} · {}",
                b.author.as_deref().unwrap_or("未知作者"),
                b.origin_name.as_deref().unwrap_or("未知来源")
            );
            if b.origins.len() > 1 {
                sub.push_str(&format!(" · {} 个源", b.origins.len()));
            }
            let cover_w = vp.len(56);
            let cover_h = vp.len(80);
            list.push_str(&format!(
                "<tr><td style=\"padding:0\">\
                 <a href=\"{href}\" style=\"display:block;height:{rh}px;border-bottom:1px solid #ccc\">\
                 <table width=\"{rw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                 <td width=\"{cw}\" valign=\"middle\"><div style=\"width:{cw}px;height:{ch}px;border:1px solid #000;font-size:{f24}px;line-height:{clh}px;text-align:center\">{first}</div></td>\
                 <td style=\"padding-left:{pl}px\" valign=\"middle\">\
                 <div style=\"font-size:{f32}px;line-height:{l48}px;white-space:nowrap;overflow:hidden\">{name}</div>\
                 <div style=\"font-size:{f20}px;line-height:{l32}px;color:#555;margin-top:{mt}px;white-space:nowrap;overflow:hidden\">{sub}</div></td>\
                 </tr></table></a></td></tr>",
                href = href,
                rh = vp.tap(112),
                rw = vp.page_w(),
                cw = cover_w,
                ch = cover_h,
                clh = cover_h.saturating_sub(2),
                f24 = vp.px(24),
                pl = vp.len(24),
                f32 = vp.px(32),
                l48 = vp.px(48),
                f20 = vp.px(20),
                l32 = vp.px(32),
                mt = vp.len(8),
                first = esc(&first.to_string()),
                name = esc(name),
                sub = esc(&sub)
            ));
        }
        list.push_str("</table>");

        // 翻页只带 job：结果来自任务（首屏边界或最终全集），不重新搜索。
        let base = format!(
            "/k/search?job={}&fs={}{}",
            qenc(job_id),
            fs,
            cooled_suffix(include_cooled)
        );
        if t_pages > 1 {
            let prev = if sp > 1 {
                Some((with_vp(&format!("{base}&sp={}", sp - 1), vp), "上一页"))
            } else {
                None
            };
            let next = if sp < t_pages {
                Some((with_vp(&format!("{base}&sp={}", sp + 1), vp), "下一页"))
            } else {
                None
            };
            pager = pager_bar(prev, next, vp);
        }
        // 运行中：把后台新入库的结果纳入当前集合，然后重新渲染本页。
        if lookup.running {
            let href = with_vp(&format!("{base}&sp={sp}&more=1"), vp);
            list.push_str(&format!(
                "<div style=\"padding:{pt}px {pad}px;text-align:center\">{btn}</div>",
                pt = vp.len(12),
                pad = vp.pad(),
                btn = inline_button(&href, "显示更多结果", vp)
            ));
        }
    }
    if upgrade_link {
        let href = with_fs(
            &format!(
                "/k/search?key={}&scope=all{}",
                qenc(key),
                if include_cooled {
                    "&include_cooled=1"
                } else {
                    ""
                }
            ),
            fs,
        );
        list.push_str(&format!(
            "<div style=\"padding:{pt}px {pad}px\">{btn}</div>",
            pt = vp.len(12),
            pad = vp.pad(),
            btn = inline_button(&with_vp(&href, vp), "用全部源搜索", vp)
        ));
    }
    let body = format!("{}{}{}", search_form(key, fs, vp), list, pager);
    let mut res = page("搜索", fs, &stage(&top, &body, vp), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
