use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use axum::http::request::Parts;
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

/// 当前登录用户（由 JWT 中间件注入到请求扩展中）
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: i64,
    pub username: String,
    pub role: String, // "user" | "admin"
}

/// 让 handler 能直接以 `user: AuthUser` 提取当前用户
#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<AuthUser>().cloned().ok_or(AppError::Unauthorized)
    }
}

/// 当前用户的独立数据库连接池（物理分桶），由鉴权中间件注入请求扩展。
/// handler 直接以 `db: UserDb` 提取即可访问该用户桶（书架/进度/缓存等）。
#[derive(Clone, Debug)]
pub struct UserDb(pub SqlitePool);

#[axum::async_trait]
impl<S> FromRequestParts<S> for UserDb
where
    S: Send + Sync,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<UserDb>().cloned().ok_or(AppError::Unauthorized)
    }
}

/// 取得（缓存）某用户的独立连接池
async fn get_user_pool(state: &AppState, uid: i64) -> AppResult<SqlitePool> {
    if let Some(p) = state.user_pools.lock().unwrap().get(&uid) {
        return Ok(p.clone());
    }
    let p = db::user_pool(&state.cfg, uid).await?;
    state.user_pools.lock().unwrap().insert(uid, p.clone());
    Ok(p)
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: i64,
    username: String,
    role: String,
    exp: usize,
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub username: String,
    pub password: String,
}
#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}
#[derive(Serialize)]
pub struct TokenRes {
    pub token: String,
}

fn create_token(id: i64, username: &str, role: &str, secret: &str) -> AppResult<String> {
    let exp = Utc::now().timestamp() as usize + 60 * 60 * 24 * 7; // 7 天
    let claims = Claims {
        sub: id,
        username: username.to_string(),
        role: role.to_string(),
        exp,
    };
    Ok(encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(AppError::Jwt)?)
}

fn verify_token(token: &str, secret: &str) -> AppResult<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(AppError::Jwt)?;
    Ok(data.claims)
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterReq>,
) -> AppResult<Json<TokenRes>> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
        .bind(&req.username)
        .fetch_one(&state.shared)
        .await?;
    if exists > 0 {
        return Err(AppError::BadRequest("username exists".into()));
    }
    let pw = hash(&req.password, DEFAULT_COST).map_err(AppError::Bcrypt)?;
    let id = sqlx::query("INSERT INTO users (username, password_hash, role) VALUES (?, ?, 'user')")
        .bind(&req.username)
        .bind(&pw)
        .execute(&state.shared)
        .await?
        .last_insert_rowid();
    let token = create_token(id, &req.username, "user", &state.cfg.jwt_secret)?;
    Ok(Json(TokenRes { token }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> AppResult<Json<TokenRes>> {
    let row: Option<(i64, String, String)> =
        sqlx::query_as("SELECT id, password_hash, role FROM users WHERE username = ?")
            .bind(&req.username)
            .fetch_optional(&state.shared)
            .await?;
    let (id, pw_hash, role) = row.ok_or(AppError::Unauthorized)?;
    if !verify(&req.password, &pw_hash).map_err(AppError::Bcrypt)? {
        return Err(AppError::Unauthorized);
    }
    let token = create_token(id, &req.username, &role, &state.cfg.jwt_secret)?;
    Ok(Json(TokenRes { token }))
}

/// JWT 鉴权中间件：公开路径直接放行，其余校验 Bearer 并把 AuthUser 注入扩展
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> AppResult<Response> {
    // 公开路径（健康检查 + 注册/登录）无需鉴权
    let path = req.uri().path().to_string();
    if path == "/api/health" || path == "/api/auth/login" || path == "/api/auth/register" {
        return Ok(next.run(req).await);
    }
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let token = bearer.ok_or(AppError::Unauthorized)?;
    let claims = verify_token(token, &state.cfg.jwt_secret)?;
    let pool = get_user_pool(&state, claims.sub).await?;
    req.extensions_mut().insert(UserDb(pool));
    req.extensions_mut().insert(AuthUser {
        id: claims.sub,
        username: claims.username,
        role: claims.role,
    });
    Ok(next.run(req).await)
}

pub fn ensure_admin(user: &AuthUser) -> AppResult<()> {
    if user.role != "admin" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// 首次启动引导：若 users 表为空，则创建管理员账号。
/// 凭据来自环境变量 ADMIN_USER / ADMIN_PASS，缺省 admin / __INITIAL_ADMIN_PASS__（仅用于首次，请尽快修改）。
pub async fn bootstrap_admin(pool: &SqlitePool) -> AppResult<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let user = std::env::var("ADMIN_USER").unwrap_or_else(|_| "admin".into());
    let pass = std::env::var("ADMIN_PASS").unwrap_or_else(|_| "__INITIAL_ADMIN_PASS__".into());
    let pw = hash(&pass, DEFAULT_COST).map_err(AppError::Bcrypt)?;
    sqlx::query("INSERT INTO users (username, password_hash, role) VALUES (?, ?, 'admin')")
        .bind(&user)
        .bind(&pw)
        .execute(pool)
        .await?;
    tracing::warn!("bootstrap: created default admin '{user}' / '{pass}' — please change it");
    Ok(())
}
