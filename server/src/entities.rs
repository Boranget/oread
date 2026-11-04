use serde::Deserializer;
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
    /// 每源在飞请求上限的 Legado 原始字段（`concurrentRate`，形如 `"3/1"`）。
    /// 解析规则见 `req::parse_source_concurrency`（取整数部分、夹 1–8、缺省默认 8）。
    #[serde(default, alias = "concurrentRate")]
    pub concurrent_rate: Option<String>,
    /// 单源请求超时（毫秒，oread 扩展字段）。0/负数/缺失/非法 → 回退全局
    /// `HTTP_TIMEOUT_SECS`；有效值由 `req` 层夹取到 1000–60000。
    #[serde(default, deserialize_with = "lenient_i64")]
    pub timeout_millisecond: i64,
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

/// 宽松解析 i64：数字、数字字符串都接受；空串/非法值/其它类型一律归 0。
/// 用于书源扩展字段——单个字段写坏不应让整份书源导入失败。
fn lenient_i64<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(d)?;
    Ok(match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    })
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
    /// 结果来源源的 tier（preferred / observing），仅用于排查与展示
    #[serde(default)]
    pub tier: Option<String>,
    /// 结果来源源的质量分
    #[serde(default)]
    pub score: Option<i64>,
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
    /// 去重后命中的全部书源（展示源名；同书多源 → 多元素）。
    /// 前端/`/k` 用它展示「N 个源」，也是多源共识加分的依据。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<String>,
    /// 相关性档位：`strong`（书名命中）/ `author`（只命中作者）/
    /// `partial`（只命中查询片段，可能相关）。空查询时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance: Option<String>,
    /// 稳定排序桶（仅 job 分页使用）：翻页游标 `(rank_bucket, rank_seq)`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_bucket: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_seq: Option<u64>,
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
    /// 写入时的解析管线指纹；旧记录为 None，读取时视为未命中并重建
    #[sqlx(default)]
    #[serde(skip)]
    pub cache_version: Option<String>,
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

/// 书源是否声明“需要登录”才能使用。
///
/// 口径（Legado 契约）：`loginUrl` / `loginUi` / `loginCheckJs` 任一非空即视为需登录。
/// 判定完全来自规则 JSON，不落库、不改 `enabled`；规则被编辑后下一次加载自动重算。
/// 导入管线与加载路径都调用本函数，保证只有一套口径。
pub fn requires_login(src: &BookSource) -> bool {
    [&src.login_url, &src.login_ui, &src.login_check_js]
        .into_iter()
        .flatten()
        .any(|v| !v.trim().is_empty())
}

/// 可搜索源的唯一运行时口径（与书源规模无关，空库/单源同样适用）：
/// 启用 + 支持的类型 + 搜索规则与搜索地址齐全 + 不要求登录。
///
/// 注意：隔离区与冷却由调用方按数据库状态另行过滤；本函数只做规则层的静态判定。
pub fn is_searchable_source(src: &BookSource) -> bool {
    src.enabled
        && is_supported_source_type(src.book_source_type)
        && src.rule_search.is_some()
        && src.search_url.is_some()
        && !requires_login(src)
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
        let json =
            r#"{"book_source_url":"https://x","book_source_name":"n","book_source_type":"2"}"#;
        let s: BookSource = serde_json::from_str(json).unwrap();
        assert_eq!(s.book_source_type, 2);
        let json2 = r#"{"book_source_url":"https://x","book_source_name":"n"}"#;
        let s2: BookSource = serde_json::from_str(json2).unwrap();
        assert_eq!(s2.book_source_type, 0, "缺失按 0（小说）");
    }

    fn searchable_base() -> BookSource {
        BookSource {
            book_source_url: "https://x.example".into(),
            book_source_name: "x".into(),
            enabled: true,
            search_url: Some("https://x.example/search?q={key}".into()),
            rule_search: Some(SearchRule {
                book_list: Some(".book".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn login_fields_make_source_unsearchable() {
        // 任一登录字段非空 -> 判否；enabled 保持不变
        for set in [
            (|s: &mut BookSource| s.login_url = Some("https://x.example/login".into()))
                as fn(&mut BookSource),
            |s: &mut BookSource| s.login_ui = Some("账号:${account}".into()),
            |s: &mut BookSource| s.login_check_js = Some("cookie.getCookie('token')".into()),
        ] {
            let mut src = searchable_base();
            set(&mut src);
            assert!(requires_login(&src));
            assert!(!is_searchable_source(&src));
            assert!(src.enabled, "登录判定不得改写 enabled");
        }
    }

    #[test]
    fn source_without_login_fields_stays_searchable() {
        let src = searchable_base();
        assert!(!requires_login(&src));
        assert!(is_searchable_source(&src));
    }

    #[test]
    fn blank_login_fields_are_ignored() {
        let mut src = searchable_base();
        src.login_url = Some("   ".into());
        src.login_ui = Some(String::new());
        src.login_check_js = Some("\n\t".into());
        assert!(!requires_login(&src), "空白登录字段不应误判");
        assert!(is_searchable_source(&src));
    }

    #[test]
    fn removing_login_field_restores_searchability() {
        // 模拟用户编辑规则去掉 loginUrl：下一次加载重新派生即可恢复可搜索，无需数据修复。
        let mut src = searchable_base();
        src.login_url = Some("https://x.example/login".into());
        assert!(!is_searchable_source(&src));
        src.login_url = None;
        assert!(is_searchable_source(&src));
    }

    #[test]
    fn disabled_source_is_not_searchable() {
        let mut src = searchable_base();
        src.enabled = false;
        assert!(!is_searchable_source(&src));
    }
}

/// 书籍分组的数据模型（对应数据库表结构），路由尚未接入；保留以对齐 schema。
#[allow(dead_code)]
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
/// 阅读配置的数据模型（对应 preferences 表），路由尚未接入；保留以对齐 schema。
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UserSettings {
    #[serde(default = "default_true")]
    pub use_replace_rule: bool,
    #[serde(default)]
    pub image_style: i64,
}
