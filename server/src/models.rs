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
    /// 书源权重（越大越靠前）
    #[sqlx(default)]
    pub weight: i64,
    /// 用户自定义顺序（越小越靠前）
    #[sqlx(default)]
    pub custom_order: i64,
    /// 书源类型（0 小说 / 2 漫画 / 1 有声 / 其他），输出字段名为 `type`
    #[serde(rename = "type")]
    #[sqlx(default)]
    pub book_source_type: i64,
    /// 类型中文名（小说/漫画/有声/其他）
    #[serde(default)]
    pub type_name: String,
    /// 非 0/2 类型：保留数据但不参与搜索，默认禁用
    #[serde(default)]
    pub unsupported_type: bool,
}

#[derive(Debug, FromRow, serde::Serialize)]
pub struct BookshelfItem {
    pub book_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub cover: Option<String>,
    pub source_id: Option<String>,
    /// 书架分组（Legado `Book.group`）
    #[sqlx(default)]
    pub group: Option<String>,
    /// 最近阅读章节索引（无进度时为 0）
    #[sqlx(default)]
    pub chapter_index: i64,
    /// 章节内滚动位置（0~1 百分比）
    #[sqlx(default)]
    pub position: f64,
    /// 目录总章节数（用于前端展示完成度）
    #[sqlx(default)]
    pub total_chapter_num: i64,
    /// 总章节数别名（前端契约使用 `total_chapters`）
    #[sqlx(default)]
    pub total_chapters: i64,
    /// 章节完成度（0~100，按已读章节/总章节）
    #[sqlx(default)]
    pub reading_progress: f64,
    /// 入架时间
    #[sqlx(default)]
    pub added_at: Option<String>,
    /// 最近一次阅读进度更新时间（无进度时为 null）
    #[sqlx(default)]
    pub last_read_at: Option<String>,
    /// 是否参与目录更新（列表 SQL 不取，由 list 单独回填）
    #[sqlx(default)]
    #[serde(default)]
    pub enable_update: bool,
    /// 未读新章节数（来自 book_updates，0 表示无角标）
    #[sqlx(default)]
    #[serde(default)]
    pub new_chapters: i64,
}
