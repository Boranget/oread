# 搜索调度与源优先级设计

本文回答一个问题：**在几十到上百个书源、几十秒预算、单机低配的前提下，怎么决定先搜谁、搜多久、什么时候停。**

它不是参数调优记录，而是一份约束模型。后续任何「某个源该不该排前面」的改动，都应该能落到本文的某个量上，而不是再加一条特判。

相关实现：[`server/src/routes/books_search_job.rs`](../server/src/routes/books_search_job.rs)、[`server/src/source_health_scoring.rs`](../server/src/source_health_scoring.rs)、[`server/src/source_bench_run.rs`](../server/src/source_bench_run.rs)、[`server/src/title_filter.rs`](../server/src/title_filter.rs)。

## 1. 现状里的三个隐含错误

### 1.1 `hits` 不是「搜到了」

`books_search_job.rs` 里的 `hits` 是 `web_book::search_book` 解析出的条目数，计数发生在 `books_search_api.rs` 的 `retain_title_matches` **之前**。所以它表示「这个源这次返回了多少条可解析结果」，不表示「有多少条书名真的包含关键词」。

由此，任何基于 `hits > 0` 的排序，实际排的是「站点活着且模板没坏」，不是「能搜到这本书」。

### 1.2 三件事被塞进一个分数

| 量 | 作用域 | 变化速度 | 现有承载 |
|---|---|---|---|
| 站点存活 `P_alive` | 站点/源 | 慢（天） | `consecutive_failures`、`tier`、`cooldown` |
| 查询产出 `P_yield` | 源 × 查询类 | 快（每次搜索） | `hit_rate`（`hit_count/search_count`） |
| 速度 `latency` | 每次请求 | 很快 | `avg_latency_ms`、`ttfb_ms_p90` |

`bench_score` 混合了前两个，`health_order_key` 用第三个加一个粗粒度存活判断，`tier` 又是前两个的另一种离散化。三者更新频率和作用域都不同，互相折算之后，每次出问题都只能加规则（超时算 partial、迁移复活、延迟只统计成功……）。这些规则本身没错，但它们是在补模型缺失。

### 1.3 排序是静态的

`preferred` / `observing` 两批 `tokio::join!`，入队顺序在开跑前定死。前面源的结果不会影响后面源的顺序；每源固定上限又让慢源在尾部吃掉整批预算；也无法早停。

## 2. 目标函数

搜索是**限时预算下的调度问题**，不是排序问题。

> 在 deadline 内最大化用户可见的有效结果数，同时保证首屏时间。

源 `i` 对查询 `q` 的期望边际价值：

```
EV(i, q) = P_alive(i) · P_yield(i | class(q)) · E[kept(i, q)] / E[latency(i)]
```

单位是「每秒有效结果」。排序按 `EV` 降序；冷启动和低频源用 UCB 加成：

```
score(i, q) = EV(i, q) + c · sqrt(ln N / n_i)
```

探索配额应设上限（建议 ≤ 10% 并发槽位），避免为了探索把首屏拖慢。

## 3. 证据模型

一张事实表，按 `(source_url, query_class)` 聚合。**只存原始观测，派生量不落库。**

| 字段 | 说明 |
|---|---|
| `ts` | 观测时间 |
| `origin` | `search` / `bench` / `patrol` |
| `kind` | `ok` / `timeout` / `fail` / `partial` |
| `latency_ms` | 只有成功请求进入延迟统计 |
| `raw_hits` | 解析条数（现有 `hits`） |
| `kept_hits` | 书名过滤后条数（新增，yield 的分母/分子都靠它） |
| `query_class` | `hot` / `normal` / `obscure` / `author` / `serial` |

派生量：

- `P_alive`：站点级 EWMA，半衰期按源更新频率自适应（高频 7 天、低频 30 天）。
- `E[latency]`：成功样本的 p50（吞吐估计）与 p90（尾部风险）。
- `P_yield`：`kept_hits > 0` 的比例，按 `query_class` 分桶，带伪计数先验。

> 现有的「最近 32 次成功样本环形均值」是过渡实现。环形窗口在低频源上样本不足，在高频源上又会记住三个月前的状态；最终应换成时间衰减。

## 4. 查询分类

语料只有 24 条热门书。`百年孤独` 和 `斗破苍穹` 在「哪些源会命中」上是两个不同的分布。没有 `query_class`，任何 yield 先验都会把冷门查询误判成「全都不行」。

分类信号要便宜、可解释：

- 命中体检语料标题 → `hot`
- 含 `作者:` / `by` 前缀 → `author`
- 含数字 / `第X章` → `serial`
- 归一化长度 ≤ 2 或 ≥ 12 → 偏 `obscure`
- 兜底 → `normal`

分错类只影响顺序，不影响正确性，所以允许启发式；但必须有 `normal` 兜底。

## 5. 体检语料的定位

体检语料**不是排序键**，它是先验和诊断：

1. **冷启动先验**：新源没有搜索证据时，用体检结果初始化 `P_yield` 的伪计数；真实样本上来后被覆盖。
2. **规则完整性**：能解析出语料 = search 链路没坏。用于 `rule_stale` 判定与 curated 资格。
3. **延迟校准**：低并发下的真实延迟，用来修正搜索时被 16 路并发污染的 latency。
4. **类型覆盖**：`novel` / `comic` 分开，避免漫画源挤占小说槽位。

明确不做：把 `bench_score` 直接当排序分。它的粒度是「源」而不是「源 × 查询类」，二值、慢变、无时间维度，且 24 条语料对冷门书零覆盖。

## 6. 调度

单一优先队列 + anytime：

```
loop {
  取 score 最高的未跑源 → 占槽
  槽释放 → 用结果更新该源证据 → 重排剩余队列
  已获得 kept ≥ 阈值 且 剩余源 EV 总和 < 阈值 → 早停
  now > deadline → 停
}
```

配套：

- 单源上限保留，但预算**动态分配**：`min(per_source_cap, 剩余时间 / 剩余高价值源数 × 2)`。
- 分层 flush：`preferred` 跑完立刻写 job state，首屏先出，慢源后台补齐。
- 超时语义不变：`timeout = 未知` → `partial` + `usage=false`，不累计失败、不冷却。

容量估算：136 源 / 16 并发 / 平均 1.5s ≈ 13s；平均 3s ≈ 25s。所以 40s 预算下「冷门查询跑完全部源」是可行的。漏召回主要来自静态排序 + 每源上限造成的尾部饥饿，不是预算不够。

## 7. 状态机简化

存储层只保留原始证据 + 人工干预（`manual_block` / `manual_pin`）。`tier` / `score` / `quarantined` / `cooling` 改为**派生视图**：

- `quarantined` = `P_alive < θ` 或 `manual_block`
- `cooling` = 最近一次 `fail` 在 T 内
- `preferred` = `P_alive` 高 且 `P_yield` 高 且 `rule_complete`
- `curated` = 体检资格规则（保留现状）

状态不落库，改规则即生效，「迁移复活误隔离源」这类补丁不再需要。

## 8. 可观测性

每次搜索输出一条 decision trace（debug 环 + 结构化日志）：

```
source, rank_in_queue, score_breakdown{p_alive, p_yield, latency, ucb},
slot_wait_ms, latency_ms, kind, raw_hits, kept_hits, stopped_by
```

`/healthz` 增加聚合：按 `query_class` 的命中率、早停率、尾部超时率。

## 9. 分阶段落地

| 阶段 | 内容 | 指标门 |
|---|---|---|
| P0 已完成 | 超时=未知、延迟只统计成功、8s/40s | — |
| P1 | 记录 `kept_hits`；yield 证据；排序键改 `(P_alive, EV, latency)` | `all` 正主命中率 ≥ 60%，首屏 p90 < 2s |
| P2 | `query_class` + 时间衰减 + UCB 探索（≤ 10% 槽位） | 冷门查询 0 结果率下降，热门查询不退化 |
| P3 | anytime 调度 + 动态预算 + 早停 + 分层 flush | 平均总耗时 −30%，命中率不降 |
| P4 | 语料降级为纯先验/诊断；`tier` 改派生；删迁移补丁 | 无新增「复活/校准」类迁移 |

### P1 的具体口径

- 新增 `kept_hit_count`（书名匹配命中条数之和）与 `yield_ok_count`（`kept_hits > 0` 的搜索次数），只在 `usage=true` 时累计。
- 平滑产出率：`y = (yield_ok + 0.3×4) / (search_count + 4)`（先验 0.3，强度 4 次观测）。
- 优先级：`ev = y / max(latency, 250ms)`，入队键为 `(bucket, -ev)`；`bucket` 仍区分「可用 / 无观测 / 近期失败」。
- 只有 `kept_hits` 参与 yield；`hits` 保留为「解析成功」证据，不参与排序。

## 10. 取舍与风险

- **UCB 探索**牺牲少量延迟换长期覆盖，必须设配额上限。
- **查询分类**会误分类，必须有 `normal` 兜底，且分类不改变参与集合。
- **时间衰减**的半衰期需要按源更新频率自适应，否则低频源证据会过期。
- **yield 先验**对「活着但确实没这本书」的源是负反馈，这正是想要的；但要防止冷启动阶段把新源永久压底，所以保留伪计数先验与 UCB。
