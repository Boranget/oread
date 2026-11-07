//! /k 的 shelf 部分（move-only 从 kindle.rs 拆出）。

use super::*;

/// 目录（带缓存）：先读 book_chapters，miss 则 book_info 补 toc_url 后抓目录并落库
pub(super) async fn ensure_chapters(
    s: &AppState,
    db: &SqlitePool,
    source_url: &str,
    book_url: &str,
) -> AppResult<(BookSource, Vec<BookChapter>)> {
    let source = load_source(&s.shared, source_url).await?;
    if let Some(chaps) = load_cached_chapters(db, book_url, s.cfg.toc_ttl_secs).await? {
        return Ok((source, chaps));
    }
    let mut book = Book {
        book_url: book_url.to_string(),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        book_source_type: source.book_source_type,
        ..Default::default()
    };
    if let Some(m) = load_book_meta(db, book_url).await? {
        book.toc_url = m.toc_url;
    }
    // 补详情拿 tocUrl（字面量 URL 的情况也要先走 book_info）
    web_book::book_info(&source, &mut book).await?;
    save_book_meta(db, &book).await?;
    let chapters = web_book::chapter_list(&source, &book).await?;
    save_chapters(db, book_url, &chapters).await?;
    Ok((source, chapters))
}

// ---------------- 登录 / 注销 ----------------

pub(super) async fn login_form(
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let err = if q.get("err").map(|v| v == "1").unwrap_or(false) {
        "<p style=\"font-size:24px;line-height:36px;color:#333;margin-top:16px\">账号或密码错误，请重试</p>"
    } else {
        ""
    };
    page(
        "登录",
        2,
        &format!(
            "{head}\
             <div style=\"padding:36px\">\
             <p style=\"font-size:20px;line-height:32px;color:#555\">Kindle 极简阅读 · 请用网页端账号登录</p>\
             <p style=\"font-size:20px;line-height:32px;color:#555\">没有账号请先在电脑浏览器打开主站注册。</p>{err}\
             <form method=\"post\" action=\"/k/login\">\
             <table width=\"928\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-top:24px\">\
             <tr><td width=\"120\" height=\"76\" style=\"font-size:24px\">账号</td>\
             <td><input type=\"text\" name=\"username\" style=\"font-size:24px;height:56px;width:640px;border:1px solid #000;padding:0 12px;font-family:serif\"></td></tr>\
             <tr><td height=\"76\" style=\"font-size:24px\">密码</td>\
             <td><input type=\"password\" name=\"password\" style=\"font-size:24px;height:56px;width:640px;border:1px solid #000;padding:0 12px;font-family:serif\"></td></tr>\
             <tr><td height=\"88\"></td>\
             <td><input type=\"submit\" value=\"登录\" style=\"font-size:24px;height:56px;width:240px;border:1px solid #000;background:#fff;color:#000;font-family:serif\"></td></tr>\
             </table></form></div>",
            head = page_head("登录"),
            err = err
        ),
        Vp::default(),
    )
}

pub(super) async fn login_post(State(s): State<AppState>, Form(q): Form<LoginQ>) -> Response {
    let row: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, password_hash, role, disabled, token_version FROM users WHERE username = ?",
    )
    .bind(&q.username)
    .fetch_optional(&s.shared)
    .await
    .unwrap_or_default();
    // 禁用账号与普通账号一样登录失败（不泄漏具体原因）
    let ok = match row {
        Some((_, ref hash, _, disabled, _)) => {
            disabled == 0 && bcrypt::verify(&q.password, hash).unwrap_or(false)
        }
        None => false,
    };
    if !ok {
        let mut res = Response::new(axum::body::Body::empty());
        *res.status_mut() = StatusCode::SEE_OTHER;
        res.headers_mut().insert(
            axum::http::header::LOCATION,
            HeaderValue::from_static("/k/login?err=1"),
        );
        return res;
    }
    let (id, _, role, _, ver) = row.expect("checked above");
    let token = match auth::create_token_days(id, &q.username, &role, ver, &s.cfg.jwt_secret, 30) {
        Ok(t) => t,
        Err(e) => return err_to_page(e),
    };
    let cookie = format!(
        "k_session={}; Path=/; HttpOnly; Max-Age={}; SameSite=Lax",
        token,
        60 * 60 * 24 * 30
    );
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    res.headers_mut()
        .insert(axum::http::header::LOCATION, HeaderValue::from_static("/k"));
    res.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).unwrap_or(HeaderValue::from_static("")),
    );
    res
}

pub(super) async fn logout() -> Response {
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    res.headers_mut().insert(
        axum::http::header::LOCATION,
        HeaderValue::from_static("/k/login"),
    );
    res.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static("k_session=; Path=/; HttpOnly; Max-Age=0"),
    );
    res
}

// ---------------- 书架 ----------------

pub(super) async fn shelf(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<FsQ>,
) -> Response {
    let vp = vp_of(&s, &headers, q.vw, q.vh);
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let rows: Vec<BookshelfItem> = match sqlx::query_as::<_, BookshelfItem>(SHELF_LIST_SQL)
        .bind(None::<String>)
        .fetch_all(&db.0)
        .await
    {
        Ok(r) => r,
        Err(e) => return err_to_page(AppError::Sqlx(e)),
    };

    let next_fs = cycle_fs(fs);
    let total_books = rows.len();
    // 每页本数与行高按「页面高 − 顶栏 − 底部分页条 − 安全余量」实算（1268 → 10 本 × 110px）
    let (per_page, row_h) = vp.shelf_layout();
    let t_pages = total_books.div_ceil(per_page).max(1);
    let sp = q.sp.unwrap_or(1).clamp(1, t_pages as i64) as usize;
    let start = (sp - 1) * per_page;
    let end = (start + per_page).min(total_books);
    let fs_url = format!("/k?fs={}&sp={}", next_fs, sp.min(t_pages.max(1)));
    // 导航只出现一次：书架/搜索/目录用顶部导航，底栏只留给阅读页
    let nav = format!(
        "{}{}{}",
        nav_link(&qlink("/k/search", fs, vp), "搜索"),
        nav_link(&with_vp(&fs_url, vp), FS_SPECS[next_fs - 1].label),
        nav_link("/k/logout", "退出"),
    );
    let top = top_bar("书架", None, &nav, vp);

    let mut list = String::new();
    if rows.is_empty() {
        list.push_str(&empty_shelf(fs, vp));
    } else {
        list.push_str(&format!(
            "<table width=\"{}\" cellpadding=\"0\" cellspacing=\"0\">",
            vp.page_w()
        ));
        for b in &rows[start..end] {
            let title = b.title.clone().unwrap_or_else(|| "未命名".into());
            let author = b.author.clone().unwrap_or_default();
            let src = b.source_id.clone().unwrap_or_default();
            let read = qlink(
                &format!(
                    "/k/read?book={}&source={}&ch={}&p=1",
                    qenc(&b.book_id),
                    qenc(&src),
                    b.chapter_index
                ),
                fs,
                vp,
            );
            let first = title.chars().next().unwrap_or('书');
            list.push_str(&format!(
                "<tr><td style=\"padding:0\">\
                 <a href=\"{read}\" style=\"display:block;height:{rh}px;border-bottom:1px solid #ccc\">\
                 <table width=\"{iw}\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
                 <td width=\"56\" valign=\"middle\"><div style=\"width:56px;height:80px;border:1px solid #000;font-size:24px;line-height:78px;text-align:center\">{first}</div></td>\
                 <td style=\"padding-left:24px\" valign=\"middle\">\
                 <div style=\"font-size:32px;line-height:48px;font-weight:700\">{title}</div>\
                 <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-top:8px\"><tr>\
                 <td align=\"left\" style=\"font-size:20px;line-height:32px;color:#555\">{author}</td>\
                 <td align=\"right\" style=\"font-size:20px;line-height:32px;color:#555\">{pct:.0}%</td>\
                 </tr></table></td></tr></table></a></td></tr>",
                read = read,
                rh = row_h,
                iw = vp.page_w(),
                first = esc(&first.to_string()),
                title = esc(&title),
                author = esc(if author.is_empty() { "未知作者" } else { author.as_str() }),
                pct = b.reading_progress
            ));
        }
        list.push_str("</table>");
    }

    let pager = if t_pages > 1 {
        let prev = if sp > 1 {
            Some((
                with_vp(&format!("/k?fs={}&sp={}", fs, sp - 1), vp),
                "上一页",
            ))
        } else {
            None
        };
        let next = if sp < t_pages {
            Some((
                with_vp(&format!("/k?fs={}&sp={}", fs, sp + 1), vp),
                "下一页",
            ))
        } else {
            None
        };
        pager_bar(prev, next, vp)
    } else {
        String::new()
    };
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        list = list,
        pager = pager
    );
    let mut res = page("书架", fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}

// ---------------- 目录 ----------------

pub(super) async fn toc(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    headers: HeaderMap,
    Query(q): Query<TocQ>,
) -> Response {
    let vp = vp_of(&s, &headers, q.vw, q.vh);
    let (fs, explicit) = resolve_fs(&headers, q.fs);
    let (_source, chapters) = match ensure_chapters(&s, &db.0, &q.source, &q.book).await {
        Ok(v) => v,
        Err(e) => return err_to_page(e),
    };
    // 分卷标题不是可读章节：目录不展示、不计入总数与分页。
    // 保留 `BookChapter.index`（原始下标），链接仍按原 index 生成，/k/read 才能对得上。
    let visible = visible_chapters(&chapters);
    let total = visible.len() as i64;
    // 每列行数按「页面高 − 顶栏 − 底部分页条 − 安全余量」实算，30 只是上限
    let toc_page = toc_page_size(vp);
    let rows_per_col = toc_page / K_TOC_COLS;
    let t_pages = ((total + toc_page as i64 - 1) / toc_page as i64).max(1);
    let tp = q.tp.unwrap_or(1).clamp(1, t_pages);
    let start = ((tp - 1) * toc_page as i64) as usize;
    let end = (start + toc_page).min(visible.len());
    let name = load_book_meta(&db.0, &q.book)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.name)
        .unwrap_or_else(|| "目录".into());

    let next_fs = cycle_fs(fs);
    let fs_href = with_vp(
        &with_fs(
            &format!(
                "/k/toc?book={}&source={}&tp={}&ch={}&p={}",
                qenc(&q.book),
                qenc(&q.source),
                tp,
                q.ch.unwrap_or(-1),
                q.p.unwrap_or(1)
            ),
            next_fs,
        ),
        vp,
    );
    // 导航只出现一次：标题=书名，右侧=书架 / 字号 / ✕（回到进入目录前的阅读页，
    // 章节与页码原样带回；✕ 用 80×72 的整格热区，墨水屏上也好点）。
    let read_back = qlink(
        &format!(
            "/k/read?book={}&source={}&ch={}&p={}",
            qenc(&q.book),
            qenc(&q.source),
            q.ch.unwrap_or(0),
            q.p.unwrap_or(1)
        ),
        fs,
        vp,
    );
    let back = format!(
        "<a href=\"{}\" style=\"display:inline-block;min-width:80px;text-align:center;font-size:34px;line-height:72px;font-weight:400\">&#10005;</a>",
        read_back
    );
    let nav = format!(
        "{}{}{}",
        nav_link(&qlink("/k", fs, vp), "书架"),
        nav_link(&fs_href, FS_SPECS[next_fs - 1].label),
        back
    );
    let top = top_bar(&name, Some(&format!("共 {total} 章")), &nav, vp);

    // 两列各 rows_per_col 行（1268 → 15 行/列）；列宽随版心自适应（1072 → 452，1024 → 428）
    let col_w = vp.content_w().saturating_sub(24) / K_TOC_COLS;
    let mut cols = [String::new(), String::new()];
    for (i, c) in visible[start..end].iter().enumerate() {
        let col = (i / rows_per_col).min(K_TOC_COLS - 1);
        let href = qlink(
            &format!(
                "/k/read?book={}&source={}&ch={}&p=1",
                qenc(&q.book),
                qenc(&q.source),
                c.index
            ),
            fs,
            vp,
        );
        let title = c.title.as_deref().unwrap_or("未命名章节");
        let cur = q.ch.map(|x| x == c.index).unwrap_or(false);
        let name = if cur {
            format!("<b>▸ {}</b>", esc(title))
        } else {
            esc(title)
        };
        cols[col].push_str(&format!(
            "<tr><td height=\"72\" style=\"border-bottom:1px solid #ccc\">\
             <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr>\
             <td width=\"80\" align=\"right\" valign=\"middle\" style=\"font-size:24px;line-height:46px;color:#555\">{no}</td>\
             <td valign=\"middle\" style=\"padding-left:16px;font-size:28px;line-height:46px\"><a href=\"{href}\" style=\"display:block\">{name}</a></td>\
             </tr></table></td></tr>",
            no = start + i + 1,
            href = href,
            name = name
        ));
    }
    for c in cols.iter_mut() {
        if c.is_empty() {
            c.push_str("<tr><td height=\"72\"></td></tr>");
        }
        *c = format!(
            "<table width=\"{col_w}\" cellpadding=\"0\" cellspacing=\"0\">{}</table>",
            c
        );
    }
    let list = format!(
        "<table width=\"{tw}\" cellpadding=\"0\" cellspacing=\"0\" style=\"margin-left:{pad}\"><tr>\
         <td width=\"{col_w}\" valign=\"top\">{left}</td><td width=\"24\"></td><td width=\"{col_w}\" valign=\"top\">{right}</td>\
         </tr></table>",
        tw = vp.content_w(),
        pad = SIDE_PAD,
        col_w = col_w,
        left = cols[0],
        right = cols[1]
    );

    let prev = if tp > 1 {
        Some((
            qlink(
                &format!(
                    "/k/toc?book={}&source={}&tp={}&ch={}&p={}",
                    qenc(&q.book),
                    qenc(&q.source),
                    tp - 1,
                    q.ch.unwrap_or(-1),
                    q.p.unwrap_or(1)
                ),
                fs,
                vp,
            ),
            "上一页",
        ))
    } else {
        None
    };
    let next = if tp < t_pages {
        Some((
            qlink(
                &format!(
                    "/k/toc?book={}&source={}&tp={}&ch={}&p={}",
                    qenc(&q.book),
                    qenc(&q.source),
                    tp + 1,
                    q.ch.unwrap_or(-1),
                    q.p.unwrap_or(1)
                ),
                fs,
                vp,
            ),
            "下一页",
        ))
    } else {
        None
    };
    let stage = format!(
        "<div id=\"k-stage\" style=\"position:absolute;top:{top}px;left:{pad}px;right:{pad}px;bottom:{safe}px;overflow:hidden\">{list}{pager}</div>",
        top = NAV_H + 1,
        pad = SIDE_PAD,
        safe = SAFE_BOTTOM,
        list = list,
        pager = pager_bar(prev, next, vp)
    );

    let mut res = page(&name, fs, &format!("{top}{stage}"), vp);
    if explicit {
        remember_fs(&mut res, fs);
    }
    res
}
