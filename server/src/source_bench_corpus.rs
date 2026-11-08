//! source_bench 的常量、内置语料与体检闸门（move-only 从 source_bench.rs 拆出）。

use super::*;

pub const TYPE_NOVEL: i64 = 0;
pub const TYPE_COMIC: i64 = 2;

/// 单条搜索预算（5s）；整批预算按源数×样本数动态估算（至少 60s，最多 600s）。
pub const PER_SAMPLE_BUDGET_SECS: u64 = 5;
pub const BATCH_BUDGET_SECS: u64 = 60;
pub const MAX_BATCH_BUDGET_SECS: u64 = 600;
/// 手动体检（用户点「跑一次体检」）的源并发数；真正的 HTTP 并发仍由
/// `req` 全局信号量（HTTP_CONCURRENCY）兜底。
pub const BENCH_CONCURRENCY: usize = 8;
/// 后台自动体检的源并发数：与用户搜索共用同一台小设备，降到 4，
/// 避免 8 路体检抢光带宽/连接，让用户搜索里的正常源被全局预算截断。
pub const BENCH_CONCURRENCY_BACKGROUND: usize = 4;

/// 整批预算：源多/样本多时按最坏情况（每条都打满单条预算）估算，
/// 避免源较多时只跑完前几十个、后面的源被记成 timeout。
pub fn batch_budget_secs(
    source_count: usize,
    samples_per_source: usize,
    concurrency: usize,
) -> u64 {
    let concurrency = concurrency.max(1) as u64;
    let est = (source_count.saturating_mul(samples_per_source) as u64)
        .saturating_mul(PER_SAMPLE_BUDGET_SECS)
        / concurrency;
    est.clamp(BATCH_BUDGET_SECS, MAX_BATCH_BUDGET_SECS)
}

/// 精选所需的最少样本数；不足的源不参与精选（冷启动回退现有 tier 逻辑）。
pub const MIN_CURATED_SAMPLES: i64 = 2;

/// 冷启动直通 preferred 的体检质量分下限。
///
/// 规则（每次体检都重新计算，是运行时判定而不是一次性名单）：
/// `samples >= MIN_CURATED_SAMPLES` 且 `hit_rate > 0` 且 `bench_score >= 50` 且未隔离
/// → 直接授予 `preferred`（`promote_streak` 抬到 promote 阈值），跳过“连续 3 次好观测”。
///
/// 50 ≈ 2/5 命中（60×0.4=24）+ 约 1.3s 平均耗时（30×0.74≈22）+ 全成功（10），
/// 0 命中或全超时的源过不了；真实搜索的降级路径（demote_streak）仍然生效。
/// 依据：2026-11 用 147 源实测，导入后 preferred=0、observing=142，一轮体检后精选恒为 0；
/// 去掉 tier 门槛时小说可取 14、漫画可取 4，卡点只在 tier 与体检脱节。
pub const BENCH_FAST_TRACK_MIN_SCORE: i32 = 50;

/// 精选资格：体检命中率必须 > 0。0 命中的源即使分数够、下限 min 未满也不入选，
/// 不拿低分源凑数——让“加强搜索”只跑真的能搜到书的源。
pub fn curated_hit_ok(hit_rate: f64) -> bool {
    hit_rate > 0.0
}

/// 从 `source_curated.meta_json` 读取上一次体检的命中率；缺失或解析失败按 0 处理（不具备资格）。
pub fn meta_hit_rate(meta_json: Option<&str>) -> f64 {
    meta_json
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get("hit_rate").and_then(|h| h.as_f64()))
        .unwrap_or(0.0)
}

pub const CATEGORY_FALLBACK: &str = "未分类";

/// 内置语料条目。标题/作者/分类只从这里读取，数据库里的 builtin 行仅承载 enabled 覆盖。
#[derive(Debug, Clone, Copy)]
pub struct BuiltinCorpus {
    pub title: &'static str,
    pub expected_author: Option<&'static str>,
    pub category: &'static str,
    pub source_type: i64,
    /// false = 可测但不计分。用于出版/实体书这类网文站普遍搜不到的条目：
    /// 它们仍会跑、仍能用来诊断站点，但不进入 hit_rate / 分数的分母。
    pub scored: bool,
}

/// 24 条：覆盖玄幻/都市/历史/科幻/出版/漫画，选长期存在的常见书名。
/// 漫画（source_type=2）单独占 12 条：按语料类型配额，避免漫画样本不足导致该类精选长期为空。
pub const BUILTIN_CORPUS: &[BuiltinCorpus] = &[
    // 玄幻（小说）
    BuiltinCorpus {
        title: "斗破苍穹",
        expected_author: Some("天蚕土豆"),
        category: "玄幻",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "凡人修仙传",
        expected_author: Some("忘语"),
        category: "玄幻",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "遮天",
        expected_author: Some("辰东"),
        category: "玄幻",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    // 都市（小说）
    BuiltinCorpus {
        title: "全职高手",
        expected_author: Some("蝴蝶蓝"),
        category: "都市",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "校花的贴身高手",
        expected_author: Some("鱼人二代"),
        category: "都市",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "黄金瞳",
        expected_author: Some("打眼"),
        category: "都市",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    // 历史（小说）
    BuiltinCorpus {
        title: "庆余年",
        expected_author: Some("猫腻"),
        category: "历史",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "明朝那些事儿",
        expected_author: Some("当年明月"),
        category: "历史",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    BuiltinCorpus {
        title: "赘婿",
        expected_author: Some("愤怒的香蕉"),
        category: "历史",
        source_type: TYPE_NOVEL,
        scored: true,
    },
    // 科幻（小说）
    BuiltinCorpus {
        title: "三体",
        expected_author: Some("刘慈欣"),
        category: "科幻",
        source_type: TYPE_NOVEL,
        // 出版/实体书：147 源实测命中率 13.6%（9/66），拉低整类而不代表源质量
        scored: false,
    },
    // 出版（小说）
    BuiltinCorpus {
        title: "活着",
        expected_author: Some("余华"),
        category: "出版",
        source_type: TYPE_NOVEL,
        // 出版/实体书：147 源实测命中率 3.0%（2/67），保留可测但不计分
        scored: false,
    },
    BuiltinCorpus {
        title: "平凡的世界",
        expected_author: Some("路遥"),
        category: "出版",
        source_type: TYPE_NOVEL,
        // 同属出版类，与《活着》同一问题（网文站覆盖率低），保留可测但不计分
        scored: false,
    },
    // 漫画（type=2）：12 条长期存在的常见书名
    BuiltinCorpus {
        title: "海贼王",
        expected_author: Some("尾田荣一郎"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "火影忍者",
        expected_author: Some("岸本齐史"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "名侦探柯南",
        expected_author: Some("青山刚昌"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "一人之下",
        expected_author: Some("米二"),
        category: "国漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "鬼灭之刃",
        expected_author: Some("吾峠呼世晴"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    // 漫画版《斗罗大陆》：用不带后缀的书名，否则与站点标题的包含率不足 0.6 会永远匹配不上
    BuiltinCorpus {
        title: "斗罗大陆",
        expected_author: Some("穆逢春"),
        category: "国漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "秦时明月",
        expected_author: None,
        category: "国漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "狐妖小红娘",
        expected_author: Some("小新"),
        category: "国漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "镇魂街",
        expected_author: Some("许辰"),
        category: "国漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "灌篮高手",
        expected_author: Some("井上雄彦"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "进击的巨人",
        expected_author: Some("谏山创"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
    BuiltinCorpus {
        title: "咒术回战",
        expected_author: Some("芥见下下"),
        category: "日漫",
        source_type: TYPE_COMIC,
        scored: true,
    },
];

/// 精选数量区间：漫画单独用更宽的区间（默认 min=3），小说仍用 min=8。
pub fn curated_bounds(cfg: &Config, source_type: i64) -> (usize, usize) {
    if source_type == TYPE_COMIC {
        (cfg.source_bench_comic_min, cfg.source_bench_comic_max)
    } else {
        (cfg.source_bench_min, cfg.source_bench_max)
    }
}

pub(super) fn next_seq() -> i64 {
    static SEQ: AtomicI64 = AtomicI64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// 同一时间只允许一个整批体检在跑（手动触发与 72h 定时共用）。
pub(super) static BENCH_INFLIGHT: AtomicBool = AtomicBool::new(false);

/// 查询是否有整批体检在跑；调度侧改用 `try_begin_bench`/`end_bench` 的返回值，本只读查询保留给诊断。
#[allow(dead_code)]
pub fn bench_inflight() -> bool {
    BENCH_INFLIGHT.load(Ordering::SeqCst)
}

pub fn try_begin_bench() -> bool {
    BENCH_INFLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

pub fn end_bench() {
    BENCH_INFLIGHT.store(false, Ordering::SeqCst);
}

/// 是否到了定时体检时间。没有历史（None/0）不触发，避免冷启动搜索顺带跑整批体检。
pub fn bench_due(last_bench_at: Option<i64>, now: i64, interval_hours: u64) -> bool {
    match last_bench_at {
        Some(t) if t > 0 => {
            now.saturating_sub(t) >= (interval_hours as i64).saturating_mul(3_600_000)
        }
        _ => false,
    }
}

pub fn now_ms() -> i64 {
    source_health::now_ms()
}
