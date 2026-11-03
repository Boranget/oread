// 在页面内执行的度量函数。注意：必须保持自包含、可序列化，
// 不能引用 Node 侧变量。
export function measurePage() {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const CHROME_TAGS = { NAV: 1, HEADER: 1, FOOTER: 1, ASIDE: 1 };
  const SKIP_TAGS = {
    SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEMPLATE: 1, HEAD: 1, META: 1, LINK: 1, TITLE: 1,
  };
  const hiddenCache = new Map();

  function styleHidden(el) {
    const cs = getComputedStyle(el);
    if (!cs) return true;
    if (cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse') return true;
    if (parseFloat(cs.opacity || '1') === 0) return true;
    return false;
  }

  function isHidden(el) {
    if (!el || el === document.body) return !document.body;
    if (hiddenCache.has(el)) return hiddenCache.get(el);
    let result = styleHidden(el);
    if (!result) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) result = true;
    }
    hiddenCache.set(el, result);
    return result;
  }

  function isVisibleChain(el) {
    let node = el;
    while (node && node !== document.body) {
      if (isHidden(node)) return false;
      node = node.parentElement;
    }
    return true;
  }

  function inChrome(el) {
    let node = el;
    while (node && node !== document.body) {
      if (CHROME_TAGS[node.tagName]) return true;
      const role = node.getAttribute && node.getAttribute('role');
      if (role === 'navigation' || role === 'banner' || role === 'contentinfo') return true;
      node = node.parentElement;
    }
    return false;
  }

  function textStats(node) {
    const raw = node.nodeValue || '';
    const chars = raw.replace(/\s+/g, '').length;
    if (!chars) return null;
    const parent = node.parentElement;
    if (!parent || SKIP_TAGS[parent.tagName]) return null;
    if (!isVisibleChain(parent)) return null;
    const range = document.createRange();
    range.selectNodeContents(node);
    const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0 && r.height > 0);
    const firstScreen = rects.some((r) => r.top < vh && r.bottom > 0 && r.left < vw && r.right > 0);
    const visibleRect = rects.find((r) => r.bottom > 0 && r.top < document.documentElement.scrollHeight);
    return {
      chars,
      firstScreen,
      chrome: inChrome(parent),
      top: visibleRect ? visibleRect.top : null,
      left: visibleRect ? visibleRect.left : null,
    };
  }

  let visibleChars = 0;
  let contentChars = 0;
  let firstScreenChars = 0;
  let firstScreenContentChars = 0;
  let firstScreenContentBlocks = 0;
  const firstScreenSamples = [];
  const visibleSamples = [];

  if (document.body) {
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
    let node = walker.nextNode();
    while (node) {
      const st = textStats(node);
      if (st) {
        visibleChars += st.chars;
        if (!st.chrome) contentChars += st.chars;
        if (st.firstScreen) {
          firstScreenChars += st.chars;
          if (!st.chrome) {
            firstScreenContentChars += st.chars;
            if (st.chars >= 20) firstScreenContentBlocks += 1;
            if (firstScreenSamples.length < 6) {
              firstScreenSamples.push((node.nodeValue || '').replace(/\s+/g, ' ').trim().slice(0, 80));
            }
          }
        }
        if (visibleSamples.length < 8) {
          visibleSamples.push((node.nodeValue || '').replace(/\s+/g, ' ').trim().slice(0, 80));
        }
      }
      node = walker.nextNode();
    }
  }

  const body = document.body;
  const bodyRect = body ? body.getBoundingClientRect() : null;
  const bodyStyle = body ? getComputedStyle(body) : null;
  const docEl = document.documentElement;
  const scrollWidth = Math.max(docEl.scrollWidth, body ? body.scrollWidth : 0);
  const scrollHeight = Math.max(docEl.scrollHeight, body ? body.scrollHeight : 0);
  // 可选：`#k-stage` 是 /k 正文区（固定 1108px 高、overflow:hidden）。
  // 它的 scrollHeight 才能看出是否有正文被裁掉（文档级 scrollHeight 恒等于 1268）。
  const stageEl = document.getElementById('k-stage');
  let stageMetrics = null;
  if (stageEl) {
    const stageRect = stageEl.getBoundingClientRect();
    const stageStyle = getComputedStyle(stageEl);
    let contentBottom = stageRect.top;
    const last = stageEl.lastElementChild;
    if (last) {
      const r = last.getBoundingClientRect();
      contentBottom = r.bottom;
    }
    const contentHeight = Math.round(contentBottom - stageRect.top);
    const lineHeight = stageStyle ? parseFloat(stageStyle.lineHeight || '0') : 0;
    stageMetrics = {
      clientWidth: stageEl.clientWidth,
      clientHeight: stageEl.clientHeight,
      scrollWidth: stageEl.scrollWidth,
      scrollHeight: stageEl.scrollHeight,
      overflowY: contentHeight - stageEl.clientHeight,
      contentHeight,
      lineHeight,
      lineCount: lineHeight > 0 ? Math.round(contentHeight / lineHeight) : 0,
      paragraphs: stageEl.getElementsByTagName('p').length,
    };
  }

  const mainCandidates = [];
  if (body) {
    const selector = 'main, article, [role="main"], .content, .reader, .article, .post, #content, #app';
    const nodes = body.querySelectorAll(selector);
    nodes.forEach((el) => {
      const text = (el.innerText || '').replace(/\s+/g, '');
      if (text.length >= 20) {
        const r = el.getBoundingClientRect();
        mainCandidates.push({
          tag: el.tagName.toLowerCase(),
          id: el.id || '',
          cls: (el.className && typeof el.className === 'string') ? el.className.slice(0, 60) : '',
          chars: text.length,
          top: r.top,
          bottom: r.bottom,
          left: r.left,
          right: r.right,
          inFirstScreen: r.top < vh && r.bottom > 0,
        });
      }
    });
  }
  mainCandidates.sort((a, b) => b.chars - a.chars);
  const mainCandidate = mainCandidates[0] || null;

  const metaViewport = document.querySelector('meta[name="viewport"]');
  const scripts = Array.from(document.querySelectorAll('script'));
  const bodyText = body ? (body.innerText || '') : '';
  const bodyTextCompact = bodyText.replace(/\s+/g, '');
  const jsNotice = /(?:请|需要|必须|建议).{0,8}(?:启用|开启|打开).{0,4}(?:JavaScript|JS)/i.test(bodyText)
    || /(?:enable|turn on).{0,12}(?:JavaScript|JS)/i.test(bodyText)
    || /JavaScript.{0,12}(?:required|disabled|not available)/i.test(bodyText);

  return {
    url: location.href,
    title: document.title || '',
    lang: docEl.getAttribute('lang') || '',
    innerWidth: vw,
    innerHeight: vh,
    dpr: window.devicePixelRatio,
    screenWidth: window.screen ? window.screen.width : 0,
    screenHeight: window.screen ? window.screen.height : 0,
    ua: navigator.userAgent,
    platform: navigator.platform || '',
    metaViewport: metaViewport ? metaViewport.getAttribute('content') : null,
    hasTouch: 'ontouchstart' in window,
    bodyDisplay: bodyStyle ? bodyStyle.display : null,
    bodyVisibility: bodyStyle ? bodyStyle.visibility : null,
    bodyRect: bodyRect ? { x: bodyRect.x, y: bodyRect.y, width: bodyRect.width, height: bodyRect.height } : null,
    visibleChars,
    contentChars,
    firstScreenChars,
    firstScreenContentChars,
    firstScreenContentBlocks,
    firstScreenSamples,
    visibleSamples,
    visibleTextSample: bodyTextCompact.slice(0, 240),
    jsNotice,
    bodyTextLength: bodyTextCompact.length,
    scrollWidth,
    scrollHeight,
    stageMetrics,
    horizontalOverflow: scrollWidth > vw + 1 ? scrollWidth - vw : 0,
    domNodes: document.getElementsByTagName('*').length,
    scriptCount: scripts.length,
    moduleScriptCount: scripts.filter((s) => (s.getAttribute('type') || '').toLowerCase() === 'module').length,
    nomoduleScriptCount: scripts.filter((s) => s.hasAttribute('nomodule')).length,
    stylesheetCount: document.styleSheets ? document.styleSheets.length : 0,
    mainCandidates,
    mainCandidate,
    mainContentInFirstScreen: mainCandidate ? mainCandidate.inFirstScreen : firstScreenContentChars >= 20,
    promiseType: typeof Promise,
    fetchType: typeof fetch,
    xhrType: typeof XMLHttpRequest,
    storageType: typeof localStorage,
    cookieEnabled: navigator.cookieEnabled,
  };
}
