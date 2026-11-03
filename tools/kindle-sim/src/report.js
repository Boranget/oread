// Markdown 报告渲染。

function mdEscape(s) {
  return String(s == null ? '' : s).replace(/\|/g, '\\|').replace(/\r?\n/g, '<br>');
}

function chars(n) {
  return n == null ? '-' : String(n);
}

function pct(n) {
  return n == null ? '∞（JS 模式白屏）' : `${n}%`;
}

function link(url, max = 90) {
  const text = url.length > max ? `${url.slice(0, max - 1)}…` : url;
  return `[${mdEscape(text)}](${url})`;
}

function shotLink(rel) {
  if (!rel) return '（截图失败）';
  const normalized = rel.replace(/\\/g, '/');
  return `[截图](${`./${normalized}`})`;
}

function errorText(e) {
  const loc = e.url ? ` @ ${e.url}${e.line ? `:${e.line}` : ''}` : '';
  return `- ${mdEscape(e.text || e.message)}${mdEscape(loc)}`;
}

export function renderReport({ runInfo, results, options, thresholds }) {
  const lines = [];
  const now = runInfo.finishedAt || new Date().toISOString();
  const jsResult = results.find((r) => r.js)?.js;
  const actualUA = jsResult?.runtime?.ua || '(未取得)';

  lines.push('# KPW3 仿真测试报告');
  lines.push('');
  lines.push(`- 生成时间：${now}`);
  lines.push(`- 浏览器：${runInfo.browser.name} ${mdEscape(runInfo.browser.version)}（${mdEscape(runInfo.browser.path)}）`);
  lines.push(`- 配置 UA：\`${mdEscape(runInfo.userAgent)}\``);
  lines.push(`- 页面实际 UA：\`${mdEscape(actualUA)}\``);
  lines.push(`- 视口：${runInfo.viewport.width}×${runInfo.viewport.height}，screen ${runInfo.screen.width}×${runInfo.screen.height}，DPR 1，isMobile=false，hasTouch=false`);
  lines.push(`- 模式：每个 URL 跑「启用 JS」与「完全禁用 JS」两遍；CSS 降级近似=${options.degradeCss !== false ? '开启' : '关闭'}`);
  lines.push('- 说明：本报告是 Chromium 仿真，不是 KPW3 真机；CSS 部分是启发式审计 + 降级样式近似，置信度见各节。');
  lines.push('');

  lines.push('## 判定判据（固定阈值，可配置）');
  lines.push('');
  lines.push(`- **白屏**：全页可见非空白字符数 < ${thresholds.visibleChars}，或 \`body.innerText\` 为空。`);
  lines.push(`- **不可用**：可见字符 ≥ ${thresholds.visibleChars}，但首屏（1072×1231 内）正文可见字符 < ${thresholds.firstScreenChars} 且无 ≥${thresholds.firstScreenChars} 字的正文文本块；无 main/article/.content 等主内容区且首屏正文 < ${thresholds.shellChars || 100} 字（疑似导航壳/“请启用 JS”提示）；或横向溢出 > 2×视口（KPW3 无手势缩放）；或页面根本没加载（DNS/TLS/超时，单独标注，不算白屏）。`);
  lines.push('- **降级可用**：首屏含正文，但存在 ES5 门禁拦截、CSS 不兼容命中、页面错误或资源失败。');
  lines.push('- **可用**：首屏含正文，且无上述任何兼容性问题。');
  lines.push('');

  lines.push('## 汇总');
  lines.push('');
  lines.push('| # | 目标 | JS 模式 | 无 JS | 可见字符 | 首屏正文 | 拦截脚本 | CSS 命中 | SSR 正文在 HTML | 截图 |');
  lines.push('|---|---|---|---|---|---|---|---|---|---|');
  results.forEach((r, i) => {
    const js = r.js.assessment;
    const no = r.nojs.assessment;
    lines.push(
      `| ${i + 1} | ${mdEscape(r.target.name || r.target.url)}<br>${link(r.target.url, 70)} | **${js.verdict}** | ${no.verdict} | ${chars(r.js.metrics.visibleChars)} / ${chars(r.nojs.metrics.visibleChars)} | ${chars(r.js.metrics.firstScreenContentChars)} / ${chars(r.nojs.metrics.firstScreenContentChars)} | ${(r.js.blockedScripts || []).length} | ${js.css.total} | ${r.ssr.bodyInHtml ? '是' : '否'} | ${shotLink(r.js.screenshot)} |`,
    );
  });
  lines.push('');

  results.forEach((r, i) => {
    lines.push(`## ${i + 1}. ${mdEscape(r.target.name || r.target.url)}`);
    lines.push('');
    lines.push(`- URL：${link(r.target.url, 200)}`);
    lines.push(`- 判定：**${r.js.assessment.verdict}**（JS）；${r.nojs.assessment.verdict}（无 JS）`);
    lines.push(`- 置信度：${mdEscape(r.js.assessment.confidence)}`);
    lines.push('');

    lines.push('### 双模式关键指标');
    lines.push('');
    lines.push('| 指标 | JS 模式 | 无 JS 模式 |');
    lines.push('|---|---|---|');
    const a = r.js.metrics;
    const b = r.nojs.metrics;
    const rows = [
      ['可见字符数（全页，去空白）', a.visibleChars, b.visibleChars],
      ['正文可见字符数（排除 nav/header/footer/aside）', a.contentChars, b.contentChars],
      ['首屏可见字符数', a.firstScreenChars, b.firstScreenChars],
      ['首屏正文可见字符数', a.firstScreenContentChars, b.firstScreenContentChars],
      ['首屏正文文本块数（≥20 字）', a.firstScreenContentBlocks, b.firstScreenContentBlocks],
      ['body.innerText 字符数', a.bodyTextLength, b.bodyTextLength],
      ['视口 innerWidth×innerHeight', `${a.innerWidth}×${a.innerHeight}`, `${b.innerWidth}×${b.innerHeight}`],
      ['DPR', a.dpr, b.dpr],
      ['横向溢出 px', a.horizontalOverflow, b.horizontalOverflow],
      ['DOM 节点数', a.domNodes, b.domNodes],
      ['script 标签数', a.scriptCount, b.scriptCount],
      ['type=module 标签数', a.moduleScriptCount, b.moduleScriptCount],
      ['nomodule 标签数', a.nomoduleScriptCount, b.nomoduleScriptCount],
      ['主内容区在首屏', a.mainContentInFirstScreen ? '是' : '否', b.mainContentInFirstScreen ? '是' : '否'],
    ];
    rows.forEach((row) => lines.push(`| ${row[0]} | ${chars(row[1])} | ${chars(row[2])} |`));
    lines.push('');

    lines.push('### SSR / 断 JS 可读性');
    lines.push('');
    lines.push(`- **JS 完全禁用后正文是否在 HTML 中：${r.ssr.bodyInHtml ? '是' : '否'}**`);
    lines.push(`- 无 JS 判定：**${r.ssr.noJsVerdict}**；无 JS 可见字符 ${r.ssr.noJsVisibleChars}，JS 模式可见字符 ${r.ssr.jsVisibleChars}，文本量比 ${pct(r.ssr.ratioPercent)}`);
    lines.push(`- 断 JS 仍可读：${r.ssr.readWithoutJs ? '是' : '否'}`);
    lines.push(`- ${mdEscape(r.ssr.note)}`);
    lines.push('');

    lines.push('### ES5 门禁（JS 模式，外部脚本 + 内联脚本）');
    lines.push('');
    if (r.js.scripts.length === 0) {
      lines.push('未捕获到脚本。');
    } else {
      lines.push('| 脚本 | 类型 | 体积 bytes | ES5 可解析 | 首个错误 | ES6+ 语法统计 | 处理 |');
      lines.push('|---|---|---|---|---|---|---|');
      r.js.scripts.forEach((s) => {
        const loc = s.error && s.error.line ? `${mdEscape(s.error.message)} (${s.error.line}:${s.error.column})` : (s.error ? mdEscape(s.error.message) : '—');
        const action = s.blocked ? `**abort**（${s.reason || 'blocked'}）` : '放行执行';
        lines.push(`| ${link(s.url, 80)} | ${s.kind || 'external'} | ${s.bytes} | ${s.parseable ? '是' : '**否**'} | ${loc} | ${mdEscape(s.featureSummary || '无')} | ${action} |`);
      });
      lines.push('');
      if (r.js.runtime.moduleScriptCount > 0) {
        lines.push(`- 页面含 ${r.js.runtime.moduleScriptCount} 个 type=module 脚本：KPW3 WebKit 不支持 ES module，按整包不执行处理。`);
        lines.push('');
      }
      if (r.js.runtime.nomoduleScriptCount > 0) {
        lines.push(`- 页面含 ${r.js.runtime.nomoduleScriptCount} 个 nomodule 脚本：Chromium 原生会跳过；本工具移除 nomodule 属性以模拟 KPW3 不认识该属性时会执行的行为。`);
        lines.push('');
      }
    }

    lines.push('### 缺少的全局对象');
    lines.push('');
    lines.push('| Promise | fetch | XMLHttpRequest | localStorage | Cookie |');
    lines.push('|---|---|---|---|---|');
    lines.push(`| ${r.js.globals.promise} | ${r.js.globals.fetch} | ${r.js.globals.xhr} | ${r.js.globals.localStorage} | ${r.js.globals.cookie} |`);
    lines.push('');
    lines.push('> Promise / fetch 在 JS 模式下被前置脚本置为 `undefined`；运行时错误见下。');
    lines.push('');

    lines.push('### CSS 能力审计（原始 CSS，含内联 style）');
    lines.push('');
    if (r.js.css.length === 0 && r.js.styleAttrs.length === 0) {
      lines.push('未捕获到 CSS。');
    } else {
      lines.push('| 文件 | 来源 | 体积 bytes | display:flex | display:grid | position:fixed | vw | vh | clamp() | @media | @supports | 降级改动 |');
      lines.push('|---|---|---|---|---|---|---|---|---|---|---|---|');
      r.js.css.forEach((c) => {
        const ch = c.changes || {};
        const changeText = ch.mediaBlocks || ch.supportsBlocks || ch.viewportDecls || ch.flex || ch.grid || ch.fixed || ch.sticky
          ? `media×${ch.mediaBlocks || 0}, supports×${ch.supportsBlocks || 0}, vw/vh声明×${ch.viewportDecls || 0}, flex×${ch.flex || 0}, grid×${ch.grid || 0}, fixed×${ch.fixed || 0}, sticky×${ch.sticky || 0}`
          : '—';
        lines.push(`| ${link(c.url, 80)} | ${c.source || 'external'} | ${c.bytes} | ${c.audit.flex} | ${c.audit.grid} | ${c.audit.fixed} | ${c.audit.vw} | ${c.audit.vh} | ${c.audit.clamp} | ${c.audit.media} | ${c.audit.supports} | ${mdEscape(changeText)} |`);
      });
      if (r.js.styleAttrs.length) {
        lines.push(`| （style="" 属性 ×${r.js.styleAttrs.length}） | inline-attr | — | 见合计 | 见合计 | 见合计 | 见合计 | 见合计 | 见合计 | 见合计 | 见合计 | 已降级 |`);
      }
      lines.push('');
      const t = r.js.assessment.css;
      lines.push(`- 合计：flex ${t.flex}、grid ${t.grid}、fixed ${t.fixed}、vw ${t.vw}、vh ${t.vh}、clamp ${t.clamp}、@media ${t.media}、@supports ${t.supports}、sticky ${t.sticky}`);
      lines.push('- 置信度：**低-中**。Chromium 无法真正删除 flexbox/grid；本工具通过改写 CSS 文本近似“样式坍塌”，不能宣称等同 KPW3 真机。');
      lines.push('');
    }

    lines.push('### 加载失败 / HTTP 错误 / 运行时错误');
    lines.push('');
    const failures = r.js.failures || [];
    if (failures.length === 0) {
      lines.push('- 无加载失败。');
    } else {
      lines.push('| 资源 | 类型 | 错误 | 是否门禁主动 abort |');
      lines.push('|---|---|---|---|');
      failures.slice(0, 40).forEach((f) => {
        lines.push(`| ${link(f.url, 70)} | ${f.resourceType} | ${mdEscape(f.errorText)} | ${f.intentional ? `是（${f.reason || ''}）` : '否'} |`);
      });
      if (failures.length > 40) lines.push(`| … | | 其余 ${failures.length - 40} 条见 results.json | |`);
      lines.push('');
    }
    if ((r.js.httpErrors || []).length) {
      lines.push('HTTP ≥400：');
      r.js.httpErrors.slice(0, 30).forEach((e) => lines.push(`- ${e.status} ${link(e.url, 110)} (${e.resourceType})`));
      if (r.js.httpErrors.length > 30) lines.push(`- … 其余 ${r.js.httpErrors.length - 30} 条见 results.json`);
      lines.push('');
    }
    if ((r.js.pageErrors || []).length) {
      lines.push('页面 JS 运行时错误：');
      r.js.pageErrors.slice(0, 20).forEach((e) => lines.push(`- ${mdEscape(e.message)}`));
      lines.push('');
    }
    if ((r.js.console || []).length) {
      lines.push('控制台 error/warning（去重前 20 条）：');
      const seen = new Set();
      let shown = 0;
      for (const c of r.js.console) {
        const key = `${c.type}:${c.text}`;
        if (seen.has(key)) continue;
        seen.add(key);
        lines.push(errorText(c));
        shown += 1;
        if (shown >= 20) break;
      }
      lines.push('');
    }

    lines.push('### 判定理由');
    lines.push('');
    r.js.assessment.reasons.forEach((x) => lines.push(`- JS：${mdEscape(x)}`));
    r.nojs.assessment.reasons.forEach((x) => lines.push(`- 无 JS：${mdEscape(x)}`));
    lines.push('');

    lines.push('### 截图');
    lines.push('');
    lines.push(`- JS 模式：${shotLink(r.js.screenshot)}；首屏：${shotLink(r.js.firstScreenShot)}`);
    lines.push(`- 无 JS 模式：${shotLink(r.nojs.screenshot)}；首屏：${shotLink(r.nojs.firstScreenShot)}`);
    if (r.js.metrics.visibleTextSample) lines.push(`- JS 模式可见文本样例：\`${mdEscape(r.js.metrics.visibleTextSample.slice(0, 200))}\``);
    if (r.nojs.metrics.visibleTextSample) lines.push(`- 无 JS 可见文本样例：\`${mdEscape(r.nojs.metrics.visibleTextSample.slice(0, 200))}\``);
    lines.push('');
  });

  lines.push('## 已知不能仿真的部分（必须如实阅读）');
  lines.push('');
  const limits = [
    '**Chromium 无法真正删除 flexbox/grid**：CSS 结论来自“审计 + 改写近似”，不是 KPW3 的排版引擎；flex/grid 页面的真实坍塌形态可能与本报告不同，置信度低-中。',
    '**字体与度量不同**：KPW3 的字体（Bookerly/宋体回退）、行高、字距与 Chromium 不同，首屏能容纳的行数会有差异；本报告按 1072×1231 像素布局近似。',
    '**WebKit 531/534 的 CSS 解析细节**：例如未加前缀的 flex、部分选择器、@media 特性的支持边界，无法逐条复刻；本工具只覆盖题目列出的主要项。',
    '**触摸/滚动退化**：KPW3 只有 click，没有滑动/手势；本工具只能验证 click 可达性，无法模拟真实墨水屏的刷新残影与慢响应。',
    '**TLS/证书链**：KPW3 CA 库老，现代证书链可能失败；本工具用本机 Chromium 的证书库，不模拟老 CA。',
    '**ES5 门禁的边界**：本工具用 acorn ecmaVersion 5 判定；WebKit 531/534 还有少量 ES5 运行时差异（如部分 Array/String 方法）未覆盖。内联脚本按块阻断，外部脚本按整包 abort。',
    '**nomodule**：Chromium 会跳过带 nomodule 的脚本，KPW3 不认识该属性时会执行；本工具移除 nomodule 属性做近似，但 module/legacy 双包的时序差异仍可能存在。',
  ];
  limits.forEach((x) => lines.push(`- ${x}`));
  lines.push('');
  return lines.join('\n');
}

export { mdEscape };
