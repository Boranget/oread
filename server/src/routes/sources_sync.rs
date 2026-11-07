//! sources 路由的合集增量对比（move-only 从 sources.rs 拆出）。

use super::*;

// ---------------- 书源更新检查（只读） ----------------

#[derive(Debug, Clone, Serialize)]
pub struct SourceUpdateItem {
    pub book_source_url: String,
    pub book_source_name: String,
    pub local_update_time: i64,
    pub incoming_update_time: i64,
}

/// 只读比对：按 `bookSourceUrl` 对齐本地与导入包，产出
/// `updated`（导入包更新）/ `new`（本地没有）/ `local_newer`（本地更新）/ `same`。
/// 不写库，调用方自行决定是否 import。
pub(crate) fn diff_sources(local: &[BookSource], incoming: &[BookSource]) -> Value {
    use std::collections::HashMap;
    let map: HashMap<&str, &BookSource> = local
        .iter()
        .map(|s| (s.book_source_url.as_str(), s))
        .collect();
    let mut updated: Vec<SourceUpdateItem> = Vec::new();
    let mut added: Vec<SourceUpdateItem> = Vec::new();
    let mut local_newer: Vec<SourceUpdateItem> = Vec::new();
    let mut same = 0usize;
    for src in incoming {
        match map.get(src.book_source_url.as_str()) {
            None => added.push(SourceUpdateItem {
                book_source_url: src.book_source_url.clone(),
                book_source_name: src.book_source_name.clone(),
                local_update_time: 0,
                incoming_update_time: src.last_update_time,
            }),
            Some(old) => {
                let item = SourceUpdateItem {
                    book_source_url: src.book_source_url.clone(),
                    book_source_name: if src.book_source_name.is_empty() {
                        old.book_source_name.clone()
                    } else {
                        src.book_source_name.clone()
                    },
                    local_update_time: old.last_update_time,
                    incoming_update_time: src.last_update_time,
                };
                if src.last_update_time > old.last_update_time {
                    updated.push(item);
                } else if src.last_update_time < old.last_update_time {
                    local_newer.push(item);
                } else {
                    same += 1;
                }
            }
        }
    }
    json!({
        "updated": updated,
        "new": added,
        "local_newer": local_newer,
        "same": same,
        "has_updates": !updated.is_empty() || !added.is_empty(),
    })
}

/// 批量比对书源更新时间：请求体为 Legado 导出数组，或 `{ "sources": [...] }`。
pub async fn check_updates(
    State(s): State<AppState>,
    _u: AuthUser,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    let raw = match body {
        Value::Object(mut o) => o.remove("sources").unwrap_or(Value::Array(Vec::new())),
        other => other,
    };
    let items = match raw {
        Value::Array(a) => a,
        Value::Null => vec![],
        one => vec![one],
    };
    let mut incoming: Vec<BookSource> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        match normalize_source(item) {
            Ok(src) => incoming.push(src),
            Err(e) => errors.push(format!("第 {} 条: {e}", i + 1)),
        }
    }
    let local = load_sources(&s.shared).await?;
    let mut out = diff_sources(&local, &incoming);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert("checked".into(), json!(incoming.len()));
        obj.insert("errors".into(), json!(errors));
    }
    Ok(Json(out))
}

pub(super) fn split_ids(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

pub(super) fn truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}
