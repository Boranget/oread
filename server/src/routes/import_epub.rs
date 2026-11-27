//! 本地 EPUB 导入：管理员上传 DRM-free EPUB，保留原结构搬进馆藏。
//!
//! 两步：
//!
//! 1. `POST /api/library/import/epub`（multipart，字段名 `file`）流式落盘、
//!    解压校验，返回 `token`、书目摘要与三层查重结果；
//! 2. `POST /api/library/import/commit`（JSON）按用户选择新增/覆盖/取消。
//!
//! 大文件不放进内存：multipart 分块直接写进 `.import/<token>/source.epub`，
//! 同时算 SHA-256；解压与检查在 `spawn_blocking` 里做。

use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, Multipart, State},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::auth::{ensure_admin, AuthUser};
use crate::error::{AppError, AppResult};
use crate::library_store::import::{self, CommitMode, ImportError, StageInfo};
use crate::library_store::import_cbz;
use crate::routes::AppState;

/// 上传包上限；解压后的体积另有校验。
const MAX_UPLOAD_BYTES: u64 = import::MAX_UPLOAD_BYTES;
/// 残留暂存目录保留时间（小时）。
const STAGE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/import/epub", post(inspect_epub))
        .route("/api/library/import/cbz", post(inspect_cbz))
        .route("/api/library/import/commit", post(commit))
        .layer(DefaultBodyLimit::max(
            MAX_UPLOAD_BYTES.min(usize::MAX as u64) as usize,
        ))
}

fn import_error(e: ImportError) -> AppError {
    match e {
        ImportError::BadRequest(m) | ImportError::TooLarge(m) => AppError::BadRequest(m),
        ImportError::Io(e) => AppError::Io(e),
        ImportError::Internal(m) => AppError::Internal(m),
    }
}

#[derive(Serialize)]
struct InspectResp {
    ok: bool,
    token: String,
    book: StageInfo,
    duplicates: Vec<import::DuplicateHit>,
}

async fn inspect_epub(
    State(s): State<AppState>,
    u: AuthUser,
    multipart: Multipart,
) -> AppResult<Json<InspectResp>> {
    inspect_upload(s, u, multipart, false).await
}

async fn inspect_cbz(
    State(s): State<AppState>,
    u: AuthUser,
    multipart: Multipart,
) -> AppResult<Json<InspectResp>> {
    inspect_upload(s, u, multipart, true).await
}

async fn inspect_upload(
    s: AppState,
    u: AuthUser,
    mut multipart: Multipart,
    cbz: bool,
) -> AppResult<Json<InspectResp>> {
    ensure_admin(&u)?;
    let label = if cbz { "CBZ" } else { "EPUB" };
    let store = s.store();
    // 先顺手清理上次进程中断留下的暂存目录；失败不影响本次导入。
    import::prune_stages(&store, STAGE_TTL).await;

    let token = import::new_token().map_err(import_error)?;
    let stage_dir = store.import_stage_dir(&token);
    tokio::fs::create_dir_all(&stage_dir).await?;
    let source = stage_dir.join(if cbz { "source.cbz" } else { "source.epub" });

    let mut field = loop {
        let item = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("读取上传数据失败：{e}")))?;
        let Some(field) = item else {
            cleanup(&stage_dir).await;
            return Err(AppError::BadRequest(format!("没有收到 {label} 文件")));
        };
        if field.name() == Some("file") {
            break field;
        }
        // 忽略其他字段（例如表单里的注释）。
    };

    let file_name = field
        .file_name()
        .unwrap_or(if cbz { "import.cbz" } else { "import.epub" })
        .to_string();
    let mut out = tokio::fs::File::create(&source).await?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut head = Vec::with_capacity(4);

    loop {
        let chunk = match field.chunk().await {
            Ok(v) => v,
            Err(e) => {
                cleanup(&stage_dir).await;
                return Err(AppError::BadRequest(format!("上传中断：{e}")));
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > MAX_UPLOAD_BYTES {
            cleanup(&stage_dir).await;
            return Err(AppError::BadRequest(format!(
                "{label} 超过 {} MiB 上限",
                MAX_UPLOAD_BYTES / 1024 / 1024
            )));
        }
        if head.len() < 4 {
            let take = (4 - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
        }
        hasher.update(&chunk);
        if let Err(e) = out.write_all(&chunk).await {
            cleanup(&stage_dir).await;
            return Err(AppError::Io(e));
        }
    }
    out.flush().await?;
    drop(out);

    if written == 0 {
        cleanup(&stage_dir).await;
        return Err(AppError::BadRequest(format!("上传的 {label} 是空文件")));
    }
    if head != b"PK\x03\x04" && head != b"PK\x05\x06" {
        cleanup(&stage_dir).await;
        return Err(AppError::BadRequest(format!(
            "不是有效的 {label}（ZIP）文件"
        )));
    }

    let sha256 = format!("{:x}", hasher.finalize());
    let staged = if cbz {
        import_cbz::stage(&store, &token, source, &file_name, sha256).await
    } else {
        import::stage(&store, &token, source, &file_name, sha256).await
    };
    let info = match staged {
        Ok(v) => v,
        Err(e) => {
            cleanup(&stage_dir).await;
            return Err(import_error(e));
        }
    };
    let duplicates = import::find_duplicates(&s.library, &info)
        .await
        .map_err(import_error)?;
    Ok(Json(InspectResp {
        ok: true,
        token,
        book: info,
        duplicates,
    }))
}

async fn cleanup(dir: &std::path::Path) {
    let _ = tokio::fs::remove_dir_all(dir).await;
}

#[derive(Deserialize)]
struct CommitReq {
    token: String,
    mode: String,
    #[serde(default)]
    target_book_url: Option<String>,
}

async fn commit(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<CommitReq>,
) -> AppResult<Json<Value>> {
    ensure_admin(&u)?;
    let mode = CommitMode::parse(&req.mode).map_err(import_error)?;
    let token = req.token.trim();
    // 取消只清理暂存，即使暂存已过期也应成功；其余模式先看 sidecar 判断书类型。
    let result = if mode == CommitMode::Cancel {
        import::commit(&s.store(), &s.library, token, mode, None).await
    } else {
        let info = import::read_stage_info(&s.store(), token)
            .await
            .map_err(import_error)?;
        if info.kind == "comic" {
            import_cbz::commit(
                &s.store(),
                &s.library,
                token,
                mode,
                req.target_book_url.as_deref(),
            )
            .await
        } else {
            import::commit(
                &s.store(),
                &s.library,
                token,
                mode,
                req.target_book_url.as_deref(),
            )
            .await
        }
    }
    .map_err(import_error)?;

    let check = result.check.map(|c| {
        json!({
            "revision": c.revision,
            "checked_files": c.checked_files,
            "issues": c.issues.into_iter().map(|i| json!({
                "severity": i.severity,
                "code": i.code,
                "path": i.path,
                "message": i.message,
            })).collect::<Vec<_>>(),
        })
    });
    Ok(Json(json!({
        "ok": true,
        "book_url": result.book_url,
        "created": result.created,
        "overwritten": result.overwritten,
        "cancelled": result.cancelled,
        "check": check,
    })))
}
