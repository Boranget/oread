//! /k 的 search 部分（move-only 从 kindle.rs 拆出）。

use super::*;

// ---------------- 搜索 ----------------

pub(super) async fn search(
    State(s): State<AppState>,
    _u: AuthUser,
    _db: UserDb,
    headers: HeaderMap,
    Query(q): Query<SearchQ>,
) -> Response {
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let vp = vp_of(&s, &headers, q.vw, q.vh);
    let key = q.key.unwrap_or_default().trim().to_string();
    let next_fs = cycle_fs(fs);
    let fs_url = format!("/k/search?fs={}&key={}", next_fs, qenc(&key));
    // 导航只出现一次：标题=搜索，右侧=书架 / 字号
    let nav = format!(
        "{}{}",
        nav_link(&qlink("/k", fs, vp), "书架"),
        nav_link(&with_vp(&fs_url, vp), FS_SPECS[next_fs - 1].label),
    );
    let top = top_bar("搜索", None, &nav, vp);
    let form = format!(
        "<div style=\"padding:20px 36px 12px\"><form method=\"get\" action=\"/k/search\">\
         <input type=\"text\" name=\"key\" value=\"{key}\" style=\"font-size:24px;height:56px;width:600px;border:1px solid #000;padding:0 12px;font-family:serif\">\
         <input type=\"submit\" value=\"搜索\" style=\"font-size:24px;height:56px;width:160px;border:1px solid #000;background:#fff;color:#000;font-family:serif;margin-left:12px\">\
         <input type=\"hidden\" name=\"fs\" value=\"{fs}\"></form></div>",
        key = esc(&key),
        fs = fs
    );
    let mut list = String::new();
    let mut pager = String::new();
    if !key.is_empty() {
        let mut sources = match load_sources(&s.shared).await {
            Ok(v) => v,
            Err(e) => return err_to_page(e),
        };
        crate::routes::sources::sort_sources(&mut sources);
        // 与 legacy `/api/search` 共用 `aggregated`：同一源集合、同一预算/并发池、
        // 同一 dedupe_and_sort + relevance 档位排序，并共享 60s 结果缓存；
        // 同一实例上 /k 首屏与 `/api/search` 前 8 条顺序一致。
        let include_cooled = matches!(
            q.include_cooled.as_deref().map(str::trim),
            Some("1" | "true" | "yes" | "on")
        );
        let requested_scope = k_scope(q.scope.as_deref());
        let plan = crate::routes::books::resolve_search_sources(
            &s.shared,
            sources,
            &[],
            &key,
            requested_scope,
            include_cooled,
            // `/k` 精选集合缺失时不退全量：只跑 preferred 层，保住 KPW3 首屏耗时。
            crate::routes::books::CuratedMissingFallback::Preferred,
        )
        .await;
        // `/k/search` 与网页端共用 `aggregated` 的边界过滤：只保留书名包含关键词的结果。
        let active = crate::routes::books::order_by_health(plan.active, &plan.health, &plan.prior);
        let (books, _errors, _title_filter) = crate::routes::books::aggregated(
            &s.shared,
            s.cfg.source_cooldown_secs,
            &crate::source_quality::TierConfig::from_config(&s.cfg),
            &active,
            &key,
            include_cooled,
            plan.scope,
        )
        .await;
        let strong_total = books
            .iter()
            .filter(|b| b.relevance.as_deref() == Some("strong"))
            .count();
        // 三层退化链的页面说明（复用现有次级文字样式，不加新视觉 token）：
        // curated_fallback = 精选不可用已退常用源；all_fallback = 常用源也为空、已退全部可搜索源。
        let fallback_note = if plan.source_total == 0 {
            None
        } else {
            match plan.source_mode {
                "curated_fallback" => Some(if plan.curated_total == 0 {
                    format!(
                        "尚未生成精选集，已改用常用源（{n} 个源）。可在书源设置里跑一次体检生成精选。",
                        n = plan.source_total
                    )
                } else {
                    format!(
                        "精选集暂无可用条目，已改用常用源（{n} 个源）。",
                        n = plan.source_total
                    )
                }),
                "all_fallback" => Some(if plan.curated_total == 0 {
                    format!(
                        "尚未生成精选集，已改用全部 {n} 个源。可在书源设置里跑一次体检生成精选。",
                        n = plan.source_total
                    )
                } else {
                    format!(
                        "精选集与常用源暂无可用条目，已改用全部 {n} 个源。",
                        n = plan.source_total
                    )
                }),
                _ => None,
            }
        };
        if let Some(note) = &fallback_note {
            list.push_str(&format!(
                "<p style=\"font-size:20px;line-height:32px;color:#555;padding:8px 36px 0\">{note}</p>"
            ));
        }
        // 结果不足（0 条或 strong < 5）时给「用全部源搜索」的主动升级入口，不是默认路径。
        let upgrade_link = plan.scope == crate::req::SearchScope::Curated
            && (books.is_empty() || strong_total < crate::search_rank::STRONG_MIN_RESULTS);
        // 回退说明 / 升级按钮要占纵向空间：多一行时少放一条结果，避免被贴底栏盖住。
        let per_page = search_page_size(vp, fallback_note.is_some() || upgrade_link);
        if books.is_empty() {
            if plan.source_total == 0 {
                list.push_str(
                    "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">\
                     当前没有可搜索的源，请先在书源设置中导入并启用。</p>",
                );
            } else {
                list.push_str(
                    "<p style=\"font-size:24px;line-height:36px;padding:0 36px;color:#555\">没有找到结果</p>",
                );
            }
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
                // 翻页必须带上实际生效的 scope 与 include_cooled：否则第二页会退回缺省
                // curated / 排除冷却源，源集合与缓存键都变了，等于重新搜一遍且结果可能不同。
                let base = format!(
                    "/k/search?key={}&fs={}&scope={}{}",
                    qenc(&key),
                    fs,
                    plan.scope.as_str(),
                    if include_cooled {
                        "&include_cooled=1"
                    } else {
                        ""
                    }
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
                    qenc(&key),
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
                href = esc(&href)
            ));
        }
    }
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{form}{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        form = form,
        list = list,
        pager = pager
    );
    let mut res = page("搜索", fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
