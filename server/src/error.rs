use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// 统一错误类型。实现 IntoResponse，可直接在 handler 中 `?` 返回。
#[derive(Debug)]
pub enum AppError {
    Sqlx(sqlx::Error),
    Bcrypt(bcrypt::BcryptError),
    Jwt(jsonwebtoken::errors::Error),
    Io(std::io::Error),
    Unauthorized,
    Forbidden,
    BadRequest(String),
    /// 前置条件不满足（如目录尚未缓存），映射 409 供调用方区分“参数错误”
    Conflict(String),
    /// 服务端内部错误，携带排障用的可读说明（映射 500）
    Internal(String),
    NotFound(String),
    /// 服务端超时（如书内搜索超过预算），映射 504 便于调用方区分“太慢”与“参数错误”
    Timeout(String),
    /// 正文请求既没带 book_url，也无法从章节缓存反查出所属书籍
    MissingBookUrl,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for AppError {}

impl AppError {
    /// 5xx 的内部细节：只写日志，绝不进入响应。
    fn detail(&self) -> String {
        match self {
            AppError::Sqlx(e) => e.to_string(),
            AppError::Bcrypt(e) => e.to_string(),
            AppError::Jwt(e) => e.to_string(),
            AppError::Io(e) => e.to_string(),
            AppError::Internal(m) => m.clone(),
            other => other.to_string(),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Sqlx(e)
    }
}
impl From<bcrypt::BcryptError> for AppError {
    fn from(e: bcrypt::BcryptError) -> Self {
        AppError::Bcrypt(e)
    }
}
impl From<jsonwebtoken::errors::Error> for AppError {
    fn from(e: jsonwebtoken::errors::Error) -> Self {
        AppError::Jwt(e)
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e)
    }
}

impl From<sqlx::migrate::MigrateError> for AppError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        AppError::Sqlx(sqlx::Error::Migrate(Box::new(e)))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        // 反序列化细节可能暴露字段/类型信息，只进日志；对外统一为 400 通用文案。
        tracing::warn!(error = %e, "serde_json parse failed");
        AppError::BadRequest("请求体格式不合法".into())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        // 内部错误一律映射 500，完整信息只进日志。
        AppError::Internal(e.to_string())
    }
}

/// 短请求 id：进程内自增 + 毫秒时间戳，足够把响应与日志对上，且不泄漏内部结构。
fn request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{ms:x}-{seq:x}")
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let rid = request_id();
        // 5xx：完整错误只进日志，响应只给通用文案 + 请求 id。
        if matches!(
            &self,
            AppError::Sqlx(_)
                | AppError::Bcrypt(_)
                | AppError::Io(_)
                | AppError::Internal(_)
                | AppError::Jwt(_)
        ) {
            let (status, code) = match self {
                AppError::Jwt(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            };
            tracing::error!(request_id = %rid, error = %self.detail(), "request failed");
            return (
                status,
                Json(json!({
                    "error": code,
                    "message": if status == StatusCode::UNAUTHORIZED {
                        "登录状态已失效，请重新登录"
                    } else {
                        "服务器内部错误，请稍后重试"
                    },
                    "request_id": rid,
                })),
            )
                .into_response();
        }

        let (status, code, message) = match self {
            AppError::MissingBookUrl => (
                StatusCode::BAD_REQUEST,
                "missing_book_url",
                "缺少书籍地址，请从书架重新打开".to_string(),
            ),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "未登录或登录状态已失效".to_string(),
            ),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "需要管理员权限".to_string(),
            ),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            AppError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            AppError::Timeout(m) => (StatusCode::GATEWAY_TIMEOUT, "timeout", m),
            // 上面的分支已处理，这里只为让 match 完整。
            AppError::Sqlx(_) | AppError::Bcrypt(_) | AppError::Io(_) | AppError::Internal(_) => {
                unreachable!("5xx 已在前面处理")
            }
            AppError::Jwt(_) => unreachable!("jwt 已在前面处理"),
        };
        (
            status,
            Json(json!({
                "error": code,
                "message": message,
                "request_id": rid,
            })),
        )
            .into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_string(res: Response) -> String {
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// 500 只返回通用文案 + request_id，内部细节（SQL/路径/密钥）不得出现。
    #[tokio::test]
    async fn internal_error_is_redacted() {
        let res = AppError::Internal("db=/secret/path password=hunter2".into()).into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_string(res).await;
        assert!(!body.contains("secret"), "响应不得包含内部细节: {body}");
        assert!(!body.contains("hunter2"), "响应不得包含内部细节: {body}");
        assert!(body.contains("internal_error"), "应给出稳定错误码: {body}");
        assert!(body.contains("request_id"), "应带请求 id: {body}");
    }

    /// 400 保留面向用户的文案，并同样带上 request_id。
    #[tokio::test]
    async fn bad_request_keeps_message_with_request_id() {
        let res = AppError::BadRequest("line_height 必须在 1.2–2.6".into()).into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_string(res).await;
        assert!(body.contains("line_height"), "400 文案应保留: {body}");
        assert!(body.contains("request_id"), "应带请求 id: {body}");
    }

    /// anyhow 不再被当成 400 原样透出，而是脱敏成 500。
    #[tokio::test]
    async fn anyhow_maps_to_redacted_500() {
        let res = AppError::from(anyhow::anyhow!("sqlx: no such table users")).into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_string(res).await;
        assert!(!body.contains("no such table"), "不得透出内部错误: {body}");
    }
}
