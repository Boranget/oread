//! sources 路由的体检语料与精选接口（move-only 从 sources.rs 拆出）。

use super::*;

#[derive(Deserialize, Default)]
pub struct BenchCorpusInput {
    pub title: Option<String>,
    pub expected_author: Option<String>,
    pub category: Option<String>,
    #[serde(default, alias = "type")]
    pub source_type: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl BenchCorpusInput {
    pub(super) fn into_corpus_input(
        self,
        enabled_default: bool,
    ) -> crate::source_bench::CorpusInput {
        crate::source_bench::CorpusInput {
            title: self.title.unwrap_or_default(),
            expected_author: self.expected_author,
            category: self.category.unwrap_or_default(),
            source_type: self.source_type.unwrap_or(crate::source_bench::TYPE_NOVEL),
            enabled: self.enabled.unwrap_or(enabled_default),
        }
    }
}

/// `GET /api/sources/bench/corpus`：内置 ∪ 用户语料（内置带 builtin 标志）。
pub async fn bench_corpus(State(s): State<AppState>, _u: AuthUser) -> AppResult<Json<Value>> {
    let corpus = crate::source_bench::load_corpus(&s.shared).await?;
    let builtin = corpus.iter().filter(|c| c.builtin).count();
    let user = corpus.len() - builtin;
    let novel = corpus
        .iter()
        .filter(|c| c.source_type == crate::source_bench::TYPE_NOVEL)
        .count();
    let comic = corpus
        .iter()
        .filter(|c| c.source_type == crate::source_bench::TYPE_COMIC)
        .count();
    Ok(Json(json!({
        "items": corpus,
        "total": corpus.len(),
        "builtin_count": builtin,
        "user_count": user,
        "novel_count": novel,
        "comic_count": comic,
    })))
}

/// `POST /api/sources/bench/corpus`：新增用户语料（撞内置书名返回 400）。
pub async fn bench_corpus_add(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchCorpusInput>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::add_user_corpus(&s.shared, req.into_corpus_input(true)).await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchCorpusUpdateReq {
    pub id: String,
    #[serde(flatten)]
    pub input: BenchCorpusInput,
}

/// `POST /api/sources/bench/corpus/update`：编辑用户语料（内置条目返回 400）。
pub async fn bench_corpus_update(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchCorpusUpdateReq>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::update_user_corpus(
        &s.shared,
        &req.id,
        req.input.into_corpus_input(true),
    )
    .await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchToggleReq {
    pub id: String,
    pub enabled: bool,
}

/// `POST /api/sources/bench/corpus/toggle`：内置可停用（写覆盖行），用户条目直接改。
pub async fn bench_corpus_toggle(
    State(s): State<AppState>,
    u: AuthUser,
    Json(req): Json<BenchToggleReq>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let item = crate::source_bench::toggle_corpus(&s.shared, &req.id, req.enabled).await?;
    Ok(Json(json!({ "ok": true, "item": item })))
}

#[derive(Deserialize)]
pub struct BenchCorpusIdQ {
    pub id: String,
}

/// `DELETE /api/sources/bench/corpus?id=...`：内置条目一律 400，不做静默忽略。
pub async fn bench_corpus_delete(
    State(s): State<AppState>,
    u: AuthUser,
    Query(q): Query<BenchCorpusIdQ>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    crate::source_bench::delete_corpus(&s.shared, &q.id).await?;
    Ok(Json(json!({ "ok": true, "id": q.id })))
}

#[derive(Deserialize, Default)]
pub struct BenchRunReq {
    /// "all" 或 ["https://..."]；省略等价于 all
    #[serde(default)]
    pub source_ids: Option<Value>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `POST /api/sources/bench/run`：启动一次后台体检，立即返回 `{run_id}`。
/// 进度用 `GET /api/sources/bench/run/{id}` 轮询（status / sources_done / finished_at）。
pub async fn bench_run(
    State(s): State<AppState>,
    u: AuthUser,
    body: Option<Json<BenchRunReq>>,
) -> AppResult<Json<Value>> {
    crate::auth::ensure_admin(&u)?;
    let req = body.map(|Json(v)| v).unwrap_or_default();
    let ids = match req.source_ids {
        None => None,
        Some(Value::String(v)) => {
            if v.eq_ignore_ascii_case("all") {
                None
            } else {
                Some(vec![v])
            }
        }
        Some(Value::Array(arr)) => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                let sid = item.as_str().ok_or_else(|| {
                    AppError::BadRequest("source_ids 必须是字符串数组或 \"all\"".into())
                })?;
                list.push(sid.to_string());
            }
            Some(list)
        }
        _ => {
            return Err(AppError::BadRequest(
                "source_ids 必须是字符串数组或 \"all\"".into(),
            ))
        }
    };
    let run_id =
        crate::source_bench::start_bench_job(s.shared.clone(), s.cfg.clone(), ids, req.limit)?;
    Ok(Json(json!({ "run_id": run_id, "status": "running" })))
}

/// `GET /api/sources/bench/run/{id}`：异步体检任务状态。
pub async fn bench_run_status(
    _s: State<AppState>,
    _u: AuthUser,
    Path(run_id): Path<String>,
) -> AppResult<Json<Value>> {
    let view = crate::source_bench::bench_job_view(&run_id)
        .ok_or_else(|| AppError::NotFound("bench run not found".into()))?;
    Ok(Json(serde_json::to_value(view).unwrap_or(Value::Null)))
}

/// `GET /api/sources/bench/curated`：当前精选清单（小说 / 漫画两套）。
pub async fn bench_curated(State(s): State<AppState>, _u: AuthUser) -> AppResult<Json<Value>> {
    let (novel, comic) = crate::source_bench::curated_lists(&s.shared).await?;
    let total = novel.len() + comic.len();
    Ok(Json(json!({
        "novel": novel,
        "comic": comic,
        "curated_total": total,
        "novel_total": novel.len(),
        "comic_total": comic.len(),
    })))
}
