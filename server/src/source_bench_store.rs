//! source_bench 的语料读写（move-only 从 source_bench.rs 拆出）。

use super::*;

// ---------------- 语料 ----------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct CorpusRow {
    pub id: String,
    pub title: String,
    pub expected_author: Option<String>,
    pub category: String,
    pub source_type: i64,
    pub enabled: bool,
    pub builtin: bool,
    pub norm_title: String,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}

/// 运行时语料条目（内置 ∪ 用户）。
#[derive(Debug, Clone, Serialize)]
pub struct CorpusEntry {
    pub id: String,
    pub title: String,
    pub expected_author: Option<String>,
    pub category: String,
    pub source_type: i64,
    pub enabled: bool,
    pub builtin: bool,
    pub norm_title: String,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    /// false = 可测但不计分（当前只用于内置的出版/实体书条目）。用户条目恒为 true。
    pub scored: bool,
}

pub fn builtin_id(item: &BuiltinCorpus) -> String {
    format!(
        "builtin:{}:{}",
        item.source_type,
        normalize_title(item.title)
    )
}

pub fn is_builtin_id(id: &str) -> bool {
    BUILTIN_CORPUS.iter().any(|b| builtin_id(b) == id)
}

pub(super) async fn load_rows(pool: &SqlitePool) -> AppResult<Vec<CorpusRow>> {
    let rows: Vec<CorpusRow> = sqlx::query_as(
        "SELECT id, title, expected_author, category, source_type, enabled, builtin, \
         norm_title, created_at, updated_at FROM source_bench_corpus",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 合并内置与用户语料：内置优先，用户追加排在后面。
pub async fn load_corpus(pool: &SqlitePool) -> AppResult<Vec<CorpusEntry>> {
    let rows = load_rows(pool).await?;
    let mut overrides: HashMap<String, bool> = HashMap::new();
    let mut user_rows: Vec<CorpusRow> = Vec::new();
    for r in rows {
        if r.builtin {
            // 内置行只信任 enabled；标题/作者/分类/类型一律由常量回填
            overrides.insert(r.id, r.enabled);
        } else {
            user_rows.push(r);
        }
    }
    let mut out: Vec<CorpusEntry> = Vec::with_capacity(BUILTIN_CORPUS.len() + user_rows.len());
    for b in BUILTIN_CORPUS {
        let id = builtin_id(b);
        let (created_at, updated_at) = (None, None);
        out.push(CorpusEntry {
            id: id.clone(),
            title: b.title.to_string(),
            expected_author: b.expected_author.map(str::to_string),
            category: b.category.to_string(),
            source_type: b.source_type,
            enabled: overrides.get(&id).copied().unwrap_or(true),
            builtin: true,
            norm_title: normalize_title(b.title),
            created_at,
            updated_at,
            scored: b.scored,
        });
    }
    user_rows.sort_by(|a, b| {
        a.created_at
            .unwrap_or(0)
            .cmp(&b.created_at.unwrap_or(0))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.extend(user_rows.into_iter().map(|r| CorpusEntry {
        id: r.id,
        title: r.title,
        expected_author: r.expected_author,
        category: if r.category.trim().is_empty() {
            CATEGORY_FALLBACK.to_string()
        } else {
            r.category
        },
        source_type: r.source_type,
        enabled: r.enabled,
        builtin: false,
        norm_title: r.norm_title,
        created_at: r.created_at,
        updated_at: r.updated_at,
        scored: true,
    }));
    Ok(out)
}

pub struct CorpusInput {
    pub title: String,
    pub expected_author: Option<String>,
    pub category: String,
    pub source_type: i64,
    pub enabled: bool,
}

pub(super) fn validate_input(input: &CorpusInput) -> AppResult<String> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("书名不能为空".into()));
    }
    if input.source_type != TYPE_NOVEL && input.source_type != TYPE_COMIC {
        return Err(AppError::BadRequest(
            "type 只能是 0（小说）或 2（漫画）".into(),
        ));
    }
    let norm = normalize_title(title);
    if norm.is_empty() {
        return Err(AppError::BadRequest("书名归一化后为空".into()));
    }
    Ok(norm)
}

/// 新增用户语料：撞内置书名返回 400 并明确说明原因。
pub async fn add_user_corpus(pool: &SqlitePool, input: CorpusInput) -> AppResult<CorpusEntry> {
    let norm = validate_input(&input)?;
    for b in BUILTIN_CORPUS {
        if b.source_type == input.source_type && normalize_title(b.title) == norm {
            return Err(AppError::BadRequest(
                "这是内置书名，不能重复添加（内置语料随版本发布，不可编辑/删除）".into(),
            ));
        }
    }
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM source_bench_corpus WHERE norm_title = ? AND source_type = ? LIMIT 1",
    )
    .bind(&norm)
    .bind(input.source_type)
    .fetch_optional(pool)
    .await?;
    if exists.is_some() {
        return Err(AppError::BadRequest(
            "已存在同名语料（同类目下书名归一化后相同）".into(),
        ));
    }
    let now = now_ms();
    let id = format!("user-{}-{}", now, next_seq());
    let category = if input.category.trim().is_empty() {
        CATEGORY_FALLBACK.to_string()
    } else {
        input.category.trim().to_string()
    };
    let author = input
        .expected_author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    sqlx::query(
        "INSERT INTO source_bench_corpus \
         (id, title, expected_author, category, source_type, enabled, builtin, norm_title, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?, ?)",
    )
    .bind(&id)
    .bind(input.title.trim())
    .bind(&author)
    .bind(&category)
    .bind(input.source_type)
    .bind(input.enabled)
    .bind(&norm)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(CorpusEntry {
        id,
        title: input.title.trim().to_string(),
        expected_author: author,
        category,
        source_type: input.source_type,
        enabled: input.enabled,
        builtin: false,
        norm_title: norm,
        created_at: Some(now),
        updated_at: Some(now),
        scored: true,
    })
}

/// 编辑用户语料；内置条目一律 400。
pub async fn update_user_corpus(
    pool: &SqlitePool,
    id: &str,
    input: CorpusInput,
) -> AppResult<CorpusEntry> {
    let row: Option<CorpusRow> = sqlx::query_as(
        "SELECT id, title, expected_author, category, source_type, enabled, builtin, \
         norm_title, created_at, updated_at FROM source_bench_corpus WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let existing = row.ok_or_else(|| AppError::NotFound("语料不存在".into()))?;
    if existing.builtin || is_builtin_id(id) {
        return Err(AppError::BadRequest(
            "内置语料不可编辑（标题/作者变动会让历史体检分失去可比性）".into(),
        ));
    }
    let norm = validate_input(&input)?;
    for b in BUILTIN_CORPUS {
        if b.source_type == input.source_type && normalize_title(b.title) == norm {
            return Err(AppError::BadRequest(
                "这是内置书名，不能改成内置书名".into(),
            ));
        }
    }
    let clash: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM source_bench_corpus WHERE norm_title = ? AND source_type = ? AND id <> ? LIMIT 1",
    )
    .bind(&norm)
    .bind(input.source_type)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if clash.is_some() {
        return Err(AppError::BadRequest(
            "已存在同名语料（同类目下书名归一化后相同）".into(),
        ));
    }
    let now = now_ms();
    let category = if input.category.trim().is_empty() {
        CATEGORY_FALLBACK.to_string()
    } else {
        input.category.trim().to_string()
    };
    let author = input
        .expected_author
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    sqlx::query(
        "UPDATE source_bench_corpus SET title = ?, expected_author = ?, category = ?, \
         source_type = ?, enabled = ?, norm_title = ?, updated_at = ? WHERE id = ?",
    )
    .bind(input.title.trim())
    .bind(&author)
    .bind(&category)
    .bind(input.source_type)
    .bind(input.enabled)
    .bind(&norm)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(CorpusEntry {
        id: id.to_string(),
        title: input.title.trim().to_string(),
        expected_author: author,
        category,
        source_type: input.source_type,
        enabled: input.enabled,
        builtin: false,
        norm_title: norm,
        created_at: existing.created_at,
        updated_at: Some(now),
        scored: true,
    })
}

/// 启停语料：内置条目允许停用（写一条 builtin=1 的覆盖行），用户条目直接改 enabled。
pub async fn toggle_corpus(pool: &SqlitePool, id: &str, enabled: bool) -> AppResult<CorpusEntry> {
    let corpus = load_corpus(pool).await?;
    let entry = corpus
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or_else(|| AppError::NotFound("语料不存在".into()))?;
    let now = now_ms();
    if entry.builtin {
        let builtin = BUILTIN_CORPUS
            .iter()
            .find(|b| builtin_id(b) == id)
            .ok_or_else(|| AppError::NotFound("内置语料不存在".into()))?;
        sqlx::query(
            "INSERT INTO source_bench_corpus \
             (id, title, expected_author, category, source_type, enabled, builtin, norm_title, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(builtin.title)
        .bind(builtin.expected_author)
        .bind(builtin.category)
        .bind(builtin.source_type)
        .bind(enabled)
        .bind(normalize_title(builtin.title))
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;
        return Ok(CorpusEntry {
            enabled,
            updated_at: Some(now),
            ..entry
        });
    }
    sqlx::query("UPDATE source_bench_corpus SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(enabled)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(CorpusEntry {
        enabled,
        updated_at: Some(now),
        ..entry
    })
}

/// 删除语料：内置条目必须 400（不能静默忽略）。
pub async fn delete_corpus(pool: &SqlitePool, id: &str) -> AppResult<()> {
    if is_builtin_id(id) {
        return Err(AppError::BadRequest(
            "内置语料不可删除；如不想参与体检请改为停用（enabled=false）".into(),
        ));
    }
    let row: Option<CorpusRow> = sqlx::query_as(
        "SELECT id, title, expected_author, category, source_type, enabled, builtin, \
         norm_title, created_at, updated_at FROM source_bench_corpus WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound("语料不存在".into()));
    };
    if row.builtin {
        return Err(AppError::BadRequest(
            "内置语料不可删除；如不想参与体检请改为停用（enabled=false）".into(),
        ));
    }
    sqlx::query("DELETE FROM source_bench_corpus WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
