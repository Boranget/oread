# 墨水屏阅读端样式规范（Boranlib）

> 版本：v1（2026-11-04）。依据真机实测：[KPW3.md](./KPW3.md)（探针 v2.1.0，run `B4YFGB`）。
> 本文的**允许/禁止清单只约束 `/k`（`server/src/routes/kindle.rs` 的服务端渲染页面）**。
> 现代墨水屏（Boox、海信 A5 等）跑的是 SPA，浏览器能力与现代手机一致，**不受本清单约束**；
> SPA 的 `?theme=eink` 只是四主题之一的直达别名（见第 7 节），不限制 `flex/grid` 等写法。

## 1. 适用范围

| 范围 | 载体 | 是否受本文约束 |
|---|---|---|
| `/k` 零脚本 SSR（书架、目录、阅读、搜索、登录、`/k/_selftest`） | `server/src/routes/kindle.rs` 内联 CSS | **是** |
| SPA（`server/web`，Svelte） | `web/src/**` | **否**（仅主题色参考第 7 节 token） |

判定前提：`kindle` / `silk` UA 由服务端 303 到 `/k`；其他 UA 进 SPA。渲染器不再由用户选择。

## 2. Design token

### 2.1 颜色（只允许纯灰阶，不得引入彩色）

| Token | 值 | 用途 | 对比度（对 `#fff`） |
|---|---|---|---|
| `ink` | `#000` | 正文、标题、可点文字 | 21.0 : 1 |
| `ink-2` | `#333` | 次级信息（作者、时间、说明） | 12.6 : 1 |
| `ink-3` | `#555` | 弱信息（提示、占位说明） | 7.45 : 1 |
| `rule` | `#ccc` | 分隔线、边框、禁用态 | 1.61 : 1（**不得承载文字**） |
| `paper` | `#fff` | 页面底色、控件底色 | — |
| `bar`（可选） | `#e6e6e6` | 顶栏/底栏底色（沿用既有 `/k`） | — |

规则：除以上 5 个灰阶（+ 既有 `#e6e6e6` 栏底色）外，**不得出现任何其他颜色**（含彩色强调、渐变、半透明遮罩）。

### 2.2 字号 / 行高（四档，px，整数）

| 档位 | 字号 | 行高 | 行高比 |
|---|---|---|---|
| 超小（1） | 20 | 36 | 1.80 |
| 标准（2） | 28 | 46 | 1.64 |
| 大（3） | 32 | 52 | 1.63 |
| 超大（4） | 38 | 62 | 1.63 |

- 只用整数 px（KPW3 不支持亚像素：`0.5px` 实测宽度为 0）。
- 不使用 `em` / `rem` / `vw` / `vh` / `calc` / `clamp`。

### 2.3 间距与线条

| Token | 值 | 用途 |
|---|---|---|
| `space-1` | 8px | 行内元素、图标与文字 |
| `space-2` | 12px | 卡片内边距、段落间距 |
| `space-3` | 16px | 区块间距 |
| `space-4` | 24px | 章节/页面级留白 |
| `rule-w` | 1px | 所有分隔线与边框 |
| 圆角 | 0 | **无圆角** |
| 阴影 | 无 | **无阴影** |
| 过渡 | 无 | **无 transition / animation** |

版心固定：总宽 `1072`，左右 `36` 边距，内容 `1000`；阅读页 stage 高 `1268`，可用 `1196`；底栏高 `64`（5 格等分）。

## 3. 允许清单（`/k` 内唯一可用写法）

| 能力 | 说明 |
|---|---|
| `display: block` / `table` / `table-cell` / `inline-block` | 布局主手段 |
| `float: left/right` + `clear` | 多列、左右分栏 |
| `display: -webkit-box` / `display: box` | 仅真机实测可用的横向排布（其它 flex 语法禁止） |
| `-webkit-box-flex` / `-webkit-box-align` / `-webkit-box-pack` | 同上，且需同时给出 block/table 兜底 |
| `width` / `height` / `margin` / `padding` | 只用 px 或百分比 |
| `border` / `border-collapse` / `border-spacing` | 1px |
| `font-family` / `font-size` / `font-weight` / `line-height` / `letter-spacing` / `text-align` / `text-indent` | 排版 |
| `color` / `background`（纯色） | 仅第 2.1 节灰阶 |
| `position: fixed` | **仅限阅读页底栏**（真机回读可用；需真机二次核验） |
| `overflow: hidden` / `overflow-x: hidden` | 防横向溢出 |
| `-webkit-text-size-adjust: none` | 防老引擎自动放大 |

## 4. 禁止清单（`/k` 内出现即视为缺陷）

来自 KPW3.md 实测不可用项，外加视觉约束：

| 禁止项 | 真机证据 / 原因 |
|---|---|
| `display: flex` / `inline-flex` | `computed_display=block`，三子块竖排 |
| `display: grid` / `inline-grid`、`grid-template-*`、`grid-auto-*`、`grid-column` | `computed_display=block`、`computed_columns=""` |
| `*vw` | `declared_accepted=false`，回读宿主宽度 |
| `*vh` | `declared_accepted=false`，回读 `0px` |
| `calc()` | 不支持，回读宿主宽度 |
| `clamp()` | 不支持，回读宿主宽度 |
| `position: sticky` | 回读 `static` |
| `transform` / `-webkit-transform` | 回读空串，元素位置不变 |
| `transition` / `-webkit-transition` / `animation` | 回读空串，`rAF` 不存在 |
| `object-fit` | 回读空串 |
| `box-shadow` | 设计约束：无阴影 |
| `border-radius` | 设计约束：无圆角 |
| `background-image`（含渐变） | 设计约束：纯色底 |
| `@media` | 设计约束：不依赖断点（`matchMedia` 单色/慢刷新查询在真机全部为 `false`） |

> 唯一例外：**重置型声明**（`box-shadow: none`、`border-radius: 0`、`transform: none`、`transition: none`）不算命中，用于抵消第三方样式；`/k` 当前无第三方样式，正常不会出现。
> `/k/_selftest` 是**故意展示不可用项**的诊断页，lint 必须按函数范围排除。

## 5. 排版规则

1. **字体**：中文用 `serif`（`font-family: serif`）；不用 `system-ui` 等无衬线作为正文。
2. **正文**：首行缩进 `2em`；段间距 `0.6` 行（即 `0.6 * line-height` 的 `margin-bottom`，取整 px）。
3. **标题**：标题字号与正文字号比值 **≥ 1.4**（如正文 20 → 标题 ≥ 28）。
4. **页码**：右对齐，单独一行，与正文同档字号。
5. **目录**：章号右对齐；等宽数字优先用 `font-variant-numeric: tabular-nums`，**真机不支持时用空格对齐**（不要依赖字体特性）。
6. **行宽**：正文单行不超过 `1000px` 版心；不做两端对齐（`text-align: justify` 在旧引擎上字距不可控）。

## 6. 对比度要求

- 正文（`#000` / `#fff`）：**≥ 7 : 1**，实测 21.0 : 1。
- 次级（`#333` / `#fff`）：**≥ 4.5 : 1**，实测 12.63 : 1。
- 弱信息（`#555` / `#fff`）：**≥ 4.5 : 1**，实测 7.45 : 1。
- `#ccc` 只能做 1px 分隔线/边框，**不得作为文字颜色**。

计算方式（WCAG 2.x 相对亮度）：

```
c = v / 255
lin(c) = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ^ 2.4
L = 0.2126 * lin(R) + 0.7152 * lin(G) + 0.0722 * lin(B)
contrast = (L_max + 0.05) / (L_min + 0.05)
```

校验阈值：正文/标题 ≥ 7.0，次级/弱信息 ≥ 4.5。

## 7. SPA 的四主题与 `?theme=eink` 别名

- SPA 主题四项：**浅色 `light` / 深色 `dark` / 护眼 `sepia` / 墨水屏 `eink`**，统一在设置页“配色主题”选择器里切换（无另行按钮、无设备下拉）。
- 持久化：四项共用同一份账号偏好 `GET/POST /api/settings` 的 `theme` 字段（服务端白名单含 `eink`），不另存 Cookie，不存在第二套存法。
- `?theme=eink` 只是**直达别名**：进入时等价于选中“墨水屏”并将偏好写成 `eink`，随后从地址栏移除该参数；刷新与重新打开都只读账号偏好。
- 墨水屏主题只改**配色/对比度/字重**：正文纯黑、次级 `#333`、弱信息 `#555`、分隔 `#ccc`、底色纯白、强调色降为黑色；**不改布局、不改字号、不改阅读模式**，完全继承移动端响应式布局。
- 现代墨水屏（Boox、海信 A5 等）默认走 SPA 普通响应式；选“墨水屏”主题才进入该配色。
- 护眼主题实测配色：底 `#f5ecd9`、正文 `#2f2a22`、次级 `#6b6357`、边框 `#d8ccb6`，正文对比度实测 **12.12:1**、次级 **5.04:1**；墨水屏 `#000` on `#fff` 为 **21:1**。

## 8. 校验

`scripts/eink-lint.mjs` 扫描 `server/src/routes/kindle.rs` 内联 CSS（仅字符串字面量，排除 `_selftest` 与 `#[cfg(test)]`），命中第 4 节禁止项即 `exit 1`，并打印文件、行号、命中词。CI 的 `check` job 会在前端构建后执行。
