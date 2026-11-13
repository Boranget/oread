//! /k 的搜索页：任务入口、进度页与结果页。
//!
//! 搜索走后台任务：`/k/search?key=...` 只负责创建任务并 303 到
//! `/k/search?job=...`，之后进度页/结果页都从任务快照读取。这样翻页不再
//! 重新搜索，离开页面任务也会继续跑，进程重启后仍能读数据库快照。

use super::*;

use crate::routes::books::{
    filtered_job_books, load_job_lookup, start_kindle_search_job, JobLookup, JobMeta, JobProgress,
    JobView,
};

/// 搜索中页的自动刷新间隔（秒）。
const SEARCH_REFRESH_SECS: u64 = 2;

fn include_cooled_of(raw: Option<&str>) -> bool {
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
        nav_link(&qlink("/k", fs, vp), "书架"),
        nav_link(&with_vp(&url, vp), FS_SPECS[next_fs - 1].label),
    )
}

fn search_form(key: &str, fs: usize) -> String {
    format!(
        "<div style=\"padding:20px 36px 12px\"><form method=\"get\" action=\"/k/search\">\
         <input type=\"text\" name=\"key\" value=\"{key}\" style=\"font-size:24px;height:56px;width:600px;border:1px solid #000;padding:0 12px;font-family:serif\">\
         <input type=\"submit\" value=\"搜索\" style=\"font-size:24px;height:56px;width:160px;border:1px solid #000;background:#fff;color:#000;font-family:serif;margin-left:12px\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\"></form></div>",
        key = esc(key),
        fs = fs
    )
}

fn note_line(text: &str) -> String {
    format!(
        "<p style=\"font-size:20px;line-height:32px;color:#555;padding:8px 36px 0\">{}</p>",
        esc(text)
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

fn stage(top: &str, inner: &str) -> String {
    format!(
        "{top}<div id=\"k-stage\" style=\"position:absolute;top:{nav}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{inner}</div>",
        nav = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
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
    let include_cooled = include_cooled_of(q.include_cooled.as_deref());
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
        Err(e) => err_to_page(e),
    }
}

fn search_home(fs: usize, explicit: bool, vp: Vp) -> Response {
    let top = top_bar("搜索", None, &search_nav("", None, false, fs, vp), vp);
    let body = search_form("", fs);
    let mut res = page("搜索", fs, &stage(&top, &body), vp);
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
    let include_cooled = include_cooled_of(q.include_cooled.as_deref());
    match load_job_lookup(shared, uid, job_id).await {
        Ok(Some(JobLookup::Running(p))) => {
            progress_page(job_id, &p, include_cooled, fs, explicit, vp)
        }
        Ok(Some(JobLookup::Finished(done))) => result_page(
            job_id,
            &done.meta,
            &done.view,
            include_cooled,
            q,
            fs,
            explicit,
            vp,
        ),
        Ok(None) => expired_page(fs, explicit, vp),
        Err(e) => err_to_page(e),
    }
}

fn expired_page(fs: usize, explicit: bool, vp: Vp) -> Response {
    let body = notice_page(
        "任务已过期",
        "搜索任务不存在或已过期，请重新搜索。",
        &qlink("/k/search", fs, vp),
        "重新搜索",
    );
    let mut res = page("搜索", fs, &body, vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

pub(super) fn progress_page(
    job_id: &str,
    p: &JobProgress,
    include_cooled: bool,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let top = top_bar(
        "搜索",
        None,
        &search_nav(&p.key, Some(job_id), include_cooled, fs, vp),
        vp,
    );
    let body = if p.source_total == 0 {
        format!(
            "<div style=\"padding:20px 36px 12px\">\
             <p style=\"font-size:28px;line-height:44px\">正在搜索「{key}」…</p>\
             <p style=\"font-size:24px;line-height:36px;color:#555;margin-top:12px\">当前没有可搜索的源，请先在书源设置中导入并启用。</p>\
             <p style=\"margin-top:24px\">{back}</p></div>",
            key = esc(&p.key),
            back = outline_button(&qlink("/k/search", fs, vp), "重新搜索")
        )
    } else {
        let done = p.finished.min(p.source_total);
        let mut detail = format!("已完成 {done} / {total} 个源", total = p.source_total);
        if p.pending > 0 {
            detail.push_str(&format!("，进行中 {} 个", p.pending));
        }
        if p.failed > 0 {
            detail.push_str(&format!("，失败 {} 个", p.failed));
        }
        if p.skipped > 0 {
            detail.push_str(&format!("，跳过 {} 个", p.skipped));
        }
        if p.cancelled > 0 {
            detail.push_str(&format!("，已取消 {} 个", p.cancelled));
        }
        format!(
            "<div style=\"padding:20px 36px 12px\">\
             <p style=\"font-size:28px;line-height:44px\">正在搜索「{key}」…</p>\
             <p style=\"font-size:20px;line-height:32px;color:#555;margin-top:12px\">{detail}</p>\
             <p style=\"font-size:20px;line-height:32px;color:#555;margin-top:12px\">页面每 {secs} 秒自动刷新，完成后显示结果。</p>\
             <p style=\"margin-top:24px\">{back}</p></div>",
            key = esc(&p.key),
            detail = esc(&detail),
            secs = SEARCH_REFRESH_SECS,
            back = outline_button(&qlink("/k/search", fs, vp), "重新搜索")
        )
    };
    let mut res = page_refresh("搜索", fs, &stage(&top, &body), vp, SEARCH_REFRESH_SECS);
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
fn result_page(
    job_id: &str,
    meta: &JobMeta,
    view: &JobView,
    include_cooled: bool,
    q: &SearchQ,
    fs: usize,
    explicit: bool,
    vp: Vp,
) -> Response {
    let key = meta.key.as_str();
    let top = top_bar(
        "搜索",
        None,
        &search_nav(key, Some(job_id), include_cooled, fs, vp),
        vp,
    );
    let books = filtered_job_books(meta, view);
    let strong_total = books
        .iter()
        .filter(|b| b.relevance.as_deref() == Some("strong"))
        .count();

    let mut notes: Vec<String> = Vec::new();
    if meta.source_total > 0 {
        match meta.status.as_str() {
            "cancelled" => notes.push("已取消搜索，以下为已得到的结果。".to_string()),
            "interrupted" => {
                notes.push("搜索被中断（服务重启或任务已过期），以下为已保存的结果。".to_string())
            }
            _ => {}
        }
        if view.skipped > 0 {
            notes.push(format!("有 {} 个源因搜索预算耗尽未完成。", view.skipped));
        }
        if let Some(note) = fallback_note(meta) {
            notes.push(note);
        }
    }
    // 结果不足（0 条或 strong < 5）时给「用全部源搜索」的主动升级入口，不是默认路径。
    let upgrade_link = meta.source_total > 0
        && meta.scope == crate::req::SearchScope::Curated
        && (books.is_empty() || strong_total < crate::search_rank::STRONG_MIN_RESULTS);
    // 状态提示 / 回退说明 / 升级按钮各占一行：按额外行数扣减每页条数，避免被贴底栏盖住。
    let per_page = search_page_size_extra(vp, notes.len() + usize::from(upgrade_link));

    let mut list = String::new();
    for note in &notes {
        list.push_str(&note_line(note));
    }
    let mut pager = String::new();
    if meta.source_total == 0 {
        list.push_str(
            "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">\
             当前没有可搜索的源，请先在书源设置中导入并启用。</p>",
        );
    } else if books.is_empty() {
        list.push_str(
            "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">没有找到结果</p>",
        );
    } else {
        let t_pages = books.len().div_ceil(per_page).max(1);
        let sp = q.sp.unwrap_or(1).clamp(1, t_pages as i64) as usize;
        let start = (sp - 1) * per_page;
        let end = (start + per_page).min(books.len());
        list.push_str(&format!(
            "<table width=\"{iw}\" cellpadding=\"0\" cellspacing=\"0\">",
            iw = vp.page_w()
        ));
        for b in &books[start..end] {
            let src = b.origin.clone().unwrap_or_default();
            let href = qlink(
                &format!("/k/toc?book={}&source={}", qenc(&b.book_url), qenc(&src)),
                fs,
                vp,
            );
            let name = b.name.as_deref().unwrap_or("未命名");
            let first = name.chars().next().unwrap_or('书');
            // 沿用现有副标题位置（字号/颜色不变）：多源共识显示「N 个源」，
            // partial 档追加「可能相关」，不与正名结果混在一起。
            let mut sub = format!(
                "{} · {}",
                b.author.as_deref().unwrap_or("未知作者"),
                b.origin_name.as_deref().unwrap_or("未知来源")
            );
            if b.origins.len() > 1 {
                sub.push_str(&format!(" · {} 个源", b.origins.len()));
            }
            if b.relevance.as_deref() == Some("partial") {
                sub.push_str(" · 可能相关");
            }
            list.push_str(&format!(
                "<tr><td style=\"padding:0\">\
                 <a href=\"{href}\" style=\"display:block;height:112px;border-bottom:1px solid #ccc\">\
                 <table width=\"{rw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                 <td width=\"56\" valign=\"middle\"><div style=\"width:56px;height:80px;border:1px solid #000;font-size:24px;line-height:78px;text-align:center\">{first}</div></td>\
                 <td style=\"padding-left:24px\" valign=\"middle\">\
                 <div style=\"font-size:32px;line-height:48px\">{name}</div>\
                 <div style=\"font-size:20px;line-height:32px;color:#555;margin-top:8px\">{sub}</div></td>\
                 </tr></table></a></td></tr>",
                href = href,
                rw = vp.page_w(),
                first = esc(&first.to_string()),
                name = esc(name),
                sub = esc(&sub)
            ));
        }
        list.push_str("</table>");
        if t_pages > 1 {
            // 翻页只带 job：结果来自任务快照，不再重新解析源集合/重新搜索。
            let base = format!(
                "/k/search?job={}&fs={}{}",
                qenc(job_id),
                fs,
                cooled_suffix(include_cooled)
            );
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
            "<div style=\"padding:12px 36px\">\
             <a href=\"{href}\" style=\"display:inline-block;height:56px;line-height:56px;\
             padding:0 24px;border:1px solid #000;font-size:24px;color:#000\">用全部源搜索</a></div>",
            href = esc(&with_vp(&href, vp))
        ));
    }
    let body = format!("{}{}{}", search_form(key, fs), list, pager);
    let mut res = page("搜索", fs, &stage(&top, &body), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
