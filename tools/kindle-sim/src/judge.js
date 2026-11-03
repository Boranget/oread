// 判定逻辑：全部基于可复现的数值阈值，不凭感觉。
export const DEFAULT_THRESHOLDS = {
  // 全页可见非空白字符数低于该值 => 白屏
  visibleChars: 50,
  // 首屏（1072×1231 内）正文非空白字符数低于该值 => 主内容不在首屏
  firstScreenChars: 20,
  // 找不到 main/article/.content 等主内容区时，首屏正文至少要有这么多字才不算“导航壳”
  shellChars: 100,
};

export function cssTotals(modeResult) {
  const total = { flex: 0, grid: 0, fixed: 0, vw: 0, vh: 0, clamp: 0, media: 0, supports: 0, sticky: 0, total: 0 };
  const add = (audit) => {
    if (!audit) return;
    for (const key of Object.keys(total)) total[key] += audit[key] || 0;
  };
  for (const f of modeResult.css || []) add(f.audit);
  for (const s of modeResult.styleAttrs || []) add(s.audit);
  return total;
}

export function firstScreenHasBody(modeResult, thresholds = DEFAULT_THRESHOLDS) {
  const m = modeResult.metrics || {};
  const chars = m.firstScreenContentChars || 0;
  const blocks = m.firstScreenContentBlocks || 0;
  return chars >= thresholds.firstScreenChars && (blocks >= 1 || chars >= thresholds.firstScreenChars * 2);
}

export function assessMode(modeResult, thresholds = DEFAULT_THRESHOLDS) {
  const m = modeResult.metrics || {};
  const css = cssTotals(modeResult);
  const blocked = (modeResult.blockedScripts || []).length;
  const pageErrors = (modeResult.pageErrors || []).length;
  const hardFailures = (modeResult.failures || []).filter((f) => !f.intentional && f.resourceType !== 'screenshot');
  const reasons = [];

  const visibleChars = m.visibleChars || 0;
  const bodyTextLength = m.bodyTextLength || 0;
  const firstChars = m.firstScreenContentChars || 0;
  const firstBlocks = m.firstScreenContentBlocks || 0;
  const severeOverflow = (m.horizontalOverflow || 0) > (m.innerWidth || 1072) * 2;

  if (modeResult.navError) {
    reasons.push(`导航异常：${modeResult.navError}`);
  }

  // 页面根本没加载（DNS/TLS/超时）时不能当作“白屏”：这是“不可用/无法测试”，
  // 否则会把网络问题误判成渲染问题。
  if (modeResult.navError && (modeResult.metricsError || !m.url || m.url === 'about:blank')) {
    reasons.push('浏览器未拿到任何页面（非白屏：没有可度量的文档）');
    return {
      verdict: '不可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '高（未加载是客观事实；需先排除网络/TLS 问题）',
    };
  }

  if (visibleChars < thresholds.visibleChars || bodyTextLength === 0) {
    if (reasons.length === 0) {
      reasons.push(`可见字符 ${visibleChars} < 阈值 ${thresholds.visibleChars}（body.innerText ${bodyTextLength} 字）`);
    }
    return {
      verdict: '白屏',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '高（Chromium 解析与 DOM 可见文本客观计数；ES5 门禁按包阻断）',
    };
  }

  if (!firstScreenHasBody(modeResult, thresholds)) {
    reasons.push(`可见字符 ${visibleChars} ≥ ${thresholds.visibleChars}，但首屏正文 ${firstChars} 字 / ${firstBlocks} 个文本块，低于阈值 ${thresholds.firstScreenChars}`);
    return {
      verdict: '不可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '中高（首屏正文判定客观；页面可能需滚动才能读到内容）',
    };
  }

  // 只有导航/脚本提示，没有主内容区：典型“JS 外壳页”，对 KPW3 实际不可读。
  if (m.jsNotice && visibleChars < 300) {
    reasons.push(`首屏文本疑似“请启用 JavaScript”提示（可见字符 ${visibleChars} < 300），没有可读正文`);
    return {
      verdict: '不可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '中高（模式匹配可能误伤少量页面，可人工复核截图）',
    };
  }

  if (!m.mainCandidate && firstChars < (thresholds.shellChars || 100)) {
    reasons.push(`未找到 main/article/.content 等主内容区，首屏正文仅 ${firstChars} 字 < ${thresholds.shellChars || 100}（疑似导航壳/脚本外壳页）`);
    return {
      verdict: '不可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '中（存在少量“无语义标签但有正文”的页面被误判的可能）',
    };
  }

  if (severeOverflow) {
    reasons.push(`横向溢出 ${m.horizontalOverflow}px（> 2×视口 1072px），KPW3 无手势缩放，内容不可操作`);
    return {
      verdict: '不可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '中（宽度溢出客观，但“不可操作”含体验判断）',
    };
  }

  const compat = [];
  if (blocked > 0) compat.push(`ES5 门禁拦截 ${blocked} 个脚本`);
  if (css.total > 0) compat.push(`CSS 不兼容命中 ${css.total} 处（flex/grid/fixed/vw/vh/clamp/@media/supports/sticky）`);
  if (pageErrors > 0) compat.push(`页面运行时错误 ${pageErrors} 条`);
  if (hardFailures.length > 0) compat.push(`资源失败 ${hardFailures.length} 个`);
  if (modeResult.metricsError) compat.push(`度量脚本异常：${modeResult.metricsError}`);

  if (compat.length > 0) {
    reasons.push('首屏含正文，但存在兼容性问题，版面可能坍塌或功能缺失');
    reasons.push(...compat);
    return {
      verdict: '降级可用',
      reasons,
      css,
      counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
      confidence: '中（DOM 文本/首屏客观；CSS 坍塌程度为近似模拟，非真机）',
    };
  }

  reasons.push('首屏含正文；无 ES5 门禁拦截、无 CSS 不兼容命中、无页面错误、无资源失败');
  return {
    verdict: '可用',
    reasons,
    css,
    counts: { blocked: blocked, pageErrors, hardFailures: hardFailures.length },
    confidence: '高（限于本仿真环境；真机字体/渲染差异未覆盖）',
  };
}

export function compareSSR(jsResult, nojsResult, jsAssessment, nojsAssessment, thresholds = DEFAULT_THRESHOLDS) {
  const j = jsResult.metrics || {};
  const n = nojsResult.metrics || {};
  const bodyInHtml = (n.visibleChars || 0) >= thresholds.visibleChars && firstScreenHasBody(nojsResult, thresholds);
  const ratio = (j.visibleChars || 0) > 0 ? +(((n.visibleChars || 0) / j.visibleChars) * 100).toFixed(1) : (n.visibleChars > 0 ? null : 0);
  const readWithoutJs = bodyInHtml && (nojsAssessment.verdict === '可用' || nojsAssessment.verdict === '降级可用');
  return {
    bodyInHtml,
    readWithoutJs,
    noJsVisibleChars: n.visibleChars || 0,
    jsVisibleChars: j.visibleChars || 0,
    ratioPercent: ratio,
    noJsVerdict: nojsAssessment.verdict,
    jsVerdict: jsAssessment.verdict,
    note: bodyInHtml
      ? `JS 完全禁用时仍测得 ${n.visibleChars || 0} 个可见字符，正文存在于服务端返回的 HTML 中`
      : 'JS 完全禁用时页面无可见正文，说明当前 HTML 中没有 SSR 正文（纯客户端渲染）',
  };
}
