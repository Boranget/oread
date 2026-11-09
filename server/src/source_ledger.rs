//! 导入账本 + 本地库决策（活跃 / 隔离）。
//!
//! 三层模型：
//! - A 本地源库：`book_sources` 是唯一搜索来源；`source_library_state` 记录当前决策。
//! - B 导入账本：`import_batches` / `import_items` 记录每次导入与逐条快照，用于追溯和 diff。
//! - C 隔离区：`decision=quarantined` 的源仍保留原始定义（仍在 `book_sources`），
//!   只是不再参与搜索/巡检；上游恢复或引擎补能力后可通过再次导入复活。
//!
//! **健康与决策都以 `bookSourceUrl`（源条目）为键，绝不按域名记账**：
//! 同站多源变体各自独立，一源多域也不会被域名归并误伤。

use std::collections::HashMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};

use crate::entities::BookSource;
use crate::error::AppResult;

pub const DECISION_ACTIVE: &str = "active";
pub const DECISION_QUARANTINED: &str = "quarantined";
pub const REVIEW_UPDATE_REJECTED: &str = "update_rejected";
pub const REVIEW_CONFLICT: &str = "conflict";

/// 纳入“规则哈希”的字段：只比较规则主体（含请求头/JS 库/URL 模板），
/// 排除 `enabled`、分组、排序、权重、更新时间等非规则字段，
/// 避免用户日常启停被误判成“本地被改过”。
const RULE_KEYS: &[&str] = &[
    "book_source_url",
    "book_source_type",
    "book_url_pattern",
    "search_url",
    "explore_url",
    "explore_screen",
    "header",
    "js_lib",
    "concurrent_rate",
    "login_url",
    "login_ui",
    "login_check_js",
    "cover_decode_js",
    "variable_comment",
    "rule_search",
    "rule_book_info",
    "rule_toc",
    "rule_content",
    "rule_explore",
    "rule_review",
    "extra",
];

/// 规则主体（用于哈希）：只取规则字段。
pub fn rule_scope_value(src: &BookSource) -> Value {
    let full = serde_json::to_value(src).unwrap_or(Value::Null);
    let mut out = Map::new();
    if let Some(obj) = full.as_object() {
        for key in RULE_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// 规则规范化哈希（键排序后 SHA-256）。用于判断“上游规则是否变化/本地是否被人工改过”。
pub fn rule_hash(src: &BookSource) -> String {
    sha256_hex(&canonical_json(&rule_scope_value(src)))
}

/// 原始定义快照（完整 JSON，保留规则主体，便于回滚与追溯）。
/// 书源通常几十 KB；SQLite 文本容量足够，这里不做截断，避免破坏 JSON 结构。
pub fn snapshot_json(src: &BookSource) -> String {
    serde_json::to_string(src).unwrap_or_else(|_| "{}".to_string())
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// 规范化 JSON：对象键排序，数组保持顺序，去除空白差异。
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            out.push('{');
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_else(|_| "\"\"".into()));
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => {
            out.push_str(&other.to_string());
        }
    }
}

/// 书库状态行（映射 `library_state` 表）；部分列只用于入库/审计，当前读取路径未全部消费。
#[allow(dead_code)]
#[derive(Debug, Clone, Default, FromRow)]
pub struct LibraryState {
    pub source_url: String,
    pub decision: String,
    pub reason: Option<String>,
    pub first_imported_at: Option<i64>,
    pub last_imported_at: Option<i64>,
    pub last_batch_id: Option<String>,
    pub content_sha: Option<String>,
    pub review_state: Option<String>,
    pub review_reason: Option<String>,
    pub review_at: Option<i64>,
}

impl LibraryState {
    pub fn is_quarantined(&self) -> bool {
        self.decision == DECISION_QUARANTINED
    }
}

#[derive(Debug, Clone)]
pub struct ItemRecord {
    pub source_url: String,
    pub definition: String,
    pub content_sha: String,
    pub probe_status: String,
    pub decision: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BatchRecord {
    pub id: String,
    pub source: String,
    pub source_kind: String,
    pub imported_at: i64,
    pub item_count: usize,
    pub raw_sha: Option<String>,
    pub probe_summary: Option<String>,
}

pub async fn load_states(pool: &SqlitePool) -> AppResult<HashMap<String, LibraryState>> {
    let rows: Vec<LibraryState> = sqlx::query_as(
        "SELECT source_url, decision, reason, first_imported_at, last_imported_at, \
         last_batch_id, content_sha, review_state, review_reason, review_at \
         FROM source_library_state",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.source_url.clone(), r))
        .collect())
}

pub async fn quarantined_set(pool: &SqlitePool) -> AppResult<std::collections::HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT source_url FROM source_library_state WHERE decision = 'quarantined'",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_state(
    pool: &SqlitePool,
    source_url: &str,
    decision: &str,
    reason: Option<&str>,
    content_sha: Option<&str>,
    batch_id: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO source_library_state \
         (source_url, decision, reason, first_imported_at, last_imported_at, last_batch_id, content_sha) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6) \
         ON CONFLICT(source_url) DO UPDATE SET \
          decision=excluded.decision, \
          reason=excluded.reason, \
          last_imported_at=excluded.last_imported_at, \
          last_batch_id=excluded.last_batch_id, \
          content_sha=COALESCE(excluded.content_sha, source_library_state.content_sha)",
    )
    .bind(source_url)
    .bind(decision)
    .bind(reason)
    .bind(now)
    .bind(batch_id)
    .bind(content_sha)
    .execute(pool)
    .await?;
    Ok(())
}

/// 仅更新人工复核状态（update_rejected / conflict），不改 decision 与 content_sha。
pub async fn set_review(
    pool: &SqlitePool,
    source_url: &str,
    state: &str,
    reason: Option<&str>,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO source_library_state (source_url, decision, review_state, review_reason, review_at) \
         VALUES (?1, 'active', ?2, ?3, ?4) \
         ON CONFLICT(source_url) DO UPDATE SET \
          review_state=excluded.review_state, \
          review_reason=excluded.review_reason, \
          review_at=excluded.review_at",
    )
    .bind(source_url)
    .bind(state)
    .bind(reason)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 隔离一条源（保留原始定义与历史账本，不物理删除）。
pub async fn quarantine(
    pool: &SqlitePool,
    source_url: &str,
    reason: Option<&str>,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO source_library_state \
         (source_url, decision, reason, first_imported_at, last_imported_at) \
         VALUES (?1, 'quarantined', ?2, ?3, ?3) \
         ON CONFLICT(source_url) DO UPDATE SET \
          decision='quarantined', reason=excluded.reason, last_imported_at=excluded.last_imported_at",
    )
    .bind(source_url)
    .bind(reason)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 自动隔离原因：可以被体检/巡检的“有效响应”自动复活。
///
/// 人工隔离（`manual_quarantine`）与清理隔离（`pruned_site_fail`）必须保持，
/// 否则用户/清理策略的显式决定会被后台体检悄悄推翻。
/// 历史数据里也可能存在 `reason IS NULL` 的自动隔离行，一并视为可复活。
pub fn is_auto_quarantine_reason(reason: Option<&str>) -> bool {
    let Some(raw) = reason else {
        return true;
    };
    let r = raw.trim();
    r.is_empty()
        || r.starts_with("site_unreachable")
        || r.starts_with("quality_quarantine")
        || r.starts_with("engine_limited")
        || r.starts_with("bench:")
        || r.starts_with("全局搜索预算耗尽")
}

/// 体检/巡检拿到有效响应后复活自动隔离源：
/// - 只处理 `decision='quarantined'` 且原因是自动隔离的行；
/// - 同时把导入隔离时强制写下的 `enabled=0` 恢复为 1；
/// - 返回实际复活的源数量。
///
/// 调用方必须只传“本轮确实拿到有效响应”的源，不能用整批源列表。
pub async fn revive_auto(pool: &SqlitePool, source_urls: &[String]) -> AppResult<usize> {
    if source_urls.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    let mut revived = 0usize;
    for url in source_urls {
        let res = sqlx::query(
            "UPDATE source_library_state SET decision = 'active', reason = NULL \
             WHERE source_url = ?1 AND decision = 'quarantined' \
               AND (reason IS NULL OR TRIM(reason) = '' \
                    OR reason LIKE 'site_unreachable%' \
                    OR reason LIKE 'quality_quarantine%' \
                    OR reason LIKE 'engine_limited%' \
                    OR reason LIKE 'bench:%' \
                    OR reason LIKE '全局搜索预算耗尽%')",
        )
        .bind(url)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            continue;
        }
        sqlx::query("UPDATE book_sources SET enabled = 1 WHERE id = ?1 AND enabled = 0")
            .bind(url)
            .execute(&mut *tx)
            .await?;
        revived += 1;
    }
    tx.commit().await?;
    Ok(revived)
}

pub async fn clear_review(pool: &SqlitePool, source_url: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE source_library_state SET review_state = NULL, review_reason = NULL, review_at = NULL \
         WHERE source_url = ?",
    )
    .bind(source_url)
    .execute(pool)
    .await?;
    Ok(())
}

/// 同一来源上一次导入的原始合集哈希（用于“合集没变就完全不探测”）。
pub async fn last_batch_sha(pool: &SqlitePool, source: &str) -> AppResult<Option<String>> {
    let sha: Option<String> = sqlx::query_scalar(
        "SELECT raw_sha FROM import_batches WHERE source = ? ORDER BY imported_at DESC LIMIT 1",
    )
    .bind(source)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(sha)
}

/// 同一来源上一批的书源 URL 集合（用于报告 `local_only`：上游删了但我们保留）。
pub async fn last_batch_urls(
    pool: &SqlitePool,
    source: &str,
) -> AppResult<std::collections::HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT i.source_url FROM import_items i \
         JOIN import_batches b ON b.id = i.batch_id \
         WHERE b.source = ? AND b.id = ( \
             SELECT id FROM import_batches WHERE source = ? ORDER BY imported_at DESC LIMIT 1 \
         )",
    )
    .bind(source)
    .bind(source)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

pub async fn record_batch(
    pool: &SqlitePool,
    batch: &BatchRecord,
    items: &[ItemRecord],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO import_batches (id, source, source_kind, imported_at, item_count, raw_sha, probe_summary) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(&batch.id)
    .bind(&batch.source)
    .bind(&batch.source_kind)
    .bind(batch.imported_at)
    .bind(batch.item_count as i64)
    .bind(&batch.raw_sha)
    .bind(&batch.probe_summary)
    .execute(&mut *tx)
    .await?;
    for it in items {
        sqlx::query(
            "INSERT INTO import_items \
             (batch_id, source_url, definition, content_sha, probe_status, decision, reason, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&batch.id)
        .bind(&it.source_url)
        .bind(&it.definition)
        .bind(&it.content_sha)
        .bind(&it.probe_status)
        .bind(&it.decision)
        .bind(&it.reason)
        .bind(batch.imported_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造对象，便于对照默认值。
#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str, rule: &str, enabled: bool) -> BookSource {
        let mut s = BookSource::default();
        s.book_source_url = url.into();
        s.book_source_name = "n".into();
        s.search_url = Some("https://x.com/search?q={key}".into());
        s.rule_search = Some(crate::entities::SearchRule {
            book_list: Some(rule.into()),
            ..Default::default()
        });
        s.enabled = enabled;
        s
    }

    #[test]
    fn rule_hash_ignores_enabled_and_name() {
        let a = src("https://a.com", ".x", true);
        let mut b = a.clone();
        b.enabled = false;
        b.book_source_name = "改名".into();
        b.book_source_group = Some("g".into());
        assert_eq!(rule_hash(&a), rule_hash(&b), "启停/改名不应算规则变化");
    }

    #[test]
    fn rule_hash_detects_rule_and_header_changes() {
        let a = src("https://a.com", ".x", true);
        let b = src("https://a.com", ".y", true);
        assert_ne!(rule_hash(&a), rule_hash(&b));
        let mut c = a.clone();
        c.header = Some(r#"{"Authorization":"Bearer t"}"#.into());
        assert_ne!(rule_hash(&a), rule_hash(&c));
    }

    #[test]
    fn canonical_json_key_order_is_stable() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE import_batches (id TEXT PRIMARY KEY, source TEXT NOT NULL, \
             source_kind TEXT NOT NULL, imported_at INTEGER NOT NULL, item_count INTEGER NOT NULL, \
             raw_sha TEXT, probe_summary TEXT); \
             CREATE TABLE import_items (batch_id TEXT NOT NULL, source_url TEXT NOT NULL, \
             definition TEXT NOT NULL, content_sha TEXT NOT NULL, probe_status TEXT NOT NULL, \
             decision TEXT NOT NULL, reason TEXT, created_at INTEGER NOT NULL, \
             PRIMARY KEY (batch_id, source_url)); \
             CREATE TABLE source_library_state (source_url TEXT PRIMARY KEY, \
             decision TEXT NOT NULL DEFAULT 'active', reason TEXT, first_imported_at INTEGER, \
             last_imported_at INTEGER, last_batch_id TEXT, content_sha TEXT, \
             review_state TEXT, review_reason TEXT, review_at INTEGER); \
             CREATE TABLE book_sources (id TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[test]
    fn auto_quarantine_reason_classification() {
        for reason in [
            None,
            Some(""),
            Some("site_unreachable: timeout"),
            Some("quality_quarantine: score=12"),
            Some("engine_limited: no items"),
            Some("bench: no usable response"),
        ] {
            assert!(
                is_auto_quarantine_reason(reason),
                "{reason:?} 应视为可自动复活"
            );
        }
        for reason in [
            Some("manual_quarantine"),
            Some("pruned_site_fail"),
            Some("unknown_reason"),
        ] {
            assert!(
                !is_auto_quarantine_reason(reason),
                "{reason:?} 不应被体检悄悄复活"
            );
        }
    }

    #[tokio::test]
    async fn revive_auto_only_restores_automatic_quarantine() {
        let pool = test_pool().await;
        for url in ["https://auto", "https://manual", "https://pruned"] {
            sqlx::query("INSERT INTO book_sources (id, enabled) VALUES (?, 0)")
                .bind(url)
                .execute(&pool)
                .await
                .unwrap();
        }
        quarantine(&pool, "https://auto", Some("site_unreachable: x"), 1000)
            .await
            .unwrap();
        quarantine(&pool, "https://manual", Some("manual_quarantine"), 1000)
            .await
            .unwrap();
        quarantine(&pool, "https://pruned", Some("pruned_site_fail"), 1000)
            .await
            .unwrap();

        let revived = revive_auto(
            &pool,
            &[
                "https://auto".into(),
                "https://manual".into(),
                "https://pruned".into(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(revived, 1, "只复活自动隔离源");

        let states = load_states(&pool).await.unwrap();
        assert!(!states.get("https://auto").unwrap().is_quarantined());
        assert!(states.get("https://manual").unwrap().is_quarantined());
        assert!(states.get("https://pruned").unwrap().is_quarantined());

        let enabled: Vec<(String, i64)> =
            sqlx::query_as("SELECT id, enabled FROM book_sources ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            enabled,
            vec![
                ("https://auto".into(), 1),
                ("https://manual".into(), 0),
                ("https://pruned".into(), 0),
            ]
        );
    }

    #[tokio::test]
    async fn ledger_roundtrip_quarantine_and_batch_index() {
        let pool = test_pool().await;
        let batch = BatchRecord {
            id: "b1".into(),
            source: "boranlib-sources.json".into(),
            source_kind: "file".into(),
            imported_at: 1000,
            item_count: 1,
            raw_sha: Some("abc".into()),
            probe_summary: None,
        };
        record_batch(
            &pool,
            &batch,
            &[ItemRecord {
                source_url: "https://a.com".into(),
                definition: "{}".into(),
                content_sha: "sha-a".into(),
                probe_status: "ok".into(),
                decision: DECISION_ACTIVE.into(),
                reason: Some("added".into()),
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            last_batch_sha(&pool, "boranlib-sources.json")
                .await
                .unwrap(),
            Some("abc".into())
        );
        assert!(last_batch_urls(&pool, "boranlib-sources.json")
            .await
            .unwrap()
            .contains("https://a.com"));

        assert!(quarantined_set(&pool).await.unwrap().is_empty());
        quarantine(&pool, "https://a.com", Some("site_unreachable"), 2000)
            .await
            .unwrap();
        assert!(quarantined_set(&pool)
            .await
            .unwrap()
            .contains("https://a.com"));
        let states = load_states(&pool).await.unwrap();
        let st = states.get("https://a.com").unwrap();
        assert!(st.is_quarantined());

        // 复活：再次写回 active
        upsert_state(
            &pool,
            "https://a.com",
            DECISION_ACTIVE,
            None,
            Some("sha-a"),
            "b2",
            3000,
        )
        .await
        .unwrap();
        assert!(!quarantined_set(&pool)
            .await
            .unwrap()
            .contains("https://a.com"));
        let states = load_states(&pool).await.unwrap();
        assert!(!states.get("https://a.com").unwrap().is_quarantined());
    }

    #[test]
    fn same_url_variants_are_independent() {
        // 同域不同 URL 的两个变体：哈希按 URL 区分，不能合并
        let a = src("https://a.com", ".x", true);
        let b = src("https://a.com/v2", ".x", true);
        assert_ne!(rule_hash(&a), rule_hash(&b));
    }
}
