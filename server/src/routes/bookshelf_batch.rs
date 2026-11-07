//! 书架路由的分组设置与批量操作（move-only 从 bookshelf.rs 拆出）。

use super::*;

/// 设置书籍分组（单本或批量）；`group` 为空表示移出分组。
pub async fn set_book_group(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<SetGroupQ>,
) -> AppResult<Json<serde_json::Value>> {
    let mut ids: Vec<String> = q.book_ids.unwrap_or_default();
    if let Some(id) = q.book_id {
        ids.push(id);
    }
    if ids.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "book_id or book_ids required".into(),
        ));
    }
    let group = q.group.filter(|g| !g.trim().is_empty());
    for id in &ids {
        sqlx::query("UPDATE bookshelf SET \"group\"=?2 WHERE book_id=?1")
            .bind(id)
            .bind(&group)
            .execute(&db.0)
            .await?;
        sqlx::query("UPDATE book_meta SET \"group\"=?2 WHERE book_url=?1")
            .bind(id)
            .bind(&group)
            .execute(&db.0)
            .await?;
    }
    Ok(Json(json!({"ok": true, "updated": ids.len()})))
}

#[derive(Deserialize)]
pub struct BatchQ {
    /// `set_group`（含 add_group）/ `remove_group` / `remove`
    action: String,
    #[serde(default)]
    book_ids: Vec<String>,
    /// set_group 时的目标分组；null/空串表示移出分组
    group: Option<String>,
    /// set_update 时是否启用目录更新
    enabled: Option<bool>,
}

pub(crate) const SHELF_BATCH_MAX: usize = 500;

#[derive(Default)]
pub(crate) struct BatchItemResult {
    pub book_id: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// 逐项执行书架批量操作：每项独立事务，失败只影响该项（不做全有或全无）。
pub(crate) async fn apply_shelf_batch(
    pool: &SqlitePool,
    action: &str,
    book_ids: &[String],
    group: Option<&str>,
    enabled: Option<bool>,
) -> Vec<BatchItemResult> {
    let mut out = Vec::with_capacity(book_ids.len());
    for id in book_ids {
        let mut res = BatchItemResult {
            book_id: id.clone(),
            ok: false,
            error: None,
        };
        let mut tx = match pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                res.error = Some(format!("打开事务失败：{e}"));
                out.push(res);
                continue;
            }
        };
        let result = match action {
            "remove" => sqlx::query("DELETE FROM bookshelf WHERE book_id = ?1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected()),
            "set_update" => {
                sqlx::query("UPDATE bookshelf SET enable_update = ?2 WHERE book_id = ?1")
                    .bind(id)
                    .bind(if enabled.unwrap_or(true) { 1i64 } else { 0i64 })
                    .execute(&mut *tx)
                    .await
                    .map(|r| r.rows_affected())
            }
            _ => sqlx::query("UPDATE bookshelf SET \"group\" = ?2 WHERE book_id = ?1")
                .bind(id)
                .bind(group)
                .execute(&mut *tx)
                .await
                .map(|r| r.rows_affected()),
        };
        match result {
            // 删除是幂等的：书已不在书架也视为成功（重复批量删除不会报失败）
            Ok(0) if action == "remove" => match tx.commit().await {
                Ok(_) => res.ok = true,
                Err(e) => res.error = Some(format!("提交失败：{e}")),
            },
            Ok(0) => {
                res.error = Some("书籍不在书架".into());
                let _ = tx.rollback().await;
            }
            Ok(_) => {
                if action != "remove" && action != "set_update" {
                    if let Err(e) =
                        sqlx::query("UPDATE book_meta SET \"group\" = ?2 WHERE book_url = ?1")
                            .bind(id)
                            .bind(group)
                            .execute(&mut *tx)
                            .await
                    {
                        res.error = Some(format!("更新 book_meta 失败：{e}"));
                        let _ = tx.rollback().await;
                        out.push(res);
                        continue;
                    }
                }
                match tx.commit().await {
                    Ok(_) => res.ok = true,
                    Err(e) => res.error = Some(format!("提交失败：{e}")),
                }
            }
            Err(e) => {
                res.error = Some(format!("写入失败：{e}"));
                let _ = tx.rollback().await;
            }
        }
        out.push(res);
    }
    out
}

/// 批量移入/移出分组、批量移除；返回逐项成功/失败，不做全有或全无。
pub async fn batch(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<BatchQ>,
) -> AppResult<Json<serde_json::Value>> {
    let action = q.action.trim().to_ascii_lowercase();
    let action = match action.as_str() {
        "set_group" | "add_group" => "set_group",
        "remove_group" => "remove_group",
        "remove" => "remove",
        "set_update" | "enable_update" | "set_refresh" | "enable_refresh" => "set_update",
        other => {
            return Err(AppError::BadRequest(format!(
                "不支持的 action：{other}（可用 set_group/remove_group/remove/set_update）"
            )))
        }
    };
    if action == "set_update" && q.enabled.is_none() {
        return Err(AppError::BadRequest(
            "set_update 需要 enabled=true/false".into(),
        ));
    }
    let mut ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in q.book_ids {
        let id = raw.trim().to_string();
        if !id.is_empty() && seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(AppError::BadRequest("book_ids 不能为空".into()));
    }
    if ids.len() > SHELF_BATCH_MAX {
        return Err(AppError::BadRequest(format!(
            "单次最多 {SHELF_BATCH_MAX} 本，请分批调用"
        )));
    }
    let group = if action == "remove_group" {
        None
    } else {
        q.group.as_deref().map(str::trim).filter(|g| !g.is_empty())
    };
    let results = apply_shelf_batch(&db.0, action, &ids, group, q.enabled).await;
    let updated = results.iter().filter(|r| r.ok).count();
    let failed = results.len() - updated;
    Ok(Json(json!({
        "ok": true,
        "action": action,
        "group": group,
        "enabled": q.enabled,
        "updated": updated,
        "failed": failed,
        "results": results.into_iter().map(|r| json!({
            "book_id": r.book_id,
            "ok": r.ok,
            "error": r.error,
        })).collect::<Vec<_>>(),
    })))
}
