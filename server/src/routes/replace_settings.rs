//! 替换规则路由的阅读设置（move-only 从 replace.rs 拆出）。

use super::*;

/// 一次性归一化标记：每个用户桶只执行一次（记录存在即视为已迁移）。
pub(crate) const DEFAULTS_MIGRATED_KEY: &str = "defaults_migrated_v1";

/// 把“恰好等于旧默认 1.8”的行距归一化为新默认 1.75。
///
/// 只匹配精确的 `1.8`：1.7 / 1.75 / 1.9 等用户主动选择的值一律不动。
/// 执行后写入 `defaults_migrated_v1` 标记，重复调用返回 0，避免把用户后来
/// 主动选回的 1.8 再次改掉。返回本次实际更新的行数。
pub(crate) async fn migrate_legacy_line_height_once(db: &SqlitePool) -> AppResult<u64> {
    let marked: Option<String> = sqlx::query_scalar("SELECT value FROM user_settings WHERE key=?1")
        .bind(DEFAULTS_MIGRATED_KEY)
        .fetch_optional(db)
        .await?;
    if marked.is_some() {
        return Ok(0);
    }
    let affected = sqlx::query(
        "UPDATE user_settings SET value='1.75' WHERE key='line_height' AND value='1.8'",
    )
    .execute(db)
    .await?
    .rows_affected();
    put_setting(db, DEFAULTS_MIGRATED_KEY, "1").await?;
    Ok(affected)
}

pub async fn get_settings(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
) -> AppResult<Json<serde_json::Value>> {
    migrate_legacy_line_height_once(&db.0).await?;
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT key, value FROM user_settings")
            .fetch_all(&db.0)
            .await?;
    let mut map: std::collections::HashMap<String, String> = rows
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or_default()))
        .collect();
    for (k, v) in SETTINGS_DEFAULTS {
        map.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    let get = |k: &str| map.get(k).cloned().unwrap_or_default();
    let b = |k: &str| get(k) == "1" || get(k) == "true";
    Ok(Json(json!({
        "use_replace_rule": b("use_replace_rule"),
        "image_style": get("image_style").parse::<i64>().unwrap_or(0),
        "theme": get("theme"),
        "font_size": get("font_size").parse::<i64>().unwrap_or(18),
        "line_height": get("line_height").parse::<f64>().unwrap_or(1.75),
        "cache_limit_mb": get("cache_limit_mb").parse::<i64>().unwrap_or(50),
        "auto_refresh": b("auto_refresh"),
        "read_mode": get("read_mode"),
        "page_width": get("page_width").parse::<i64>().unwrap_or(720),
        "font_family": get("font_family"),
        "builtin_clean": b("builtin_clean"),
    })))
}

pub(crate) async fn put_setting(db: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO user_settings (key,value) VALUES (?1,?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// 服务端校验：枚举白名单 + 数值范围，非法值直接 400（验收 P1）。
/// 范围与前端 `Settings` 页控件保持一致（theme 四档、字号 12–30、行距 1.2–2.6 等）。
pub(crate) fn validate_settings(q: &SettingsQ) -> AppResult<()> {
    if let Some(v) = &q.theme {
        if !matches!(v.as_str(), "light" | "dark" | "sepia" | "eink") {
            return Err(AppError::BadRequest(
                "theme 必须是 light/dark/sepia/eink".into(),
            ));
        }
    }
    if let Some(v) = q.font_size {
        if !(12..=30).contains(&v) {
            return Err(AppError::BadRequest("font_size 必须在 12–30 之间".into()));
        }
    }
    if let Some(v) = q.line_height {
        if !v.is_finite() || !(1.2..=2.6).contains(&v) {
            return Err(AppError::BadRequest(
                "line_height 必须在 1.2–2.6 之间".into(),
            ));
        }
    }
    if let Some(v) = q.cache_limit_mb {
        if !(10..=1000).contains(&v) {
            return Err(AppError::BadRequest(
                "cache_limit_mb 必须在 10–1000 之间".into(),
            ));
        }
    }
    if let Some(v) = q.image_style {
        if !(0..=3).contains(&v) {
            return Err(AppError::BadRequest("image_style 必须是 0–3".into()));
        }
    }
    if let Some(v) = &q.read_mode {
        if !matches!(v.as_str(), "scroll" | "paged" | "fade") {
            return Err(AppError::BadRequest(
                "read_mode 必须是 scroll/paged/fade".into(),
            ));
        }
    }
    if let Some(v) = q.page_width {
        if !(320..=2000).contains(&v) {
            return Err(AppError::BadRequest(
                "page_width 必须在 320–2000 之间".into(),
            ));
        }
    }
    if let Some(v) = &q.font_family {
        if !matches!(
            v.as_str(),
            "serif"
                | "sans"
                | "sans-serif"
                | "kai"
                | "kaiti"
                | "song"
                | "songti"
                | "hei"
                | "heiti"
                | "mono"
                | "system"
        ) {
            return Err(AppError::BadRequest(
                "font_family 必须是 serif/sans/kai/song/hei/mono/system 等枚举值".into(),
            ));
        }
    }
    Ok(())
}

pub async fn set_settings(
    State(_s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Json(q): Json<SettingsQ>,
) -> AppResult<Json<serde_json::Value>> {
    validate_settings(&q)?;
    if let Some(v) = q.use_replace_rule {
        put_setting(&db.0, "use_replace_rule", if v { "1" } else { "0" }).await?;
    }
    if let Some(v) = q.image_style {
        put_setting(&db.0, "image_style", &v.to_string()).await?;
    }
    if let Some(v) = &q.theme {
        put_setting(&db.0, "theme", v).await?;
    }
    if let Some(v) = q.font_size {
        put_setting(&db.0, "font_size", &v.to_string()).await?;
    }
    if let Some(v) = q.line_height {
        put_setting(&db.0, "line_height", &v.to_string()).await?;
    }
    if let Some(v) = q.cache_limit_mb {
        put_setting(&db.0, "cache_limit_mb", &v.to_string()).await?;
    }
    if let Some(v) = q.auto_refresh {
        put_setting(&db.0, "auto_refresh", if v { "1" } else { "0" }).await?;
    }
    if let Some(v) = &q.read_mode {
        put_setting(&db.0, "read_mode", v).await?;
    }
    if let Some(v) = q.page_width {
        put_setting(&db.0, "page_width", &v.to_string()).await?;
    }
    if let Some(v) = &q.font_family {
        put_setting(&db.0, "font_family", v).await?;
    }
    if let Some(v) = q.builtin_clean {
        put_setting(&db.0, "builtin_clean", if v { "1" } else { "0" }).await?;
    }
    Ok(Json(json!({"ok": true})))
}
