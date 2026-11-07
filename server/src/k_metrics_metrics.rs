//! k_metrics 的请求指标聚合与导出（move-only 从 k_metrics.rs 拆出）。

use super::*;

#[derive(Deserialize)]
pub struct MetricsQ {
    pub date: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Stat {
    count: usize,
    render_ms: Pct,
    bytes_avg: f64,
    since_prev_ms: Pct,
    ua_class: BTreeMap<String, usize>,
}

#[derive(Serialize)]
pub(crate) struct Pct {
    p50: u64,
    p95: u64,
    max: u64,
}

pub(crate) fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[idx.saturating_sub(1).min(sorted.len() - 1)]
}

/// `GET /api/k/metrics?date=YYYY-MM-DD`：当日聚合 + 最近慢请求（>1000ms）。
pub async fn metrics(
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
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-metrics", &date))
            .await
            .unwrap_or_default();
    let lines: Vec<KLine> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<KLine>(l).ok())
        .collect();

    let mut stats: BTreeMap<String, Stat> = BTreeMap::new();
    for line in &lines {
        let entry = stats.entry(line.path.clone()).or_insert_with(|| Stat {
            count: 0,
            render_ms: Pct {
                p50: 0,
                p95: 0,
                max: 0,
            },
            bytes_avg: 0.0,
            since_prev_ms: Pct {
                p50: 0,
                p95: 0,
                max: 0,
            },
            ua_class: BTreeMap::new(),
        });
        entry.count += 1;
        *entry.ua_class.entry(line.ua_class.clone()).or_default() += 1;
    }
    for (path, stat) in stats.iter_mut() {
        let mut render: Vec<u64> = lines
            .iter()
            .filter(|l| &l.path == path)
            .map(|l| l.render_ms)
            .collect();
        render.sort_unstable();
        let mut since: Vec<u64> = lines
            .iter()
            .filter(|l| &l.path == path)
            .filter_map(|l| l.since_prev_ms.map(|v| v.max(0) as u64))
            .collect();
        since.sort_unstable();
        let bytes_sum: u64 = lines
            .iter()
            .filter(|l| &l.path == path)
            .map(|l| l.bytes)
            .sum();
        stat.render_ms = Pct {
            p50: percentile(&render, 50.0),
            p95: percentile(&render, 95.0),
            max: render.last().copied().unwrap_or(0),
        };
        stat.bytes_avg = if stat.count == 0 {
            0.0
        } else {
            bytes_sum as f64 / stat.count as f64
        };
        stat.since_prev_ms = Pct {
            p50: percentile(&since, 50.0),
            p95: percentile(&since, 95.0),
            max: since.last().copied().unwrap_or(0),
        };
    }

    // 最近 N 条慢请求（文件按时间追加，取尾部）
    let slow: Vec<KLine> = lines
        .iter()
        .filter(|l| l.render_ms > 1000)
        .rev()
        .take(20)
        .cloned()
        .collect();

    Ok(Json(serde_json::json!({
        "date": date,
        "requests": stats.values().map(|s| s.count).sum::<usize>(),
        "paths": stats,
        "slow": slow,
    })))
}

/// `GET /api/k/metrics/export?date=YYYY-MM-DD`：直接下载该日 JSONL 原文。
pub async fn export(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MetricsQ>,
) -> AppResult<Response> {
    ensure_admin(&user)?;
    let date = q.date.unwrap_or_else(default_date);
    if !valid_date(&date) {
        return Err(AppError::BadRequest("date 需为 YYYY-MM-DD".into()));
    }
    let body =
        tokio::fs::read_to_string(log_file(Path::new(&state.cfg.data_dir), "k-metrics", &date))
            .await
            .map_err(|_| AppError::NotFound(format!("该日没有埋点文件：{date}")))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-ndjson".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=k-metrics-{date}.jsonl"),
            ),
        ],
        body,
    )
        .into_response())
}
