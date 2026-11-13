//! WebBook 编排（五类解析：搜索/发现/详情/目录/正文）
//! 输入 HTML/JSON + 书源规则 -> 输出结构化书籍数据（纯函数管线）。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use url::Url;

use crate::entities::{
    Book, BookChapter, BookContent, BookImage, BookInfoRule, BookSource, ContentRule, ExploreRule,
    SearchBook, SearchRule, TocRule,
};
use crate::req;
use crate::rule_engine::{AnalyzeRule, BookMeta};

/// 抓取可注入点：生产走 [`ReqFetcher`]，路由冒烟测试可换成 fixture 实现，
/// 从而在不起网络服务的前提下覆盖 toc/content。
pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;

pub trait Fetcher: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        method: &'a str,
        headers: &'a HashMap<String, String>,
        body: Option<&'a str>,
        source_key: &'a str,
        timeout: Option<Duration>,
        limit: req::SourceLimit,
    ) -> FetchFuture<'a>;
}

pub struct ReqFetcher;

impl Fetcher for ReqFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        method: &'a str,
        headers: &'a HashMap<String, String>,
        body: Option<&'a str>,
        source_key: &'a str,
        timeout: Option<Duration>,
        limit: req::SourceLimit,
    ) -> FetchFuture<'a> {
        Box::pin(async move {
            req::fetch_for_source_limited(url, method, headers, body, source_key, timeout, limit)
                .await
        })
    }
}

/// 判断书源是否为 JSON 型（规则以 $. 开头或 url 含 json）
fn is_json_source(source: &BookSource) -> bool {
    source
        .search_url
        .as_deref()
        .map(|u| u.contains("json"))
        .unwrap_or(false)
        || source
            .rule_search
            .as_ref()
            .and_then(|r| r.book_list.as_deref())
            .map(|b| b.starts_with("$."))
            .unwrap_or(false)
}

fn make_ar(source: &BookSource) -> AnalyzeRule {
    let mut ar = AnalyzeRule::new();
    ar.js_lib = source.js_lib.clone().unwrap_or_default();
    ar.is_json = is_json_source(source);
    // Legado JS 里的 `source.key` / `source.getKey()` 即 bookSourceUrl
    ar.source_key.replace(source.book_source_url.clone());
    ar.source_login_url
        .replace(source.login_url.clone().unwrap_or_default());
    ar
}

// ---------------- 跨请求的 book 层状态（变量 + cache） ----------------
//
// Legado 的 `@put`/`cache.put` 落在 Book 对象上，同书的 info→toc→content 共享；
// 本服务是纯函数管线，因此由编排层（本模块）在请求开始时按 `book_key` 预载，
// 请求结束时把快照交给路由层落库。存储实现（SQLite）在 `routes/books.rs`。

use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// `book_key = sha256(source_url + '\n' + book_url)`：不把原始长 URL 当主键。
pub fn book_key(source: &BookSource, book_url: &str) -> String {
    let origin = source.book_source_url.trim().trim_end_matches('/');
    let url = book_url.trim().trim_end_matches('/');
    let mut h = Sha256::new();
    h.update(format!("{origin}\n{url}").as_bytes());
    format!("{:x}", h.finalize())
}

#[derive(Default)]
struct StoreInner {
    /// 请求开始时的持久化快照（用于 diff 出增量）
    seed_vars: HashMap<String, HashMap<String, String>>,
    seed_cache: HashMap<String, HashMap<String, String>>,
    /// 请求结束时的快照
    cur_vars: HashMap<String, HashMap<String, String>>,
    cur_cache: HashMap<String, HashMap<String, String>>,
}

/// 一本书在两次请求之间的增量：变量全量快照（None 表示无变化）+ cache 增删。
pub struct BookChange {
    pub key: String,
    pub vars: Option<HashMap<String, String>>,
    pub cache_upserts: Vec<(String, String)>,
    pub cache_deletes: Vec<String>,
}

/// 请求级的 book 状态容器（可跨并发任务共享）。
#[derive(Default)]
pub struct BookStore {
    inner: Mutex<StoreInner>,
}

impl BookStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预载某本书的持久化状态（缺失时为两个空 map）。
    pub fn seed(&self, key: String, vars: HashMap<String, String>, cache: HashMap<String, String>) {
        let mut g = self.inner.lock().unwrap();
        g.seed_vars.insert(key.clone(), vars);
        g.seed_cache.insert(key, cache);
    }

    pub fn vars(&self, key: &str) -> HashMap<String, String> {
        self.inner
            .lock()
            .unwrap()
            .seed_vars
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn cache(&self, key: &str) -> HashMap<String, String> {
        self.inner
            .lock()
            .unwrap()
            .seed_cache
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// 记录某本书本次请求结束后的完整快照。
    pub fn record(
        &self,
        key: String,
        vars: HashMap<String, String>,
        cache: HashMap<String, String>,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.cur_vars.insert(key.clone(), vars);
        g.cur_cache.insert(key, cache);
    }

    /// 取走自上次调用以来的增量（并发任务安全：每本书只由记录它的任务消费一次）。
    pub fn take_changes(&self) -> Vec<BookChange> {
        let mut g = self.inner.lock().unwrap();
        let keys: HashSet<String> = g
            .cur_vars
            .keys()
            .chain(g.cur_cache.keys())
            .cloned()
            .collect();
        let mut out = Vec::new();
        for key in keys {
            let cur_vars = g.cur_vars.remove(&key).unwrap_or_default();
            let cur_cache = g.cur_cache.remove(&key).unwrap_or_default();
            let seed_vars = g.seed_vars.get(&key).cloned().unwrap_or_default();
            let seed_cache = g.seed_cache.get(&key).cloned().unwrap_or_default();
            if cur_vars.is_empty() && cur_cache.is_empty() && seed_vars.is_empty() {
                continue;
            }
            let vars = if !cur_vars.is_empty() && cur_vars != seed_vars {
                Some(cur_vars.clone())
            } else {
                None
            };
            let mut cache_upserts = Vec::new();
            for (k, v) in &cur_cache {
                if seed_cache.get(k) != Some(v) {
                    cache_upserts.push((k.clone(), v.clone()));
                }
            }
            let cache_deletes: Vec<String> = seed_cache
                .keys()
                .filter(|k| !cur_cache.contains_key(*k))
                .cloned()
                .collect();
            if vars.is_some() || !cache_upserts.is_empty() || !cache_deletes.is_empty() {
                out.push(BookChange {
                    key: key.clone(),
                    vars,
                    cache_upserts,
                    cache_deletes,
                });
            }
            // 已落库的状态成为新的基线，避免同一请求内重复写
            g.seed_vars.insert(key.clone(), cur_vars);
            g.seed_cache.insert(key, cur_cache);
        }
        out
    }
}

#[path = "web_book_util.rs"]
mod web_book_util;
use web_book_util::*;
#[path = "web_book_info.rs"]
mod web_book_info;
#[path = "web_book_search.rs"]
mod web_book_search;
use web_book_info::*;
#[path = "web_book_toc.rs"]
mod web_book_toc;
use web_book_toc::*;
#[path = "web_book_content.rs"]
mod web_book_content;
// 单测直接验证正文回退规则生成器。
#[cfg(test)]
use web_book_content::content_text_fallback;
#[path = "web_book_images.rs"]
mod web_book_images;
// 单测直接驱动图片抽取的私有辅助函数；生产构建由入口的 pub use 提供公开项。
#[cfg(test)]
use web_book_images::{build_images, clean_image_url, images_from_html, images_from_value};
#[path = "web_book_rank.rs"]
mod web_book_rank;
use web_book_rank::*;
#[path = "web_book_explore.rs"]
mod web_book_explore;
use web_book_explore::*;

// 外部（路由层/其它抓取管线）原先从 `web_book::*` 取用的公开项，拆分后在入口重新导出。
pub use web_book_content::{chapter_content, chapter_content_book};
pub use web_book_explore::{explore_all, explore_categories};
pub use web_book_images::{chapter_images, chapter_images_book, ChapterImages, MAX_CHAPTER_IMAGES};
pub use web_book_info::{book_info, book_info_book, parse_book_info_html};
pub(crate) use web_book_rank::dedupe_key;
pub use web_book_rank::{
    search_all, search_all_scoped_evidenced, search_all_with_errors,
    search_all_with_errors_budgeted, search_all_with_errors_scoped, ExploreCategory, SearchOutcome,
    SourceFailure,
};
pub use web_book_search::{explore, search, search_book};
pub use web_book_toc::{chapter_list, chapter_list_book};
pub use web_book_util::{decode_image, has_image_decode};

#[allow(clippy::field_reassign_with_default)] // 测试里按字段逐个构造书源/结果，便于对照默认值。
#[cfg(test)]
#[path = "web_book_tests.rs"]
mod tests;
