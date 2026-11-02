use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::auth::{AuthUser, UserDb};
use crate::entities::{Book, BookChapter, BookContent, SearchBook};
use crate::error::{AppError, AppResult};
use crate::routes::AppState;
use crate::routes::sources::load_sources;
use crate::web_book;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/search", get(search))
        .route("/api/explore", get(explore))
        .route("/api/book/info", get(book_info))
        .route("/api/book/toc", get(toc))
        .route("/api/book/content", get(content))
}

#[derive(Deserialize)]
pub struct SearchQ {
    key: String,
    page: Option<i32>,
}
#[derive(Deserialize)]
pub struct ExploreQ {
    url: String,
}
#[derive(Deserialize)]
pub struct BookQ {
    source: String,
    book_url: String,
}
#[derive(Deserialize)]
pub struct ContentQ {
    source: String,
    chapter_url: String,
}

async fn search(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<SearchQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let page = q.page.unwrap_or(1);
    let books = web_book::search_all(&sources, &q.key, page).await;
    Ok(Json(books))
}

async fn explore(
    State(s): State<AppState>,
    _u: AuthUser,
    Query(q): Query<ExploreQ>,
) -> AppResult<Json<Vec<SearchBook>>> {
    let sources = load_sources(&s.shared).await?;
    let books = web_book::explore_all(&sources, &q.url).await;
    Ok(Json(books))
}

async fn book_info(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Book>> {
    let sources = load_sources(&s.shared).await?;
    let source = sources
        .into_iter()
        .find(|x| x.book_source_url == q.source)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut book = Book {
        book_url: q.book_url.clone(),
        origin: Some(source.book_source_url.clone()),
        origin_name: Some(source.book_source_name.clone()),
        ..Default::default()
    };
    web_book::book_info(&source, &mut book).await?;
    save_book_meta(&db.0, &book).await?;
    Ok(Json(book))
}

async fn toc(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<BookQ>,
) -> AppResult<Json<Vec<BookChapter>>> {
    let sources = load_sources(&s.shared).await?;
    let source = sources
        .into_iter()
        .find(|x| x.book_source_url == q.source)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let mut book = Book {
        book_url: q.book_url.clone(),
        ..Default::default()
    };
    if let Some(m) = load_book_meta(&db.0, &q.book_url).await? {
        book.toc_url = m.toc_url;
    }
    let chapters = web_book::chapter_list(&source, &book).await?;
    save_chapters(&db.0, &chapters).await?;
    Ok(Json(chapters))
}

async fn content(
    State(s): State<AppState>,
    _u: AuthUser,
    db: UserDb,
    Query(q): Query<ContentQ>,
) -> AppResult<Json<BookContent>> {
    let sources = load_sources(&s.shared).await?;
    let source = sources
        .into_iter()
        .find(|x| x.book_source_url == q.source)
        .ok_or_else(|| AppError::NotFound("source not found".into()))?;
    let chapter = BookChapter {
        url: q.chapter_url.clone(),
        ..Default::default()
    };
    let c = web_book::chapter_content(&source, &chapter).await?;
    save_content(&db.0, &c).await?;
    Ok(Json(c))
}

// ---------------- 缓存落库辅助 ----------------

async fn save_book_meta(pool: &SqlitePool, b: &Book) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO book_meta \
         (book_url, toc_url, origin, origin_name, name, author, kind, cover_url, intro, charset, \
          book_source_type, \"group\", latest_chapter_title, latest_chapter_time, total_chapter_num, \
          dur_chapter_title, dur_chapter_index, dur_chapter_pos, dur_chapter_time, word_count, \
          can_update, \"order\", origin_order, variable, custom_tag, custom_cover_url, custom_intro) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27) \
         ON CONFLICT(book_url) DO UPDATE SET \
          toc_url=excluded.toc_url, origin=excluded.origin, origin_name=excluded.origin_name, \
          name=excluded.name, author=excluded.author, kind=excluded.kind, cover_url=excluded.cover_url, \
          intro=excluded.intro, charset=excluded.charset, book_source_type=excluded.book_source_type, \
          \"group\"=excluded.\"group\", latest_chapter_title=excluded.latest_chapter_title, \
          latest_chapter_time=excluded.latest_chapter_time, total_chapter_num=excluded.total_chapter_num, \
          dur_chapter_title=excluded.dur_chapter_title, dur_chapter_index=excluded.dur_chapter_index, \
          dur_chapter_pos=excluded.dur_chapter_pos, dur_chapter_time=excluded.dur_chapter_time, \
          word_count=excluded.word_count, can_update=excluded.can_update, \"order\"=excluded.\"order\", \
          origin_order=excluded.origin_order, variable=excluded.variable, custom_tag=excluded.custom_tag, \
          custom_cover_url=excluded.custom_cover_url, custom_intro=excluded.custom_intro",
    )
    .bind(&b.book_url)
    .bind(&b.toc_url)
    .bind(&b.origin)
    .bind(&b.origin_name)
    .bind(&b.name)
    .bind(&b.author)
    .bind(&b.kind)
    .bind(&b.cover_url)
    .bind(&b.intro)
    .bind(&b.charset)
    .bind(b.book_source_type)
    .bind(&b.group)
    .bind(&b.latest_chapter_title)
    .bind(b.latest_chapter_time)
    .bind(b.total_chapter_num)
    .bind(&b.dur_chapter_title)
    .bind(b.dur_chapter_index)
    .bind(b.dur_chapter_pos)
    .bind(b.dur_chapter_time)
    .bind(&b.word_count)
    .bind(b.can_update)
    .bind(b.order)
    .bind(b.origin_order)
    .bind(&b.variable)
    .bind(&b.custom_tag)
    .bind(&b.custom_cover_url)
    .bind(&b.custom_intro)
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_book_meta(pool: &SqlitePool, book_url: &str) -> anyhow::Result<Option<Book>> {
    let b: Option<Book> = sqlx::query_as::<_, Book>("SELECT * FROM book_meta WHERE book_url = ?")
        .bind(book_url)
        .fetch_optional(pool)
        .await?;
    Ok(b)
}

async fn save_chapters(pool: &SqlitePool, chapters: &[BookChapter]) -> anyhow::Result<()> {
    for c in chapters {
        sqlx::query(
            "INSERT INTO book_chapters \
             (url, book_url, title, is_volume, base_url, \"index\", is_vip, is_pay, resource_url, tag, start, \"end\") \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, is_volume=excluded.is_volume, \
              base_url=excluded.base_url, \"index\"=excluded.\"index\", is_vip=excluded.is_vip, \
              is_pay=excluded.is_pay, resource_url=excluded.resource_url, tag=excluded.tag, \
              start=excluded.start, \"end\"=excluded.\"end\"",
        )
        .bind(&c.url)
        .bind(&c.book_url)
        .bind(&c.title)
        .bind(c.is_volume)
        .bind(&c.base_url)
        .bind(c.index)
        .bind(c.is_vip)
        .bind(c.is_pay)
        .bind(&c.resource_url)
        .bind(&c.tag)
        .bind(c.start)
        .bind(c.end)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn save_content(pool: &SqlitePool, c: &BookContent) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO book_content (url, book_url, title, content, chapter_index, resource_url, is_local) \
         VALUES (?1,?2,?3,?4,?5,?6,?7) \
         ON CONFLICT(url) DO UPDATE SET title=excluded.title, content=excluded.content, \
          chapter_index=excluded.chapter_index, resource_url=excluded.resource_url, is_local=excluded.is_local",
    )
    .bind(&c.url)
    .bind(&c.book_url)
    .bind(&c.title)
    .bind(&c.content)
    .bind(c.chapter_index)
    .bind(&c.resource_url)
    .bind(c.is_local)
    .execute(pool)
    .await?;
    Ok(())
}
