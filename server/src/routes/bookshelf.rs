use axum::{
    extract::{Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::error::{AppError, AppResult};
use crate::models::BookshelfItem;
use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/bookshelf", get(list).post(add).delete(remove))
        .route("/api/bookshelf/group", post(set_book_group))
        .route("/api/bookshelf/batch", post(batch))
        .route("/api/bookshelf/seen", post(mark_seen))
        .route("/api/bookshelf/stats", get(shelf_stats))
        .route(
            "/api/bookshelf/groups",
            get(list_groups).post(add_group).delete(delete_group),
        )
        .route("/api/bookshelf/groups/delete", delete(delete_group))
        .route(
            "/api/bookshelf/progress",
            get(get_progress).post(set_progress),
        )
}

#[derive(Deserialize)]
pub struct AddQ {
    book_id: String,
    source_id: Option<String>,
    name: Option<String>,
    author: Option<String>,
    cover: Option<String>,
    /// 加入书架时可选分组（Legado `Book.group`）
    group: Option<String>,
}
#[derive(Deserialize)]
pub struct BookIdQ {
    book_id: String,
}
#[derive(Deserialize)]
pub struct ShelfQ {
    book_id: Option<String>,
    group: Option<String>,
}
#[derive(Deserialize)]
pub struct ProgressQ {
    book_id: String,
    chapter_index: Option<i64>,
    position: Option<f64>,
    total_chapters: Option<i64>,
}
#[derive(Deserialize)]
pub struct GroupQ {
    id: Option<i64>,
    name: Option<String>,
    order: Option<i64>,
    book_sort: Option<i64>,
    /// 分组封面；未传/空串保持原值（COALESCE），传非空才覆盖
    cover: Option<String>,
    /// 是否在书架展示；未传保持原值
    show: Option<bool>,
}
#[derive(Deserialize)]
pub struct GroupIdQ {
    id: i64,
}
/// 把书籍移入/移出分组：支持单本 `{book_id}` 或批量 `{book_ids}`；`group=null` 表示移出分组
#[derive(Deserialize)]
pub struct SetGroupQ {
    book_id: Option<String>,
    book_ids: Option<Vec<String>>,
    group: Option<String>,
}

/// 书架列表 SQL。`position` 必须显式 `CAST(... AS REAL)`：`COALESCE(rp.position, 0)`
/// 在无进度记录时会得到 SQLite INTEGER，而结构体字段是 f64，sqlx 严格解码会报
/// `mismatched types` 把整个列表打成 500（验收 P0）。
/// 总章节数优先 `book_meta.total_chapter_num`，为 0 时回退 `book_stats.total_chapters`。
pub(crate) const SHELF_LIST_SQL: &str = "SELECT bs.book_id, COALESCE(bm.name, bs.title) as title, \
     COALESCE(bm.author, bs.author) as author, COALESCE(bm.cover_url, bs.cover) as cover, \
     bs.source_id, COALESCE(bs.\"group\", bm.\"group\") as \"group\", \
     COALESCE(rp.chapter_index, 0) as chapter_index, CAST(COALESCE(rp.position, 0) AS REAL) as position, \
     COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) as total_chapter_num, \
     COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) as total_chapters, \
     CASE WHEN COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0) > 0 \
          THEN MIN(100.0, (COALESCE(rp.chapter_index, 0) \
               + MIN(1.0, MAX(0.0, CAST(COALESCE(rp.position, 0) AS REAL)))) * 100.0 \
               / COALESCE(NULLIF(bm.total_chapter_num, 0), bst.total_chapters, 0)) \
          ELSE 0.0 END as reading_progress, \
     bs.added_at, rp.updated_at as last_read_at \
     FROM bookshelf bs \
     LEFT JOIN book_meta bm ON bs.book_id = bm.book_url \
     LEFT JOIN book_stats bst ON bs.book_id = bst.book_id \
     LEFT JOIN reading_progress rp ON bs.book_id = rp.book_id \
     WHERE (?1 IS NULL OR COALESCE(bs.\"group\", bm.\"group\") = ?1) \
     ORDER BY COALESCE(rp.updated_at, bs.added_at) DESC";

#[path = "bookshelf_entries.rs"]
mod entries;
pub use entries::*;
#[path = "bookshelf_batch.rs"]
mod batch;
pub use batch::*;
#[path = "bookshelf_groups.rs"]
mod groups;
pub use groups::*;
#[path = "bookshelf_progress.rs"]
mod progress;
pub use progress::*;

#[cfg(test)]
#[path = "bookshelf_tests.rs"]
mod tests;
