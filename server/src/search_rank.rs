//! 搜索结果的排序 / 快照游标 / 合并去重 / 分层配额 —— 纯逻辑，不依赖网络、数据库与任何路由。
//!
//! 设计约束（与前端分页协议一致）：
//! - 排序信号优先级：书名精确 > 前缀 > 包含 > 编辑距离近似；其次作者匹配；
//!   最后才是源权重与多源共识加分。**延迟不参与排序**。
//! - 分数按 5 分一桶，桶内按结果首次出现时的单调序号 `seq` 升序。
//! - 条目一经入池，`score` 与 `seq` 不再变化：后到的结果只能追加在桶内尾部，
//!   不会挤掉已经翻过的页。因此 `Cursor { bucket, seq }` 翻页不重不漏。
//! - 多源同书合并按「归一化书名 + 归一化作者」做键，保留首次出现的位置与分数桶，
//!   后来的源只追加到 `sources`。共识加分只在首次入池时计算，合并不改分。

use std::cmp::Ordering;
use std::collections::HashMap;

/// 书名精确命中（不含作者加分）
pub const SCORE_EXACT: u8 = 90;
/// 书名前缀命中
pub const SCORE_PREFIX: u8 = 78;
/// 书名包含命中
pub const SCORE_CONTAINS: u8 = 66;
/// 编辑距离近似命中的上限（下限为 `SCORE_APPROX_MIN`）
pub const SCORE_APPROX_MAX: u8 = 50;
/// 编辑距离近似命中的下限
pub const SCORE_APPROX_MIN: u8 = 40;
/// 作者匹配加分
pub const AUTHOR_BONUS: u8 = 3;
/// 源权重加分上限
pub const WEIGHT_BONUS_MAX: u8 = 3;
/// 多源共识加分上限（每多一个源 +1）
pub const CONSENSUS_BONUS_MAX: u8 = 3;

/// 分数桶宽：每 5 分一档，高分桶编号更大。
pub const BUCKET_WIDTH: u8 = 5;

// ---------- 归一化 ----------

fn fold_width(c: char) -> char {
    match c as u32 {
        0x3000 => ' ',
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

fn normalize(raw: &str) -> String {
    raw.chars()
        .map(fold_width)
        .flat_map(|c| c.to_lowercase())
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// 归一化书名：全角转半角、去所有空白（含首尾与内部）、大小写归一。
/// 故意不做繁简转换，避免误伤「发/髮」这类形近字。
pub fn normalize_title(raw: &str) -> String {
    normalize(raw)
}

/// 归一化作者：规则与书名一致。
pub fn normalize_author(raw: &str) -> String {
    normalize(raw)
}

// ---------- 相关性打分 ----------

/// 有界 Levenshtein：只计算 `|i-j| ≤ max_dist` 的带内格子，带外视作无穷大。
/// 若最终距离超过 `max_dist` 返回 `None`（不会做长度×长度的全量 DP）。
fn capped_levenshtein(a: &[char], b: &[char], max_dist: usize) -> Option<usize> {
    if a.len() < b.len() {
        return capped_levenshtein(b, a, max_dist);
    }
    if a.len() - b.len() > max_dist {
        return None;
    }
    const INF: usize = usize::MAX / 4;
    let n = b.len();
    let mut prev = vec![INF; n + 1];
    for (j, slot) in prev.iter_mut().enumerate().take(n.min(max_dist) + 1) {
        *slot = j;
    }
    for i in 1..=a.len() {
        let mut cur = vec![INF; n + 1];
        if i <= max_dist {
            cur[0] = i;
        }
        let lo = i.saturating_sub(max_dist).max(1);
        let hi = (i + max_dist).min(n);
        for j in lo..=hi {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    let d = prev[n];
    (d <= max_dist).then_some(d)
}

fn approx_score(q: &str, title: &str) -> u8 {
    let q: Vec<char> = q.chars().collect();
    let t: Vec<char> = title.chars().collect();
    let max_len = q.len().max(t.len());
    if max_len == 0 {
        return 0;
    }
    // 长度差超过 40% 直接判不近似，不做全量 DP。
    let diff = q.len().abs_diff(t.len());
    if diff * 100 > max_len * 40 {
        return 0;
    }
    // 只有 20% 以内的编辑距离才算「近似」。
    let max_dist = (max_len / 5).max(1);
    let Some(d) = capped_levenshtein(&q, &t, max_dist) else {
        return 0;
    };
    let span = (max_dist - d) as f32 / max_dist as f32;
    SCORE_APPROX_MIN + (span * (SCORE_APPROX_MAX - SCORE_APPROX_MIN) as f32).round() as u8
}

fn author_bonus(q: &str, author: &str) -> u8 {
    if author.is_empty() || q.is_empty() {
        return 0;
    }
    if author == q || author.contains(q) || q.contains(author) {
        AUTHOR_BONUS
    } else {
        0
    }
}

/// 书名 + 作者的相关性分数（0–100）。源权重与共识加分由 [`combined_score`] 追加。
///
/// 各档之间留出 ≥12 分的间隔，保证"书名层级"永远压过"作者加分"。
pub fn relevance_score(query: &str, title: &str, author: &str) -> u8 {
    let q = normalize_title(query);
    let t = normalize_title(title);
    if q.is_empty() || t.is_empty() {
        return 0;
    }
    let base = if t == q {
        SCORE_EXACT
    } else if t.starts_with(&q) {
        SCORE_PREFIX
    } else if t.contains(&q) {
        SCORE_CONTAINS
    } else {
        approx_score(&q, &t)
    };
    let bonus = author_bonus(&q, &normalize_author(author));
    if base == 0 {
        // 书名完全不相关时，作者命中只保留极低的基础分（仍低于近似档）。
        return bonus;
    }
    (base + bonus).min(100)
}

/// 把源权重折算成 0–`WEIGHT_BONUS_MAX` 的加分。
pub fn weight_bonus(weight: i64) -> u8 {
    (weight.clamp(0, 9) / 3) as u8
}

/// 多源共识加分：命中源数每多一个 +1，上限 `CONSENSUS_BONUS_MAX`。
pub fn consensus_bonus(sources: usize) -> u8 {
    (sources.saturating_sub(1)).min(CONSENSUS_BONUS_MAX as usize) as u8
}

/// 最终分数：相关性 + 源权重 + 多源共识，饱和到 100。
/// 三项加分总和 ≤ 9，不会跨过书名层级（最小档间距 12）。
pub fn combined_score(relevance: u8, weight: i64, sources: usize) -> u8 {
    (relevance + weight_bonus(weight) + consensus_bonus(sources)).min(100)
}

// ---------- 桶与游标 ----------

/// 分数桶：每 5 分一档，0–100 → 0–20。bucket 越大越靠前。
pub fn bucket(score: u8) -> u8 {
    score / BUCKET_WIDTH
}

/// 翻页游标：上一页最后一条的 `(bucket, seq)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub bucket: u8,
    pub seq: u64,
}

impl Cursor {
    pub fn of(item: &RankItem) -> Self {
        Self {
            bucket: bucket(item.score),
            seq: item.seq,
        }
    }

    /// 该条目是否严格位于游标之后（应出现在下一页）。
    pub fn item_after(&self, item: &RankItem) -> bool {
        let b = bucket(item.score);
        match b.cmp(&self.bucket) {
            Ordering::Less => true,
            Ordering::Equal => item.seq > self.seq,
            Ordering::Greater => false,
        }
    }
}

/// 入池条目。`seq` 由调用方在结果**首次出现**时分配，单调递增；入池后不再改变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankItem {
    pub seq: u64,
    /// 入池时确定，之后不再变化（合并也不改分）
    pub score: u8,
    pub title: String,
    pub author: String,
    pub book_url: String,
    /// 命中的书源（按加入顺序，首个为首次命中源）
    pub sources: Vec<String>,
}

impl RankItem {
    pub fn new(
        seq: u64,
        score: u8,
        title: impl Into<String>,
        author: impl Into<String>,
        book_url: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            seq,
            score,
            title: title.into(),
            author: author.into(),
            book_url: book_url.into(),
            sources: vec![source.into()],
        }
    }

    pub fn bucket(&self) -> u8 {
        bucket(self.score)
    }

    pub fn cursor(&self) -> Cursor {
        Cursor::of(self)
    }
}

/// 全序：分数桶降序，桶内 `seq` 升序。`seq` 唯一，故排序结果确定。
pub fn compare(a: &RankItem, b: &RankItem) -> Ordering {
    b.bucket()
        .cmp(&a.bucket())
        .then_with(|| a.seq.cmp(&b.seq))
}

pub fn sort_ranked(items: &mut [RankItem]) {
    items.sort_by(compare);
}

/// 取一页：跳过游标及之前的条目，按全序取前 `limit` 条。
/// 先调用 [`sort_ranked`]，再对本函数返回的切片取值。
pub fn page<'a>(items: &'a [RankItem], after: Option<Cursor>, limit: usize) -> Vec<&'a RankItem> {
    items
        .iter()
        .filter(|it| after.map_or(true, |c| c.item_after(it)))
        .take(limit)
        .collect()
}

// ---------- 多源同书合并 ----------

fn dup_key(item: &RankItem) -> String {
    let title = normalize_title(&item.title);
    if title.is_empty() {
        format!("url:{}", item.book_url.trim())
    } else {
        format!("{title}|{}", normalize_author(&item.author))
    }
}

/// 原地合并重复条目：保留首次出现的位置、`seq`、`score` 与桶，
/// 后来的条目只把源追加进 `sources`（去重）并补全空的 `book_url`。
/// 返回 `(合并后的列表, 被合并掉的条数)`。
pub fn merge_duplicates(items: Vec<RankItem>) -> (Vec<RankItem>, usize) {
    let mut first_index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<RankItem> = Vec::with_capacity(items.len());
    let mut merged = 0usize;

    for item in items {
        let key = dup_key(&item);
        if let Some(&idx) = first_index.get(&key) {
            let head = &mut out[idx];
            if head.book_url.is_empty() && !item.book_url.is_empty() {
                head.book_url = item.book_url;
            }
            for src in item.sources {
                if !head.sources.contains(&src) {
                    head.sources.push(src);
                }
            }
            merged += 1;
        } else {
            first_index.insert(key, out.len());
            out.push(item);
        }
    }
    (out, merged)
}

// ---------- 分层配额 ----------

/// 书源分层：优选 / 观察 / 隔离。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTier {
    Preferred,
    Observing,
    Quarantined,
}

/// 每层每源的单次搜索配额。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaCaps {
    pub preferred: usize,
    pub observing: usize,
    pub quarantined: usize,
}

impl Default for QuotaCaps {
    fn default() -> Self {
        Self {
            preferred: 60,
            observing: 20,
            quarantined: 0,
        }
    }
}

/// 配额策略：`observing_floor` 是观察源的**下限**——即使配置把观察配额调成 0，
/// 有命中的观察源仍至少保留这么多条，避免长尾源被永久饿死。
/// 配额只作用于**首屏批次**；后续补批（`top_up` / 后台继续灌）不设上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaPolicy {
    pub caps: QuotaCaps,
    pub observing_floor: usize,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            caps: QuotaCaps::default(),
            observing_floor: 2,
        }
    }
}

#[derive(Debug, Default)]
pub struct QuotaOutcome {
    pub kept: Vec<RankItem>,
    pub dropped: Vec<RankItem>,
    /// 因配额上限被丢掉的条数
    pub capped_out: usize,
}

fn cap_of(tier: SourceTier, caps: &QuotaCaps) -> usize {
    match tier {
        SourceTier::Preferred => caps.preferred,
        SourceTier::Observing => caps.observing,
        SourceTier::Quarantined => caps.quarantined,
    }
}

fn effective_cap(tier: SourceTier, policy: &QuotaPolicy) -> usize {
    let cap = cap_of(tier, &policy.caps);
    if tier == SourceTier::Observing {
        cap.max(policy.observing_floor)
    } else {
        cap
    }
}

fn tier_of(tiers: &HashMap<String, SourceTier>, source: &str) -> SourceTier {
    tiers
        .get(source)
        .copied()
        .unwrap_or(SourceTier::Observing)
}

/// 按分层配额截断。输入须已按 [`compare`] 排序。
///
/// - 每条按 `sources` 顺序记到第一个还有余量的源；全部源都满则丢弃。
/// - 隔离源配额为 0，任何只由隔离源命中的条目都会被丢弃。
/// - 观察源另有下限（`observing_floor`），配额调到 0 时也不会被饿死。
/// - 本函数只处理**首屏批次**；补批走 [`top_up`]，不受上限约束。
pub fn apply_quota(
    items: Vec<RankItem>,
    tiers: &HashMap<String, SourceTier>,
    policy: &QuotaPolicy,
) -> QuotaOutcome {
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut kept = Vec::with_capacity(items.len());
    let mut dropped = Vec::new();

    for item in items {
        let charged = item
            .sources
            .iter()
            .find(|src| {
                let tier = tier_of(tiers, src);
                used.get(*src).copied().unwrap_or(0) < effective_cap(tier, policy)
            })
            .cloned();
        if let Some(src) = charged {
            *used.entry(src).or_insert(0) += 1;
            kept.push(item);
        } else {
            dropped.push(item);
        }
    }

    let capped_out = dropped.len();
    QuotaOutcome {
        kept,
        dropped,
        capped_out,
    }
}

/// 二次补充：首轮配额截断后若不足一页，按排名（高分源优先）从被丢弃的条目里补，
/// 隔离源不补。返回补入条数；`kept` 保持全序。
pub fn top_up(
    kept: &mut Vec<RankItem>,
    mut dropped: Vec<RankItem>,
    page_size: usize,
    tiers: &HashMap<String, SourceTier>,
) -> usize {
    if kept.len() >= page_size {
        return 0;
    }
    sort_ranked(&mut dropped);
    let mut added = 0usize;
    for item in dropped {
        if kept.len() >= page_size {
            break;
        }
        let all_quarantined = item
            .sources
            .iter()
            .all(|s| tier_of(tiers, s) == SourceTier::Quarantined);
        if !all_quarantined {
            kept.push(item);
            added += 1;
        }
    }
    sort_ranked(kept);
    added
}

// ---------- 提前收工 ----------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StopConfig {
    /// 开关：默认关，实测无损后再默认开
    pub enabled: bool,
    /// 可用结果阈值
    pub min_usable: usize,
    /// 优选源完成比例阈值
    pub preferred_done_ratio: f32,
}

impl Default for StopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_usable: 20,
            preferred_done_ratio: 0.8,
        }
    }
}

/// 是否提前收工：开关打开 + 可用结果 ≥ `min_usable` + 优选源完成比例 ≥ 阈值。
/// 全局 10 s 上限仍由调用方保留。
pub fn should_stop(total_usable: usize, preferred_done_ratio: f32, cfg: &StopConfig) -> bool {
    cfg.enabled
        && total_usable >= cfg.min_usable
        && preferred_done_ratio >= cfg.preferred_done_ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers(pairs: &[(&str, SourceTier)]) -> HashMap<String, SourceTier> {
        pairs.iter().map(|(s, t)| (s.to_string(), *t)).collect()
    }

    fn item(seq: u64, score: u8, title: &str, source: &str) -> RankItem {
        RankItem::new(seq, score, title, "", format!("https://x/{seq}"), source)
    }

    // ----- 归一化 -----

    #[test]
    fn normalizes_width_space_and_case() {
        assert_eq!(normalize_title("　斗罗 大陆"), "斗罗大陆");
        assert_eq!(normalize_title("One Piece"), "onepiece");
        assert_eq!(normalize_title("ＡＢＣ　１２３"), "abc123");
        assert_eq!(normalize_author(" 唐家三少 "), "唐家三少");
    }

    // ----- 四档分数 -----

    #[test]
    fn scores_exact_prefix_contains_approx() {
        let q = "斗罗大陆";
        assert_eq!(relevance_score(q, "斗罗大陆", ""), SCORE_EXACT);
        assert_eq!(relevance_score(q, "斗罗大陆2绝世唐门", ""), SCORE_PREFIX);
        assert_eq!(relevance_score(q, "新斗罗大陆", ""), SCORE_CONTAINS);
        let approx = relevance_score(q, "斗罗大路", "");
        assert!(
            (SCORE_APPROX_MIN..=SCORE_APPROX_MAX).contains(&approx),
            "approx score {approx} out of band"
        );
        assert_eq!(relevance_score(q, "海贼王", ""), 0);
        assert_eq!(relevance_score("", "斗罗大陆", ""), 0);
    }

    #[test]
    fn approx_rejects_large_length_gap() {
        assert_eq!(relevance_score("xyz", "abcdefghij", ""), 0);
    }

    #[test]
    fn author_bonus_stays_below_title_tiers() {
        // 查询词是作者名时，只有极低基础分
        assert_eq!(relevance_score("唐家三少", "斗罗大陆", "唐家三少"), AUTHOR_BONUS);
        // 书名精确命中不因作者字段额外加分（作者没命中查询词）
        assert_eq!(relevance_score("斗罗大陆", "斗罗大陆", "唐家三少"), SCORE_EXACT);
        // 作者命中时仅在书名档位上追加 3 分
        assert_eq!(
            relevance_score("斗罗大陆", "斗罗大陆", "斗罗"),
            SCORE_EXACT + AUTHOR_BONUS
        );
    }

    #[test]
    fn combined_score_does_not_break_tiers() {
        // 前缀 + 作者 + 满权重 + 满共识 仍低于"精确书名但无加分"
        let prefix_max = combined_score(SCORE_PREFIX + AUTHOR_BONUS, 9, 4);
        assert!(prefix_max < SCORE_EXACT, "{prefix_max}");
        let contain_max = combined_score(SCORE_CONTAINS + AUTHOR_BONUS, 9, 4);
        assert!(contain_max < SCORE_PREFIX, "{contain_max}");
        let approx_max = combined_score(SCORE_APPROX_MAX + AUTHOR_BONUS, 9, 4);
        assert!(approx_max < SCORE_CONTAINS, "{approx_max}");
        assert_eq!(combined_score(100, 9, 4), 100);
    }

    // ----- 桶与游标 -----

    #[test]
    fn bucket_is_five_points() {
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(4), 0);
        assert_eq!(bucket(5), 1);
        assert_eq!(bucket(100), 20);
    }

    #[test]
    fn cursor_pagination_no_dup_no_gap_after_late_insert() {
        let mut items = vec![
            item(1, 90, "A", "s1"),
            item(2, 80, "B", "s1"),
            item(3, 80, "C", "s1"),
            item(4, 50, "D", "s1"),
        ];
        sort_ranked(&mut items);
        let p1 = page(&items, None, 2);
        let seen1: Vec<u64> = p1.iter().map(|i| i.seq).collect();
        assert_eq!(seen1, vec![1, 2]);
        let cur = p1.last().unwrap().cursor();

        // 迟到结果：一个更高分（应排到游标之前，不进入后续页），一个同桶更大 seq（可追加）
        items.push(item(6, 100, "LATE-HIGH", "s2"));
        items.push(item(7, 80, "LATE-SAME", "s2"));
        sort_ranked(&mut items);

        let mut seen = seen1.clone();
        let mut cursor = cur;
        loop {
            let p = page(&items, Some(cursor), 2);
            if p.is_empty() {
                break;
            }
            seen.extend(p.iter().map(|i| i.seq));
            cursor = p.last().unwrap().cursor();
        }
        // 原始 4 条不重不漏
        for seq in 1..=4u64 {
            assert_eq!(seen.iter().filter(|s| **s == seq).count(), 1, "seq {seq}");
        }
        // 高分迟到项在游标之前：不重复出现
        assert!(!seen.contains(&6));
        // 同桶迟到项只出现一次
        assert_eq!(seen.iter().filter(|s| **s == 7).count(), 1);
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn existing_items_never_reorder_on_insert() {
        let mut items = vec![item(1, 55, "A", "s1"), item(2, 40, "B", "s1")];
        sort_ranked(&mut items);
        let before: Vec<u64> = items.iter().map(|i| i.seq).collect();
        items.push(item(3, 55, "C", "s1"));
        sort_ranked(&mut items);
        let after: Vec<u64> = items.iter().map(|i| i.seq).collect();
        assert_eq!(after.iter().filter(|s| **s != 3).copied().collect::<Vec<_>>(), before);
    }

    // ----- 合并 -----

    #[test]
    fn merge_keeps_first_position_and_score() {
        let items = vec![
            RankItem::new(1, 66, "斗罗 大陆", "唐家三少", "https://a", "s1"),
            RankItem::new(2, 90, "斗罗大陆", "唐家三少", "", "s2"),
            RankItem::new(3, 60, "海贼王", "", "https://c", "s1"),
        ];
        let (out, merged) = merge_duplicates(items);
        assert_eq!(merged, 1);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].seq, 1);
        assert_eq!(out[0].score, 66);
        assert_eq!(out[0].sources, vec!["s1".to_string(), "s2".to_string()]);
        assert_eq!(out[0].book_url, "https://a");
        assert_eq!(out[1].seq, 3);
    }

    #[test]
    fn merge_uses_book_url_when_title_empty() {
        let items = vec![
            RankItem::new(1, 20, "", "", "https://a", "s1"),
            RankItem::new(2, 30, "", "", "https://a", "s2"),
            RankItem::new(3, 30, "", "", "https://b", "s2"),
        ];
        let (out, merged) = merge_duplicates(items);
        assert_eq!(merged, 1);
        assert_eq!(out.len(), 2);
    }

    // ----- 配额 -----

    #[test]
    fn quota_caps_first_batch_only_and_top_up_is_uncapped() {
        let tier_map = tiers(&[
            ("pref", SourceTier::Preferred),
            ("obs", SourceTier::Observing),
            ("bad", SourceTier::Quarantined),
        ]);
        // 单个优选源 80 条：首屏批次按 60 截断，补批必须能突破 60。
        let mut items: Vec<RankItem> = (0..80)
            .map(|i| item(i as u64, 90, &format!("P{i}"), "pref"))
            .collect();
        items.push(item(80, 20, "OBS-1", "obs"));
        items.push(item(81, 19, "OBS-2", "obs"));
        items.push(item(82, 10, "BAD", "bad"));
        sort_ranked(&mut items);

        let policy = QuotaPolicy::default();
        let out = apply_quota(items, &tier_map, &policy);
        assert_eq!(out.kept.iter().filter(|i| i.sources[0] == "pref").count(), 60);
        assert_eq!(out.kept.iter().filter(|i| i.sources[0] == "obs").count(), 2);
        assert_eq!(out.kept.iter().filter(|i| i.sources[0] == "bad").count(), 0);
        assert_eq!(out.capped_out, 21); // 20 条优选超额 + 1 条隔离

        // 补批不设上限：把 20 条超额优选全部补回，总数 82，仍排除隔离源。
        let mut kept = out.kept;
        let added = top_up(&mut kept, out.dropped, 100, &tier_map);
        assert_eq!(added, 20);
        assert_eq!(kept.len(), 82);
        assert_eq!(kept.iter().filter(|i| i.sources[0] == "pref").count(), 80);
        assert!(!kept.iter().any(|i| i.sources[0] == "bad"));
    }

    #[test]
    fn observing_source_not_starved_by_preferred_swarm() {
        let tier_map = tiers(&[
            ("pref", SourceTier::Preferred),
            ("obs", SourceTier::Observing),
        ]);
        // 优选源 30 条高分结果；观察源只有 2 条低分结果。
        let mut items: Vec<RankItem> = (0..30)
            .map(|i| item(i as u64, 100, &format!("P{i}"), "pref"))
            .collect();
        items.push(item(30, 35, "OBS-1", "obs"));
        items.push(item(31, 34, "OBS-2", "obs"));
        sort_ranked(&mut items);

        // 即使把观察配额调成 0，下限仍保证它有 2 条进入结果池。
        let policy = QuotaPolicy {
            caps: QuotaCaps {
                observing: 0,
                ..QuotaCaps::default()
            },
            observing_floor: 2,
        };
        let out = apply_quota(items, &tier_map, &policy);
        assert_eq!(out.kept.len(), 32);
        assert_eq!(out.kept.iter().filter(|i| i.sources[0] == "obs").count(), 2);
        assert!(!out.dropped.iter().any(|i| i.sources[0] == "obs"));
    }

    #[test]
    fn should_stop_defaults_off_and_requires_both_thresholds() {
        let off = StopConfig::default();
        assert_eq!(off.min_usable, 20);
        assert!(!should_stop(100, 1.0, &off));

        let on = StopConfig {
            enabled: true,
            ..StopConfig::default()
        };
        assert!(should_stop(20, 0.8, &on));
        assert!(!should_stop(19, 0.8, &on));
        assert!(!should_stop(20, 0.79, &on));
    }
}
