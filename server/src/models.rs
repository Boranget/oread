use sqlx::FromRow;

#[derive(Debug, FromRow, serde::Serialize)]
pub struct BookSourceListItem {
    #[sqlx(rename = "id")]
    pub book_source_url: String,
    #[sqlx(rename = "name")]
    pub book_source_name: String,
    pub enabled: bool,
    #[sqlx(default)]
    pub book_source_group: Option<String>,
}

#[derive(Debug, FromRow, serde::Serialize)]
pub struct BookshelfItem {
    pub book_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub cover: Option<String>,
    pub source_id: Option<String>,
}
