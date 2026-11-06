use axum::http::request::Parts;
use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::routes::AppState;

/// 当前登录用户（由 JWT 中间件注入到请求扩展中）
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: i64,
    // 登录态保留用户名供审计/展示；当前路由暂未读取，但属于对外身份字段，保留。
    #[allow(dead_code)]
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
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or(AppError::Unauthorized)
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
        parts
            .extensions
            .get::<UserDb>()
            .cloned()
            .ok_or(AppError::Unauthorized)
    }
}

/// 取得（缓存）某用户的独立连接池，并按 `db_max_open_users` 做 LRU 关闭。
/// 内存预算：每个用户池 2 连接，同时最多 N 个池；淘汰时 `pool.close()` 释放文件句柄。
async fn get_user_pool(state: &AppState, uid: i64) -> AppResult<SqlitePool> {
    {
        let mut cache = state.user_pools.lock().unwrap();
        cache.tick += 1;
        let tick = cache.tick;
        if let Some((pool, last)) = cache.entries.get_mut(&uid) {
            *last = tick;
            return Ok(pool.clone());
        }
    }
    let pool = db::user_pool(&state.cfg, uid).await?;
    let mut evicted: Vec<SqlitePool> = Vec::new();
    {
        let mut cache = state.user_pools.lock().unwrap();
        cache.tick += 1;
        let tick = cache.tick;
        cache.entries.insert(uid, (pool.clone(), tick));
        while cache.entries.len() > state.cfg.db_max_open_users {
            // 找最久未使用的用户（tick 最小）
            let lru = cache
                .entries
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| *k);
            match lru {
                Some(k) if k != uid => {
                    if let Some((p, _)) = cache.entries.remove(&k) {
                        evicted.push(p);
                    }
                }
                _ => break,
            }
        }
    }
    for p in evicted {
        p.close().await;
    }
    Ok(pool)
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: i64,
    username: String,
    role: String,
    /// 令牌版本：改密/禁用时递增，使旧 JWT 立即失效（旧令牌缺省按 0 处理）
    #[serde(default)]
    ver: i64,
    exp: usize,
}

/// 账号实时状态：每个已鉴权请求都从共享库读取，禁用/改密立即生效。
struct AccountRow {
    username: String,
    role: String,
    disabled: bool,
    token_version: i64,
}

async fn lookup_account(pool: &SqlitePool, uid: i64) -> AppResult<Option<AccountRow>> {
    let row: Option<(String, String, i64, i64)> =
        sqlx::query_as("SELECT username, role, disabled, token_version FROM users WHERE id = ?")
            .bind(uid)
            .fetch_optional(pool)
            .await?;
    Ok(
        row.map(|(username, role, disabled, token_version)| AccountRow {
            username,
            role,
            disabled: disabled != 0,
            token_version,
        }),
    )
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

fn create_token(id: i64, username: &str, role: &str, ver: i64, secret: &str) -> AppResult<String> {
    create_token_days(id, username, role, ver, secret, 7)
}

/// 指定有效天数的 token（Kindle /k 端用长寿命 Cookie，30 天）
pub fn create_token_days(
    id: i64,
    username: &str,
    role: &str,
    ver: i64,
    secret: &str,
    days: usize,
) -> AppResult<String> {
    let exp = Utc::now().timestamp() as usize + 60 * 60 * 24 * days;
    let claims = Claims {
        sub: id,
        username: username.to_string(),
        role: role.to_string(),
        ver,
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(AppError::Jwt)
}

/// 从 Cookie 头里取 k_session（Kindle /k 端会话）
fn cookie_token(req: &Request) -> Option<String> {
    req.headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| {
            c.split(';')
                .map(str::trim)
                .find_map(|kv| kv.strip_prefix("k_session="))
                .map(|s| s.to_string())
        })
}

fn redirect_login() -> Response {
    let mut res = Response::new(axum::body::Body::empty());
    *res.status_mut() = StatusCode::SEE_OTHER;
    res.headers_mut().insert(
        axum::http::header::LOCATION,
        "/k/login".parse().expect("static location"),
    );
    res
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
    let username = req.username.trim().to_string();
    if !valid_username(&username) {
        return Err(AppError::BadRequest(
            "用户名需为 2–32 个字符且不含空白".into(),
        ));
    }
    if !valid_password(&req.password) {
        return Err(AppError::BadRequest("密码长度需在 6–128 个字符之间".into()));
    }
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
        .bind(&username)
        .fetch_one(&state.shared)
        .await?;
    if exists > 0 {
        return Err(AppError::BadRequest("username exists".into()));
    }
    let pw = hash(&req.password, DEFAULT_COST).map_err(AppError::Bcrypt)?;
    let id = sqlx::query("INSERT INTO users (username, password_hash, role) VALUES (?, ?, 'user')")
        .bind(&username)
        .bind(&pw)
        .execute(&state.shared)
        .await?
        .last_insert_rowid();
    let token = create_token(id, &username, "user", 0, &state.cfg.jwt_secret)?;
    Ok(Json(TokenRes { token }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> AppResult<Json<TokenRes>> {
    let row: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, password_hash, role, disabled, token_version FROM users WHERE username = ?",
    )
    .bind(&req.username)
    .fetch_optional(&state.shared)
    .await?;
    let (id, pw_hash, role, disabled, ver) = row.ok_or(AppError::Unauthorized)?;
    if disabled != 0 {
        return Err(AppError::Unauthorized);
    }
    if !verify(&req.password, &pw_hash).map_err(AppError::Bcrypt)? {
        return Err(AppError::Unauthorized);
    }
    // 记录最后登录时间（UTC），供管理员在设置页查看；失败不影响登录本身
    let _ = sqlx::query("UPDATE users SET last_login_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.shared)
        .await;
    let token = create_token(id, &req.username, &role, ver, &state.cfg.jwt_secret)?;
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
    if path == "/api/health"
        || path == "/healthz"
        || path == "/api/auth/login"
        || path == "/api/auth/register"
    {
        return Ok(next.run(req).await);
    }
    // Kindle 极简端 /k：表单登录页公开，其余用 HttpOnly Cookie 会话；
    // 校验失败不重定向 JSON 401，而是 303 回登录页（老浏览器友好）
    if path.starts_with("/k") {
        if path == "/k/login" {
            return Ok(next.run(req).await);
        }
        let claims = cookie_token(&req).and_then(|t| verify_token(&t, &state.cfg.jwt_secret).ok());
        let claims = match claims {
            Some(c) => c,
            None => return Ok(redirect_login()),
        };
        // 每次请求都查库：禁用/改密后旧 Cookie 立即失效
        let account = match lookup_account(&state.shared, claims.sub).await? {
            Some(a) if !a.disabled && a.token_version == claims.ver => a,
            _ => return Ok(redirect_login()),
        };
        let pool = get_user_pool(&state, claims.sub).await?;
        req.extensions_mut().insert(UserDb(pool));
        req.extensions_mut().insert(AuthUser {
            id: claims.sub,
            username: account.username,
            role: account.role,
        });
        return Ok(next.run(req).await);
    }
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string());
    // Kindle /k 页内的 <img> 由浏览器直接发起，带不上 Authorization 头，只会有 k_session Cookie
    // 自报探针与书图一样由浏览器直接发起，只带 k_session Cookie，带不上 Authorization
    let cookie_fallback =
        if bearer.is_none() && (path == "/api/book/image" || path == "/api/k/probe") {
            cookie_token(&req)
        } else {
            None
        };
    let token = bearer.or(cookie_fallback).ok_or(AppError::Unauthorized)?;
    let claims = verify_token(&token, &state.cfg.jwt_secret)?;
    // 状态实时校验：账号被禁用或令牌版本已递增（改密/禁用）→ 401
    let account = match lookup_account(&state.shared, claims.sub).await? {
        Some(a) if !a.disabled && a.token_version == claims.ver => a,
        _ => return Err(AppError::Unauthorized),
    };
    let pool = get_user_pool(&state, claims.sub).await?;
    req.extensions_mut().insert(UserDb(pool));
    req.extensions_mut().insert(AuthUser {
        id: claims.sub,
        username: account.username,
        role: account.role,
    });
    Ok(next.run(req).await)
}

pub fn ensure_admin(user: &AuthUser) -> AppResult<()> {
    if user.role != "admin" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub old_password: String,
    pub new_password: String,
}

pub(crate) fn valid_password(pw: &str) -> bool {
    let n = pw.chars().count();
    (6..=128).contains(&n)
}

/// 用户名规则：trim 后 2–32 字符，且不含任何空白（含全角空格）。
pub(crate) fn valid_username(name: &str) -> bool {
    let n = name.chars().count();
    (2..=32).contains(&n) && !name.chars().any(char::is_whitespace)
}

/// 用户自助改密：校验旧密码；成功后 token_version+1，所有旧 JWT 立即失效。
pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(q): Json<ChangePasswordReq>,
) -> AppResult<Json<serde_json::Value>> {
    if !valid_password(&q.new_password) {
        return Err(AppError::BadRequest(
            "新密码长度需在 6–128 个字符之间".into(),
        ));
    }
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT password_hash, token_version FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_optional(&state.shared)
            .await?;
    let (pw_hash, ver) = row.ok_or(AppError::Unauthorized)?;
    if !verify(&q.old_password, &pw_hash).map_err(AppError::Bcrypt)? {
        return Err(AppError::BadRequest("旧密码不正确".into()));
    }
    let new_hash = hash(&q.new_password, DEFAULT_COST).map_err(AppError::Bcrypt)?;
    sqlx::query(
        "UPDATE users SET password_hash = ?, token_version = token_version + 1, \
         password_changed_at = datetime('now') WHERE id = ?",
    )
    .bind(&new_hash)
    .bind(user.id)
    .execute(&state.shared)
    .await?;
    Ok(Json(json!({
        "ok": true,
        "relogin_required": true,
        "previous_token_version": ver,
    })))
}

#[derive(Deserialize)]
pub struct UserStatusReq {
    pub user_id: i64,
    pub disabled: bool,
}

/// 禁用保护：不能禁用自己；不能把最后一个启用中的管理员禁掉。
pub(crate) fn can_disable_user(
    target_id: i64,
    current_id: i64,
    target_role: &str,
    enabled_admins: i64,
) -> Result<(), &'static str> {
    if target_id == current_id {
        return Err("不能禁用当前登录的管理员账号");
    }
    if target_role == "admin" && enabled_admins <= 1 {
        return Err("不能禁用最后一个启用中的管理员");
    }
    Ok(())
}

pub async fn admin_list_users(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&user)?;
    let rows: Vec<(i64, String, String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, username, role, disabled, created_at, last_login_at FROM users ORDER BY id",
    )
    .fetch_all(&state.shared)
    .await?;
    Ok(Json(json!({
        "ok": true,
        "users": rows.into_iter().map(|(id, username, role, disabled, created_at, last_login_at)| json!({
            "id": id,
            "username": username,
            "role": role,
            "disabled": disabled != 0,
            "created_at": created_at,
            "last_login_at": last_login_at,
        })).collect::<Vec<_>>(),
    })))
}

/// 管理员禁用/启用账号。禁用时同时 token_version+1，旧 JWT 永久失效。
pub async fn admin_set_user_status(
    State(state): State<AppState>,
    user: AuthUser,
    Json(q): Json<UserStatusReq>,
) -> AppResult<Json<serde_json::Value>> {
    ensure_admin(&user)?;
    let target: Option<(String, i64)> =
        sqlx::query_as("SELECT role, disabled FROM users WHERE id = ?")
            .bind(q.user_id)
            .fetch_optional(&state.shared)
            .await?;
    let (role, disabled) = target.ok_or_else(|| AppError::BadRequest("用户不存在".into()))?;
    if q.disabled {
        let enabled_admins: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0")
                .fetch_one(&state.shared)
                .await?;
        if let Err(msg) = can_disable_user(q.user_id, user.id, &role, enabled_admins) {
            return Err(AppError::BadRequest(msg.into()));
        }
        if disabled == 0 {
            sqlx::query(
                "UPDATE users SET disabled = 1, token_version = token_version + 1 WHERE id = ?",
            )
            .bind(q.user_id)
            .execute(&state.shared)
            .await?;
        }
    } else {
        sqlx::query("UPDATE users SET disabled = 0 WHERE id = ?")
            .bind(q.user_id)
            .execute(&state.shared)
            .await?;
    }
    Ok(Json(json!({
        "ok": true,
        "user_id": q.user_id,
        "disabled": q.disabled,
    })))
}

// 单测模块之后仍有鉴权辅助函数（历史排布）；不重排生产代码，就地放行该风格 lint。
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;

    // 禁用保护：不能禁自己，也不能禁最后一个启用中的管理员
    #[test]
    fn admin_disable_guard() {
        assert!(can_disable_user(2, 1, "user", 5).is_ok());
        assert!(can_disable_user(1, 1, "admin", 5).is_err(), "不能禁用自己");
        assert!(
            can_disable_user(2, 1, "admin", 1).is_err(),
            "不能禁用最后一个管理员"
        );
        assert!(can_disable_user(2, 1, "admin", 2).is_ok());
    }

    // 新密码长度 6–128
    #[test]
    fn password_length_policy() {
        assert!(!valid_password("12345"));
        assert!(valid_password("123456"));
        assert!(valid_password(&"a".repeat(128)));
        assert!(!valid_password(&"a".repeat(129)));
    }

    // 用户名 trim 后 2–32 字符，且不含空白
    #[test]
    fn username_policy() {
        assert!(!valid_username(""), "空用户名应拒绝");
        assert!(!valid_username("a"), "1 个字符应拒绝");
        assert!(!valid_username(&"a".repeat(33)), "33 个字符应拒绝");
        assert!(valid_username(&"a".repeat(32)), "32 个字符应接受");
        assert!(valid_username("ab"));
        assert!(!valid_username("a b"), "中间空白应拒绝");
        assert!(!valid_username("中文 名"), "全角/半角空白应拒绝");
        // 前端传入前后空格时，register 先 trim 再校验
        assert!(valid_username("  ab  ".trim()));
    }

    /// 用内存库 + 最小 users 表构造 AppState，验证 register 的实际行为。
    async fn register_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL, \
             password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user', \
             disabled INTEGER NOT NULL DEFAULT 0, token_version INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        AppState {
            shared: pool,
            cfg: crate::config::Config::from_env(),
            user_pools: Default::default(),
        }
    }

    #[tokio::test]
    async fn register_rejects_invalid_input() {
        let state = State(register_state().await);
        for (username, password) in [
            ("", "123456"),
            ("a", "123456"),
            (&"a".repeat(33) as &str, "123456"),
            ("a b", "123456"),
            ("ok", ""),
            ("ok", "12345"),
        ] {
            let res = register(
                state.clone(),
                Json(RegisterReq {
                    username: username.to_string(),
                    password: password.to_string(),
                }),
            )
            .await;
            match res {
                Ok(_) => panic!("{username:?}/{password:?} 应被拒绝"),
                Err(e) => assert_eq!(e.into_response().status(), 400),
            }
        }
    }

    #[tokio::test]
    async fn register_trims_username() {
        let state = State(register_state().await);
        let res = register(
            state.clone(),
            Json(RegisterReq {
                username: "  alice  ".into(),
                password: "123456".into(),
            }),
        )
        .await
        .expect("合法注册应成功");
        assert!(!res.0.token.is_empty());
        let stored: String = sqlx::query_scalar("SELECT username FROM users WHERE id = 1")
            .fetch_one(&state.shared)
            .await
            .unwrap();
        assert_eq!(stored, "alice", "落库用户名必须已 trim");
    }

    // 生产首次启动：必须显式提供足够强的 ADMIN_PASS；开发允许缺省 __INITIAL_ADMIN_PASS__。
    #[test]
    fn production_bootstrap_requires_strong_password() {
        assert_eq!(resolve_bootstrap_pass(false, None).unwrap(), "__INITIAL_ADMIN_PASS__");
        assert!(resolve_bootstrap_pass(false, Some("x".into())).is_ok());
        assert!(resolve_bootstrap_pass(true, None).is_err());
        assert!(resolve_bootstrap_pass(true, Some("__INITIAL_ADMIN_PASS__".into())).is_err());
        assert!(resolve_bootstrap_pass(true, Some("short".into())).is_err());
        assert_eq!(
            resolve_bootstrap_pass(true, Some("correct-horse".into())).unwrap(),
            "correct-horse"
        );
    }
}

/// 生产环境管理员口令最小长度（字节）。
const MIN_ADMIN_PASS_LEN: usize = 8;

/// 解析首次启动口令：生产环境必须显式设置且足够强；开发环境允许缺省 __INITIAL_ADMIN_PASS__。
fn resolve_bootstrap_pass(production: bool, pass: Option<String>) -> AppResult<String> {
    match pass {
        Some(pass) if production && (pass.len() < MIN_ADMIN_PASS_LEN || pass == "__INITIAL_ADMIN_PASS__") => {
            Err(AppError::Internal(format!(
                "生产环境 ADMIN_PASS 至少 {MIN_ADMIN_PASS_LEN} 字节，且不能使用默认口令 __INITIAL_ADMIN_PASS__"
            )))
        }
        Some(pass) => Ok(pass),
        None if production => Err(AppError::Internal(format!(
            "生产环境首次启动必须设置 ADMIN_PASS（BORANLIB_ADMIN_PASS 或 LEGADO_ADMIN_PASS，至少 {MIN_ADMIN_PASS_LEN} 字节，且不能是 __INITIAL_ADMIN_PASS__）"
        ))),
        None => Ok("__INITIAL_ADMIN_PASS__".into()),
    }
}

/// 首次启动引导：若 users 表为空，则创建管理员账号。
/// 用户名来自 `ADMIN_USER`（缺省 admin）；口令来自 `ADMIN_PASS`。
/// 生产环境必须显式设置 `ADMIN_PASS`（至少 8 字节且不能是 __INITIAL_ADMIN_PASS__），否则拒绝创建。
pub async fn bootstrap_admin(pool: &SqlitePool, cfg: &crate::config::Config) -> AppResult<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let user = crate::config::env_prefixed("ADMIN_USER").unwrap_or_else(|| "admin".into());
    let pass = resolve_bootstrap_pass(cfg.production, crate::config::env_prefixed("ADMIN_PASS"))?;
    let pw = hash(&pass, DEFAULT_COST).map_err(AppError::Bcrypt)?;
    sqlx::query("INSERT INTO users (username, password_hash, role) VALUES (?, ?, 'admin')")
        .bind(&user)
        .bind(&pw)
        .execute(pool)
        .await?;
    if cfg.production {
        tracing::warn!(%user, "bootstrap: created admin from BORANLIB_ADMIN_PASS");
    } else {
        tracing::warn!(%user, "bootstrap: created default admin — please change the password");
    }
    Ok(())
}
