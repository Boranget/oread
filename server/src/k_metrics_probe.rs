//! k_metrics 的客户端视口探针（move-only 从 k_metrics.rs 拆出）。

use super::*;

/// `GET /api/k/probe`：页内自报视口探针（ES5 XHR，失败静默，不重载页面）。
///
/// 字段同时记录「URL 参数视口」与「页面实测视口」，用来判断 Kindle 报的
/// `innerHeight` 是否等于真正参与排版的布局视口高（`clientHeight`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeLine {
    pub ts: String,
    pub path: String,
    #[serde(default)]
    pub vw_param: Option<String>,
    #[serde(default)]
    pub vh_param: Option<String>,
    #[serde(default)]
    pub inner_w: Option<String>,
    #[serde(default)]
    pub inner_h: Option<String>,
    #[serde(default)]
    pub client_w: Option<String>,
    #[serde(default)]
    pub client_h: Option<String>,
    #[serde(default)]
    pub scroll_w: Option<String>,
    #[serde(default)]
    pub scroll_h: Option<String>,
    #[serde(default)]
    pub dpr: Option<String>,
    /// v2：`screen.width`，用于判断系统栏/工具栏是否占掉了可视视口
    #[serde(default)]
    pub screen_w: Option<String>,
    #[serde(default)]
    pub screen_h: Option<String>,
    /// v2：`screen.availHeight`（去掉系统栏后的可用高）
    #[serde(default)]
    pub avail_h: Option<String>,
    /// v2：`outerWidth/outerHeight`
    #[serde(default)]
    pub outer_w: Option<String>,
    #[serde(default)]
    pub outer_h: Option<String>,
    /// v2：`document.body.clientHeight`
    #[serde(default)]
    pub body_h: Option<String>,
    /// v2：`#k-stage` 的 `clientHeight`，页面无该元素时为 `-`
    #[serde(default)]
    pub stage_h: Option<String>,
    /// v3：`calib_js` 标定并写入 `k_vh` 的真实可见高
    #[serde(default)]
    pub visible_h: Option<String>,
    /// v3：底栏 `#k-bar` 实际 top（`getBoundingClientRect().top`）
    #[serde(default)]
    pub bar_top: Option<String>,
    /// v4：标定结果（`ok`/`fail`，失败时不再静默回退）
    #[serde(default)]
    pub calib: Option<String>,
    /// v4：实测最大滚动量（`scrollTo` 后两次采样的较大值；未测量为 `-`）
    #[serde(default)]
    pub meas_y: Option<String>,
    /// v4：底栏实际 bottom（`getBoundingClientRect().bottom`），用于判断是否被可见区裁掉
    #[serde(default)]
    pub bar_bottom: Option<String>,
    /// v4：上报时的 `window.pageYOffset`，非 0 表示浏览器真的平移了文档
    #[serde(default)]
    pub page_y: Option<String>,
    /// v4：`body.scrollHeight`，与 `client_h` 对比可区分「文档真溢出」与「浏览器平移」
    #[serde(default)]
    pub body_scroll_h: Option<String>,
    /// v4：URL 上是否还带旧版 `kfix` 闩锁（1/0/空）
    #[serde(default)]
    pub kfix: Option<String>,
    /// v5：`body` 的 `getBoundingClientRect().bottom`，判断 `body` 是否真的超出可见区
    #[serde(default)]
    pub body_rect_bottom: Option<String>,
    /// v5：`body` 直接子元素 `top+height` 的最大值（漏排/贴底元素是否越界）
    #[serde(default)]
    pub lowest_el_bottom: Option<String>,
    /// v5：是否为用户滑动后补报（1 = scroll 事件触发的那条）
    #[serde(default)]
    pub scroll_evt: Option<String>,
    /// v6：打点通道（`sync`/`load`/`scroll`/`gerr`/`calib`/`probe`），确认脚本真的执行到了
    #[serde(default)]
    pub stage: Option<String>,
    /// v6：脚本异常信息（`stage=gerr|calib|probe` 时非空，截 60 字）
    #[serde(default)]
    pub err: Option<String>,
    /// v6：异常行号（`window.onerror` 的 lineno；脚本内 try/catch 填 0）
    #[serde(default)]
    pub ln: Option<String>,
    /// v7：`documentElement.scrollTop`（旧引擎 `pageYOffset` 恒 0，用这个判断真实滚动）
    #[serde(default)]
    pub doc_top: Option<String>,
    /// v7：`body.scrollTop`（与 `doc_top` 分开上报，确认是谁在滚）
    #[serde(default)]
    pub scroll_top_body: Option<String>,
    /// v7：`documentElement.scrollTop`
    #[serde(default)]
    pub scroll_top_doc: Option<String>,
    /// v7：本页生命周期内观测到的最大位移（区分「滑一下又弹回」）
    #[serde(default)]
    pub max_seen: Option<String>,
    /// v7：`visualViewport.pageTop`（不支持时为 `-`），判断是否为可视视口平移
    #[serde(default)]
    pub vv_off: Option<String>,
    /// v7：底栏 `#k-bar` 的 `getBoundingClientRect().top`（滑动前后是否跟着动）
    #[serde(default)]
    pub bar_rect_top: Option<String>,
    /// v7：`stage=move` 的来源（`touchmove`/`scroll`/`end`/`settle`）
    #[serde(default)]
    pub move_src: Option<String>,
    /// v7：底栏 top 相对 load 基线的位移（pan 代理量）
    #[serde(default)]
    pub bar_top_delta: Option<String>,
    /// v7：`window.scrollY`（与 scrollTop 三路对照）
    #[serde(default)]
    pub scroll_y: Option<String>,
    /// v7-touch：触摸事件类型（`start`/`move`/`end`）
    #[serde(default)]
    pub tev: Option<String>,
    /// v7-touch：事件是否可取消（决定 `preventDefault` 是否可能生效）
    #[serde(default)]
    pub cancelable: Option<String>,
    /// v7-touch：`touches.length`
    #[serde(default)]
    pub touches_len: Option<String>,
    /// v7-touch：首个触点的 `clientY`
    #[serde(default)]
    pub client_y: Option<String>,
    /// v7-touch：首个触点的 `pageY`
    #[serde(default)]
    pub page_y_touch: Option<String>,
    /// v7：锁定文档高（`documentElement.style.height`，即 `visibleH − 88`）
    #[serde(default)]
    pub doc_h: Option<String>,
    /// v7：`doc_h − outerHeight`（真机 1136 − 1232 = −96）
    #[serde(default)]
    pub doc_minus_outer: Option<String>,
    /// v7：`doc_h − clientHeight`
    #[serde(default)]
    pub doc_minus_client: Option<String>,
    /// v7：`outerHeight − doc_h`
    #[serde(default)]
    pub outer_minus_doc: Option<String>,
    /// v7：viewport meta 里的 `height=`（没输出则 `-`）
    #[serde(default)]
    pub meta_h: Option<String>,
    /// v7：同 `meta_h`，命名对齐外部建议
    #[serde(default)]
    pub vw_meta_height: Option<String>,
    #[serde(default)]
    pub fs: Option<String>,
    pub ua_class: String,
    #[serde(default)]
    pub ua: Option<String>,
    #[serde(default)]
    pub sid: Option<String>,
}

/// 探针单字段上限（防超长 query 落盘）
pub(crate) const PROBE_LIMIT: usize = 32;

#[derive(Deserialize)]
pub struct ProbeQ {
    pub vw_param: Option<String>,
    pub vh_param: Option<String>,
    pub inner_w: Option<String>,
    pub inner_h: Option<String>,
    pub client_w: Option<String>,
    pub client_h: Option<String>,
    pub scroll_w: Option<String>,
    pub scroll_h: Option<String>,
    pub dpr: Option<String>,
    pub screen_w: Option<String>,
    pub screen_h: Option<String>,
    pub avail_h: Option<String>,
    pub outer_w: Option<String>,
    pub outer_h: Option<String>,
    pub body_h: Option<String>,
    pub stage_h: Option<String>,
    pub visible_h: Option<String>,
    pub bar_top: Option<String>,
    pub calib: Option<String>,
    pub meas_y: Option<String>,
    pub bar_bottom: Option<String>,
    pub page_y: Option<String>,
    pub body_scroll_h: Option<String>,
    pub kfix: Option<String>,
    pub body_rect_bottom: Option<String>,
    pub lowest_el_bottom: Option<String>,
    pub scroll_evt: Option<String>,
    pub stage: Option<String>,
    pub err: Option<String>,
    pub ln: Option<String>,
    pub doc_top: Option<String>,
    pub scroll_top_body: Option<String>,
    pub scroll_top_doc: Option<String>,
    pub max_seen: Option<String>,
    pub vv_off: Option<String>,
    pub bar_rect_top: Option<String>,
    pub move_src: Option<String>,
    pub bar_top_delta: Option<String>,
    pub scroll_y: Option<String>,
    pub tev: Option<String>,
    pub cancelable: Option<String>,
    pub touches_len: Option<String>,
    /// 触摸打点用驼峰键名（与页面 JS 的 `clientY=`/`pageY=` 对齐）
    #[serde(rename = "clientY")]
    pub client_y: Option<String>,
    #[serde(rename = "pageY")]
    pub page_y_touch: Option<String>,
    pub doc_h: Option<String>,
    pub doc_minus_outer: Option<String>,
    pub doc_minus_client: Option<String>,
    pub outer_minus_doc: Option<String>,
    pub meta_h: Option<String>,
    pub vw_meta_height: Option<String>,
    pub fs: Option<String>,
    pub path: Option<String>,
}

pub(crate) fn norm(v: Option<String>) -> Option<String> {
    v.map(|s| truncate(s.trim(), PROBE_LIMIT))
        .filter(|s| !s.is_empty())
}

/// 异常信息放宽到 60 字（`PROBE_LIMIT` 32 字会截掉关键报错后半段）
pub(crate) fn norm60(v: Option<String>) -> Option<String> {
    v.map(|s| truncate(s.trim(), 60)).filter(|s| !s.is_empty())
}

pub async fn probe(
    State(state): State<AppState>,
    _u: AuthUser,
    headers: axum::http::HeaderMap,
    Query(q): Query<ProbeQ>,
) -> StatusCode {
    let ua_raw = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let sid = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(session_token)
        .map(|t| sid_hash(&t));
    let line = ProbeLine {
        ts: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        path: norm(q.path).unwrap_or_else(|| "/".into()),
        vw_param: norm(q.vw_param),
        vh_param: norm(q.vh_param),
        inner_w: norm(q.inner_w),
        inner_h: norm(q.inner_h),
        client_w: norm(q.client_w),
        client_h: norm(q.client_h),
        scroll_w: norm(q.scroll_w),
        scroll_h: norm(q.scroll_h),
        dpr: norm(q.dpr),
        screen_w: norm(q.screen_w),
        screen_h: norm(q.screen_h),
        avail_h: norm(q.avail_h),
        outer_w: norm(q.outer_w),
        outer_h: norm(q.outer_h),
        body_h: norm(q.body_h),
        stage_h: norm(q.stage_h),
        visible_h: norm(q.visible_h),
        bar_top: norm(q.bar_top),
        calib: norm(q.calib),
        meas_y: norm(q.meas_y),
        bar_bottom: norm(q.bar_bottom),
        page_y: norm(q.page_y),
        body_scroll_h: norm(q.body_scroll_h),
        kfix: norm(q.kfix),
        body_rect_bottom: norm(q.body_rect_bottom),
        lowest_el_bottom: norm(q.lowest_el_bottom),
        scroll_evt: norm(q.scroll_evt),
        stage: norm(q.stage),
        err: norm60(q.err),
        ln: norm(q.ln),
        doc_top: norm(q.doc_top),
        scroll_top_body: norm(q.scroll_top_body),
        scroll_top_doc: norm(q.scroll_top_doc),
        max_seen: norm(q.max_seen),
        vv_off: norm(q.vv_off),
        bar_rect_top: norm(q.bar_rect_top),
        move_src: norm(q.move_src),
        bar_top_delta: norm(q.bar_top_delta),
        scroll_y: norm(q.scroll_y),
        tev: norm(q.tev),
        cancelable: norm(q.cancelable),
        touches_len: norm(q.touches_len),
        client_y: norm(q.client_y),
        page_y_touch: norm(q.page_y_touch),
        doc_h: norm(q.doc_h),
        doc_minus_outer: norm(q.doc_minus_outer),
        doc_minus_client: norm(q.doc_minus_client),
        outer_minus_doc: norm(q.outer_minus_doc),
        meta_h: norm(q.meta_h),
        vw_meta_height: norm(q.vw_meta_height),
        fs: norm(q.fs),
        ua_class: classify_ua(&ua_raw).to_string(),
        ua: Some(truncate(&ua_raw, 120)),
        sid,
    };
    spawn_write(PathBuf::from(&state.cfg.data_dir), "k-probe", line);
    StatusCode::NO_CONTENT
}

#[derive(Serialize)]
pub(crate) struct ProbeStat {
    count: usize,
    /// vw/vh 参数组合（`vw_param`x`vh_param`）
    param: BTreeMap<String, usize>,
    /// innerWidth×innerHeight
    inner: BTreeMap<String, usize>,
    /// documentElement.clientWidth×clientHeight（真正的布局视口）
    client: BTreeMap<String, usize>,
    /// documentElement.scrollWidth×scrollHeight
    scroll: BTreeMap<String, usize>,
    /// `scroll_h > client_h` 的条数（有纵向滚动）
    overflow_v: usize,
    /// `scroll_w > client_w` 的条数（有横向滚动）
    overflow_h: usize,
    /// v2：`screen_h − client_h` 的分布；正值表示屏幕比布局视口高，
    /// 差额就是系统栏/工具栏可能占掉的可视高度
    gap: BTreeMap<String, usize>,
}

pub(crate) fn num(v: &Option<String>) -> Option<i64> {
    v.as_deref().and_then(|s| s.parse::<i64>().ok())
}

/// `GET /api/k/probe/summary?date=YYYY-MM-DD`：管理员查看探针聚合 + 最近 20 条。
pub async fn probe_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MetricsQ>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&user)?;
    let date = q.date.unwrap_or_else(default_date);
    if !valid_date(&date) {
        return Err(AppError::BadRequest("date 需为 YYYY-MM-DD".into()));
    }
    let text =
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-probe", &date))
            .await
            .unwrap_or_default();
    let lines: Vec<ProbeLine> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<ProbeLine>(l).ok())
        .collect();

    let mut stats: BTreeMap<String, ProbeStat> = BTreeMap::new();
    for line in &lines {
        let stat = stats.entry(line.path.clone()).or_insert_with(|| ProbeStat {
            count: 0,
            param: BTreeMap::new(),
            inner: BTreeMap::new(),
            client: BTreeMap::new(),
            scroll: BTreeMap::new(),
            overflow_v: 0,
            overflow_h: 0,
            gap: BTreeMap::new(),
        });
        stat.count += 1;
        let dim = |a: &Option<String>, b: &Option<String>| match (a.as_deref(), b.as_deref()) {
            (Some(x), Some(y)) => format!("{x}x{y}"),
            (Some(x), None) => x.to_string(),
            (None, Some(y)) => format!("?x{y}"),
            (None, None) => "?".into(),
        };
        *stat
            .param
            .entry(dim(&line.vw_param, &line.vh_param))
            .or_default() += 1;
        *stat
            .inner
            .entry(dim(&line.inner_w, &line.inner_h))
            .or_default() += 1;
        *stat
            .client
            .entry(dim(&line.client_w, &line.client_h))
            .or_default() += 1;
        *stat
            .scroll
            .entry(dim(&line.scroll_w, &line.scroll_h))
            .or_default() += 1;
        let ch = num(&line.client_h);
        let sh = num(&line.scroll_h);
        let cw = num(&line.client_w);
        let sw = num(&line.scroll_w);
        if let (Some(a), Some(b)) = (sh, ch) {
            if b > 0 && a > b {
                stat.overflow_v += 1;
            }
        }
        if let (Some(a), Some(b)) = (sw, cw) {
            if b > 0 && a > b {
                stat.overflow_h += 1;
            }
        }
        if let (Some(s), Some(c)) = (num(&line.screen_h), ch) {
            *stat.gap.entry((s - c).to_string()).or_default() += 1;
        }
    }

    let recent: Vec<ProbeLine> = lines.iter().rev().take(20).cloned().collect();
    // 最近一条的完整字典（含 v2 新字段），方便直接判断"可视视口是否更小"
    let latest = lines.last().cloned();
    Ok(Json(serde_json::json!({
        "date": date,
        "probes": lines.len(),
        "paths": stats,
        "latest": latest,
        "recent": recent,
    })))
}
