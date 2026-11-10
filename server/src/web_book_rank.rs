//! web_book 的去重排序与批量搜索（move-only 从 web_book.rs 拆出）。

use super::*;

/// 单个书源的失败信息（聚合搜索时单源失败不影响整体）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceFailure {
    pub source: String,
    pub source_name: String,
    pub error: String,
    /// 失败类型：`site`（站点/连接）、`engine`（规则解析）、`timeout`（单源超时）、
    /// `truncated`（全局预算截断，请求未跑完）、`skipped`（排队未执行）。
    /// 前端据此把「真实失败」与「预算未完成」分开显示，健康写入也只看前两类。
    pub kind: String,
}

/// 聚合搜索结果：书列表 + 各源失败明细
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub books: Vec<SearchBook>,
    pub errors: Vec<SourceFailure>,
}

/// 发现分类（由 `exploreUrl` 展开）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExploreCategory {
    pub title: String,
    pub url: String,
}

/// 单次搜索会话的稳定排序状态（仅 `/api/search/result` 使用）：
/// - `order`：去重键 → 首次入池时确定的 `(bucket, seq)`，之后不再变化；
/// - `next_seq`：单调递增的入池序号；
/// - `snapshot`：一旦用户开始翻页（cursor / page>1），冻结整份有序结果，
///   后续页都从同一份快照读取，保证同一会话翻页不重不漏。
#[derive(Default)]
pub(crate) struct RankFreeze {
    pub order: HashMap<String, (u8, u64)>,
    pub next_seq: u64,
    pub snapshot: Option<Vec<SearchBook>>,
}

/// 去重键：归一化书名 + 归一化作者；书名为空时退化为 book_url。
pub(crate) fn dedupe_key(b: &SearchBook) -> String {
    let name = b.name.as_deref().unwrap_or("").trim().to_lowercase();
    let author = b.author.as_deref().unwrap_or("").trim().to_lowercase();
    if name.is_empty() {
        b.book_url.clone()
    } else {
        format!("{name}|{author}")
    }
}

/// 一条去重后的候选（内部结构，排序前构造）。
pub(super) struct RankedBook {
    pub(super) book: SearchBook,
    pub(super) relevance: u8,
    pub(super) weight: i64,
    pub(super) custom_order: i64,
    /// 命中该书的源：`(url, 展示名)`，按 url 去重。
    pub(super) origins: Vec<(String, String)>,
    pub(super) bucket: u8,
    pub(super) seq: u64,
}

impl RankedBook {
    pub(super) fn score(&self) -> u8 {
        crate::search_rank::combined_score(self.relevance, self.weight, self.origins.len())
    }
}

/// 聚合去重 + 排序。
///
/// - `query` 非空（搜索结果）：先丢弃与查询词完全无关的条目，再按相关性降序排序——
///   书名精确/前缀/包含/近似命中优先，只命中查询词片段（如「百年」之于「百年孤独」）
///   固定为 `SCORE_PARTIAL`，严格排在所有书名命中之后。同分再按源权重降序、
///   `customOrder` 升序、`book_url` 升序。源权重只作为同档内的小幅加分（≤3），
///   不会让低相关的高权重源压过完整命中。
///   多源同书合并后按真实来源数计算共识加分，并在结果里带出 `origins`。
///   强命中（exact/prefix/contains/approx）≥ `STRONG_MIN_RESULTS` 时，
///   只命中片段的 partial 被折叠掉，避免首屏只剩“可能相关”。
/// - `query` 为空（发现/探索页）：保持旧的"纯源权重"排序，不做相关性过滤。
///
/// `freeze` 为 `Some` 时用于 job 分页：条目首次入池即固定 `(bucket, seq)`，
/// 后到结果不会挤动已翻过的页。
pub(crate) fn dedupe_and_sort_with(
    books: Vec<SearchBook>,
    sources: &[BookSource],
    query: &str,
    freeze: Option<&mut RankFreeze>,
) -> Vec<SearchBook> {
    let order: HashMap<&str, (i64, i64)> = sources
        .iter()
        .map(|s| (s.book_source_url.as_str(), (s.weight, s.custom_order)))
        .collect();
    let weight_order = |b: &SearchBook| -> (i64, i64) {
        b.origin
            .as_deref()
            .and_then(|o| order.get(o))
            .copied()
            .unwrap_or((0, 0))
    };
    let q = crate::search_rank::normalize_title(query);

    // 先去重：同书多源合并成一条，累计真实来源（共识加分依据）。
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut ranked: Vec<RankedBook> = Vec::new();
    for b in books {
        let rel = if q.is_empty() {
            0
        } else {
            crate::search_rank::search_relevance(
                &q,
                b.name.as_deref().unwrap_or(""),
                b.author.as_deref().unwrap_or(""),
            )
        };
        if !q.is_empty() && rel == 0 {
            continue;
        }
        let (w, o) = weight_order(&b);
        let origin_url = b.origin.clone().unwrap_or_default();
        let origin_name = b
            .origin_name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| origin_url.clone());
        let key = dedupe_key(&b);
        match index.get(&key) {
            Some(&i) => {
                let e = &mut ranked[i];
                if !origin_url.is_empty() && !e.origins.iter().any(|(u, _)| u == &origin_url) {
                    e.origins.push((origin_url, origin_name));
                }
                // 同书多源：保留更相关（同相关时权重更高）的那一行做展示。
                if rel > e.relevance || (rel == e.relevance && w > e.weight) {
                    let origins = std::mem::take(&mut e.origins);
                    *e = RankedBook {
                        book: b,
                        relevance: rel,
                        weight: w,
                        custom_order: o,
                        origins,
                        bucket: 0,
                        seq: 0,
                    };
                }
            }
            None => {
                index.insert(key, ranked.len());
                let origins = if origin_url.is_empty() {
                    Vec::new()
                } else {
                    vec![(origin_url, origin_name)]
                };
                ranked.push(RankedBook {
                    book: b,
                    relevance: rel,
                    weight: w,
                    custom_order: o,
                    origins,
                    bucket: 0,
                    seq: 0,
                });
            }
        }
    }

    let mut frozen = false;
    if let Some(f) = freeze {
        frozen = true;
        for e in ranked.iter_mut() {
            let key = dedupe_key(&e.book);
            let (bucket, seq) = match f.order.get(&key).copied() {
                Some(v) => v,
                None => {
                    let seq = f.next_seq;
                    f.next_seq += 1;
                    let b = crate::search_rank::bucket(e.score());
                    f.order.insert(key, (b, seq));
                    (b, seq)
                }
            };
            e.bucket = bucket;
            e.seq = seq;
            e.book.rank_bucket = Some(bucket);
            e.book.rank_seq = Some(seq);
        }
    }

    // 排序：job 分页用固定 (bucket, seq)，其余用分数 + 权重 + 顺序 + url。
    if frozen {
        ranked.sort_by(|a, b| b.bucket.cmp(&a.bucket).then(a.seq.cmp(&b.seq)));
    } else if !q.is_empty() {
        ranked.sort_by(|a, b| {
            b.score()
                .cmp(&a.score())
                .then_with(|| b.weight.cmp(&a.weight))
                .then_with(|| a.custom_order.cmp(&b.custom_order))
                .then_with(|| a.book.book_url.cmp(&b.book.book_url))
        });
    } else {
        ranked.sort_by(|a, b| {
            b.weight
                .cmp(&a.weight)
                .then_with(|| a.custom_order.cmp(&b.custom_order))
                .then_with(|| a.book.book_url.cmp(&b.book.book_url))
        });
    }

    // 相关性档位 + origins 回填。
    for e in ranked.iter_mut() {
        if !q.is_empty() {
            e.book.relevance = Some(crate::search_rank::relevance_class(e.relevance).to_string());
        }
        e.book.origins = e
            .origins
            .iter()
            .map(|(_, name)| name.clone())
            .filter(|n| !n.is_empty())
            .collect();
    }

    // 有条件折叠：强命中足够多时，partial 不再进入首屏（整体丢弃）。
    // 空查询（发现页）不过滤、不折叠。
    if !q.is_empty() {
        let strong = ranked
            .iter()
            .filter(|e| e.relevance >= crate::search_rank::SCORE_APPROX_MIN)
            .count();
        if crate::search_rank::should_fold_partials(strong) {
            ranked.retain(|e| e.relevance != crate::search_rank::SCORE_PARTIAL);
        }
    }

    ranked.into_iter().map(|e| e.book).collect()
}

pub(crate) fn dedupe_and_sort(
    books: Vec<SearchBook>,
    sources: &[BookSource],
    query: &str,
) -> Vec<SearchBook> {
    dedupe_and_sort_with(books, sources, query, None)
}

/// 多源搜索：并发请求所有启用书源；单源失败只记录错误，不影响整体。
/// 每个书源有独立的搜索预算（默认 10s），超时只丢该源，避免一个慢站拖住整页。
pub async fn search_all_with_errors(sources: &[BookSource], key: &str, page: i32) -> SearchOutcome {
    search_all_with_errors_budgeted(sources, key, page, req::search_budget()).await
}

pub async fn search_all_with_errors_budgeted(
    sources: &[BookSource],
    key: &str,
    page: i32,
    budget: std::time::Duration,
) -> SearchOutcome {
    // 旧调用方语义：budget 同时是整批与单源预算，走 curated 并发池。
    search_all_budgeted_scoped(
        sources,
        key,
        page,
        budget,
        budget,
        crate::req::SearchScope::Curated,
    )
    .await
}

/// 按 scope 使用与 `/api/search/start` 相同的单源/整批预算与并发池：
/// `curated` 8s/6s、`all` 5s/25s。legacy `/api/search` 走这里，
/// 保证同一关键词在同一源集合下拿到的结果集合与两段式一致。
pub async fn search_all_with_errors_scoped(
    sources: &[BookSource],
    key: &str,
    page: i32,
    scope: crate::req::SearchScope,
) -> SearchOutcome {
    search_all_budgeted_scoped(
        sources,
        key,
        page,
        crate::req::search_budget_for(scope),
        crate::req::search_total_budget(scope),
        scope,
    )
    .await
}

pub(super) async fn search_all_budgeted_scoped(
    sources: &[BookSource],
    key: &str,
    page: i32,
    per_source: std::time::Duration,
    total: std::time::Duration,
    scope: crate::req::SearchScope,
) -> SearchOutcome {
    let deadline = tokio::time::Instant::now() + total;
    let mut tasks = Vec::new();
    for s in sources {
        if !crate::entities::is_searchable_source(s) {
            continue;
        }
        let src = s.clone();
        let comic = crate::req::is_comic_source_type(src.book_source_type);
        let fut = async move {
            // 先排队拿源级并发槽（不计入单源预算），再开始计时。
            let _slot = crate::req::acquire_search_slot(comic, deadline, scope).await?;
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                anyhow::bail!("全局搜索预算耗尽（排队未及执行）");
            }
            match tokio::time::timeout(remain.min(per_source), search(&src, key, page)).await {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!(
                    "搜索超时（单源 {}s 预算）",
                    per_source.as_secs()
                )),
            }
        };
        tasks.push((s.clone(), fut));
    }
    let results = futures::future::join_all(
        tasks
            .into_iter()
            .map(|(src, fut)| async move { (src, fut.await) }),
    )
    .await;
    let mut outcome = SearchOutcome::default();
    for (src, r) in results {
        match r {
            Ok(mut books) => outcome.books.append(&mut books),
            Err(e) => {
                let msg = e.to_string();
                let kind = if msg.starts_with("全局搜索预算耗尽") {
                    "truncated"
                } else if msg.starts_with("搜索超时") {
                    "timeout"
                } else {
                    "site"
                };
                outcome.errors.push(SourceFailure {
                    source: src.book_source_url.clone(),
                    source_name: src.book_source_name.clone(),
                    error: msg,
                    kind: kind.to_string(),
                })
            }
        }
    }
    outcome.books = dedupe_and_sort(outcome.books, sources, key);
    outcome
}

pub async fn search_all(sources: &[BookSource], key: &str, page: i32) -> Vec<SearchBook> {
    search_all_with_errors(sources, key, page).await.books
}
