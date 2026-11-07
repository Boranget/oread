use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::time::Instant;

use crate::auth::{AuthUser, UserDb};
use crate::entities::ReplaceRule;
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/replace-rules", get(list).post(add))
        .route("/api/replace-rules/export", get(export_rules))
        .route("/api/replace-rules/import", post(import_rules))
        .route("/api/replace-rules/preview", post(preview_rules))
        .route("/api/replace-rules/delete", delete(remove))
        .route("/api/settings", get(get_settings).post(set_settings))
}

#[derive(Deserialize)]
pub struct IdQ {
    id: i64,
}

/// 用户设置。字段名与前端 `Settings` 页保持一致；新增字段均为可选，旧前端不传时保持原值。
#[derive(Deserialize, Default)]
pub struct SettingsQ {
    use_replace_rule: Option<bool>,
    image_style: Option<i64>,
    theme: Option<String>,
    font_size: Option<i64>,
    line_height: Option<f64>,
    cache_limit_mb: Option<i64>,
    auto_refresh: Option<bool>,
    read_mode: Option<String>,
    /// 阅读页宽（CSS px）
    page_width: Option<i64>,
    /// 字体族枚举（跨设备同步）
    font_family: Option<String>,
    /// 内置正文净化规则集开关（默认关闭）
    builtin_clean: Option<bool>,
}

/// 设置默认值表（GET 时缺省返回，前端可直接绑定）
const SETTINGS_DEFAULTS: &[(&str, &str)] = &[
    ("use_replace_rule", "1"),
    ("image_style", "0"),
    ("theme", "light"),
    ("font_size", "18"),
    ("line_height", "1.75"),
    ("cache_limit_mb", "50"),
    ("auto_refresh", "1"),
    ("read_mode", "scroll"),
    ("page_width", "720"),
    ("font_family", "serif"),
    ("builtin_clean", "0"),
];

#[path = "replace_rules.rs"]
mod rules;
pub use rules::*;
#[path = "replace_import.rs"]
mod import;
pub use import::*;
#[path = "replace_preview.rs"]
mod preview;
pub use preview::*;
#[path = "replace_settings.rs"]
mod settings;
pub use settings::*;
#[path = "replace_pipeline.rs"]
mod pipeline;
pub use pipeline::*;

#[cfg(test)]
#[path = "replace_tests.rs"]
mod tests;
