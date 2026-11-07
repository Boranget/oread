//! web_book 的目录抓取与分页（move-only 从 web_book.rs 拆出）。

use super::*;

// ---------------- 目录 ----------------
pub async fn chapter_list(source: &BookSource, book: &Book) -> anyhow::Result<Vec<BookChapter>> {
    chapter_list_book(source, book, None).await
}

/// 带 book 作用域的目录：预载同书变量与 cache（快看目录要从 info 阶段缓存的
/// `kkmh` 里 `java.setContent` 解析章节；爱看要用搜索阶段写的 `bid`）。
pub async fn chapter_list_book(
    source: &BookSource,
    book: &Book,
    store: Option<&BookStore>,
) -> anyhow::Result<Vec<BookChapter>> {
    let rule: &TocRule = match &source.rule_toc {
        Some(r) => r,
        None => return Ok(vec![]),
    };
    let headers = req::source_headers(source, &book.book_url);
    let mut url = book
        .toc_url
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| book.book_url.clone());
    // 目录地址本身也可以是分页模板（`.../toc_{{page}}.html`），首屏固定为第 1 页
    url = url.replace("{{page}}", "1").replace("${page}", "1");
    let ar = make_ar_book(source, store, &book.book_url);
    inject_book_meta(&ar, book);
    ar.book_toc_url.replace(url.clone());
    // Legado `TocRule.preUpdateJs`：刷新目录前执行；可改写 book.bookUrl / 调 refreshTocUrl。
    // 上游规则异常会中断目录更新；本服务按“解析不拖垮源”的原则降级为日志 + 台账后继续。
    if let Some(pre) = &rule.pre_update_js {
        if let Err(e) = ar.eval_script(pre, &book.book_url, &book.book_url) {
            note_field_error(source, "pre_update_js", &e);
        }
        record_ar(&ar, store);
        let refreshed_book_url = ar.book_url.borrow().clone();
        let book_url_changed =
            !refreshed_book_url.is_empty() && refreshed_book_url != book.book_url;
        if ar.refresh_toc.get() || book_url_changed {
            // `java.refreshTocUrl()` 的同步等价：重抓详情页并按 `toc_url` 规则重算目录地址，
            // 然后再走下面的目录抓取（否则会继续用旧的 book.tocUrl）。
            let target = if refreshed_book_url.is_empty() {
                book.book_url.clone()
            } else {
                refreshed_book_url
            };
            if let Some(new_toc) = refresh_toc_url_from_info(source, store, &target).await {
                url = new_toc.replace("{{page}}", "1").replace("${page}", "1");
            }
        }
    }
    let mut chaps = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: std::collections::VecDeque<(String, i32)> = std::collections::VecDeque::new();
    queue.push_back((url, 1));
    // 最多抓 10 页目录，与正文多页保护一致，避免书源死链把请求数放大
    while let Some((url, page)) = queue.pop_front() {
        if seen.len() >= 10 || seen.contains(&url) {
            continue;
        }
        seen.insert(url.clone());
        // 让规则里的 `${page}` / `{{page}}` 拿到当前页号
        ar.set_var("page", page.to_string());
        let html = req::fetch_for_source_limited(
            &url,
            "GET",
            &headers,
            None,
            &source.book_source_url,
            req::source_timeout(source),
            req::source_limit(source),
        )
        .await?;
        let rule_text = rule.chapter_list.as_deref().unwrap_or("");
        // Legado 目录规则前缀：`-` 表示倒序（如 `-class.list@tag.li`），`+` 表示保持原序
        // （`+@js:…` 的「正序」标记，见 WebBookChapterList 对 chapterList 的前缀剥离）；
        // 直接当选择器/JS 会抓空或语法报错。
        let (list_rule, reverse) = match rule_text.trim().strip_prefix('-') {
            Some(rest) => (rest.trim(), true),
            None => match rule_text.trim().strip_prefix('+') {
                Some(rest) => (rest.trim(), false),
                None => (rule_text, false),
            },
        };
        let mut items = eval_list_field(source, "chapter_list", &ar, &html, list_rule);
        if reverse {
            items.reverse();
        }
        for item in &items {
            // `@js` 返回 `[{text,href}]` 展开后的对象项：空项/缺字段项跳过并记一条日志
            let is_volume = flag(source, "is_volume", &ar, item, &rule.is_volume, &url)
                || item_json_volume(item);
            let title = opt(source, "chapter_name", &ar, item, &rule.chapter_name, &url);
            let raw_url = eval_field(
                source,
                "chapter_url",
                &ar,
                item,
                rule.chapter_url.as_deref().unwrap_or(""),
                true,
                &url,
            );
            if !is_volume
                && title.as_deref().map(blank_value).unwrap_or(true)
                && blank_value(&raw_url)
            {
                tracing::warn!(
                    source = source.book_source_url.as_str(),
                    "目录项缺少标题与链接，已跳过"
                );
                continue;
            }
            let idx = chaps.len() as i64;
            // 分卷行通常没有真实链接：用稳定合成 URL 作主键，避免覆盖正文章节或产生假 URL
            let url_final = if is_volume {
                format!("volume:{}#{idx}", book.book_url)
            } else if blank_value(&raw_url) {
                format!("{}{}", book.book_url, idx)
            } else {
                abs(&url, &raw_url)
            };
            let is_vip = flag(source, "is_vip", &ar, item, &rule.is_vip, &url);
            // isPay：付费章节标记，与 isVip 走同一条「书源字段映射 -> 布尔」路径
            let is_pay = flag(source, "is_pay", &ar, item, &rule.is_pay, &url);
            chaps.push(BookChapter {
                url: url_final,
                title,
                is_volume,
                is_vip,
                is_pay,
                update_time: opt(source, "update_time", &ar, item, &rule.update_time, &url),
                book_url: Some(book.book_url.clone()),
                base_url: Some(url.clone()),
                index: idx,
                ..Default::default()
            });
        }
        let nexts = next_toc_urls(
            &ar,
            &html,
            rule.next_toc_url.as_deref().unwrap_or(""),
            &url,
            page,
        );
        for next in nexts {
            if next != url && !seen.contains(&next) {
                queue.push_back((next, page + 1));
            }
        }
    }
    // Legado TocRule.formatJs：逐章执行，返回字符串替换章节标题；
    // 单章执行失败只跳过该章，不影响整份目录。
    if let Some(js) = rule.format_js.as_deref().filter(|s| !s.trim().is_empty()) {
        for (i, ch) in chaps.iter_mut().enumerate() {
            ar.set_var("index", (i + 1).to_string());
            ar.set_var("title", ch.title.clone().unwrap_or_default());
            match ar.eval_script(js, "", &ch.url) {
                Ok(v) if !v.trim().is_empty() => ch.title = Some(v),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    source = source.book_source_url.as_str(),
                    "formatJs 执行失败：{e}"
                ),
            }
        }
    }
    record_ar(&ar, store);
    Ok(chaps)
}

// ---------------- 正文 ----------------
/// 抓取正文所有分页：返回 `(当前页 URL, 原始 HTML)`。
/// 正文与图片章节共用同一套 nextContentUrl 多页逻辑，规则各自只应用一次。
pub(super) async fn fetch_html_pages(
    source: &BookSource,
    chapter: &BookChapter,
    rule: &ContentRule,
) -> anyhow::Result<Vec<(String, String)>> {
    let headers = req::source_headers(source, &chapter.url);
    let mut url = chapter.url.clone();
    let mut pages: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut guard = 0;
    loop {
        if guard >= 10 || seen.contains(&url) {
            break;
        }
        guard += 1;
        seen.insert(url.clone());
        let html = req::fetch_for_source_limited(
            &url,
            "GET",
            &headers,
            None,
            &source.book_source_url,
            req::source_timeout(source),
            req::source_limit(source),
        )
        .await?;
        let ar = make_ar(source);
        let next = eval_field(
            source,
            "next_content_url",
            &ar,
            &html,
            rule.next_content_url.as_deref().unwrap_or(""),
            true,
            &url,
        );
        pages.push((url.clone(), html));
        if blank_value(&next) {
            break;
        }
        let next_abs = abs(&url, &next);
        if next_abs == url {
            break;
        }
        url = next_abs;
    }
    Ok(pages)
}
