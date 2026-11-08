//! 服务端书名包含过滤：所有客户端入口共用的最后一道边界过滤。
//!
//! 入口覆盖：
//! - 两段式 `/api/search/start` → `/api/search/result`（在 `search_result` 出参前）；
//! - 同步 `/api/search`、`/api/search/aggregate` 与 Kindle `/k/search`
//!   （三者都走 `routes::books::aggregated`，在返回给调用方之前）。
//!
//! 语义（与前端开关无关，网页端 / `/k` / API 一视同仁）：
//! - 只比对**书名**是否包含关键词；作者、简介、分类不参与判定；
//! - 归一化：全角→半角、大小写、空白、标点与符号（《》、间隔号 `·`、括号等）全部剔除，
//!   再判 `contains`，避免《百年孤独》因书名号/空格被误杀；
//! - 不做繁简转换：需要 opencc 之类带词典的重依赖（体积与常驻内存不适合 512MB 设备），
//!   且繁简一对多会引入误判，故不做；
//! - 不做别名/译名/副标题的降权保留——字面不匹配直接丢弃（误杀不管）；
//! - 过滤发生在排序/去重**之后**、响应之前，不改变打分、精选判定与源健康统计
//!   （健康/体检只用源返回的原始命中数）；被过滤掉不等于该源搜索失败；
//! - 全部被过滤时正常返回空列表，不报错。
//!
//! 观测：
//! - 每个响应带 `title_filter: {raw, kept, dropped}`（无响应体的 `/api/search` 只打日志）；
//! - 进程内累计计数由 `/healthz` 的 `search.title_filter` 暴露。
//! - `raw` 指「已通过相关性门槛并完成去重/排序、尚未做书名匹配」的候选数；
//!   它不等于书源抓到的原始条数（后者仍按原口径写入源健康）。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::entities::SearchBook;

/// 书名匹配专用归一化：先按通用规则（全角/大小写/空白）规整，
/// 再只保留 Unicode 字母与数字，剔除标点与符号。
pub fn normalize_for_title_match(raw: &str) -> String {
    crate::search_rank::normalize_title(raw)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// 归一化后的书名是否包含归一化后的关键词。
///
/// 关键词归一化后为空（空串或纯标点）时不过滤，保持与「空查询不筛」的既有语义一致。
pub fn title_contains(keyword: &str, title: &str) -> bool {
    let key = normalize_for_title_match(keyword);
    if key.is_empty() {
        return true;
    }
    normalize_for_title_match(title).contains(&key)
}

/// 一次边界过滤的计数，直接序列化进响应。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TitleFilterStats {
    /// 过滤前候选条数（已去重排序、未做书名匹配）
    pub raw: usize,
    /// 通过书名匹配、继续交给客户端的条数
    pub kept: usize,
    /// 书名不含关键词而丢弃的条数
    pub dropped: usize,
}

impl TitleFilterStats {
    pub fn new(raw: usize, kept: usize) -> Self {
        Self {
            raw,
            kept,
            dropped: raw.saturating_sub(kept),
        }
    }
}

static CALLS: AtomicU64 = AtomicU64::new(0);
static RAW: AtomicU64 = AtomicU64::new(0);
static KEPT: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// 进程生命周期内累计的过滤调用次数与条数，供 `/healthz` 观察「是不是筛太狠」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TitleFilterMetrics {
    pub calls: u64,
    pub raw: u64,
    pub kept: u64,
    pub dropped: u64,
}

pub fn metrics() -> TitleFilterMetrics {
    TitleFilterMetrics {
        calls: CALLS.load(Ordering::Relaxed),
        raw: RAW.load(Ordering::Relaxed),
        kept: KEPT.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
    }
}

fn record(stats: &TitleFilterStats) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    RAW.fetch_add(stats.raw as u64, Ordering::Relaxed);
    KEPT.fetch_add(stats.kept as u64, Ordering::Relaxed);
    DROPPED.fetch_add(stats.dropped as u64, Ordering::Relaxed);
}

/// 边界过滤：保留书名包含关键词的条目，返回保留结果与计数。
/// 顺序、分数与 `seq` 均原样保留，调用方不应再重新排序。
pub fn retain_title_matches(
    books: Vec<SearchBook>,
    keyword: &str,
) -> (Vec<SearchBook>, TitleFilterStats) {
    let raw = books.len();
    let kept: Vec<SearchBook> = books
        .into_iter()
        .filter(|b| title_contains(keyword, b.name.as_deref().unwrap_or("")))
        .collect();
    let stats = TitleFilterStats::new(raw, kept.len());
    record(&stats);
    (kept, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(name: &str) -> SearchBook {
        SearchBook {
            name: Some(name.to_string()),
            book_url: format!("https://x/{name}"),
            ..Default::default()
        }
    }

    #[test]
    fn normalization_folds_width_case_space_and_punctuation() {
        assert_eq!(normalize_for_title_match("《百年孤独》"), "百年孤独");
        assert_eq!(normalize_for_title_match("百年·孤独"), "百年孤独");
        assert_eq!(normalize_for_title_match("百年 孤独"), "百年孤独");
        assert_eq!(normalize_for_title_match("（百年孤独）"), "百年孤独");
        assert_eq!(normalize_for_title_match("百年，孤独！"), "百年孤独");
        assert_eq!(normalize_for_title_match("ＯＮＥ　ＰＩＥＣＥ"), "onepiece");
        assert_eq!(normalize_for_title_match("One Piece"), "onepiece");
        assert_eq!(normalize_for_title_match("１２３"), "123");
        // 纯标点/空白归一化为空串
        assert_eq!(normalize_for_title_match("《》·（）"), "");
    }

    #[test]
    fn matches_despite_book_title_symbols() {
        assert!(title_contains("百年孤独", "《百年孤独》"));
        assert!(title_contains("《百年孤独》", "百年孤独"));
        assert!(title_contains("百年 孤独", "百年·孤独"));
        assert!(title_contains("one piece", "ＯＮＥ　ＰＩＥＣＥ"));
        assert!(title_contains("斗罗大陆2", "斗罗大陆 ２"));
    }

    #[test]
    fn only_title_participates() {
        // 作者命中不算：过滤只看书名
        assert!(!title_contains("唐家三少", "斗罗大陆"));
        // 简介/分类即使含关键词也不保留
        let mut b = book("海贼王");
        b.intro = Some("百年孤独读后感".into());
        b.kind = Some("百年孤独".into());
        let (kept, _) = retain_title_matches(vec![b], "百年孤独");
        assert!(kept.is_empty());
    }

    #[test]
    fn literal_non_match_is_dropped_without_alias_fallback() {
        // 别名 / 译名 / 副标题：字面不匹配的直接丢，不做降权保留
        // 副标题仍包含原关键词 → 归一化后包含，保留（属字面匹配）
        assert!(title_contains("百年孤独", "百年孤独（精装版）"));
        assert!(!title_contains("百年孤独", "孤独百年"));
        assert!(!title_contains("One Piece", "海贼王"));
        assert!(!title_contains("百年孤独", ""));
    }

    #[test]
    fn empty_or_punctuation_only_keyword_does_not_filter() {
        assert!(title_contains("", "任意书名"));
        assert!(title_contains("《》·", "任意书名"));
    }

    #[test]
    fn retain_reports_raw_kept_and_dropped() {
        let books = vec![
            book("《百年孤独》"),
            book("百年争战"),
            book("海贼王"),
            book("百年孤独 精装"),
        ];
        let (kept, stats) = retain_title_matches(books, "百年孤独");
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].name.as_deref(), Some("《百年孤独》"));
        assert_eq!(kept[1].name.as_deref(), Some("百年孤独 精装"));
        assert_eq!(stats.raw, 4);
        assert_eq!(stats.kept, 2);
        assert_eq!(stats.dropped, 2);
    }
}
