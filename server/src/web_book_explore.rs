//! web_book 的发现分类与 Legado 替换（move-only 从 web_book.rs 拆出）。

use super::*;

/// 展开书源的 `exploreUrl`：每行 `分类名::url`（无分类名时用书源名），
/// 支持 `{{page}}` 模板；空行/注释行忽略。
/// `exploreUrl` 为 `@js:` / `<js>` 时交给规则引擎执行（受 JS_ENABLED 与耗时预算约束），
/// 结果既可为 `标题::url` 多行文本，也可为 `[{title,url}]` JSON 数组。
pub fn explore_categories(source: &BookSource, page: i32) -> anyhow::Result<Vec<ExploreCategory>> {
    let raw = match source.explore_url.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(vec![]),
    };
    let raw = if raw.starts_with("@js:") || raw.contains("<js>") {
        let ar = make_ar(source);
        ar.get_string("", &raw, false, "")
            .map_err(|e| anyhow::anyhow!("exploreUrl JS 执行失败: {e}"))?
    } else {
        raw
    };
    // JS 结果可能是 JSON 数组
    if raw.trim_start().starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(raw.trim()) {
            let mut out = Vec::new();
            for it in items {
                let title = it
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&source.book_source_name)
                    .to_string();
                let url = match it.get("url").and_then(|v| v.as_str()) {
                    Some(u) if !u.trim().is_empty() => u.trim().to_string(),
                    _ => continue,
                };
                out.push(ExploreCategory { title, url });
            }
            return Ok(out);
        }
    }
    let mut out: Vec<ExploreCategory> = Vec::new();
    let mut seen = HashSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (title, url) = match line.split_once("::") {
            Some((t, u)) => (t.trim().to_string(), u.trim().to_string()),
            None => (source.book_source_name.clone(), line.to_string()),
        };
        if url.is_empty() {
            continue;
        }
        let url = url
            .replace("{{page}}", &page.to_string())
            .replace("{{key}}", "");
        if !seen.insert(url.clone()) {
            continue;
        }
        out.push(ExploreCategory {
            title: if title.is_empty() {
                source.book_source_name.clone()
            } else {
                title
            },
            url,
        });
    }
    Ok(out)
}

/// 多源发现：并发请求所有启用且开启发现的书源（按同一个分类 URL）
pub async fn explore_all(sources: &[BookSource], url: &str) -> Vec<SearchBook> {
    let mut tasks = Vec::new();
    for s in sources {
        if !s.enabled
            || !s.enabled_explore
            || !crate::entities::is_supported_source_type(s.book_source_type)
            || s.rule_explore.is_none()
        {
            continue;
        }
        tasks.push(explore(s, url));
    }
    let results = futures::future::join_all(tasks).await;
    let mut out = Vec::new();
    for mut books in results.into_iter().flatten() {
        out.append(&mut books);
    }
    dedupe_and_sort(out, sources, "")
}

/// 按 Legado 约定执行 `正则##替换` 形式的净化规则（多行可多条）。
/// 兼容 `/pat/repl/` 单条形式；无效正则跳过，不影响正文返回。
pub(super) fn apply_legado_replacements(input: &str, rules: &Option<String>) -> String {
    let rules = match rules {
        Some(r) if !r.trim().is_empty() => r,
        _ => return input.to_string(),
    };
    let mut out = input.to_string();
    for line in rules.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pat, repl) = if let Some((p, r)) = line.split_once("##") {
            (p.trim(), r)
        } else if line.starts_with('/') && line.len() > 2 && line.ends_with('/') {
            let inner = &line[1..line.len() - 1];
            match inner.split_once('/') {
                Some((p, r)) => (p, r),
                None => (inner, ""),
            }
        } else {
            (line, "")
        };
        if pat.is_empty() {
            continue;
        }
        if let Ok(re) = regex::Regex::new(pat) {
            out = re.replace_all(&out, repl).to_string();
        }
    }
    out
}

#[allow(dead_code)]
pub(super) fn _unused(_: HashMap<String, String>) {}
