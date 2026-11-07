//! 替换规则路由的规则试跑预览（move-only 从 replace.rs 拆出）。

use super::*;

// ---------------- 规则试跑预览 ----------------

/// 预览请求：`url` 与 `input` 二选一（同时给出时优先 url）。
/// `rules`/`rule` 支持 Legado camelCase 或内部 snake_case。
#[derive(Deserialize, Default)]
pub struct PreviewReq {
    #[serde(default)]
    pub rules: Option<Value>,
    #[serde(default)]
    pub rule: Option<Value>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<Value>,
    #[serde(default)]
    pub book: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub for_title: bool,
}

/// 与抓取层一致的预算：响应/输入 5 MiB、单次预览 15 s。
pub(crate) const PREVIEW_MAX_BYTES: usize = 5 * 1024 * 1024;
pub(crate) const PREVIEW_TIMEOUT_MS: u64 = 15_000;
/// 单个中间值最多返回 2 万字符，最终值最多 20 万字符，超出截断并置 truncated 标记。
pub(crate) const PREVIEW_STEP_OUTPUT_MAX: usize = 20_000;
pub(crate) const PREVIEW_FINAL_MAX: usize = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct PreviewStep {
    pub index: usize,
    pub name: Option<String>,
    pub pattern: String,
    pub replacement: String,
    pub is_regex: bool,
    pub applied: bool,
    pub skipped_reason: Option<String>,
    pub hits: usize,
    pub output: String,
    pub output_truncated: bool,
}

/// 按 UTF-8 字符边界安全截断。
pub(crate) fn clip_output(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// 依次应用规则并记录每一步的中间值（与 `apply_rules` 的生效条件保持一致）。
pub(crate) fn preview_apply(
    rules: &[ReplaceRule],
    input: &str,
    book: &str,
    origin: &str,
    for_title: bool,
) -> (String, Vec<PreviewStep>) {
    let mut out = input.to_string();
    let mut steps: Vec<PreviewStep> = Vec::with_capacity(rules.len());
    for (index, r) in rules.iter().enumerate() {
        let mut step = PreviewStep {
            index,
            name: r.name.clone(),
            pattern: r.pattern.clone(),
            replacement: r.replacement.clone(),
            is_regex: r.is_regex,
            applied: false,
            skipped_reason: None,
            hits: 0,
            output: String::new(),
            output_truncated: false,
        };
        if !r.is_enabled {
            step.skipped_reason = Some("disabled".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if for_title && !r.scope_title || !for_title && !r.scope_content {
            step.skipped_reason = Some("scope_flag".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if r.pattern.trim().is_empty() {
            step.skipped_reason = Some("empty_pattern".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if let Some(scope) = r.scope.as_deref() {
            if !scope.trim().is_empty() && !scope_hit(&r.scope, book, origin) {
                step.skipped_reason = Some("scope_mismatch".into());
                step.output = out.clone();
                steps.push(step);
                continue;
            }
        }
        if scope_hit(&r.exclude_scope, book, origin) {
            step.skipped_reason = Some("excluded".into());
            step.output = out.clone();
            steps.push(step);
            continue;
        }
        if r.is_regex {
            if r.pattern.len() > 512 {
                step.skipped_reason = Some("pattern_too_long".into());
                step.output = out.clone();
                steps.push(step);
                continue;
            }
            match regex::Regex::new(&r.pattern) {
                Ok(re) => {
                    step.hits = re.find_iter(&out).count();
                    out = re.replace_all(&out, r.replacement.as_str()).into_owned();
                    step.applied = true;
                }
                Err(e) => {
                    step.skipped_reason = Some(format!("invalid_regex: {e}"));
                }
            }
        } else {
            step.hits = out.matches(&r.pattern).count();
            out = out.replace(&r.pattern, &r.replacement);
            step.applied = true;
        }
        let (clipped, truncated) = clip_output(&out, PREVIEW_STEP_OUTPUT_MAX);
        step.output = clipped;
        step.output_truncated = truncated;
        steps.push(step);
    }
    (out, steps)
}

pub(crate) fn preview_rules_from_req(q: &PreviewReq) -> AppResult<Vec<ReplaceRule>> {
    let raw = if let Some(rules) = &q.rules {
        rules.clone()
    } else if let Some(rule) = &q.rule {
        rule.clone()
    } else {
        return Err(AppError::BadRequest("rules 或 rule 至少提供一个".into()));
    };
    let items = import_items(raw)?;
    let mut out = Vec::new();
    for item in items {
        out.push(normalize_import_rule(item)?);
    }
    Ok(out)
}

pub async fn preview_rules(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(q): Json<PreviewReq>,
) -> AppResult<Json<Value>> {
    let rules = preview_rules_from_req(&q)?;
    let (text, fetched) = match (&q.url, &q.input) {
        (Some(url), _) if !url.trim().is_empty() => {
            let url = url.trim();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(AppError::BadRequest("url 必须是 http(s) URL".into()));
            }
            let headers = crate::req::parse_headers(&q.headers.as_ref().map(|v| v.to_string()));
            let started = Instant::now();
            // SSRF 防护：默认拒绝回环/私网/链路本地，逐跳校验重定向
            let body =
                crate::req::fetch_checked(url, "GET", &headers, None, s.cfg.preview_allow_private)
                    .await
                    // 抓取层已区分超时/连接失败/非法地址，这里透传为 400，
                    // 而不是 500 的 “internal: request failed: error sending request”
                    .map_err(|e| AppError::BadRequest(format!("预览抓取失败：{e}")))?;
            if started.elapsed().as_millis() as u64 > PREVIEW_TIMEOUT_MS {
                return Err(AppError::BadRequest("预览超时：超过 15 s 预算".into()));
            }
            if body.len() > PREVIEW_MAX_BYTES {
                return Err(AppError::BadRequest("响应超过 5 MiB 预算".into()));
            }
            (body, true)
        }
        (_, Some(input)) => (input.clone(), false),
        _ => return Err(AppError::BadRequest("input 或 url 至少提供一个".into())),
    };
    if text.len() > PREVIEW_MAX_BYTES {
        return Err(AppError::BadRequest("input 超过 5 MiB 预算".into()));
    }
    let book = q.book.clone().unwrap_or_default();
    let origin = q.origin.clone().unwrap_or_default();
    let (final_out, steps) = preview_apply(&rules, &text, &book, &origin, q.for_title);
    let (final_clipped, final_truncated) = clip_output(&final_out, PREVIEW_FINAL_MAX);
    Ok(Json(json!({
        "ok": true,
        "fetched": fetched,
        "input_length": text.len(),
        "final": final_clipped,
        "output_length": final_out.len(),
        "final_truncated": final_truncated,
        "steps": steps,
    })))
}
