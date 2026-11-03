# KPW3 仿真测试台（kindle-sim）

在本机用 Chromium 近似判断 Web 页面在 **Kindle Paperwhite 3（2011–2012，mesquite，WebKit 531.2/534.26）** 上的表现，不依赖真机。

工具只写入本目录；不修改 `server/src` 与 `server/web`。

## 快速开始

```powershell
cd D:\boraget\oread\tools\kindle-sim

# 依赖（已安装；重装时用 npmmirror）
npm install --registry=https://registry.npmmirror.com

# 单个 URL（会跑“启用 JS / 完全禁用 JS”两遍）
node src/cli.js http://127.0.0.1:3000/

# 批量（本地 SPA + 阅读页样例 + 简单静态页 + ES6 阳性对照）
node src/cli.js --config kpw3.config.json --out out/real-run

# 外网阳性对照（r.qq.com、example.com）
node src/cli.js --config kpw3.positive.config.json --out out/positive-run

# 本地 fixtures（不依赖 3000 端口）
node src/cli.js fixture:simple.html fixture:reader-page.html
```

浏览器默认用本机 Edge；也可 `--browser chrome` 或传 Chromium 内核浏览器的绝对路径。依赖只装 `playwright-core` 与 `acorn`，不下载浏览器。

## 输出

```
out/<run>/
├── summary.md        # Markdown 汇总表 + 每 URL 明细
├── results.json      # 全部原始数据（脚本、CSS、错误、指标）
└── screens/
    ├── 01-xxx-js-full.png          # JS 模式整页截图（1072 宽）
    ├── 01-xxx-js-firstscreen.png   # JS 模式首屏 1072×1231
    ├── 01-xxx-nojs-full.png        # 无 JS 模式整页
    └── ...
```

`summary.md` 汇总表包含：判定、可见字符、首屏正文、拦截脚本数、CSS 命中数、SSR 是否成立、截图链接。明细包含逐脚本 ES5 门禁表、逐 CSS 文件审计、加载失败清单、控制台/页面错误、判定理由与置信度。

## 配置

`kpw3.config.json`：

```json
{
  "waitMs": 1500,
  "timeoutMs": 30000,
  "degradeCss": true,
  "thresholds": { "visibleChars": 50, "firstScreenChars": 20 },
  "targets": [
    { "name": "spa-home", "url": "http://127.0.0.1:3000/" },
    { "name": "reader-page", "url": "fixture:reader-page.html" },
    { "name": "simple-static", "url": "fixture:simple.html" }
  ]
}
```

- `fixture:` 前缀由工具内置的临时 HTTP 服务托管 `fixtures/`，避免 `file://` 跨源限制。
- 命令行也支持逗号分隔：`node src/cli.js url1,url2`。
- 单目标可覆盖 `waitMs` / `timeoutMs`。

常用参数：

| 参数 | 说明 |
|---|---|
| `-o, --out <dir>` | 输出目录 |
| `-c, --config <file>` | JSON 批量配置 |
| `--wait <ms>` | load 后额外等待（默认 1500） |
| `--timeout <ms>` | 导航超时（默认 30000） |
| `--browser <edge\|chrome\|路径>` | 浏览器选择 |
| `--headed` | 显示窗口 |
| `--no-degrade-css` | 只审计 CSS，不改写 |
| `--visible-chars <n>` | 白屏阈值（默认 50） |
| `--first-screen-chars <n>` | 首屏正文阈值（默认 20） |
| `--shell-chars <n>` | 无主内容区时的首屏正文最低字数（默认 100） |
| `--user-agent <str>` | 覆盖默认 KPW3 UA |

## 仿真内容

### 1. 视口与 UA

- 视口 `1072×1231`，screen `1072×1268`，`deviceScaleFactor: 1`。
- `isMobile: false` + `hasTouch: false`：Chromium 不启用移动端 viewport 语义，`<meta viewport>` 被忽略，按真实像素排版；报告里会记录页面实际的 `meta viewport`、`innerWidth/innerHeight`、DPR，可直接核对。
- 默认 UA：

```
Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/534.26+ (KHTML, like Gecko) Safari/534.26+ Kindle/3.0
```

报告同时打印“配置 UA”和“页面实际 UA”（`navigator.userAgent`）。

### 2. ES5 语法门禁（核心）

- 拦截所有外部脚本（`script`/`worker`）响应，用 `acorn` 以 **`ecmaVersion: 5`** 解析。
- 解析失败 => `route.abort('blockedbyclient')`，模拟“这个包在 KPW3 上解析期失败、整包不执行”。
- 内联 `<script>` 通过重写文档处理：非 ES5 的内联块被替换成注释，不执行。
- `type=module` 一律视为不支持（KPW3 WebKit 没有 ES module），即使内容刚好是 ES5 也 abort。
- `nomodule` 属性会被移除：Chromium 原生会跳过，而 KPW3 不认识该属性时会执行，移除后更接近 KPW3。
- 逐脚本记录：URL、体积（bytes）、是否可解析、首个错误（message + 行:列）、ES6+ 语法统计（箭头/let/const/模板串/Promise/class/async/await/for-of/spread/module/`?.`/`??`/`**`）、处理结果（放行/abort 及原因）。

### 3. 缺少的全局对象

- JS 模式下通过 `addInitScript` 在所有 frame（含 iframe）前置注入：

```js
window.Promise = undefined;
window.fetch = undefined;
```

- 报告会记录 `typeof Promise` / `typeof fetch` / `typeof XMLHttpRequest` / `localStorage`，页面因此产生的 `pageerror` 会进入“运行时错误”清单。

### 4. CSS 能力审计 + 降级样式近似

- 审计外部 CSS、内联 `<style>`、`style=""` 属性中的：`display:flex`、`display:grid`、`position:fixed`、`vw`/`vh`、`clamp(`、`@media`、`@supports`、`position:sticky`。
- 渲染前改写 CSS（近似 KPW3 的样式坍塌）：
  - `display:flex/grid` → `block`，`inline-flex/inline-grid` → `inline-block`
  - `position:fixed` → `absolute`，`sticky` → `relative`
  - 删除含 `vw/vh/vmin/vmax` 或 `clamp()` 的声明
  - 删除整个 `@media` / `@supports` 块
- 每一项命中、每个文件的计数与降级改动量都写入报告。

### 5. 双模式对照

同一 URL 跑两遍：

1. JS 模式：启用页面 JS，但经过 ES5 门禁，且 Promise/fetch 为 undefined。
2. 无 JS 模式：`javaScriptEnabled: false`，任何页面脚本都不执行（用于验证 SSR）。

`SSR 正文在 HTML：是/否` 的硬指标是：**无 JS 模式下可见字符 ≥ 50 且首屏有正文**。满足即说明正文在服务端返回的 HTML 里，不依赖脚本二次渲染。

## 判定判据

默认阈值（可用参数覆盖）：

| 判定 | 判据 |
|---|---|
| **白屏** | 全页可见非空白字符数 `< 50`，或 `body.innerText` 为空 |
| **不可用** | 可见字符 `≥ 50`，但首屏（1072×1231 内）正文可见字符 `< 20` 且无 `≥20` 字的正文文本块；无 `main/article/.content` 等主内容区且首屏正文 `< 100` 字（疑似导航壳/“请启用 JS”提示）；横向溢出 `> 2×视口`；或页面根本没加载（DNS/TLS/超时，单独标注，不算白屏） |
| **降级可用** | 首屏含正文，但存在 ES5 门禁拦截 / CSS 不兼容命中 / 页面运行时错误 / 资源失败 |
| **可用** | 首屏含正文，且以上兼容性问题全部为 0 |

“可见字符”统计方式：遍历 `body` 文本节点，跳过 `script/style/noscript/template`，过滤 `display:none`、`visibility:hidden`、`opacity:0`、零尺寸节点，去掉空白后按字符计数；“首屏正文”额外排除 `nav/header/footer/aside` 与 `role=navigation/banner/contentinfo`。

置信度：

- ES5 门禁、白屏/可见字符、SSR 判定：**高**（解析器与 DOM 计数是客观的）。
- 首屏/横向溢出：**中高**。
- CSS 坍塌程度：**低-中**（Chromium 无法真正删除 flexbox/grid，只能近似）。

## 已知不能仿真的部分

1. **Chromium 无法真正删除 flexbox/grid**：CSS 结论是“启发式审计 + 改写近似”，不是 KPW3 排版引擎；flex/grid 页面的真实坍塌形态可能与本报告不同。
2. **字体/度量不同**：KPW3 字体、行高、字距与 Chromium 不同，首屏能容纳的行数会有差异。
3. **WebKit 531/534 细节**：未加前缀的 flex、部分选择器、`@media` 特性支持边界无法逐条复刻。
4. **触摸/手势退化**：KPW3 只有 `click`，没有滑动/手势；工具只能验证 click 可达性，无法模拟墨水屏刷新残影、慢响应。
5. **TLS/CA**：KPW3 CA 库老，现代证书链可能失败；工具使用本机 Chromium 的证书库，不模拟老 CA。
6. **ES5 门禁边界**：acorn 的 `ecmaVersion: 5` 覆盖语法层；WebKit 531/534 少量 ES5 运行时差异（部分 Array/String 方法）未覆盖。
7. **module/legacy 双包时序**：本工具按“module 不执行、nomodule 执行”模拟，但真实页面的加载顺序/竞态仍可能不同。

## 当前仓库的真实运行结果

`out/real-run/summary.md`（2026-11-02，Edge 156，1072×1231）：

| 目标 | JS 模式 | 无 JS | 关键证据 |
|---|---|---|---|
| SPA 首页 `http://127.0.0.1:3000/` | **白屏** | 白屏 | 唯一的 `type=module` bundle 含箭头/let/const/模板串等，被门禁 abort；HTML 里没有正文 |
| 阅读页版式样例 `fixture:reader-page.html` | **降级可用** | 降级可用 | 首屏正文 728 字；仅 1 处 `@media` 被降级 |
| 简单静态页 `fixture:simple.html` | **可用** | 可用 | 236 字首屏正文；0 拦截、0 CSS 命中 |
| ES6-only 阳性对照 `fixture:js-only-es6.html` | **白屏** | 白屏 | 经典脚本在 3:1 处 `const` 解析失败，整包 abort，内容由脚本渲染 |

> SPA 的阅读器不是独立 URL：`Reader.svelte` 只能从登录后的书架/发现页进入（`App.svelte` 内部状态），且整个 SPA 由 ES6 module bundle 渲染。因此用 `fixtures/reader-page.html` 作为“阅读页版式样例”验证工具对正文阅读页的判定；真实 SPA 阅读页与首页一样会因 bundle 被门禁而白屏。

外网阳性对照 `out/positive-run/summary.md`：

| 目标 | JS 模式 | 无 JS | 说明 |
|---|---|---|---|
| `https://r.qq.com/`（微信读书） | 降级可用 | 降级可用 | 1089 可见字符，首屏 121 字；28 个脚本被拦，CSS 命中 241 处，但 SSR 文本仍在 |
| `https://example.com/` | 降级可用 | 降级可用 | 132 字；1 个脚本被拦，1 处 `@media` |

预期“现代站点应白屏/不可用”，但实测 r.qq.com 和 example.com 都带 SSR 文本，按判据只能是“降级可用”。这反而说明：**白屏不是“现代站点”的必然结果，而是“正文只由 JS 渲染”的结果**——我们的 SPA 就是后者。

## 目录结构

```
tools/kindle-sim/
├── src/
│   ├── cli.js            # 参数、批量、输出编排
│   ├── runner.js         # Playwright 上下文、请求拦截、截图
│   ├── es5.js            # acorn ES5 门禁 + ES6 语法统计
│   ├── css.js            # CSS 审计 + 降级改写
│   ├── html.js           # 内联 script/style 重写
│   ├── measure.js        # 页面内 DOM 度量
│   ├── judge.js          # 判定 + SSR 对照
│   ├── report.js         # Markdown 报告
│   └── fixture-server.js # 本地 fixtures HTTP 服务
├── fixtures/             # 简单页 / 阅读页版式样例 / ES6 阳性对照
├── kpw3.config.json
├── kpw3.positive.config.json
└── README.md
```
