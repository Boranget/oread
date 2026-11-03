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
        AppError::BadRequest(format!("json: {e}"))
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::BadRequest(format!("internal: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::MissingBookUrl => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "missing_book_url",
                        "message": "缺少书籍地址，请从书架重新打开",
                    })),
                )
                    .into_response();
            }
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            AppError::Timeout(m) => (StatusCode::GATEWAY_TIMEOUT, m),
            AppError::Sqlx(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}")),
            AppError::Bcrypt(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("bcrypt: {e}")),
            AppError::Jwt(e) => (StatusCode::UNAUTHORIZED, format!("jwt: {e}")),
            AppError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("io: {e}")),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
