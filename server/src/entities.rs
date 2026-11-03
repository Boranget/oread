use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::serde_util::{string_or_bool, string_or_i64};

// ============ 书源及五类规则子对象（对齐 Legado BookSource 契约）============

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BookSource {
    pub book_source_url: String, // 书源唯一 ID（站点根域名）
    pub book_source_name: String,
    #[serde(default)]
    pub book_source_group: Option<String>,
    #[serde(default)]
    pub search_url: Option<String>, // 搜索地址（搜索类书源必填）
    #[serde(default, deserialize_with = "string_or_i64")]
    pub book_source_type: i64, // 0=小说 2=漫画 1=音频（仅 0/2 支持阅读）
    /// 由数据库列写入：非 0/2 类型标记为不支持，默认禁用且不参与搜索
    #[serde(default, skip_serializing)]
    pub unsupported_type: bool,
    #[serde(default)]
    pub book_url_pattern: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    pub custom_order: i64,
    #[serde(default = "default_true", deserialize_with = "string_or_bool")]
    pub enabled: bool,
    #[serde(default, deserialize_with = "string_or_bool")]
    pub enabled_explore: bool,
    #[serde(default)]
    pub js_lib: Option<String>,
    #[serde(default, deserialize_with = "string_or_bool")]
    pub enabled_cookie_jar: bool,
    #[serde(default)]
    pub concurrent_rate: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub login_url: Option<String>,
    #[serde(default)]
    pub login_ui: Option<String>,
    #[serde(default)]
    pub login_check_js: Option<String>,
    #[serde(default)]
    pub cover_decode_js: Option<String>,
    #[serde(default)]
    pub book_source_comment: Option<String>,
    #[serde(default)]
    pub variable_comment: Option<String>,
    #[serde(default, deserialize_with = "string_or_i64")]
    pub last_update_time: i64,
    #[serde(default, deserialize_with = "string_or_i64")]
    pub respond_time: i64,
    #[serde(default, deserialize_with = "string_or_i64")]
    pub weight: i64,
    #[serde(default)]
    pub explore_url: Option<String>,
    #[serde(default)]
    pub explore_screen: Option<String>,
    #[serde(default)]
    pub rule_search: Option<SearchRule>,
    #[serde(default)]
    pub rule_book_info: Option<BookInfoRule>,
    #[serde(default)]
    pub rule_toc: Option<TocRule>,
    #[serde(default)]
    pub rule_content: Option<ContentRule>,
    #[serde(default)]
    pub rule_explore: Option<ExploreRule>,
    /// Legado 的评论/书评规则（服务端不解析，仅原样保留，避免导入导出丢字段）
    #[serde(default)]
    pub rule_review: Option<Value>,
    /// 未建模的新字段（原样保存，导出时无损回写）
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchRule {
    #[serde(default)]
    pub check_key_word: Option<String>,
    #[serde(default)]
    pub book_list: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub last_chapter: Option<String>,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub book_url: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub word_count: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

// 发现规则与搜索规则字段相同（仅无 check_key_word）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExploreRule {
    #[serde(default)]
    pub book_list: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub last_chapter: Option<String>,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub book_url: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub word_count: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BookInfoRule {
    #[serde(default)]
    pub init: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub last_chapter: Option<String>,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub toc_url: Option<String>,
    #[serde(default)]
    pub word_count: Option<String>,
    #[serde(default)]
    pub can_re_name: Option<String>,
    #[serde(default)]
    pub download_urls: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TocRule {
    #[serde(default)]
    pub pre_update_js: Option<String>,
    #[serde(default)]
    pub chapter_list: Option<String>,
    #[serde(default)]
    pub chapter_name: Option<String>,
    #[serde(default)]
    pub chapter_url: Option<String>,
    #[serde(default)]
    pub format_js: Option<String>,
    #[serde(default)]
    pub is_volume: Option<String>,
    #[serde(default)]
    pub is_vip: Option<String>,
    #[serde(default)]
    pub is_pay: Option<String>,
    #[serde(default)]
    pub update_time: Option<String>,
    #[serde(default)]
    pub next_toc_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContentRule {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub next_content_url: Option<String>,
    #[serde(default)]
    pub web_js: Option<String>,
    #[serde(default)]
    pub source_regex: Option<String>,
    #[serde(default)]
    pub replace_regex: Option<String>,
    #[serde(default)]
    pub image_style: Option<String>,
    #[serde(default)]
    pub image_decode: Option<String>,
    #[serde(default)]
    pub pay_action: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

// ============ 核心业务实体 ============

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "snake_case")]
pub struct Book {
    #[serde(default)]
    pub book_url: String,
    #[serde(default)]
    pub toc_url: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub origin_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub custom_tag: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub custom_cover_url: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub custom_intro: Option<String>,
    #[serde(default)]
    pub charset: Option<String>,
    #[serde(default)]
    pub book_source_type: i64,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub latest_chapter_title: Option<String>,
    #[serde(default)]
    pub latest_chapter_time: i64,
    #[sqlx(default)]
    #[serde(default)]
    pub last_check_time: i64,
    #[sqlx(default)]
    #[serde(default)]
    pub last_check_count: i64,
    #[serde(default)]
    pub total_chapter_num: i64,
    #[serde(default)]
    pub dur_chapter_title: Option<String>,
    #[serde(default)]
    pub dur_chapter_index: i64,
    #[serde(default)]
    pub dur_chapter_pos: i64,
    #[serde(default)]
    pub dur_chapter_time: i64,
    #[serde(default)]
    pub word_count: Option<String>,
    #[serde(default)]
    pub can_update: bool,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub origin_order: i64,
    #[serde(default)]
    pub variable: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SearchBook {
    #[serde(default)]
    pub book_url: String,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub origin_name: Option<String>,
    #[serde(rename = "type", default)]
    pub book_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
    #[serde(default)]
    pub word_count: Option<String>,
    #[serde(default)]
    pub latest_chapter_title: Option<String>,
    #[serde(default)]
    pub toc_url: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub variable: Option<String>,
    #[serde(default)]
    pub origin_order: i64,
    #[serde(default)]
    pub chapter_word_count_text: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "snake_case")]
pub struct BookChapter {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_volume: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub book_url: Option<String>,
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub is_vip: bool,
    #[serde(default)]
    pub is_pay: bool,
    #[serde(default)]
    pub resource_url: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub start: i64,
    #[serde(default)]
    pub end: i64,
    /// 来源站点给出的章节更新时间（原样字符串，未知为 null）
    #[serde(default)]
    pub update_time: Option<String>,
    /// 正文字数（未知为 0；正文缓存落库时回填）
    #[serde(default)]
    pub word_count: i64,
    /// 所属分卷标题：分卷行返回自身标题，普通章节返回最近的前置分卷（目录分组用）
    #[sqlx(default)]
    #[serde(default)]
    pub volume_title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "snake_case")]
pub struct BookContent {
    #[serde(default)]
    pub book_url: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub chapter_index: i64,
    #[serde(default)]
    pub resource_url: Option<String>,
    #[serde(default)]
    pub is_local: bool,
    /// `text`（默认）或 `image`；老缓存行由迁移填 `text`
    #[sqlx(default)]
    #[serde(default)]
    pub content_type: String,
    /// 图片章节缓存（`{images:[...], truncated}` 的 JSON）；文本章节为 None
    #[sqlx(default)]
    #[serde(skip)]
    pub images_json: Option<String>,
}

/// 图片章节的单张图片。`index` 与页面顺序一致，从 1 开始。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookImage {
    pub index: i64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// 已知的字节数（源页面通常不给，代理抓取后可补）；未知为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// 预取优先级：前三张为 `high`，其余为 `normal`（KPW3 端按此分批加载）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// 书源类型的中文名（0 小说 / 2 漫画 / 1 有声 / 其他）
pub fn source_type_name(t: i64) -> &'static str {
    match t {
        0 => "小说",
        2 => "漫画",
        1 => "有声",
        _ => "其他",
    }
}

/// 阅读器支持的书源类型：仅小说(0)与漫画(2)
pub fn is_supported_source_type(t: i64) -> bool {
    matches!(t, 0 | 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_type_names_and_support() {
        assert_eq!(source_type_name(0), "小说");
        assert_eq!(source_type_name(2), "漫画");
        assert_eq!(source_type_name(1), "有声");
        assert_eq!(source_type_name(7), "其他");
        assert!(is_supported_source_type(0));
        assert!(is_supported_source_type(2));
        assert!(!is_supported_source_type(1));
        assert!(!is_supported_source_type(3));
    }

    #[test]
    fn book_source_type_accepts_legado_string() {
        let json = r#"{"book_source_url":"https://x","book_source_name":"n","book_source_type":"2"}"#;
        let s: BookSource = serde_json::from_str(json).unwrap();
        assert_eq!(s.book_source_type, 2);
        let json2 = r#"{"book_source_url":"https://x","book_source_name":"n"}"#;
        let s2: BookSource = serde_json::from_str(json2).unwrap();
        assert_eq!(s2.book_source_type, 0, "缺失按 0（小说）");
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BookGroup {
    #[serde(default)]
    pub group_id: i64,
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub enable_refresh: bool,
    #[serde(default)]
    pub show: bool,
    #[serde(default)]
    pub book_sort: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "snake_case")]
pub struct ReplaceRule {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub replacement: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub scope_title: bool,
    #[serde(default)]
    pub scope_content: bool,
    #[serde(default)]
    pub exclude_scope: Option<String>,
    #[serde(default)]
    pub is_enabled: bool,
    #[serde(default)]
    pub is_regex: bool,
    #[sqlx(default)]
    #[serde(default)]
    pub timeout_millisecond: i64,
    #[serde(default)]
    pub order: i64,
}

/// 服务端只保留解析相关的阅读配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UserSettings {
    #[serde(default = "default_true")]
    pub use_replace_rule: bool,
    #[serde(default)]
    pub image_style: i64,
}
