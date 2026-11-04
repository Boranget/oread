//! 体检语料与精选书源（curated）。
//!
//! 设计要点：
//! - **内置语料写在 Rust 常量**（随版本发布、不可删除/编辑，只能临时停用），
//!   用户追加的语料存 `source_bench_corpus`（builtin=0，可改可删）。
//! - 体检按“书名 + 匹配度 + 速度”给源打分，**单条书名的失败不直接扣源的分**：
//!   先在 category 内聚合命中率，再把明显低于同 category 中位数的语料标记为
//!   `suspicious`（语料可能失效）并排除出评分。
//! - `curated` 只是 `preferred` 的子集标记，不新增 tier；断层截断数量夹在 8..14。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use crate::config::Config;
use crate::entities::BookSource;
use crate::error::{AppError, AppResult};
use crate::source_health::{self, classify, FailureKind};
use crate::web_book;

pub const TYPE_NOVEL: i64 = 0;
pub const TYPE_COMIC: i64 = 2;

/// 单条搜索预算（5s）；整批默认 60s，超时后中断并保留已完成的结果。
pub const PER_SAMPLE_BUDGET_SECS: u64 = 5;
pub const BATCH_BUDGET_SECS: u64 = 60;
/// 体检源的并发数；真正的 HTTP 并发仍由 `req` 全局信号量（HTTP_CONCURRENCY）兜底。
pub const BENCH_CONCURRENCY: usize = 8;
/// 精选所需的最少样本数；不足的源不参与精选（冷启动回退现有 tier 逻辑）。
pub const MIN_CURATED_SAMPLES: i64 = 2;

pub const CATEGORY_FALLBACK: &str = "未分类";

/// 内置语料条目。标题/作者/分类只从这里读取，数据库里的 builtin 行仅承载 enabled 覆盖。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinCorpus {
    pub title: &'static str,
    pub expected_author: Option<&'static str>,
    pub category: &'static str,
    pub source_type: i64,
}

/// 首批 16 条：覆盖玄幻/都市/历史/科幻/出版/漫画，选长期存在的常见书名。
pub const BUILTIN_CORPUS: &[BuiltinCorpus] = &[
    // 玄幻（小说）
    BuiltinCorpus { title: "斗破苍穹", expected_author: Some("天蚕土豆"), category: "玄幻", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "凡人修仙传", expected_author: Some("忘语"), category: "玄幻", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "遮天", expected_author: Some("辰东"), category: "玄幻", source_type: TYPE_NOVEL },
    // 都市（小说）
    BuiltinCorpus { title: "全职高手", expected_author: Some("蝴蝶蓝"), category: "都市", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "校花的贴身高手", expected_author: Some("鱼人二代"), category: "都市", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "黄金瞳", expected_author: Some("打眼"), category: "都市", source_type: TYPE_NOVEL },
    // 历史（小说）
    BuiltinCorpus { title: "庆余年", expected_author: Some("猫腻"), category: "历史", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "明朝那些事儿", expected_author: Some("当年明月"), category: "历史", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "赘婿", expected_author: Some("愤怒的香蕉"), category: "历史", source_type: TYPE_NOVEL },
    // 科幻（小说）
    BuiltinCorpus { title: "三体", expected_author: Some("刘慈欣"), category: "科幻", source_type: TYPE_NOVEL },
    // 出版（小说）
    BuiltinCorpus { title: "活着", expected_author: Some("余华"), category: "出版", source_type: TYPE_NOVEL },
    BuiltinCorpus { title: "平凡的世界", expected_author: Some("路遥"), category: "出版", source_type: TYPE_NOVEL },
    // 漫画（type=2）
    BuiltinCorpus { title: "海贼王", expected_author: Some("尾田荣一郎"), category: "漫画", source_type: TYPE_COMIC },
    BuiltinCorpus { title: "火影忍者", expected_author: Some("岸本齐史"), category: "漫画", source_type: TYPE_COMIC },
    BuiltinCorpus { title: "名侦探柯南", expected_author: Some("青山刚昌"), category: "漫画", source_type: TYPE_COMIC },
    BuiltinCorpus { title: "一人之下", expected_author: Some("米二"), category: "漫画", source_type: TYPE_COMIC },
];

fn next_seq() -> i64 {
    static SEQ: AtomicI64 = AtomicI64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn now_ms() -> i64 {
    source_health::now_ms()
}

fn fullwidth_to_half(ch: char) -> char {
    let code = ch as u32;
    if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0).unwrap_or(ch)
    } else if code == 0x3000 {
        ' '
    } else {
        ch
    }
}

fn is_noise_punct(c: char) -> bool {
    matches!(
        c,
        '《' | '》'
            | '「'
            | '」'
            | '『'
            | '』'
            | '（'
            | '）'
            | '('
            | ')'
            | '：'
            | ':'
            | '·'
            | '・'
            | '、'
            | '，'
            | ','
            | '。'
            | '.'
            | '！'
            | '!'
            | '？'
            | '?'
            | '—'
            | '-'
            | '_'
            | '~'
            | '～'
            | '"'
            | '\''
            | '“'
            | '”'
            | '‘'
            | '’'
            | '/'
            | '|'
            | '+'
            | '&'
            | '['
            | ']'
            | '【'
            | '】'
            | '<'
            | '>'
            | '*'
            | '#'
            | '@'
            | '%'
            | '×'
            | '✕'
    )
}

/// 标题归一化：去空白、全半角统一、去常见标点；**不做繁简转换**。
pub fn normalize_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let c = fullwidth_to_half(ch);
        if c.is_whitespace() || is_noise_punct(c) {
            continue;
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

const TITLE_DECORATIONS: &[&str] = &[
    "最新章节",
    "全文阅读",
    "免费阅读",
    "在线阅读",
    "无弹窗",
    "全文",
    "小说",
];

fn strip_decorations(mut s: String) -> String {
    loop {
        let mut changed = false;
        for suffix in TITLE_DECORATIONS {
            if s.len() > suffix.len() && s.ends_with(suffix) {
                let keep = s.len() - suffix.len();
                s.truncate(keep);
                changed = true;
            }
        }
        if !changed {
            return s;
        }
    }
}

fn norm_len(s: &str) -> usize {
    s.chars().count()
}

/// 标题匹配：归一化后完全相等，或包含率 ≥ 0.8。
pub fn title_matches(got: &str, expected: &str) -> bool {
    let a = strip_decorations(normalize_title(got));
    let b = strip_decorations(normalize_title(expected));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    if a.contains(&b) || b.contains(&a) {
        let short = norm_len(&a).min(norm_len(&b));
        let long = norm_len(&a).max(norm_len(&b));
        return short as f64 / long as f64 >= 0.8;
    }
    false
}

/// 作者归一化：在标题归一化的基础上再去掉常见署名后缀。
pub fn normalize_author(raw: &str) -> String {
    let mut s = normalize_title(raw);
    for suffix in ["著", "编", "译", "繪", "绘", "作者"] {
        while s.ends_with(suffix) && norm_len(&s) > norm_len(suffix) {
            let keep = s.len() - suffix.len();
            s.truncate(keep);
        }
    }
    s
}

/// 作者匹配：expected 为空视为不校验；否则任何一位作者归一化后相等即算匹配。
pub fn author_matches(got: Option<&str>, expected: Option<&str>) -> bool {
    let Some(exp) = expected.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let Some(got) = got.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let want = normalize_author(exp);
    if want.is_empty() {
        return true;
    }
    got.split(['/', '、', ',', '，', ';', '；', '&'])
        .map(normalize_author)
        .any(|a| a == want)
}

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
}

pub fn builtin_id(item: &BuiltinCorpus) -> String {
    format!("builtin:{}:{}", item.source_type, normalize_title(item.title))
}

pub fn is_builtin_id(id: &str) -> bool {
    BUILTIN_CORPUS.iter().any(|b| builtin_id(b) == id)
}

async fn load_rows(pool: &SqlitePool) -> AppResult<Vec<CorpusRow>> {
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
        category: if r.category.trim().is_empty() { CATEGORY_FALLBACK.to_string() } else { r.category },
        source_type: r.source_type,
        enabled: r.enabled,
        builtin: false,
        norm_title: r.norm_title,
        created_at: r.created_at,
        updated_at: r.updated_at,
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

fn validate_input(input: &CorpusInput) -> AppResult<String> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("书名不能为空".into()));
    }
    if input.source_type != TYPE_NOVEL && input.source_type != TYPE_COMIC {
        return Err(AppError::BadRequest("type 只能是 0（小说）或 2（漫画）".into()));
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
        return Err(AppError::BadRequest("已存在同名语料（同类目下书名归一化后相同）".into()));
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
            return Err(AppError::BadRequest("这是内置书名，不能改成内置书名".into()));
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
        return Err(AppError::BadRequest("已存在同名语料（同类目下书名归一化后相同）".into()));
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

// ---------------- 抽样 ----------------

/// 按 type 分组、内置优先、category 轮转抽 `limit` 条。
/// `salt` 用于每次体检轮换起点，保证长期覆盖到所有 category。
pub fn pick_samples(entries: &[CorpusEntry], source_type: i64, limit: usize, salt: usize) -> Vec<CorpusEntry> {
    let usable: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|e| e.enabled && e.source_type == source_type)
        .collect();
    let builtin: Vec<&CorpusEntry> = usable.iter().copied().filter(|e| e.builtin).collect();
    let user: Vec<&CorpusEntry> = usable.iter().copied().filter(|e| !e.builtin).collect();
    if limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<CorpusEntry> = Vec::with_capacity(limit);
    // 用户条目至少留 1 个抽样位：否则内置一多，用户新增的书名永远进不了下一次体检
    // （验收要求“新增书名必须出现在下批抽样里”）。内置仍占绝对多数，覆盖不被挤占。
    let user_reserved = usize::from(!user.is_empty() && limit >= 2);
    let builtin_slots = limit.saturating_sub(user_reserved);
    round_robin(&builtin, builtin_slots, salt, &mut out);
    if !user.is_empty() {
        let mut rotated = user.clone();
        let offset = salt % rotated.len();
        rotated.rotate_left(offset);
        for e in rotated {
            if out.len() >= limit {
                break;
            }
            out.push(e.clone());
        }
    }
    if out.len() < limit {
        round_robin(&builtin, limit - out.len(), salt, &mut out);
    }
    out
}

fn round_robin(items: &[&CorpusEntry], limit: usize, salt: usize, out: &mut Vec<CorpusEntry>) {
    if items.is_empty() || limit == 0 {
        return;
    }
    let mut cats: Vec<String> = Vec::new();
    for e in items {
        if !cats.contains(&e.category) {
            cats.push(e.category.clone());
        }
    }
    if cats.is_empty() {
        return;
    }
    let mut buckets: HashMap<&str, Vec<&CorpusEntry>> = HashMap::new();
    for e in items {
        buckets.entry(e.category.as_str()).or_default().push(e);
    }
    let offset = salt % cats.len();
    let mut idx: HashMap<&str, usize> = HashMap::new();
    let mut placed = 0usize;
    let mut guard = 0usize;
    while placed < limit && guard < items.len() * 2 + 8 {
        guard += 1;
        let mut any = false;
        for step in 0..cats.len() {
            if placed >= limit {
                break;
            }
            let cat = &cats[(offset + step) % cats.len()];
            let list = buckets.get(cat.as_str()).cloned().unwrap_or_default();
            let pos = idx.entry(cat.as_str()).or_insert(0);
            if *pos < list.len() {
                out.push(list[*pos].clone());
                *pos += 1;
                placed += 1;
                any = true;
            }
        }
        if !any {
            break;
        }
    }
}

// ---------------- 体检执行 ----------------

#[derive(Debug, Clone)]
pub struct BenchRun {
    pub batch_id: String,
    pub source_url: String,
    pub corpus_key: String,
    pub title: String,
    pub category: String,
    pub source_type: i64,
    pub builtin_sample: bool,
    pub found: bool,
    pub title_match: bool,
    pub author_match: bool,
    pub latency_ms: i64,
    pub error_kind: Option<String>,
    pub suspicious: bool,
    pub run_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRate {
    pub category: String,
    pub samples: i64,
    pub hits: i64,
    pub hit_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceBenchRow {
    pub source_url: String,
    pub name: String,
    pub source_type: i64,
    pub samples: i64,
    pub hits: i64,
    pub hit_rate: f64,
    pub avg_latency_ms: i64,
    pub errors: i64,
    pub error_kinds: serde_json::Value,
    pub bench_score: i32,
    pub curated: bool,
    pub excluded_samples: i64,
    pub per_category_hit_rate: Vec<CategoryRate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuspiciousCorpus {
    pub corpus_key: String,
    pub title: String,
    pub category: String,
    pub hit_rate: f64,
    pub category_median: f64,
    pub observations: i64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedItem {
    pub rank: usize,
    pub source_url: String,
    pub name: String,
    pub source_type: i64,
    pub bench_score: i32,
    pub samples: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_gap: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub batch_id: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub elapsed_ms: i64,
    pub interrupted: bool,
    pub corpus_total: usize,
    pub samples_per_source: usize,
    pub sources: Vec<SourceBenchRow>,
    pub novel_curated: Vec<CuratedItem>,
    pub comic_curated: Vec<CuratedItem>,
    pub suspicious: Vec<SuspiciousCorpus>,
    pub excluded_from_score: i64,
}

struct RawSample {
    corpus_key: String,
    title: String,
    category: String,
    builtin_sample: bool,
    found: bool,
    title_match: bool,
    author_match: bool,
    latency_ms: i64,
    error_kind: Option<String>,
}

async fn run_one_sample(src: &BookSource, item: &CorpusEntry, deadline: Instant) -> RawSample {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < Duration::from_millis(100) {
        return RawSample {
            corpus_key: item.id.clone(),
            title: item.title.clone(),
            category: item.category.clone(),
            builtin_sample: item.builtin,
            found: false,
            title_match: false,
            author_match: false,
            latency_ms: 0,
            error_kind: Some("timeout".into()),
        };
    }
    let budget = Duration::from_secs(PER_SAMPLE_BUDGET_SECS).min(remaining);
    let started = Instant::now();
    let result = tokio::time::timeout(budget, web_book::search(src, &item.title, 1)).await;
    let latency_ms = started.elapsed().as_millis() as i64;
    match result {
        Ok(Ok(books)) => {
            let first = books.first();
            let title_match = first
                .and_then(|b| b.name.as_deref())
                .map(|name| title_matches(name, &item.title))
                .unwrap_or(false);
            let author_match = author_matches(
                first.and_then(|b| b.author.as_deref()),
                item.expected_author.as_deref(),
            );
            RawSample {
                corpus_key: item.id.clone(),
                title: item.title.clone(),
                category: item.category.clone(),
                builtin_sample: item.builtin,
                found: title_match && author_match,
                title_match,
                author_match,
                latency_ms,
                error_kind: None,
            }
        }
        Ok(Err(e)) => {
            let kind = match classify(&e) {
                FailureKind::Site => "network",
                FailureKind::Engine => "parse",
            };
            tracing::debug!(source = %src.book_source_url, title = %item.title, error = %e, "bench sample failed");
            RawSample {
                corpus_key: item.id.clone(),
                title: item.title.clone(),
                category: item.category.clone(),
                builtin_sample: item.builtin,
                found: false,
                title_match: false,
                author_match: false,
                latency_ms,
                error_kind: Some(kind.to_string()),
            }
        }
        Err(_) => RawSample {
            corpus_key: item.id.clone(),
            title: item.title.clone(),
            category: item.category.clone(),
            builtin_sample: item.builtin,
            found: false,
            title_match: false,
            author_match: false,
            latency_ms,
            error_kind: Some("timeout".into()),
        },
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// 标记“语料可能失效”的条目：命中率显著低于同 category 中位数（< 中位数 × 0.5）。
/// 只影响评分口径，run 明细全部保留。
fn mark_suspicious(runs: &mut [BenchRun], corpus: &[CorpusEntry]) {
    let mut per_key: HashMap<String, (i64, i64, String)> = HashMap::new();
    for r in runs.iter() {
        let e = per_key
            .entry(r.corpus_key.clone())
            .or_insert((0, 0, r.category.clone()));
        e.1 += 1;
        if r.found {
            e.0 += 1;
        }
    }
    // 同 category 中位数只用内置样本计算（用户冷门书名不应拉低基线）
    let mut cat_rates: HashMap<String, Vec<f64>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin) {
        if let Some((hits, obs, _)) = per_key.get(&e.id) {
            if *obs >= MIN_CURATED_SAMPLES {
                cat_rates
                    .entry(e.category.clone())
                    .or_default()
                    .push(*hits as f64 / *obs as f64);
            }
        }
    }
    let medians: HashMap<String, f64> = cat_rates
        .iter()
        .map(|(k, v)| (k.clone(), median(v)))
        .collect();
    let mut flagged: HashSet<String> = HashSet::new();
    for (key, (hits, obs, category)) in per_key.iter() {
        if *obs < MIN_CURATED_SAMPLES {
            continue;
        }
        let Some(med) = medians.get(category).copied() else {
            continue;
        };
        if med <= 0.0 {
            continue;
        }
        let rate = *hits as f64 / *obs as f64;
        if rate < med * 0.5 {
            flagged.insert(key.clone());
        }
    }
    for r in runs.iter_mut() {
        if flagged.contains(&r.corpus_key) {
            r.suspicious = true;
        }
    }
}

fn compute_source_rows(
    runs: &[BenchRun],
    sources: &HashMap<String, BookSource>,
    corpus: &[CorpusEntry],
    cfg: &Config,
    health: &HashMap<String, source_health::HealthRow>,
    prev_fail_streak: &HashMap<String, i64>,
) -> Vec<SourceBenchRow> {
    let mut by_source: HashMap<String, Vec<&BenchRun>> = HashMap::new();
    for r in runs {
        by_source.entry(r.source_url.clone()).or_default().push(r);
    }
    // 参与计分的 category：每个 type 下至少含 1 条内置样本
    let mut scoring_cats: HashMap<i64, HashSet<String>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin) {
        scoring_cats
            .entry(e.source_type)
            .or_default()
            .insert(e.category.clone());
    }

    let mut out: Vec<SourceBenchRow> = Vec::new();
    for (url, list) in by_source.iter() {
        let src = sources.get(url);
        let name = src.map(|s| s.book_source_name.clone()).unwrap_or_else(|| url.clone());
        let source_type = src.map(|s| s.book_source_type).unwrap_or(TYPE_NOVEL);
        let total = list.len() as i64;
        let excluded = list.iter().filter(|r| r.suspicious).count() as i64;
        let usable: Vec<&&BenchRun> = list.iter().filter(|r| !r.suspicious).collect();
        let obs = usable.len() as i64;
        let hits = usable.iter().filter(|r| r.found).count() as i64;
        let errors = usable.iter().filter(|r| r.error_kind.is_some()).count() as i64;
        let avg_latency = if obs == 0 {
            0
        } else {
            usable.iter().map(|r| r.latency_ms.max(0)).sum::<i64>() / obs
        };
        let mut kinds: HashMap<String, i64> = HashMap::new();
        for r in usable.iter() {
            if let Some(k) = r.error_kind.as_deref() {
                *kinds.entry(k.to_string()).or_insert(0) += 1;
            }
        }
        // 按 category 聚合命中率（单条冷门书名不会单独把源判死）
        let cats = scoring_cats.get(&source_type).cloned().unwrap_or_default();
        let mut per_cat: Vec<CategoryRate> = Vec::new();
        for cat in cats.iter() {
            let cs = usable.iter().filter(|r| r.category == *cat).count() as i64;
            if cs == 0 {
                continue;
            }
            let ch = usable
                .iter()
                .filter(|r| r.category == *cat && r.found)
                .count() as i64;
            per_cat.push(CategoryRate {
                category: cat.clone(),
                samples: cs,
                hits: ch,
                hit_rate: ch as f64 / cs as f64,
            });
        }
        let hit_rate = if per_cat.is_empty() {
            0.0
        } else {
            per_cat.iter().map(|c| c.hit_rate).sum::<f64>() / per_cat.len() as f64
        };
        let latency_score = 1.0 - (avg_latency as f64 / (PER_SAMPLE_BUDGET_SECS * 1000) as f64).clamp(0.0, 1.0);
        let success = if obs == 0 {
            0.0
        } else {
            1.0 - errors as f64 / obs as f64
        };
        let bench_score = (cfg.source_bench_w_hit as f64 * hit_rate
            + cfg.source_bench_w_latency as f64 * latency_score
            + cfg.source_bench_w_fail as f64 * success)
            .round()
            .clamp(0.0, 100.0) as i32;
        let _ = (total, prev_fail_streak, health); // 精选资格在 recompute_curated 阶段统一判定
        out.push(SourceBenchRow {
            source_url: url.clone(),
            name,
            source_type,
            samples: obs,
            hits,
            hit_rate: (hit_rate * 1000.0).round() / 1000.0,
            avg_latency_ms: avg_latency,
            errors,
            error_kinds: serde_json::to_value(&kinds).unwrap_or(serde_json::Value::Null),
            bench_score,
            curated: false,
            excluded_samples: excluded,
            per_category_hit_rate: per_cat,
        });
    }
    out.sort_by(|a, b| b.bench_score.cmp(&a.bench_score).then_with(|| a.name.cmp(&b.name)));
    out
}

/// 断层截断：分数降序，遇到相邻差 >= gap 就在断点截断，数量夹在 min..max。
pub fn pick_curated_count(sorted_scores: &[i32], min: usize, max: usize, gap: i32) -> usize {
    let n = sorted_scores.len();
    if n == 0 {
        return 0;
    }
    let lo = min.max(1);
    let hi = max.max(lo);
    if n <= lo {
        return n;
    }
    let cap = hi.min(n);
    let mut i = lo - 1;
    while i < cap {
        let next = i + 1;
        if next >= n {
            return cap;
        }
        if sorted_scores[i] - sorted_scores[next] >= gap {
            return next;
        }
        i += 1;
    }
    cap
}

struct CuratedSelection {
    novel: Vec<CuratedItem>,
    comic: Vec<CuratedItem>,
    flags: HashMap<String, bool>,
}

fn select_curated(rows: &[SourceBenchRow], cfg: &Config, eligible: &HashSet<String>) -> CuratedSelection {
    let mut result = CuratedSelection {
        novel: Vec::new(),
        comic: Vec::new(),
        flags: HashMap::new(),
    };
    for source_type in [TYPE_NOVEL, TYPE_COMIC] {
        let mut pool: Vec<&SourceBenchRow> = rows
            .iter()
            .filter(|r| r.source_type == source_type && r.samples >= MIN_CURATED_SAMPLES)
            .filter(|r| eligible.contains(&r.source_url))
            .collect();
        pool.sort_by(|a, b| {
            b.bench_score
                .cmp(&a.bench_score)
                .then_with(|| a.source_url.cmp(&b.source_url))
        });
        let scores: Vec<i32> = pool.iter().map(|r| r.bench_score).collect();
        let count = pick_curated_count(&scores, cfg.source_bench_min, cfg.source_bench_max, cfg.source_bench_gap);
        let cut_gap = if count > 0 && count < pool.len() {
            Some(scores[count - 1] - scores[count])
        } else {
            None
        };
        let mut items: Vec<CuratedItem> = Vec::new();
        for (idx, r) in pool.iter().take(count).enumerate() {
            result.flags.insert(r.source_url.clone(), true);
            items.push(CuratedItem {
                rank: idx + 1,
                source_url: r.source_url.clone(),
                name: r.name.clone(),
                source_type,
                bench_score: r.bench_score,
                samples: r.samples,
                cut_gap: if idx + 1 == count { cut_gap } else { None },
            });
        }
        for r in pool.iter().skip(count) {
            result.flags.insert(r.source_url.clone(), false);
        }
        if source_type == TYPE_NOVEL {
            result.novel = items;
        } else {
            result.comic = items;
        }
    }
    result
}

/// 执行一次体检。`source_ids` 为 None 表示全部启用源（排除隔离区）。
pub async fn run_bench(
    pool: &SqlitePool,
    cfg: &Config,
    source_ids: Option<Vec<String>>,
    limit: Option<usize>,
) -> AppResult<BenchReport> {
    let started_at = now_ms();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(BATCH_BUDGET_SECS);
    let batch_seq = next_seq();
    let batch_id = format!("b{}-{}", started_at, batch_seq);

    let all = crate::routes::sources::load_sources(pool).await?;
    let by_url: HashMap<String, BookSource> =
        all.iter().cloned().map(|s| (s.book_source_url.clone(), s)).collect();
    let quarantined = crate::source_ledger::quarantined_set(pool).await.unwrap_or_default();
    let corpus = load_corpus(pool).await?;
    let samples_per_source = limit.unwrap_or(cfg.source_bench_samples).clamp(1, 20);

    let mut sources: Vec<BookSource> = Vec::new();
    match source_ids {
        None => {
            for s in all.iter() {
                if s.enabled
                    && crate::entities::is_supported_source_type(s.book_source_type)
                    && s.rule_search.is_some()
                    && s.search_url.is_some()
                    && !quarantined.contains(&s.book_source_url)
                {
                    sources.push(s.clone());
                }
            }
        }
        Some(ids) => {
            for id in ids {
                if let Some(s) = by_url.get(&id) {
                    sources.push(s.clone());
                }
            }
        }
    }

    let corpus_for_type = |t: i64| -> Vec<CorpusEntry> {
        corpus.iter().filter(|e| e.source_type == t).cloned().collect()
    };
    let novel_corpus = corpus_for_type(TYPE_NOVEL);
    let comic_corpus = corpus_for_type(TYPE_COMIC);

    let health = source_health::load_map(pool).await.unwrap_or_default();
    let prev_streak: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT source_url, fail_streak FROM source_curated",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .collect();

    // 每个源内部顺序执行 N 条；源之间用 buffer_unordered 限制在 BENCH_CONCURRENCY。
    // 真正的 HTTP 并发仍由 req 的全局信号量（HTTP_CONCURRENCY）兜底。
    let mut tasks = futures::stream::iter(sources.into_iter().enumerate().map(|(idx, src)| {
        let novel = novel_corpus.clone();
        let comic = comic_corpus.clone();
        async move {
            let pool_corpus = if src.book_source_type == TYPE_COMIC { comic } else { novel };
            let picked = pick_samples(&pool_corpus, src.book_source_type, samples_per_source, batch_seq as usize + idx);
            let mut out = Vec::with_capacity(picked.len());
            for item in picked {
                if Instant::now() >= deadline {
                    break;
                }
                let sample = run_one_sample(&src, &item, deadline).await;
                out.push((src.book_source_url.clone(), src.book_source_type, sample));
            }
            out
        }
    }))
    .buffer_unordered(BENCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let run_at = now_ms();
    let mut runs: Vec<BenchRun> = Vec::new();
    for list in tasks.iter_mut() {
        for (url, source_type, s) in list.drain(..) {
            runs.push(BenchRun {
                batch_id: batch_id.clone(),
                source_url: url,
                corpus_key: s.corpus_key,
                title: s.title,
                category: s.category,
                source_type,
                builtin_sample: s.builtin_sample,
                found: s.found,
                title_match: s.title_match,
                author_match: s.author_match,
                latency_ms: s.latency_ms,
                error_kind: s.error_kind,
                suspicious: false,
                run_at,
            });
        }
    }

    mark_suspicious(&mut runs, &corpus);
    let mut suspicious: Vec<SuspiciousCorpus> = Vec::new();
    {
        let mut per_key: HashMap<String, (i64, i64, String, String)> = HashMap::new();
        for r in runs.iter().filter(|r| r.suspicious) {
            let e = per_key
                .entry(r.corpus_key.clone())
                .or_insert((0, 0, r.category.clone(), r.title.clone()));
            e.1 += 1;
            if r.found {
                e.0 += 1;
            }
        }
        let medians = category_medians(&runs, &corpus);
        for (key, (hits, obs, category, title)) in per_key.into_iter() {
            let med = medians.get(&category).copied().unwrap_or(0.0);
            suspicious.push(SuspiciousCorpus {
                corpus_key: key,
                title,
                category,
                hit_rate: if obs > 0 { (hits as f64 / obs as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
                category_median: (med * 1000.0).round() / 1000.0,
                observations: obs,
                reason: "命中率低于同 category 中位数的 50%，判定为“语料可能失效”，已排除出源评分".into(),
            });
        }
        suspicious.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.title.cmp(&b.title)));
    }

    let mut rows = compute_source_rows(&runs, &by_url, &corpus, cfg, &health, &prev_streak);

    // 落库批次明细
    let mut tx = pool.begin().await?;
    for r in runs.iter() {
        sqlx::query(
            "INSERT INTO source_bench_runs \
             (batch_id, source_url, corpus_key, title, category, source_type, builtin_sample, \
              found, title_match, author_match, latency_ms, error_kind, suspicious, run_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.batch_id)
        .bind(&r.source_url)
        .bind(&r.corpus_key)
        .bind(&r.title)
        .bind(&r.category)
        .bind(r.source_type)
        .bind(r.builtin_sample)
        .bind(r.found)
        .bind(r.title_match)
        .bind(r.author_match)
        .bind(r.latency_ms)
        .bind(&r.error_kind)
        .bind(r.suspicious)
        .bind(r.run_at)
        .execute(&mut *tx)
        .await?;
    }
    // 当前状态写入 source_curated（保留 fail_streak 供“连续 2 批失败退出精选”）
    for row in rows.iter() {
        let all_failed = row.samples > 0 && row.errors == row.samples;
        let prev = prev_streak.get(&row.source_url).copied().unwrap_or(0);
        let fail_streak = if all_failed { prev.saturating_add(1) } else { 0 };
        sqlx::query(
            "INSERT INTO source_curated \
             (source_url, source_type, bench_score, curated, samples, fail_streak, last_bench_at, meta_json) \
             VALUES (?, ?, ?, 0, ?, ?, ?, ?) \
             ON CONFLICT(source_url) DO UPDATE SET \
              source_type=excluded.source_type, bench_score=excluded.bench_score, \
              samples=excluded.samples, fail_streak=excluded.fail_streak, \
              last_bench_at=excluded.last_bench_at, meta_json=excluded.meta_json",
        )
        .bind(&row.source_url)
        .bind(row.source_type)
        .bind(row.bench_score)
        .bind(row.samples)
        .bind(fail_streak)
        .bind(run_at)
        .bind(serde_json::json!({ "batch_id": batch_id, "hit_rate": row.hit_rate, "avg_latency_ms": row.avg_latency_ms }).to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // 重算精选：用当前库内最新分数（含本次批次），资格要求 preferred + 样本足够 + 未连续失败 2 批。
    let (novel_sel, comic_sel) = recompute_curated(pool, cfg).await?;
    let mut flags: HashMap<String, bool> = HashMap::new();
    for item in novel_sel.iter().chain(comic_sel.iter()) {
        flags.insert(item.source_url.clone(), true);
    }
    for row in rows.iter_mut() {
        row.curated = flags.get(&row.source_url).copied().unwrap_or(false);
    }
    // 用库内重算结果覆盖返回里的精选清单
    let novel_items = novel_sel;
    let comic_items = comic_sel;

    let finished_at = now_ms();
    Ok(BenchReport {
        batch_id,
        started_at,
        finished_at,
        elapsed_ms: finished_at - started_at,
        interrupted: Instant::now() >= deadline,
        corpus_total: corpus.len(),
        samples_per_source,
        sources: rows,
        novel_curated: novel_items,
        comic_curated: comic_items,
        excluded_from_score: runs.iter().filter(|r| r.suspicious).count() as i64,
        suspicious,
    })
}

fn category_medians(runs: &[BenchRun], corpus: &[CorpusEntry]) -> HashMap<String, f64> {
    let mut per_key: HashMap<String, (i64, i64)> = HashMap::new();
    for r in runs.iter() {
        let e = per_key.entry(r.corpus_key.clone()).or_insert((0, 0));
        e.1 += 1;
        if r.found {
            e.0 += 1;
        }
    }
    let mut cat_rates: HashMap<String, Vec<f64>> = HashMap::new();
    for e in corpus.iter().filter(|e| e.builtin) {
        if let Some((hits, obs)) = per_key.get(&e.id) {
            if *obs >= MIN_CURATED_SAMPLES {
                cat_rates
                    .entry(e.category.clone())
                    .or_default()
                    .push(*hits as f64 / *obs as f64);
            }
        }
    }
    cat_rates.into_iter().map(|(k, v)| (k, median(&v))).collect()
}

struct StoredScoreRow {
    source_url: String,
    name: String,
    source_type: i64,
    bench_score: i32,
    samples: i64,
    fail_streak: i64,
    tier: Option<String>,
}

/// 从库内状态重算精选并回写 curated 标记。
async fn recompute_curated(pool: &SqlitePool, cfg: &Config) -> AppResult<(Vec<CuratedItem>, Vec<CuratedItem>)> {
    let rows: Vec<(String, i64, i64, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT c.source_url, c.source_type, c.bench_score, c.samples, c.fail_streak, h.tier, \
                (SELECT b.name FROM book_sources b WHERE b.id = c.source_url) \
         FROM source_curated c LEFT JOIN source_health h ON h.source_url = c.source_url",
    )
    .fetch_all(pool)
    .await?;
    let stored: Vec<StoredScoreRow> = rows
        .into_iter()
        .map(|(source_url, source_type, bench_score, samples, fail_streak, tier, name)| StoredScoreRow {
            name: name.unwrap_or_else(|| source_url.clone()),
            source_url,
            source_type,
            bench_score: bench_score as i32,
            samples,
            fail_streak,
            tier,
        })
        .collect();

    let mut novel_items: Vec<CuratedItem> = Vec::new();
    let mut comic_items: Vec<CuratedItem> = Vec::new();
    // 先全部清 0，再只把本次入选的源置 1：这样非 preferred、样本不足、连续失败、
    // 掉出断层的源都会自动退出精选（不依赖“它是否在本次体检名单里”）。
    sqlx::query("UPDATE source_curated SET curated = 0")
        .execute(pool)
        .await?;
    let mut selected: Vec<String> = Vec::new();
    for source_type in [TYPE_NOVEL, TYPE_COMIC] {
        let mut pool_rows: Vec<&StoredScoreRow> = stored
            .iter()
            .filter(|r| r.source_type == source_type)
            .filter(|r| r.samples >= MIN_CURATED_SAMPLES && r.fail_streak < 2)
            .filter(|r| r.tier.as_deref() == Some(crate::source_quality::TIER_PREFERRED))
            .collect();
        pool_rows.sort_by(|a, b| b.bench_score.cmp(&a.bench_score).then_with(|| a.source_url.cmp(&b.source_url)));
        let scores: Vec<i32> = pool_rows.iter().map(|r| r.bench_score).collect();
        let count = pick_curated_count(&scores, cfg.source_bench_min, cfg.source_bench_max, cfg.source_bench_gap);
        let cut_gap = if count > 0 && count < pool_rows.len() {
            Some(scores[count - 1] - scores[count])
        } else {
            None
        };
        let mut items: Vec<CuratedItem> = Vec::new();
        for (idx, r) in pool_rows.iter().take(count).enumerate() {
            selected.push(r.source_url.clone());
            items.push(CuratedItem {
                rank: idx + 1,
                source_url: r.source_url.clone(),
                name: r.name.clone(),
                source_type,
                bench_score: r.bench_score as i32,
                samples: r.samples,
                cut_gap: if idx + 1 == count { cut_gap } else { None },
            });
        }
        if source_type == TYPE_NOVEL {
            novel_items = items;
        } else {
            comic_items = items;
        }
    }
    if !selected.is_empty() {
        let mut tx = pool.begin().await?;
        for url in selected {
            sqlx::query("UPDATE source_curated SET curated = 1 WHERE source_url = ?")
                .bind(url)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    Ok((novel_items, comic_items))
}

/// 只读：当前 curated 集合（供搜索 scope=curated 使用）。
pub async fn curated_set(pool: &SqlitePool) -> AppResult<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT source_url FROM source_curated WHERE curated = 1")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// 只读：精选清单（前端展示用）。
pub async fn curated_lists(pool: &SqlitePool) -> AppResult<(Vec<CuratedItem>, Vec<CuratedItem>)> {
    let rows: Vec<(String, i64, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT c.source_url, c.source_type, c.bench_score, c.samples, \
                (SELECT b.name FROM book_sources b WHERE b.id = c.source_url) \
         FROM source_curated c WHERE c.curated = 1 ORDER BY c.source_type, c.bench_score DESC, c.source_url",
    )
    .fetch_all(pool)
    .await?;
    let mut novel = Vec::new();
    let mut comic = Vec::new();
    for (source_url, source_type, bench_score, samples, name) in rows {
        let item = CuratedItem {
            rank: 0,
            source_url: source_url.clone(),
            name: name.unwrap_or(source_url),
            source_type,
            bench_score: bench_score as i32,
            samples,
            cut_gap: None,
        };
        if source_type == TYPE_COMIC {
            comic.push(item);
        } else {
            novel.push(item);
        }
    }
    for (i, item) in novel.iter_mut().enumerate() {
        item.rank = i + 1;
    }
    for (i, item) in comic.iter_mut().enumerate() {
        item.rank = i + 1;
    }
    Ok((novel, comic))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, cat: &str, ty: i64, builtin: bool) -> CorpusEntry {
        CorpusEntry {
            id: title.into(),
            title: title.into(),
            expected_author: None,
            category: cat.into(),
            source_type: ty,
            enabled: true,
            builtin,
            norm_title: normalize_title(title),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn builtin_corpus_has_expected_shape() {
        assert_eq!(BUILTIN_CORPUS.len(), 16);
        let novel = BUILTIN_CORPUS.iter().filter(|b| b.source_type == TYPE_NOVEL).count();
        let comic = BUILTIN_CORPUS.iter().filter(|b| b.source_type == TYPE_COMIC).count();
        assert_eq!(novel, 12);
        assert_eq!(comic, 4);
        let mut seen = HashSet::new();
        for b in BUILTIN_CORPUS {
            assert!(seen.insert((normalize_title(b.title), b.source_type)), "内置语料重复: {}", b.title);
        }
    }

    #[test]
    fn normalization_is_stable() {
        assert_eq!(normalize_title("《斗破苍穹》 最新"), "斗破苍穹最新");
        assert_eq!(normalize_title("ＳＰＹ×ＦＡＭＩＬＹ"), "spyfamily");
        // 繁简不做转换：繁体与简体不同
        assert_ne!(normalize_title("鬥破蒼穹"), normalize_title("斗破苍穹"));
    }

    #[test]
    fn title_match_rules() {
        assert!(title_matches("斗破苍穹", "斗破苍穹"));
        assert!(title_matches("斗破苍穹 最新章节", "斗破苍穹"));
        assert!(title_matches("《斗破苍穹》", "斗破苍穹"));
        // 包含率 < 0.8 不算匹配
        assert!(!title_matches("斗破苍穹之少年归来", "斗破苍穹"));
        assert!(!title_matches("别的书", "斗破苍穹"));
    }

    #[test]
    fn author_match_rules() {
        assert!(author_matches(Some("天蚕土豆"), Some("天蚕土豆")));
        assert!(author_matches(Some("天蚕土豆 著"), Some("天蚕土豆")));
        assert!(author_matches(Some("刘慈欣/王晋康"), Some("刘慈欣")));
        assert!(!author_matches(Some("别人"), Some("天蚕土豆")));
        assert!(!author_matches(None, Some("天蚕土豆")));
        assert!(author_matches(None, None), "无期望作者时不校验");
    }

    #[test]
    fn sample_pick_prefers_builtin_and_rotates_categories() {
        let corpus = vec![
            entry("a1", "玄幻", TYPE_NOVEL, true),
            entry("a2", "玄幻", TYPE_NOVEL, true),
            entry("b1", "都市", TYPE_NOVEL, true),
            entry("c1", "历史", TYPE_NOVEL, true),
            entry("u1", "都市", TYPE_NOVEL, false),
        ];
        let picked = pick_samples(&corpus, TYPE_NOVEL, 5, 0);
        assert_eq!(picked.len(), 5);
        assert!(picked[..4].iter().all(|e| e.builtin), "先覆盖内置（4 条）");
        assert_eq!(picked[4].title, "u1", "用户条目只补足，不挤占内置");
        let cats: HashSet<&str> = picked[..4].iter().map(|e| e.category.as_str()).collect();
        assert_eq!(cats.len(), 3, "内置按 category 轮转覆盖 3 个 category");
        // 漫画源只取 type=2
        let comic = pick_samples(&corpus, TYPE_COMIC, 5, 1);
        assert!(comic.is_empty());
    }

    #[test]
    fn user_corpus_gets_a_slot_even_with_many_builtins() {
        let mut corpus: Vec<CorpusEntry> = (1..=10)
            .map(|i| entry(&format!("内置{i}"), "玄幻", TYPE_NOVEL, true))
            .collect();
        corpus.push(entry("用户新书", "自定义", TYPE_NOVEL, false));
        let picked = pick_samples(&corpus, TYPE_NOVEL, 5, 7);
        assert_eq!(picked.len(), 5);
        assert!(
            picked.iter().any(|e| !e.builtin && e.title == "用户新书"),
            "用户新增书名必须能进入下一批抽样"
        );
        assert_eq!(picked.iter().filter(|e| e.builtin).count(), 4, "内置仍占绝对多数");
    }

    #[test]
    fn curated_gap_truncates_between_min_and_max() {
        let scores = vec![90, 88, 86, 84, 82, 80, 78, 76, 60, 58, 56, 54, 52, 50, 48];
        // 8 分断层在 76→60（第 9 名之前）
        assert_eq!(pick_curated_count(&scores, 8, 14, 8), 8);
        let flat = vec![80; 20];
        assert_eq!(pick_curated_count(&flat, 8, 14, 8), 14, "无断层取上限");
        let small = vec![90, 89, 88];
        assert_eq!(pick_curated_count(&small, 8, 14, 8), 3, "不足下限就全取（但仍要求样本>=2）");
    }

    fn run(url: &str, cat: &str, key: &str, found: bool, suspicious: bool) -> BenchRun {
        BenchRun {
            batch_id: "b".into(),
            source_url: url.into(),
            corpus_key: key.into(),
            title: key.into(),
            category: cat.into(),
            source_type: TYPE_NOVEL,
            builtin_sample: true,
            found,
            title_match: found,
            author_match: true,
            latency_ms: 100,
            error_kind: None,
            suspicious,
            run_at: 1,
        }
    }

    #[test]
    fn suspicious_corpus_is_flagged_and_excluded_from_score() {
        // 3 条内置语料：正常两条 100% 命中，冷门一条全失败
        let corpus = vec![
            entry("正常1", "玄幻", TYPE_NOVEL, true),
            entry("正常2", "玄幻", TYPE_NOVEL, true),
            entry("冷门", "玄幻", TYPE_NOVEL, true),
        ];
        let mut runs = vec![
            run("https://s1", "玄幻", "正常1", true, false),
            run("https://s1", "玄幻", "正常2", true, false),
            run("https://s1", "玄幻", "冷门", false, false),
            run("https://s2", "玄幻", "正常1", true, false),
            run("https://s2", "玄幻", "正常2", true, false),
            run("https://s2", "玄幻", "冷门", false, false),
        ];
        mark_suspicious(&mut runs, &corpus);
        assert!(runs.iter().find(|r| r.corpus_key == "冷门").unwrap().suspicious);
        assert!(!runs.iter().find(|r| r.corpus_key == "正常1").unwrap().suspicious);

        let cfg = Config::from_env();
        let mut src = BookSource::default();
        src.book_source_url = "https://s1".into();
        src.book_source_name = "s1".into();
        let sources = HashMap::from([("https://s1".to_string(), src)]);
        let rows = compute_source_rows(&runs, &sources, &corpus, &cfg, &HashMap::new(), &HashMap::new());
        let row = &rows[0];
        assert_eq!(row.samples, 2, "可疑语料被排除出计分样本");
        assert_eq!(row.excluded_samples, 1);
        assert_eq!(row.hits, 2);
        assert!((row.hit_rate - 1.0).abs() < 1e-9, "剔除冷门语料后命中率不受拖累");
    }

    #[test]
    fn curated_never_includes_non_preferred_or_low_samples() {
        let scores = vec![95, 94, 90];
        let cfg = Config::from_env();
        let eligible: HashSet<String> = ["https://a".into(), "https://c".into()].into_iter().collect();
        let rows = vec![
            SourceBenchRow { source_url: "https://a".into(), name: "a".into(), source_type: TYPE_NOVEL, samples: 5, hits: 5, hit_rate: 1.0, avg_latency_ms: 100, errors: 0, error_kinds: serde_json::json!({}), bench_score: 95, curated: false, excluded_samples: 0, per_category_hit_rate: vec![] },
            SourceBenchRow { source_url: "https://b".into(), name: "b".into(), source_type: TYPE_NOVEL, samples: 5, hits: 5, hit_rate: 1.0, avg_latency_ms: 100, errors: 0, error_kinds: serde_json::json!({}), bench_score: 94, curated: false, excluded_samples: 0, per_category_hit_rate: vec![] },
            SourceBenchRow { source_url: "https://c".into(), name: "c".into(), source_type: TYPE_NOVEL, samples: 1, hits: 1, hit_rate: 1.0, avg_latency_ms: 100, errors: 0, error_kinds: serde_json::json!({}), bench_score: 90, curated: false, excluded_samples: 0, per_category_hit_rate: vec![] },
        ];
        let sel = select_curated(&rows, &cfg, &eligible);
        let urls: Vec<&str> = sel.novel.iter().map(|i| i.source_url.as_str()).collect();
        assert_eq!(urls, vec!["https://a"], "b 非 preferred、c 样本不足都不参与");
        let _ = scores;
    }
}
