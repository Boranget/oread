# KPW3 真机能力事实表（Boranlib `/k` 与漫画阅读器基线）

> 生成时间：2026-11-03（Asia/Shanghai）。本文只记录**真机实测**与**明确未验证**项，不写“我记得”“应该可以”。
> 数据源：`D:\tools\kpw3-probe\reports\20261103-172026-B4YFGB.json`（探针 v2.1.0，run `B4YFGB`）。
> 说明：`D:\tools\kpw3-probe\analysis\report.md` 是 v1.0.1 的旧结论；v2.0 run `KHMQCT` 的部分网络/图片口径已被本次 v2.1 复测取代，冲突处一律以本 run 为准。

## 1. 设备基线

| 项 | 实测值 | 备注 |
|---|---|---|
| 可用视口 `innerWidth × innerHeight` | **1072 × 1268** | 设计一屏按 1072 宽、1268 高；`docClientWidth=1072` |
| `outerWidth × outerHeight` | 1072 × 1232 | 含浏览器外壳，不能当内容高度 |
| `screenWidth × screenHeight` | **1072 × 1448** | `availWidth/availHeight` 同为 1072 × 1448 |
| devicePixelRatio | **1.7964072227478027（≈1.7964）** | 非整数；位图素材按约 1.8x 准备，但受体积约束 |
| 亚像素 | **不支持**：`0.5px` → `offsetWidth=0`、`rectWidth=0`、`computed=0px` | `fractional_px_supported=false`；布局只用整数 px |
| colorDepth / pixelDepth | **8 / 8** | 与旧 WebKit 一致 |
| 插件 / MIME 类型 | **0 / 0** | 无 PDF、Flash 等插件，不能依赖插件能力 |
| platform / vendor | `Linux armv7l` / `Apple Inc.` | vendor 是 WebKit 自报，不代表硬件厂 |
| language / languages | `zh-cn` / 空 | 不能依赖 `navigator.languages` |
| compatMode | `CSS1Compat` | 标准模式；`documentElement.clientWidth=1072` |
| JavaScript UA | `Mozilla/5.0 (X11; ; U; Linux armv7l; zh-cn) AppleWebKit/534.26+ (KHTML, like Gecko) Version/5.0 Safari/534.26+` | 页面内 `navigator.userAgent` |
| HTTP UA | `Mozilla/5.0 (X11; U; Linux armv7l like Android; en-us) AppleWebKit/531.2+ (KHTML, like Gecko) Version/5.0 Safari/533.2+ Kindle/3.0+` | 服务端 `server_requests` 实际收到 |
| UA 一致性 | **不一致**：JS 534.26+ vs HTTP 531.2+ / Kindle/3.0+ | **UA 不能用来判定能力**；服务端分支以请求头为准，前端能力必须实测 |
| 时区 | **UTC，`offset_minutes=0`**；`Date` 为 `GMT+0000 (UTC)` | **日期/时间必须服务端格式化**，客户端不得用本地 `Date` 拼展示时间 |
| cookie / localStorage | `cookieEnabled=true`；localStorage 可写 100KB | **cookie 写入/删除/跨请求持久化均成功**（见 2.3）；localStorage 100KB 可读写 |

> 已知坑：设备时区是 UTC。服务端返回 UTC 时间时，`/k` 与漫画阅读器不能直接把它当本地时间显示；统一由服务端按目标时区格式化，或前端明确按 UTC 展示。

## 2. 能力表

### 2.1 CSS 布局与视觉（16 项）

| 能力 | 结论 | 证据数值 |
|---|---|---|
| `display:flex` | **不可用** | `computed_display=block`；三子块 `top=[0,20,40]`、`left=[0,0,0]`、`is_row=false` |
| `display:grid` | **不可用** | `computed_display=block`、`computed_columns=""`、`two_columns=false`；`coords_relative_to=host`、`child_lefts=[0,0]`、`child_tops=[0,20]` |
| `display:-webkit-box` | **可用** | `computed_display=-webkit-box`、`is_row=true`、`left=[0,60]` |
| `float` | **可用** | 两块 `left=0/100`、`same_top=true`；配合百分比宽度可做多列 |
| `vw` | **不可用** | `declared_accepted=false`、`style_readback=""`；`20vw` 回读 `320px`（=宿主宽度回落，不是 20vw 计算结果），期望 `214.4px`，`fallback_used=true` |
| `vh` | **不可用** | `declared_accepted=false`、`style_readback=""`；`20vh` 回读 `0px`（宿主高度回落），期望 `253.6px`，`fallback_used=true` |
| `calc()` | **不可用** | `declared_accepted=false`、`style_readback=""`；`calc(100px + 20px)` 回读 `320px`（宿主宽度回落，不是计算结果），期望 `120px`，`fallback_used=true` |
| `clamp()` | **不可用** | `declared_accepted=false`、`style_readback=""`；`clamp(40px,50px,60px)` 回读 `320px`（宿主宽度回落，不是计算结果），期望 `50px`，`fallback_used=true` |
| `position:fixed` | **可用（仅回读）** | `computed_position=fixed`、`rect=(0,0)`；本轮未做二次视觉核验，落地时仍需真机看一眼 |
| `position:sticky` | **不可用** | `computed_position=static`、`declared_readback=""`、`pass=false` |
| `border-radius` | **可用** | `computed_radius=10px`；`-webkit-border-radius` 同样回读 `10px` |
| `box-shadow` | **可用** | `computed_shadow=rgb(0, 0, 0) 2px 2px 2px 0px` |
| `transform` | **不可用** | `computed_transform=""`；30×30 元素 `rect` 未变化，`pass=false` |
| `transition` | **不可用** | `computed_duration=""`、`computed_property=""`、`pass=false` |
| `opacity` | **可用** | `computed_opacity=0.5` |
| `object-fit` | **不可用** | `computed_object_fit=""`、`pass=false` |

补充：`matchMedia` 本身可用，但 `(monochrome)`、`(hover:hover)`、`(pointer:coarse)`、`(update:slow)` 全部为 `false`，不能做“墨水屏专属”媒体查询分支。

### 2.2 JavaScript 运行时与网络（16 项）

| 能力 | 结论 | 证据数值 |
|---|---|---|
| `XMLHttpRequest` | **可用** | `xmlhttprequest.xhr={status:200, bytes:8, elapsed_ms:193}`；`typeof=object` |
| `WebSocket` | **部分：可构造** | `constructed, readyState=0`；未做握手，**连通性未验证** |
| `FileReader` | **部分：可构造** | `constructed, readAsText present`；未做实际读取，**读取结果未验证** |
| `FormData` | **可用** | `construct+append ok` |
| `Promise` | **不可用** | `typeof=undefined`，`Error: absent` |
| `fetch` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Map` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Set` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Symbol` | **不可用** | `typeof=undefined`，`Error: absent` |
| `WeakMap` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Object.assign` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Array.from` | **不可用** | `typeof=undefined`，`Error: absent` |
| `Array.prototype.includes` | **不可用** | `typeof=undefined`，`Error: absent` |
| `String.prototype.startsWith` | **不可用** | `typeof=undefined`，`Error: absent` |
| `navigator.sendBeacon` | **不可用** | `typeof=undefined`，`Error: absent` |
| `CSS.supports` | **不可用** | `typeof=undefined`，`Error: absent` |

补充可用项：`JSON`（stringify/parse）、`Date`（`Date.now` 可采样：`min_delta_ms=1`、`samples=14`、`span_ms=20`；v2.0 采不到是探针缺陷）、`forEach/map/filter/indexOf/sort`、`split/trim/replace`、`matchMedia`、`getComputedStyle`、`querySelector`、`addEventListener`。不可用项：`Blob`（`'[object BlobConstructor]' is not a constructor`）。

### 2.3 存储、绘图与 Cookie（8 项）

| 能力 | 结论 | 证据数值 |
|---|---|---|
| canvas 2D / `getImageData` | **可用** | `fill+getImageData ok`，7ms |
| canvas `toDataURL` | **不可用** | PNG/JPEG 都返回 `data:,`，长度 6，`pass=false` |
| WebSQL | **部分：仅构造器存在** | `window.openDatabase` 为 `function`；`html5_databases=undefined`；未做建库/事务验证 |
| IndexedDB | **不可用** | `indexeddb=undefined`、`webkit_indexeddb=undefined` |
| Service Worker | **不可用** | `serviceworker=undefined` |
| Cache API | **不可用** | `caches=undefined`（`appcache` 仍为 object，但不建议依赖） |
| localStorage | **可用（100KB）** | 请求 102400B，`write_ok=true`，`readback_length=102400`，7ms |
| cookie | **可用（写入/删除/跨请求持久化均成功）** | `cookie_enabled=true`、`write_ok=true`、`delete_ok=true`；`raw_cookie_after_write` 可见 `kpw3probe=1`；另设 `kpw3persist=1` 后，服务端后续请求实际收到 `Cookie: kpw3persist=1` |

补充：`sessionStorage` 的 set/get/remove 均 OK；但持久化仍以 localStorage 为准。

### 2.4 图片格式（4 项）

| 能力 | 结论 | 证据数值 |
|---|---|---|
| JPEG | **可用** | 640×480 **loaded=425ms**；canvas 解码 **267ms**、drawImage **245ms** |
| PNG | **可用** | 64×64 **loaded=440ms**，`ok=true` |
| GIF | **可用** | 1×1 **loaded=439ms**，`ok=true`；1×1 GIF data URI **208ms** |
| WebP | **不可用** | `onerror`，**438ms**，`width=0/height=0` |

### 2.5 性能接口（2 项）

| 能力 | 结论 | 证据数值 |
|---|---|---|
| `window.performance` / Navigation Timing | **不可用** | `exists=false`、`timing_exists=false`、`now_present=false` |
| `requestAnimationFrame` | **不可用** | `exists=false`、`webkit_prefix=false`；**不能做动画** |

## 3. 语法边界

只有 `const` 能被解析，语义未验证；其余 15 类 ES6 语法全部 `SyntaxError: Parse error`：

| 语法 | 结果 | 语法 | 结果 |
|---|---|---|---|
| `const` | **可解析（语义未验证）** | `let` | SyntaxError |
| 箭头函数 | SyntaxError | 模板字符串 | SyntaxError |
| 默认参数 | SyntaxError | 数组解构 | SyntaxError |
| 对象解构 | SyntaxError | `for...of` | SyntaxError |
| `class` 声明 | SyntaxError | 数组展开 | SyntaxError |
| 对象简写 | SyntaxError | 计算属性名 | SyntaxError |
| generator | SyntaxError | async 函数 | SyntaxError |
| 可选链 `?.` | SyntaxError | 空值合并 `??` | SyntaxError |

**可行基线**：纯 ES5 + XHR + JSON + DOM + localStorage；单页脚本/资源体积 **≤100KB**。`const` 虽然能解析，但语义未验证，不纳入基线。

## 4. 性能与网络

| 场景 | 实测 | 解读 |
|---|---|---|
| 32KB 下载 | **328ms，97.56KB/s** | TTFB **323ms**，真实传输约 **4ms**；首屏预算按 0.3s 量级估 |
| 256KB 下载 | **816ms，313.73KB/s** | `got_bytes=262144`、`complete_bytes=true`；TTFB **171ms**，body 约 **645ms（≈397KB/s）**。v2.0 的 2805ms 超时是探针缺陷，v2.1 已修复并实测下完 |
| DOM 插入 1000 节点 | **51ms**（移除 22ms） | 列表渲染按此估算；200 节点插入 12ms（移除 5ms） |
| 两万字正文排版 | **68ms**（5000 字 17ms） | 单章两万字可接受；超长章节仍需分页/分段 |
| JPEG 加载/解码 | **loaded 425ms；canvas 解码 267ms、drawImage 245ms** | 图片页切换要预取，不能等滚动到再加载 |
| PNG 加载 | **440ms** | 单张 64×64；真机漫画图更大，仍需预取 |
| GIF 加载 | **439ms** | 1×1；data URI GIF 208ms；GIF 不作为漫画主格式 |

## 5. 三条设计红线

| # | 红线 | 具体要求 |
|---|---|---|
| 1 | **布局与视觉** | 不用 `flex`、`grid`、`vw`、`calc`、`transform`、`transition`、`object-fit`；`vh`、`clamp`、`sticky` 同样不用（实测均不可用）。用 px + float/百分比宽度 + `-webkit-box`（仅必要时），圆角/阴影/透明度可用 |
| 2 | **不做动画** | `requestAnimationFrame` 与 `transition` 都不存在；不做淡入淡出、滑动、骨架动画；交互反馈只做静态样式或整页刷新 |
| 3 | **图片预算** | 单张 **≤100KB**；预取 **2–3 页**；优先 JPEG；WebP 不可用。网络实测 313KB/s 且单请求约 0.3s TTFB，预算可放宽；但真机漫画图远大于测试图（本轮 JPEG 仅 640×480），仍需预取控制 |

> 另：设备时区 UTC，所有日期文本由服务端格式化；客户端不要用 `new Date()` 生成用户可见时间。

## 6. 未验证项（不许写成结论）

| 项 | 状态 | 说明 |
|---|---|---|
| 触摸 / 多点触控 | **未验证** | `touch="not_tested"`、`maxTouchPoints=undefined`、`ontouchstart=false`；本 run 未做 touchstart/touchmove/touchend 真触发，`(pointer:coarse)=false` 不能反推“不支持触摸” |
| 触控目标尺寸 | **未验证** | 没有命中区域/最小点击尺寸测试 |
| 图片内存上限 | **未验证** | 只测了 640×480 JPEG 与 64×64 PNG，没有大图/OOM 压力样本 |
| `display:table` | **未验证** | 探针未测该布局 |
| `box-sizing` | **未验证** | 探针未测 `content-box`/`border-box` 差异 |
| `em` / `rem` | **未验证** | 只测了 px 与 0.5px，没有相对单位测试 |
| 字体族 | **未验证** | 没有字体枚举与字宽/行高度量 |
| 横竖屏切换 | **未验证** | 只确认当前竖屏媒体查询；没有旋转后重新测量 |

## 7. 数据来源与版本

- 真机 run：`D:\tools\kpw3-probe\reports\20261103-172026-B4YFGB.json`
- 探针版本：**v2.1.0**；run_id：`B4YFGB`；server_python：`3.12.8`
- 采集时间：`2026-11-03 17:20:26`；总耗时：**7,564ms**
- 完整性：**10 个块全部到齐**（`start` + A–H 8 个数据块 + `complete`）；`missing_chunks` 为空、`post_failures=0`、`js_errors=0`
- 参考分析：`D:\tools\kpw3-probe\analysis\report.md`（v1.0.1 旧结论，冲突处以本 run 为准）
- **v2.1 复测已完成，吞吐/回落/图片数值已更新**；v2.0 的 2805ms 超时、PNG/GIF≈1.8s、cookie 不可依赖等口径均为探针缺陷或旧值，已作废。
