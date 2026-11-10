# 搜索调度与源优先级设计

本文回答一个问题：**在几十到上百个书源、几十秒预算、单机低配的前提下，怎么决定先搜谁、搜多久、什么时候停。**

它不是参数调优记录，而是一份约束模型。后续任何「某个源该不该排前面」的改动，都应该能落到本文的某个量上，而不是再加一条特判。

相关实现：[`server/src/routes/books_search_job.rs`](../server/src/routes/books_search_job.rs)、[`server/src/source_health_scoring.rs`](../server/src/source_health_scoring.rs)、[`server/src/source_bench_run.rs`](../server/src/source_bench_run.rs)、[`server/src/title_filter.rs`](../server/src/title_filter.rs)。

## 1. 改造前的三个隐含错误

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

**站点级慢变量**留在 `source_health`（存活、延迟、全局产出率），**源 × 查询类快变量**存 `source_query_stats`：

| 表 | 粒度 | 字段 |
|---|---|---|
| `source_health` | 源 | `last_status`、`avg_latency_ms`（仅成功样本）、`ttfb_ms_p90`、`search_count`、`kept_hit_count`、`yield_ok_count`、`parse_ok_count` |
| `source_query_stats` | 源 × `query_class` | `search_count`、`yield_ok_count`、`kept_hit_count`、`updated_at` |

口径：

- `kept_hits` = 书名过滤后条数（`title_filter::count_title_matches`），是 yield 的唯一分子来源；`hits` 只表示「解析成功」。
- 只有真实搜索（`usage=true`）写这两张表。体检/巡检只更新可用性与延迟，不污染 yield。
- `source_query_stats` 用**指数衰减**存储：写入时先按 `updated_at` 打折（半衰期 14 天）再累加，读路径不再算时间差。
- 延迟只统计成功请求（最近 32 次成功样本环形均值），超时/失败不参与。

派生量（读时计算，不落库）：

- `P_yield`：`(yield_ok + 4·global) / (search_count + 4)` 的分层平滑；`global` 是站点级 `smoothed_yield`（先验 0.30、强度 4）。
- `E[latency]`：成功样本均值 + `ttfb_ms_p90` 作为尾部参考。
- `rule_stale`：真实搜索 ≥ 5 次且解析成功率不足一半时置为诊断标记（只提示，不改档位）。

## 4. 查询分类

语料只有 24 条热门书。`百年孤独` 和 `斗破苍穹` 在「哪些源会命中」上是两个不同的分布。没有 `query_class`，任何 yield 先验都会把冷门查询误判成「全都不行」。

分类信号要便宜、可解释：

- 命中体检语料标题 → `hot`
- 含 `作者:` / `by` 前缀 → `author`
- 含数字 / `第X章` → `serial`
- 归一化长度 ≤ 2 或 ≥ 12 → 偏 `obscure`
- 兜底 → `normal`

分错类只影响顺序，不影响正确性，所以允许启发式；但必须有 `normal` 兜底。

实现：`server/src/query_class.rs`。分类顺序为 `author > serial > hot > obscure > normal`，`obscure` 的阈值是归一化长度 ≤ 2 或 ≥ 12。分类只用于选择 `source_query_stats` 的分桶，不改变参与搜索的源集合。

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
  已获得 kept ≥ 阈值 且 已完成源数 ≥ 门槛 → 早停
  now > deadline → 停
}
```

配套：

- 单源上限保留，但预算**动态分配**：`clamp(剩余时间 / 剩余轮数 × 2, 2s, per_source_cap)`，`剩余轮数 = ceil(剩余源数 / 槽位)`。越接近 deadline，单源预算越短，保证还能再试一个源而不是卡在最后一个慢源上。
- 早停：`finished ≥ min(24, 总源数)` 且 `kept_total ≥ 60`（两页）时停止**取新源**；已在飞的请求照常完成，结果照常入池。
- 停止只影响取源：队列里没被取出的源记为 `skipped`，不写健康、不算站点失败；被 deadline 截断的请求记为 `truncated`，同样不写健康。
- 分层 flush：每个源完成即写 job state，首屏先出，慢源后台补齐。
- 超时语义不变：`timeout = 未知` → `partial` + `usage=false`，不累计失败、不冷却。
- 排队等槽不计入单源预算；剩余不足 500ms 时不再发请求，直接记为 `skipped`。

容量估算：136 源 / 16 并发 / 平均 1.5s ≈ 13s；平均 3s ≈ 25s。所以 40s 预算下「冷门查询跑完全部源」是可行的。漏召回主要来自静态排序 + 每源上限造成的尾部饥饿，不是预算不够。

实现位置：`run_search_job` 用 `futures::stream::unfold` 拉取优先队列、`buffer_unordered` 维持并发；每个源完成回到主循环后调用 `resort_queue` 重排。`scheduled_sources` 在真正取源时累加，因此进度接口仍能区分「已排上 / 尚未排上」。

## 7. 状态机简化

存储层只保留原始证据 + 人工干预（`manual_block` / `manual_pin`）。`tier` / `score` / `quarantined` / `cooling` 改为**派生视图**：

- `quarantined` = `P_alive < θ` 或 `manual_block`
- `cooling` = 最近一次 `fail` 在 T 内
- `preferred` = `P_alive` 高 且 `P_yield` 高 且 `rule_complete`
- `curated` = 体检资格规则（保留现状）

状态不落库，改规则即生效，「迁移复活误隔离源」这类补丁不再需要。

## 8. 可观测性

每次搜索输出结构化日志（默认 `debug`）：

- `search_schedule`：`job_id`、`scope`、`query_class`、源数、槽位；早停时输出 `finished` / `kept_total` / `remaining`。
- `search_source`：`source`、`scope`、`kind`（`ok`/`error`/`timeout`/`truncated`/`skipped`）、`timed_out`、`latency_ms`、`hits`、`kept_hits`。

`/healthz` 已暴露 `all_per_source_secs`；按 `query_class` 的命中率聚合仍是后续项（P4）。

## 9. 分阶段落地

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 已完成 | 超时=未知、延迟只统计成功、8s/40s、预算拆分 | ✅ 已发布 |
| P1 已完成 | 记录 `kept_hits`；yield 证据；排序键改 `(bucket, -EV)` | ✅ 已实现（迁移 0015） |
| P2 已完成 | `query_class` + 14 天时间衰减 + 类内分层平滑 + UCB 探索乘子 | ✅ 已实现（迁移 0016） |
| P3 已完成 | anytime 调度 + 动态预算 + 早停 + 逐源 flush | ✅ 已实现 |
| P4 部分完成 | 体检降级为纯先验/诊断（已做）；`tier`/`quarantined` 改派生（未做） | ⏸ 见下 |

### P1 的具体口径

- 新增 `kept_hit_count`（书名匹配命中条数之和）与 `yield_ok_count`（`kept_hits > 0` 的搜索次数），只在 `usage=true` 时累计。
- 平滑产出率：`y = (yield_ok + 0.3×4) / (search_count + 4)`（先验 0.3，强度 4 次观测）。
- 优先级：`ev = y / max(latency, 250ms)`，入队键为 `(bucket, -ev)`；`bucket` 仍区分「可用 / 无观测 / 近期失败」。
- 只有 `kept_hits` 参与 yield；`hits` 保留为「解析成功」证据，不参与排序。

### P2 的具体口径

- 类内产出率：`y_class = (yield_ok_class + 4·y_global) / (search_count_class + 4)`；类内证据积累到 4 次后与全局先验等权。
- 探索乘子：`1 + 0.10·sqrt(ln(N+1) / (n_i+1))`，`N` 是该查询类的历史搜索总数、`n_i` 是该源在该类下的样本数。样本越多，加成越小。
- 冷启动：完全没有真实搜索证据（`source_health.search_count = 0` 且类内样本为 0）时，用体检 `meta_json.hit_rate` 作为产出率先验；一旦有真实搜索立即让位。
- 衰减：写入时按 `0.5^(Δ/14天)` 折扣旧计数再累加，避免三个月前的证据和昨天一样重。

### P3 的具体口径

- 单一优先队列 + `buffer_unordered`，每完成一个源就按最新证据重排，不再有 `preferred` 整批占槽。
- 动态预算与早停阈值见第 6 节。
- 停止只停「取新源」，不取消在飞请求；剩余源记 `skipped`，截断请求记 `truncated`，两者都不写健康。

### P4 为什么只做了一半

已做：体检命中率仅作冷启动先验（不再参与排序）、`rule_stale` 诊断标记。

未做：把 `tier` / `score` / `quarantined` 从存储改为派生视图。原因：

1. `tier` / `quarantined` 同时被 curated 重算、源账本（`source_ledger`）和人工干预使用，改成派生会改动多个写入方与历史数据；
2. 当前设备不可达，无法用真实库验证迁移后的 curated/账本一致性；
3. 收益是「改规则即生效」，而现有规则已经能通过 `apply_outcome` 平滑演进。

因此这一步留到设备恢复、能在真实数据上做前后对比时再动。

## 10. 取舍与风险

- **UCB 探索**牺牲少量延迟换长期覆盖，必须设配额上限。
- **查询分类**会误分类，必须有 `normal` 兜底，且分类不改变参与集合。
- **时间衰减**的半衰期需要按源更新频率自适应，否则低频源证据会过期。
- **yield 先验**对「活着但确实没这本书」的源是负反馈，这正是想要的；但要防止冷启动阶段把新源永久压底，所以保留伪计数先验与 UCB。
