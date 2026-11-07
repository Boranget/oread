<script>
  import { createEventDispatcher, onMount, onDestroy, tick } from 'svelte';
  import { api, describeFailure } from '../lib/api.js';
  import ExportDialog from '../lib/ExportDialog.svelte';
  import ReaderBookmarks from '../lib/ReaderBookmarks.svelte';
  import ReaderSearch from '../lib/ReaderSearch.svelte';
  import ReaderToc from '../lib/ReaderToc.svelte';
  import { convertZh, zhLabels } from '../lib/zhconvert.js';
  import { isEmptyChapter, isVolumeTitle } from '../lib/readerContent.js';
  import { applyTheme } from '../lib/theme.js';
  import {
    FONT_OPTIONS,
    FONT_STACKS,
    WIDTH_OPTIONS,
    fontServerValue,
    readLocalLayout,
    syncLayoutSettings,
    widthCss,
    widthPxFromId,
  } from '../lib/readingPrefs.js';

  export let book; // { bookUrl, sourceId, name, author, coverUrl, intro }
  const dispatch = createEventDispatcher();

  const MODES = ['scroll', 'paged', 'fade'];

  const clamp = (v, min, max) => Math.min(max, Math.max(min, v));
  const clamp01 = (v) => clamp(Number(v) || 0, 0, 1);
  const num = (v, def) =>
    v === null || v === undefined || v === '' || !Number.isFinite(Number(v)) ? def : Number(v);

  let info = null;
  let chapters = [];
  let tocLoading = true;
  let tocError = '';

  // 搜索结果 / 书架带来的 name、author、cover、intro 作为详情与目录的 seed。
  // 换源后 App 会用新源的 book 重建 Reader，seed 只可能来自新源，不会沿用旧源。
  $: seed = {
    name: book.name || '',
    author: book.author || '',
    intro: book.intro || '',
    coverUrl: book.coverUrl || '',
  };

  let current = null;
  let currentPos = -1;
  let contentLoading = false;
  let contentError = '';

  // ---- 正文内嵌图片 ----
  // 与漫画链路共用 /api/book/image 代理：先 fetch 成 Blob，再由 objectURL 渲染，
  // 这样 <img src> 也能带上 Bearer，不新增任何图片请求方式。
  let imgState = {};
  const imgCache = new Map();
  const IMG_TAG_RE = /<img\b[^>]*>/gi;

  // 付费/VIP 章节来自 /api/book/toc 的 is_vip / is_pay；同时为真只按 V 展示
  let payNoticeClosed = -1;
  $: payChapter = currentPos >= 0 ? chapters[currentPos] || null : null;
  $: payKind = payChapter ? (payChapter.is_vip ? 'vip' : payChapter.is_pay ? 'pay' : '') : '';
  $: showPayNotice = !!payKind && payNoticeClosed !== currentPos;

  let showToc = false;
  let showPanel = false;
  let showSwitch = false;
  let showExport = false;
  let switchLoading = false;
  let switchErr = '';
  let candidates = [];
  let controlsVisible = true;
  let toast = '';
  // 正文替换规则：账号设置 use_replace_rule 控制的显式开关
  let useReplaceRules = true;
  let seenMarked = false;

  // 排版偏好：初始来自后端 user_settings，宽度/亮度/字形/速度仅存本机
  let font = 18;
  let lineHeight = 1.75;
  let theme = 'light';
  // 页宽/字体族以服务端 user_settings 为准，本机 key 仅作离线兜底
  const initialLayout = readLocalLayout();
  let width = initialLayout.width;
  let fontFamily = initialLayout.fontFamily;

  // 书签 / 书内搜索
  let showBookmarks = false;
  let showSearch = false;

  // 跳转定位（章节 + 章内比例/页码 + 段落）
  let showJump = false;
  let jumpChapter = 1;
  let jumpPct = 0;
  let jumpPage = 1;
  let jumpPara = '';
  let readMode = MODES.includes(localStorage.getItem('oread_read_mode'))
    ? localStorage.getItem('oread_read_mode')
    : 'scroll';
  let brightness = clamp(num(localStorage.getItem('oread_brightness'), 100), 0, 100);
  let zhMode = ['off', 's', 't'].includes(localStorage.getItem('oread_zh_mode'))
    ? localStorage.getItem('oread_zh_mode')
    : 'off';
  let autoSpeed = clamp(num(localStorage.getItem('oread_auto_speed'), 3), 1, 10);
  let autoOn = false;

  // 分页 / 仿真翻页
  let viewW = 0;
  let viewH = 0;
  let pageCount = 1;
  let pageIndex = 0;
  let flipDir = 0;

  let scrollEl = null;

  let saveTimer = null;
  // 进度上报节流：滚动/翻页最多每 30s 写一次后端；换章、离开页面时强制补发
  const SAVE_INTERVAL = 30000;
  let lastSaveAt = 0;
  let snapTimer = null;
  let flipTimer = null;
  let autoRaf = 0;
  let autoTimer = null;
  let autoLast = 0;
  let autoAdvancing = false;
  let resizeTimer = null;
  let toastTimer = null;
  let reqId = 0;
  let suppressScroll = false;
  let restored = false;
  let touchStart = null;
  let suppressClickUntil = 0;

  // 阅读统计：本会话累计，定期增量上报
  let statsTimer = null;
  let statsSeconds = 0;
  let statsChars = 0;
  let statsBusy = false;
  const statsSeen = new Set();


  const themeName = { light: '浅色', dark: '深色', sepia: '护眼', eink: '墨水屏' };
  const modeName = { scroll: '滚动', paged: '翻页', fade: '仿真' };

  const progressKey = (id) => `oread_progress:${id}`;

  onMount(() => {
    try {
      localStorage.setItem(`oread_lastread:${book.bookUrl}`, String(Date.now()));
    } catch {
      /* ignore */
    }
    loadAll();
    statsTimer = setInterval(onStatsTick, 1000);
    window.addEventListener('pagehide', onPageHide);
    window.addEventListener('beforeunload', onPageHide);
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('keydown', onKeydown);
    window.addEventListener('resize', onResize);
    window.addEventListener('orientationchange', onResize);
    return () => {
      window.removeEventListener('pagehide', onPageHide);
      window.removeEventListener('beforeunload', onPageHide);
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('keydown', onKeydown);
      window.removeEventListener('resize', onResize);
      window.removeEventListener('orientationchange', onResize);
    };
  });

  onDestroy(() => {
    clearTimeout(saveTimer);
    clearTimeout(snapTimer);
    clearTimeout(flipTimer);
    clearTimeout(autoTimer);
    clearTimeout(resizeTimer);
    clearTimeout(toastTimer);
    clearInterval(statsTimer);
    flushStats(true);
    cancelAnimationFrame(autoRaf);
    for (const rec of imgCache.values()) {
      if (rec && rec.objectUrl) URL.revokeObjectURL(rec.objectUrl);
    }
    imgCache.clear();
  });

  function onPageHide() {
    // 页面不可见/卸载：强制补发一次（fetch keepalive 可在卸载过程中完成）
    flushSave(true);
    flushStats(true);
  }

  function onVisibility() {
    if (document.visibilityState === 'hidden') {
      flushSave(true);
      flushStats(true);
    }
  }

  function onStatsTick() {
    if (document.visibilityState === 'visible') statsSeconds += 1;
    if (statsSeconds >= 60) flushStats();
  }

  function plainChars(s) {
    return String(s || '').replace(/<[^>]*>/g, '').replace(/\s/g, '').length;
  }

  // 读取 <img> 属性；兼容带引号/不带引号两种写法
  function imgAttr(tag, name) {
    const re = new RegExp(`\\b${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i');
    const m = re.exec(tag);
    return m ? (m[1] ?? m[2] ?? m[3] ?? '') : '';
  }

  // 懒加载属性优先（src 常是 1px 占位图），顺序与后端 pick_img_src 保持一致
  function pickImgSrc(tag) {
    for (const k of ['data-src', 'data-original', 'data-lazy-src', 'data-original-src', 'data-echo', 'data-url']) {
      const v = imgAttr(tag, k).trim();
      if (v) return v;
    }
    const src = imgAttr(tag, 'src').trim();
    if (src) return src;
    for (const k of ['srcset', 'data-srcset']) {
      const first = imgAttr(tag, k).split(',')[0].trim().split(/\s+/)[0];
      if (first) return first;
    }
    return '';
  }

  // 相对地址按章节页归并为绝对地址（图片代理只接 http/https）
  function absImg(raw, base) {
    const s = String(raw || '').trim();
    if (!s || /^data:/i.test(s) || /^javascript:/i.test(s)) return '';
    try {
      return new URL(s, base || location.href).href;
    } catch {
      return '';
    }
  }

  function textToParas(html) {
    const text = String(html || '')
      .replace(/<\s*(br|\/p|\/div|\/h[1-6]|\/li|\/tr|\/blockquote)\s*\/?\s*>/gi, '\n')
      .replace(/<[^>]*>/g, '')
      .replace(/&nbsp;/g, ' ')
      .replace(/&lt;/g, '<')
      .replace(/&gt;/g, '>')
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, '&');
    return text
      .replace(/[ \t\u00a0]{2,}/g, ' ')
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean);
  }

  // 正文解析：保留 <img>，按页面顺序切成文本段 / 图片段，图片随后走代理加载
  function htmlToParas(html, base) {
    const src = String(html || '');
    const out = [];
    let last = 0;
    IMG_TAG_RE.lastIndex = 0;
    let m;
    while ((m = IMG_TAG_RE.exec(src))) {
      for (const t of textToParas(src.slice(last, m.index))) out.push({ type: 'text', text: t });
      const url = absImg(pickImgSrc(m[0]), base);
      if (url) out.push({ type: 'image', src: url, alt: imgAttr(m[0], 'alt') });
      last = m.index + m[0].length;
    }
    for (const t of textToParas(src.slice(last))) out.push({ type: 'text', text: t });
    return out;
  }

  function setImg(src, rec) {
    imgCache.set(src, rec);
    imgState = { ...imgState, [src]: rec };
  }

  // 最多保留 40 张，超出按插入顺序淘汰并释放 objectURL，避免长时间阅读内存上涨
  function dropOldImages() {
    while (imgCache.size > 40) {
      const [key, val] = imgCache.entries().next().value;
      imgCache.delete(key);
      if (val && val.objectUrl) URL.revokeObjectURL(val.objectUrl);
      const next = { ...imgState };
      delete next[key];
      imgState = next;
    }
  }

  async function loadInlineImage(src, force = false) {
    if (!src) return;
    const prev = imgCache.get(src);
    if (!force && prev && prev.status !== 'error') return;
    if (prev && prev.objectUrl) URL.revokeObjectURL(prev.objectUrl);
    setImg(src, { status: 'loading' });
    const chapterUrl = (current && current.url) || '';
    try {
      const rec = await api.comicImage(
        book.sourceId || '',
        src,
        chapterUrl,
        book.bookUrl || ''
      );
      setImg(src, { status: 'ok', objectUrl: rec.objectUrl, bytes: rec.bytes });
      dropOldImages();
    } catch (e) {
      setImg(src, { status: 'error', error: (e && e.message) || '图片加载失败，请重试' });
    }
  }

  // 触发本章图片加载：已在缓存（含加载中/成功）的跳过，失败的等用户点“重试”
  function queueImages(segs) {
    for (const p of segs) {
      if (p.type !== 'image') continue;
      const rec = imgCache.get(p.src);
      if (!rec) loadInlineImage(p.src);
    }
  }

  function retryImage(src) {
    loadInlineImage(src, true);
  }

  function onImgError(src) {
    setImg(src, { status: 'error', error: '图片解码失败，请重试' });
  }

  function flushStats(keepalive = false) {
    if (statsBusy || !book || currentPos < 0) return;
    if (statsSeconds < 1 && statsChars < 1) return;
    const seconds = Math.min(86400, statsSeconds);
    const chars = Math.min(10000000, statsChars);
    statsBusy = true;
    api
      .recordReading(
        {
          book_id: book.bookUrl,
          name: (info && info.name) || book.name,
          seconds,
          chars,
          chapter_index: currentPos,
          total_chapters: chapters.length,
        },
        keepalive
      )
      .then(() => {
        statsSeconds = Math.max(0, statsSeconds - seconds);
        statsChars = Math.max(0, statsChars - chars);
      })
      .catch(() => {
        /* 失败保留计数，下次心跳重试 */
      })
      .finally(() => {
        statsBusy = false;
      });
  }

  function readLocalProgress() {
    try {
      return JSON.parse(localStorage.getItem(progressKey(book.bookUrl)) || 'null');
    } catch {
      return null;
    }
  }

  async function loadPrefs() {
    try {
      const s = await api.settings();
      if (!s) return;
      if (s.font_size) font = s.font_size;
      if (s.line_height) lineHeight = s.line_height;
      if (['light', 'dark', 'sepia', 'eink'].includes(s.theme)) theme = s.theme;
      if (typeof s.use_replace_rule === 'boolean') useReplaceRules = s.use_replace_rule;
      if (MODES.includes(s.read_mode) && s.read_mode !== readMode) {
        changeMode(s.read_mode, false);
      }
      // 页宽/字体族：服务端优先；首次读取时把旧本机值迁移到账号（服务端仍为默认值才写）
      const layout = await syncLayoutSettings(s);
      if (layout.width !== width || layout.fontFamily !== fontFamily) {
        const keep = restored ? currentRatio() : 0;
        width = layout.width;
        fontFamily = layout.fontFamily;
        if (restored) {
          await measurePages();
          applyRatio(keep);
        }
      }
    } catch {
      /* 离线：继续用本机缓存值 */
    }
  }

  // 替换规则开关：写入账号设置，并立即按当前章内比例重载本章，让效果可见
  async function setUseReplace(v) {
    useReplaceRules = !!v;
    try {
      await api.saveSettings({ use_replace_rule: useReplaceRules });
    } catch (e) {
      toast = '设置保存失败：' + (e.message || '未知错误');
      return;
    }
    toast = useReplaceRules ? '已开启替换规则' : '已关闭替换规则';
    // 目录标题与正文用同一个开关：切换后立刻拉一次目录，抽屉里的章名同步变化
    if (book.bookUrl) {
      try {
        const toc = await api.toc(book.sourceId, book.bookUrl, seed, useReplaceRules);
        if (Array.isArray(toc) && toc.length) {
          chapters = toc.slice().sort((x, y) => x.index - y.index);
        }
      } catch {
        /* 目录刷新失败不阻塞正文切换 */
      }
    }
    if (currentPos >= 0) {
      const ratio = currentRatio();
      await openChapter(currentPos, { ratio });
    }
  }

  async function loadAll() {
    tocLoading = true;
    tocError = '';
    contentError = '';
    loadPrefs();
    if (!book.bookUrl) {
      tocError = '缺少书籍地址，无法加载目录。请从书架或搜索结果重新打开这本书。';
      tocLoading = false;
      return;
    }
    try {
      api
        .addToShelf({
          book_id: book.bookUrl,
          source_id: book.sourceId,
          name: seed.name,
          author: seed.author,
          cover: seed.coverUrl,
        })
        .catch(() => {});
      const [b, toc] = await Promise.all([
        api.bookInfo(book.sourceId, book.bookUrl, seed).catch(() => null),
        api.toc(book.sourceId, book.bookUrl, seed),
      ]);
      info = b;
      chapters = (toc || []).slice().sort((x, y) => x.index - y.index);
      if (!chapters.length) {
        tocError = '目录为空：书源可能未返回章节，或目录规则失效。';
        return;
      }
      let prog = null;
      try {
        prog = await api.progress(book.bookUrl);
      } catch {
        prog = readLocalProgress();
      }
      if (!prog || (prog.chapter_index === 0 && prog.position === 0)) {
        prog = readLocalProgress() || prog || { chapter_index: 0, position: 0 };
      }
      // position 约定为 0..1 的章内比例；若来自其他客户端的字符偏移则从章首开始
      const ratio = prog.position > 0 && prog.position <= 1 ? prog.position : 0;
      const start = clampPos(prog.chapter_index || 0);
      // 先渲染阅读容器，再加载正文：分页模式必须先有容器宽度/高度才能测量列数
      tocLoading = false;
      await tick();
      await openChapter(start, { ratio, initial: true });
    } catch (e) {
      tocError = describeFailure(e, '读取目录');
    } finally {
      tocLoading = false;
    }
  }

  function clampPos(i) {
    return Math.min(Math.max(0, i), Math.max(0, chapters.length - 1));
  }

  function isReadable(i) {
    return i >= 0 && i < chapters.length && !chapters[i].is_volume;
  }

  // 跳过卷标题，找上一/下一可读章节
  function sibling(pos, dir) {
    let i = pos + dir;
    while (i >= 0 && i < chapters.length) {
      if (!chapters[i].is_volume) return i;
      i += dir;
    }
    return -1;
  }

  $: prevPos = sibling(currentPos, -1);
  $: nextPos = sibling(currentPos, 1);
  // 后端 content 可能是 HTML 片段（含 <p>/<br>/<img>）：保留图片，文本按段落切开
  $: paras = current && current.content
    ? htmlToParas(
        current.content,
        current.url || (chapters[currentPos] && chapters[currentPos].url) || ''
      )
    : [];
  $: imageParas = paras.filter((p) => p.type === 'image');
  $: if (current && imageParas.length) queueImages(imageParas);
  // 阅读模式由用户在阅读器内选择；墨水屏主题只改配色，不改布局与模式
  $: isPaged = readMode !== 'scroll';
  $: isFade = readMode === 'fade';
  $: effFont = font;
  $: effLine = lineHeight;
  $: displayTitle = convertZh(current?.title || chapters[currentPos]?.title || (info && info.name) || book.name || '', zhMode);
  $: displayParas = paras.map((p) => (p.type === 'text' ? { ...p, text: convertZh(p.text, zhMode) } : p));
  // 空正文不再渲染成“只有标题的空白页”：给出明确原因与可操作入口（重试/换源/下一章/目录）。
  $: chapterEmpty = !!current && !contentError && !contentLoading && displayParas.length === 0 && isEmptyChapter(current);
  $: emptyIsVolume = chapterEmpty && isVolumeTitle(displayTitle);
  $: emptyTitle = emptyIsVolume ? '这是分卷标题' : '本章内容为空';
  $: emptyHint = emptyIsVolume
    ? '该目录项只用于分卷分组，没有正文。请跳到下一章，或从目录选择正文。'
    : current?.placeholder_suspected
      ? '源站返回的是付费/占位内容，没有解析到正文。可以重试本章或换一个书源。'
      : '可能是书源规则失效、源站返回占位页，或上一次抓取失败。可以重试本章、换源，或先阅读其他章节。';
  $: dimOpacity = ((100 - brightness) / 100) * 0.82;
  $: contentStyle = isPaged && viewW > 0 && viewH > 0
    ? `width:${viewW}px;height:${viewH}px;max-width:none;margin:0;padding:0;column-width:${viewW}px;column-gap:0;column-fill:auto;font-size:${effFont}px;line-height:${effLine};font-family:${FONT_STACKS[fontFamily]}`
    : `max-width:${widthCss(width)};font-size:${effFont}px;line-height:${effLine};font-family:${FONT_STACKS[fontFamily]}`;
  async function measurePages() {
    await tick();
    if (!scrollEl) return;
    viewW = scrollEl.clientWidth || viewW;
    viewH = scrollEl.clientHeight || viewH;
    await tick();
    if (isPaged) {
      const w = Math.max(1, viewW);
      pageCount = Math.max(1, Math.round(scrollEl.scrollWidth / w));
      pageIndex = clamp(Math.round(scrollEl.scrollLeft / w), 0, pageCount - 1);
    } else {
      pageCount = 1;
      pageIndex = 0;
    }
  }

  // 后端 /api/book/content 会三级回填 book_url；但若仍返回 400 missing_book_url，
  // 用本地 book_id（书架 book_id / info.book_url）重试一次，重试也失败才提示。
  function isMissingBookUrl(e) {
    return !!(e && e.status === 400 && e.data && e.data.error === 'missing_book_url');
  }

  async function fetchContent(chapterUrl, bookUrl, title) {
    try {
      return await api.content(book.sourceId, chapterUrl, bookUrl, title, useReplaceRules);
    } catch (e) {
      if (!isMissingBookUrl(e)) throw e;
      const localId = book.bookUrl || (info && info.book_url) || '';
      if (!localId || localId === bookUrl) throw e;
      return api.content(book.sourceId, chapterUrl, localId, title, useReplaceRules);
    }
  }

  async function openChapter(pos, opts = {}) {
    if (!isReadable(pos)) {
      if (pos < 0) return;
      // 点到分卷标题：自动进入它后面的第一段正文
      let i = pos;
      while (i < chapters.length && chapters[i].is_volume) i += 1;
      if (i >= chapters.length) return;
      pos = i;
    }
    if (!opts.initial) flushStats();
    // 打开本书任一章节即视为已看到最新目录，清空书架新章节角标（每次阅读会话只发一次）
    if (!seenMarked && book.bookUrl) {
      seenMarked = true;
      api.markShelfSeen(book.bookUrl).catch(() => {});
    }
    const myReq = ++reqId;
    currentPos = pos;
    // 先渲染标题与加载态，正文接口返回后再填充：慢源（1–5s）下不再是整页空白。
    current = {
      title: chapters[pos].title,
      url: chapters[pos].url,
      content: '',
      content_type: 'text',
      images: [],
    };
    contentLoading = true;
    contentError = '';
    showToc = false;
    suppressScroll = true;
    restored = false;
    // book_url 三级回填（缓存 → 请求参数 → book_chapters 反查）由后端负责，
    // 本地没地址也照发，不硬停；后端仍判 missing_book_url 时才用本地地址重试一次。
    const contentBookUrl = book.bookUrl || (info && info.book_url) || '';
    try {
      const data = await fetchContent(
        chapters[pos].url,
        contentBookUrl,
        (info && info.name) || book.name || chapters[pos].title || ''
      );
      if (myReq !== reqId) return;
      current = data;
      contentLoading = false;
      if (!statsSeen.has(pos)) {
        statsSeen.add(pos);
        statsChars += plainChars(data && data.content);
      }
      await tick();
      await measurePages();
      applyRatio(opts.ratio || 0);
      restored = true;
      if (!opts.initial) saveProgressNow(opts.ratio || 0);
    } catch (e) {
      if (myReq !== reqId) return;
      contentError = describeFailure(e, '加载正文');
      contentLoading = false;
    }
  }

  function retryChapter() {
    if (currentPos >= 0) openChapter(currentPos, { ratio: currentRatio() });
  }

  // 把 0..1 的阅读比例还原到当前模式（滚动用 scrollTop，分页用 scrollLeft）
  function applyRatio(r) {
    if (!scrollEl) return;
    const ratio = clamp01(r);
    suppressScroll = true;
    if (isPaged) {
      const max = Math.max(0, scrollEl.scrollWidth - scrollEl.clientWidth);
      scrollEl.scrollLeft = ratio * max;
    } else {
      const max = Math.max(0, scrollEl.scrollHeight - scrollEl.clientHeight);
      scrollEl.scrollTop = ratio * max;
    }
    // 图片等异步布局可能改变高度/列数，稍后再校正一次
    setTimeout(() => {
      if (!scrollEl) return;
      if (isPaged) {
        const w = Math.max(1, viewW);
        pageCount = Math.max(1, Math.round(scrollEl.scrollWidth / w));
        const idx = clamp(Math.round(scrollEl.scrollLeft / w), 0, pageCount - 1);
        scrollEl.scrollLeft = idx * w;
        pageIndex = idx;
      } else {
        const max = Math.max(0, scrollEl.scrollHeight - scrollEl.clientHeight);
        scrollEl.scrollTop = ratio * max;
      }
      suppressScroll = false;
    }, 240);
  }

  function currentRatio() {
    if (!scrollEl) return 0;
    if (isPaged) {
      const max = scrollEl.scrollWidth - scrollEl.clientWidth;
      return max > 0 ? clamp01(scrollEl.scrollLeft / max) : 0;
    }
    const max = scrollEl.scrollHeight - scrollEl.clientHeight;
    return max > 0 ? clamp01(scrollEl.scrollTop / max) : 0;
  }

  function recordRatio(ratio) {
    if (currentPos < 0) return;
    try {
      localStorage.setItem(
        progressKey(book.bookUrl),
        JSON.stringify({ chapter_index: currentPos, position: ratio, at: Date.now() })
      );
    } catch {
      /* 本地存储不可用时忽略 */
    }
    const pos = currentPos;
    // 滚动过程中只更新本地与内存；满 30s 才写一次后端，避免每页一个 POST
    if (Date.now() - lastSaveAt >= SAVE_INTERVAL) persist(pos, ratio);
  }

  function onScroll() {
    if (currentPos < 0) return;
    if (isPaged && viewW > 0) {
      pageIndex = clamp(Math.round(scrollEl.scrollLeft / viewW), 0, pageCount - 1);
    }
    if (suppressScroll) return;
    recordRatio(currentRatio());
    if (isPaged) {
      clearTimeout(snapTimer);
      snapTimer = setTimeout(snapToPage, 160);
    }
  }

  // 触摸横滑后吸附到整页
  function snapToPage() {
    if (!isPaged || suppressScroll || !scrollEl) return;
    const w = Math.max(1, viewW);
    const target = clamp(Math.round(scrollEl.scrollLeft / w), 0, pageCount - 1);
    const left = target * w;
    pageIndex = target;
    if (Math.abs(scrollEl.scrollLeft - left) > 1) {
      scrollEl.scrollTo({ left, behavior: 'smooth' });
    }
  }

  function flushSave(keepalive = false) {
    clearTimeout(saveTimer);
    if (currentPos >= 0 && restored) persist(currentPos, currentRatio(), keepalive);
  }

  function saveProgressNow(ratio) {
    persist(currentPos, ratio);
  }

  function persist(pos, ratio, keepalive = false) {
    if (pos < 0) return;
    lastSaveAt = Date.now();
    api
      .setProgress(
        {
          book_id: book.bookUrl,
          chapter_index: pos,
          position: clamp01(ratio),
          total_chapters: chapters.length,
        },
        keepalive
      )
      .catch(() => {});
  }

  function chooseChapter(pos) {
    if (pos === currentPos) {
      showToc = false;
      return;
    }
    openChapter(pos, { ratio: 0 });
  }

  function toggleToc() {
    if (!showToc) {
      showPanel = false;
      showBookmarks = false;
      showSearch = false;
      showJump = false;
    }
    showToc = !showToc;
  }

  // ---- 跳转定位：章节 + 章内比例/页码 + 段落 ----
  function openJump() {
    if (currentPos < 0) return;
    showPanel = false;
    showToc = false;
    showBookmarks = false;
    showSearch = false;
    showJump = true;
    jumpChapter = currentPos + 1;
    jumpPct = Math.round(currentRatio() * 100);
    jumpPage = pageIndex + 1;
    jumpPara = '';
  }

  // 换到另一章时回到章首（比例/页码清零），避免“跳到第 3 章”却落在中间的 8%
  function onJumpChapter(v) {
    jumpChapter = v;
    const idx = clamp(Math.round(num(v, currentPos + 1)) - 1, 0, Math.max(0, chapters.length - 1));
    if (idx !== currentPos) {
      jumpPct = 0;
      jumpPage = 1;
    }
  }

  // 翻页模式下“比例”和“页码”是同一位置的两种写法，改动任意一个同步另一个，
  // 避免用户拖了比例却被旧的页码覆盖。
  function syncJumpPage() {
    if (!isPaged || pageCount < 2) return;
    jumpPage = clamp(Math.round(1 + clamp01(num(jumpPct, 0) / 100) * (pageCount - 1)), 1, pageCount);
  }

  function syncJumpPct() {
    if (!isPaged || pageCount < 2) return;
    const p = clamp(Math.round(num(jumpPage, 1)), 1, pageCount);
    jumpPct = Math.round(((p - 1) / (pageCount - 1)) * 100);
  }

  async function doJump() {
    const idx = clamp(Math.round(num(jumpChapter, currentPos + 1)) - 1, 0, Math.max(0, chapters.length - 1));
    let ratio = clamp01(num(jumpPct, 0) / 100);
    // 翻页模式下按页码折算成同一套 0..1 章内比例（同章才有页概念）
    if (isPaged && idx === currentPos && pageCount > 1) {
      ratio = (clamp(Math.round(num(jumpPage, pageIndex + 1)), 1, pageCount) - 1) / (pageCount - 1);
    }
    const para = Math.round(num(jumpPara, 0));
    showJump = false;
    if (idx !== currentPos) {
      await openChapter(idx, { ratio });
    } else {
      applyRatio(ratio);
      saveProgressNow(ratio);
    }
    if (para > 0) {
      // applyRatio 会在 240ms 后做一次布局校正，等它落定再按段精确定位
      await new Promise((r) => setTimeout(r, 320));
      await jumpToParagraph(para - 1);
    }
  }

  async function jumpToParagraph(i) {
    await tick();
    if (!scrollEl) return;
    if (!current || i >= paras.length) {
      showToast(`本章只有 ${paras.length} 段`);
      return;
    }
    const el = scrollEl.querySelector(`[data-para="${i}"]`);
    if (!el) return;
    suppressScroll = true;
    const host = scrollEl.getBoundingClientRect();
    if (isPaged) {
      const left = el.getBoundingClientRect().left - host.left + scrollEl.scrollLeft;
      const w = Math.max(1, viewW);
      scrollEl.scrollLeft = Math.max(0, Math.floor(left / w) * w);
    } else {
      const top = el.getBoundingClientRect().top - host.top + scrollEl.scrollTop;
      scrollEl.scrollTop = Math.max(0, top - 6);
    }
    setTimeout(() => {
      suppressScroll = false;
      measurePages();
      persist(currentPos, currentRatio());
    }, 260);
  }

  // ---- 书签：位置沿用阅读进度同一套 0..1 章内比例 ----
  function togglePanel() {
    if (!showPanel) {
      showToc = false;
      showBookmarks = false;
      showSearch = false;
      showJump = false;
    }
    showPanel = !showPanel;
  }

  function toggleBookmarks() {
    showSearch = false;
    showPanel = false;
    showToc = false;
    showJump = false;
    showBookmarks = !showBookmarks;
  }

  async function gotoBookmark(bm) {
    const idx = clamp(Number(bm.chapter_index) || 0, 0, Math.max(0, chapters.length - 1));
    const ratio = clamp01(bm.position);
    showBookmarks = false;
    if (idx === currentPos && current) {
      // 同章直接复用已有比例定位，不重新请求正文
      applyRatio(ratio);
      saveProgressNow(ratio);
      showToast('已跳到书签位置');
      return;
    }
    await openChapter(idx, { ratio });
  }

  // ---- 书内搜索：后端只检索已缓存正文，complete=false 要如实提示 ----
  function toggleBookSearch() {
    showBookmarks = false;
    showPanel = false;
    showToc = false;
    showJump = false;
    showSearch = !showSearch;
  }

  async function gotoSearchHit(hit) {
    const idx = clamp(Number(hit.chapter_index) || 0, 0, Math.max(0, chapters.length - 1));
    const q = String(hit.query || '').trim().toLowerCase();
    showSearch = false;
    await openChapter(idx, { ratio: 0 });
    // 复用段落定位：找到第一个含关键词的段落再滚过去
    if (!q) return;
    const p = paras.findIndex((x) => x.type === 'text' && x.text.toLowerCase().includes(q));
    if (p < 0) return;
    await new Promise((r) => setTimeout(r, 320));
    await jumpToParagraph(p);
  }

  // ---- 模式切换：保持当前阅读比例 ----
  async function changeMode(m, persist = true) {
    if (!MODES.includes(m) || m === readMode) return;
    const keep = restored ? currentRatio() : 0;
    stopAuto();
    readMode = m;
    localStorage.setItem('oread_read_mode', m);
    if (persist) queueSettingsSave();
    await measurePages();
    applyRatio(keep);
  }

  function triggerFlip(dir) {
    flipDir = 0;
    clearTimeout(flipTimer);
    tick().then(() => {
      flipDir = dir;
      flipTimer = setTimeout(() => (flipDir = 0), 340);
    });
  }

  // ---- 翻页 ----
  function setPage(idx, smooth = true) {
    if (!isPaged || !scrollEl || contentLoading) return;
    const target = clamp(idx, 0, pageCount - 1);
    if (target === pageIndex && Math.abs(scrollEl.scrollLeft - target * viewW) < 2) return;
    const dir = target > pageIndex ? 1 : -1;
    const ratio = pageCount > 1 ? target / (pageCount - 1) : 0;
    pageIndex = target;
    suppressScroll = true;

    scrollEl.scrollTo({ left: target * viewW, behavior: smooth ? 'smooth' : 'auto' });
    recordRatio(ratio);
    if (isFade) triggerFlip(dir);
    clearTimeout(snapTimer);
    setTimeout(() => {
      suppressScroll = false;
      if (scrollEl) pageIndex = clamp(Math.round(scrollEl.scrollLeft / Math.max(1, viewW)), 0, pageCount - 1);
    }, 300);
  }

  function prevPage() {
    if (autoOn) stopAuto();
    if (!scrollEl || contentLoading) return;
    if (isPaged) {
      if (pageIndex > 0) setPage(pageIndex - 1);
      else if (prevPos >= 0) openChapter(prevPos, { ratio: 0.999 });
      else showToast('已经是第一页');
      return;
    }
    if (scrollEl.scrollTop <= 2) {
      if (prevPos >= 0) openChapter(prevPos, { ratio: 0.999 });
      else showToast('已经是第一页');
      return;
    }
    scrollEl.scrollBy({ top: -Math.max(120, scrollEl.clientHeight * 0.9), behavior: 'smooth' });
  }

  function nextPage() {
    if (autoOn) stopAuto();
    if (!scrollEl || contentLoading) return;
    if (isPaged) {
      if (pageIndex < pageCount - 1) setPage(pageIndex + 1);
      else if (nextPos >= 0) openChapter(nextPos, { ratio: 0 });
      else showToast('已经是最后一页');
      return;
    }
    const max = scrollEl.scrollHeight - scrollEl.clientHeight;
    if (scrollEl.scrollTop >= max - 2) {
      if (nextPos >= 0) openChapter(nextPos, { ratio: 0 });
      else showToast('已经是最后一页');
      return;
    }
    scrollEl.scrollBy({ top: Math.max(120, scrollEl.clientHeight * 0.9), behavior: 'smooth' });
  }

  async function toggleControls() {
    const keep = currentRatio();
    controlsVisible = !controlsVisible;
    await tick();
    await measurePages();
    applyRatio(keep);
  }

  // ---- 手势：左右 1/3 翻页，中间显隐控制栏；上下滑到边界快速切章 ----
  function onReaderClick(e) {
    if (Date.now() < suppressClickUntil) return;
    if (!current || contentLoading || showToc || showPanel || showBookmarks || showSearch || showJump) return;
    if (e.target.closest('button,a,input,select,textarea,label')) return;
    const sel = window.getSelection ? String(window.getSelection()).trim() : '';
    if (sel) return; // 长按选中文本时不触发翻页
    const el = scrollEl;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const x = e.clientX - r.left;
    if (x < r.width / 3) prevPage();
    else if (x > (r.width * 2) / 3) nextPage();
    else toggleControls();
  }

  function onReaderTouchStart(e) {
    if (!e.touches || !e.touches.length) return;
    const t = e.touches[0];
    touchStart = {
      x: t.clientX,
      y: t.clientY,
      at: Date.now(),
      top: scrollEl ? scrollEl.scrollTop : 0,
    };
    if (autoOn) stopAuto();
  }

  function onReaderTouchEnd(e) {
    const s = touchStart;
    touchStart = null;
    if (!s || !e.changedTouches || !e.changedTouches.length) return;
    const t = e.changedTouches[0];
    const dx = t.clientX - s.x;
    const dy = t.clientY - s.y;
    const dt = Date.now() - s.at;
    // 有明显位移时抑制随后的 click，避免横滑/纵滑结束后误触翻页
    if (Math.abs(dx) > 24 || Math.abs(dy) > 24) suppressClickUntil = Date.now() + 400;
    // 长按 / 小幅滑动 / 以横滑为主时不切章，避免误触与选择文本冲突
    if (dt > 600 || Math.abs(dy) < 100 || Math.abs(dy) <= Math.abs(dx) * 1.3) return;
    if (window.getSelection && String(window.getSelection()).trim()) return;
    suppressClickUntil = Date.now() + 400;
    if (isPaged) {
      if (dy < 0 && nextPos >= 0) openChapter(nextPos, { ratio: 0 });
      else if (dy > 0 && prevPos >= 0) openChapter(prevPos, { ratio: 0.999 });
      return;
    }
    const el = scrollEl;
    if (!el) return;
    const max = el.scrollHeight - el.clientHeight;
    if (dy > 0 && s.top <= 4 && prevPos >= 0) openChapter(prevPos, { ratio: 0.999 });
    else if (dy < 0 && s.top >= max - 4 && nextPos >= 0) openChapter(nextPos, { ratio: 0 });
  }

  // ---- 排版调整：保持当前阅读比例不跳动 ----
  async function applyStyle(mutator) {
    const keep = currentRatio();
    mutator();
    await measurePages();
    applyRatio(keep);
    queueSettingsSave();
  }

  function stepFont(delta) {
    applyStyle(() => (font = clamp(effFont + delta, 12, 30)));
  }
  function stepLine(delta) {
    applyStyle(() => (lineHeight = clamp(Math.round((effLine + delta) * 10) / 10, 1.2, 2.6)));
  }
  function setTheme(t) {
    applyStyle(() => (theme = t));
    // 与设置页共用全局主题（html[data-theme]），阅读器内选择也会同步整站配色
    applyTheme(t);
  }
  function setWidth(w) {
    if (!WIDTH_OPTIONS.some((x) => x.id === w)) return;
    localStorage.setItem('oread_read_width', w);
    applyStyle(() => (width = w));
  }
  function setFontFamily(f) {
    if (!FONT_STACKS[f]) return;
    localStorage.setItem('oread_font_family', f);
    applyStyle(() => (fontFamily = f));
  }
  function setZh(m) {
    zhMode = m;
    localStorage.setItem('oread_zh_mode', m);
  }
  function setBrightness(v) {
    brightness = clamp(Number(v), 0, 100);
    localStorage.setItem('oread_brightness', String(brightness));
  }
  function setAutoSpeed(v) {
    autoSpeed = clamp(Number(v), 1, 10);
    localStorage.setItem('oread_auto_speed', String(autoSpeed));
    if (autoOn && isPaged) scheduleAutoPage();
  }

  let settingsTimer = null;
  function queueSettingsSave() {
    clearTimeout(settingsTimer);
    settingsTimer = setTimeout(() => {
      api
        .saveSettings({
          font_size: font,
          line_height: lineHeight,
          theme,
          read_mode: readMode,
          page_width: widthPxFromId(width),
          font_family: fontServerValue(fontFamily),
        })
        .catch(() => {});
    }, 800);
  }

  // ---- 自动滚动 / 自动翻页 ----
  function toggleAuto() {
    if (autoOn) {
      stopAuto();
      return;
    }
    if (!current || contentLoading) {
      showToast('正文尚未加载');
      return;
    }
    showPanel = false;
    autoOn = true;
    autoLast = 0;
    if (isPaged) scheduleAutoPage(400);
    else autoRaf = requestAnimationFrame(autoTick);
  }

  function stopAuto(msg) {
    autoOn = false;
    cancelAnimationFrame(autoRaf);
    autoRaf = 0;
    clearTimeout(autoTimer);
    autoTimer = null;
    if (msg) showToast(msg);
  }

  function autoTick(ts) {
    if (!autoOn || isPaged) return;
    if (!autoLast) autoLast = ts;
    const dt = Math.min(80, ts - autoLast);
    autoLast = ts;
    if (contentLoading) {
      autoRaf = requestAnimationFrame(autoTick);
      return;
    }
    if (contentError) {
      stopAuto('正文加载失败，已停止自动滚动');
      return;
    }
    const el = scrollEl;
    if (!el) {
      autoRaf = requestAnimationFrame(autoTick);
      return;
    }
    const max = el.scrollHeight - el.clientHeight;
    if (el.scrollTop >= max - 1) {
      if (nextPos >= 0) {
        if (!autoAdvancing) {
          autoAdvancing = true;
          openChapter(nextPos, { ratio: 0 }).finally(() => (autoAdvancing = false));
        }
      } else {
        stopAuto('已到全书末尾');
        return;
      }
    } else {
      el.scrollTop += (autoSpeed * 14) * dt / 1000;
    }
    autoRaf = requestAnimationFrame(autoTick);
  }

  function scheduleAutoPage(delay) {
    clearTimeout(autoTimer);
    if (!autoOn || !isPaged) return;
    // 自动翻页节奏按档位线性变化，最低 900ms
    const wait = delay != null ? delay : (11 - autoSpeed) * 1000;
    autoTimer = setTimeout(autoPageStep, Math.max(900, wait));
  }

  async function autoPageStep() {
    if (!autoOn || !isPaged) return;
    if (contentLoading) {
      scheduleAutoPage(900);
      return;
    }
    if (contentError) {
      stopAuto('正文加载失败，已停止自动滚动');
      return;
    }
    if (pageIndex < pageCount - 1) {
      setPage(pageIndex + 1);
      scheduleAutoPage();
    } else if (nextPos >= 0) {
      await openChapter(nextPos, { ratio: 0 });
      scheduleAutoPage();
    } else {
      stopAuto('已到全书末尾');
    }
  }

  function showToast(msg) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ''), 2200);
  }

  let wheelAt = 0;
  // 桌面滚轮：滚动模式只负责停止自动滚动；分页模式把滚轮映射为翻页
  function onWheel(e) {
    if (autoOn) stopAuto();
    if (!isPaged || showToc || showPanel || showBookmarks || showSearch || showJump) return;
    if (Math.abs(e.deltaY) < 6 && Math.abs(e.deltaX) < 6) return;
    const now = Date.now();
    if (now - wheelAt < 320) return;
    wheelAt = now;
    if (e.deltaY > 0 || e.deltaX > 0) nextPage();
    else prevPage();
  }

  function onKeydown(e) {
    const tag = ((e.target && e.target.tagName) || '').toLowerCase();
    if (['input', 'textarea', 'select', 'button'].includes(tag)) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (showExport && e.key !== 'Escape') return;
    if (e.key === 'ArrowLeft') {
      e.preventDefault();
      prevPage();
    } else if (e.key === 'ArrowRight') {
      e.preventDefault();
      nextPage();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (!isPaged && scrollEl) scrollEl.scrollBy({ top: -80, behavior: 'smooth' });
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (!isPaged && scrollEl) scrollEl.scrollBy({ top: 80, behavior: 'smooth' });
    } else if (e.key === ' ' || e.key === 'Spacebar') {
      e.preventDefault();
      toggleAuto();
    } else if (e.key === 't' || e.key === 'T') {
      // 键盘用户无需先点中间唤出控制栏再点“目录”。
      toggleToc();
    } else if (e.key === 'Escape') {
      if (showExport) showExport = false;
      else if (showSwitch) showSwitch = false;
      else if (showSearch) showSearch = false;
      else if (showBookmarks) showBookmarks = false;
      else if (showJump) showJump = false;
      else if (showPanel) showPanel = false;
      else if (showToc) showToc = false;
    }
  }

  // 窗口缩放/横竖屏切换：滚动与分页都保留章内阅读比例
  function onResize() {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(async () => {
      if (!scrollEl) return;
      const keep = currentRatio();
      await tick();
      await measurePages();
      applyRatio(keep);
    }, 280);
  }

  function close() {
    stopAuto();
    flushSave();
    flushStats(true);
    dispatch('close');
  }

  // ---- 换源 ----
  function toggleSwitch() {
    if (!showSwitch) {
      showPanel = false;
      showToc = false;
      showBookmarks = false;
      showSearch = false;
      showJump = false;
    }
    showSwitch = !showSwitch;
    if (showSwitch && !candidates.length && !switchLoading) loadCandidates();
  }

  async function loadCandidates() {
    switchLoading = true;
    switchErr = '';
    candidates = [];
    try {
      const list = await api.switchSource(
        (info && info.name) || seed.name || '',
        (info && info.author) || seed.author || '',
        book.sourceId
      );
      candidates = Array.isArray(list) ? list : [];
    } catch (e) {
      switchErr = describeFailure(e, '换源');
    } finally {
      switchLoading = false;
    }
  }

  function useCandidate(c) {
    // 换源后旧源的 seed 全部作废：只带候选自己的地址、书源与书名/作者/封面；
    // 候选缺地址或缺来源时拒绝切换，避免回退到旧源造成“看似换源、实际还在旧源”。
    const nextUrl = c.book_url || c.bookUrl || '';
    const nextSource = c.origin || c.source_id || '';
    if (!nextUrl || !nextSource) {
      switchErr = '该候选缺少书籍地址或书源标识，无法切换。请换一个书源或重新搜索。';
      return;
    }
    stopAuto();
    flushSave();
    flushStats(true);
    showSwitch = false;
    dispatch('switch', {
      bookUrl: nextUrl,
      sourceId: nextSource,
      name: c.name || '',
      author: c.author || '',
      coverUrl: c.cover_url || c.coverUrl || '',
      intro: c.intro || '',
    });
  }
</script>

<div class="rd-root theme-{theme}">
  {#if controlsVisible}
    <header class="rd-bar">
      <button class="rd-btn" on:click={close} aria-label="返回书架">‹ 返回</button>
      <div class="rd-title">{info?.name || book.name || '阅读'}</div>
      <button class="rd-btn" on:click={toggleToc} aria-expanded={showToc} aria-label="打开目录">目录</button>
      <button class="rd-btn" on:click={toggleBookmarks} aria-expanded={showBookmarks} aria-label="书签">书签</button>
      <button class="rd-btn" on:click={() => (showExport = true)} aria-label="导出 TXT">导出</button>
      <button class="rd-btn" on:click={toggleSwitch} aria-expanded={showSwitch} aria-label="换源">换源</button>
      <button class="rd-btn" on:click={togglePanel} aria-label="阅读设置">Aa</button>
    </header>
  {/if}

  {#if showPayNotice && !tocLoading && !tocError}
    <div class="pay-notice" role="note">
      <span class="pay-text">
        {payKind === 'vip' ? '目录标记 V（VIP）' : '目录标记 P（付费）'}：本章可能为付费或试读内容。
      </span>
      <button class="rd-btn" on:click={() => (payNoticeClosed = currentPos)} aria-label="关闭付费提示">知道了</button>
    </div>
  {/if}

  <div
    class="rd-scroll"
    class:mode-paged={isPaged}
    bind:this={scrollEl}
    on:scroll={onScroll}
    on:click={onReaderClick}
    on:touchstart={onReaderTouchStart}
    on:touchend={onReaderTouchEnd}
    on:touchcancel={() => (touchStart = null)}
    on:wheel={onWheel}
  >
    {#if tocLoading}
      <div class="rd-state">
        <div class="spinner"></div>
        <p>正在打开书籍…</p>
        <p class="rd-muted">首次加载目录可能需要几秒</p>
      </div>
    {:else if tocError}
      <div class="rd-state">
        <p class="rd-error">{tocError}</p>
        <button class="rd-btn solid" on:click={loadAll}>重试</button>
      </div>
    {:else}
      <div
        class="rd-content"
        class:paged={isPaged}
        class:flip-next={flipDir === 1}
        class:flip-prev={flipDir === -1}
        style={contentStyle}
      >
        <div class="rd-meta">
          {#if info?.author}<span>{info.author}</span>{/if}
          {#if info?.origin_name}<span class="dot">·</span><span>{info.origin_name}</span>{/if}
          <span class="dot">·</span><span>第 {currentPos + 1} / {chapters.length} 章</span>
          {#if isPaged}<span class="dot">·</span><span>{pageIndex + 1} / {pageCount} 页</span>{/if}
        </div>

        {#if current && current.replace_applied}
          <div class="replace-hint" role="note">已应用替换规则 · 可在 Aa 设置中关闭</div>
        {/if}

        {#if contentLoading && !current}
          <div class="rd-state slim"><div class="spinner"></div><p>加载正文…</p></div>
        {:else if contentError}
          <div class="rd-state">
            <p class="rd-error">{contentError}</p>
            <div class="err-actions">
              <button class="rd-btn solid" on:click={() => openChapter(currentPos, { ratio: currentRatio() })}>重试本章</button>
              <button class="rd-btn" on:click={toggleSwitch}>换源</button>
            </div>
          </div>
        {:else if current}
          <h1 class="rd-chapter-title">{displayTitle}</h1>
          {#if contentLoading}
            <div class="rd-state">加载正文…</div>
          {:else if chapterEmpty}
            <div class="rd-state rd-empty" role="status">
              <p class="rd-empty-title">{emptyTitle}</p>
              <p class="rd-muted">{emptyHint}</p>
              <div class="err-actions">
                <button class="rd-btn solid" on:click={retryChapter}>重试本章</button>
                <button class="rd-btn" on:click={toggleSwitch}>换源</button>
                {#if nextPos >= 0}
                  <button class="rd-btn" on:click={() => openChapter(nextPos, { ratio: 0 })}>下一章</button>
                {/if}
                <button class="rd-btn" on:click={toggleToc}>目录</button>
              </div>
            </div>
          {:else}
            {#each displayParas as p, i}
              {#if p.type === 'image'}
                <div class="rd-img" data-para={i}>
                  {#if imgState[p.src]?.status === 'ok'}
                    <img
                      src={imgState[p.src].objectUrl}
                      alt={p.alt || '插图'}
                      on:error={() => onImgError(p.src)}
                    />
                  {:else if imgState[p.src]?.status === 'error'}
                    <div class="rd-img-ph">
                      <div>插图加载失败</div>
                      <div class="rd-img-err">{imgState[p.src].error}</div>
                      <button class="rd-btn" on:click={() => retryImage(p.src)}>重试</button>
                    </div>
                  {:else}
                    <div class="rd-img-ph">插图加载中…</div>
                  {/if}
                </div>
              {:else}
                <p class="rd-p" data-para={i}>{p.text}</p>
              {/if}
            {/each}
          {/if}
          <div class="rd-navrow">
            <button class="rd-btn" disabled={prevPos < 0} on:click={() => openChapter(prevPos, { ratio: 0.999 })}>上一章</button>
            <button class="rd-btn" on:click={toggleToc}>目录</button>
            <button class="rd-btn" disabled={nextPos < 0} on:click={() => openChapter(nextPos, { ratio: 0 })}>下一章</button>
          </div>
        {/if}
      </div>
    {/if}
  </div>

  <div class="dim-layer" style={`opacity:${dimOpacity}`}></div>

  {#if isPaged && controlsVisible}
    <div class="rd-pager">
      <button class="rd-btn" on:click={prevPage}>‹ 上一页</button>
      <span class="pager-num">{pageIndex + 1} / {pageCount}</span>
      <button class="rd-btn" on:click={nextPage}>下一页 ›</button>
    </div>
  {/if}

  {#if autoOn}
    <button class="auto-pill" on:click={() => stopAuto()} aria-label="暂停自动滚动">
      自动滚动 {autoSpeed} 档 · 暂停
    </button>
  {/if}

  {#if toast}<div class="rd-toast">{toast}</div>{/if}

  <ReaderToc
    bind:show={showToc}
    chapters={chapters}
    currentPos={currentPos}
    on:choose={(e) => chooseChapter(e.detail)}
  />

  {#if showPanel}
    <div class="panel-mask" on:click|self={() => (showPanel = false)}>
      <div class="panel">
        <div class="panel-head">
          <strong>阅读设置</strong>
          <button class="rd-btn" on:click={() => (showPanel = false)}>收起</button>
        </div>

        <div class="panel-row">
          <span>模式</span>
          <div class="seg small">
            {#each MODES as m}
              <button class:on={readMode === m} on:click={() => changeMode(m)}>{modeName[m]}</button>
            {/each}
          </div>
        </div>

        <div class="panel-row">
          <span>字号</span>
          <div class="stepper">
            <button class="rd-btn" on:click={() => stepFont(-1)} disabled={effFont <= 12}>A−</button>
            <span class="val">{effFont}</span>
            <button class="rd-btn" on:click={() => stepFont(1)} disabled={effFont >= 30}>A+</button>
          </div>
        </div>

        <div class="panel-row">
          <span>行距</span>
          <div class="stepper">
            <button class="rd-btn" on:click={() => stepLine(-0.1)} disabled={effLine <= 1.2}>−</button>
            <span class="val">{effLine.toFixed(1)}</span>
            <button class="rd-btn" on:click={() => stepLine(0.1)} disabled={effLine >= 2.6}>+</button>
          </div>
        </div>

        <div class="panel-row">
          <span>字体</span>
          <div class="seg small">
            {#each FONT_OPTIONS as f}
              <button class:on={fontFamily === f.id} on:click={() => setFontFamily(f.id)}>{f.label}</button>
            {/each}
          </div>
        </div>

        <div class="panel-row">
          <span>定位</span>
          <button class="rd-btn" on:click={openJump}>跳转章节 / 段</button>
        </div>

        <div class="panel-row">
          <span>配色</span>
          <div class="seg small">
            {#each ['light', 'dark', 'sepia', 'eink'] as t}
              <button class:on={theme === t} on:click={() => setTheme(t)}>{themeName[t]}</button>
            {/each}
          </div>
        </div>

        <div class="panel-row">
          <span>字形</span>
          <div class="seg small">
            {#each ['off', 's', 't'] as m}
              <button class:on={zhMode === m} on:click={() => setZh(m)}>{zhLabels[m]}</button>
            {/each}
          </div>
        </div>

        <div class="panel-row">
          <span>替换规则</span>
          <button class="rd-btn" class:solid={useReplaceRules} on:click={() => setUseReplace(!useReplaceRules)}>
            {useReplaceRules ? '已开启' : '已关闭'}
          </button>
        </div>

        {#if !isPaged}
          <div class="panel-row">
            <span>页宽</span>
            <div class="seg small">
              {#each WIDTH_OPTIONS as w}
                <button class:on={width === w.id} on:click={() => setWidth(w.id)}>{w.label}</button>
              {/each}
            </div>
          </div>
        {/if}

        <div class="panel-row">
          <span>亮度</span>
          <div class="slider-wrap">
            <input type="range" min="0" max="100" step="5" bind:value={brightness} on:input={(e) => setBrightness(e.target.value)} />
            <span class="val">{brightness}%</span>
          </div>
        </div>

        <div class="panel-row">
          <span>自动滚动</span>
          <div class="stepper">
            <button class="rd-btn" class:solid={autoOn} on:click={toggleAuto}>{autoOn ? '暂停' : '开始'}</button>
            <span class="val">{autoSpeed} 档</span>
          </div>
        </div>

        <div class="panel-row">
          <span>速度</span>
          <div class="slider-wrap">
            <input type="range" min="1" max="10" step="1" bind:value={autoSpeed} on:input={(e) => setAutoSpeed(e.target.value)} />
            <span class="val">{autoSpeed}</span>
          </div>
        </div>

        <div class="panel-row">
          <span>本书</span>
          <div class="row-actions">
            <button class="rd-btn" on:click={toggleBookmarks}>书签</button>
            <button class="rd-btn" on:click={toggleBookSearch}>书内搜索</button>
            <button class="rd-btn" on:click={() => { showPanel = false; toggleSwitch(); }}>换源</button>
          </div>
        </div>

        <p class="rd-muted small-note">
          左 1/3 上一页，右 1/3 下一页，中间显隐控制栏；上下滑到边界快速切章；长按正文可选中复制。
          字号/行距/配色/模式/页宽/字体族保存到账号设置，本机保留一份用于离线兜底；亮度、字形、速度仅存本机。
        </p>
        <p class="rd-muted small-note desk-only">
          快捷键 <kbd>←</kbd> <kbd>→</kbd> 翻页，<kbd>↑</kbd> <kbd>↓</kbd> 滚动，<kbd>空格</kbd> 暂停/开始自动滚动，<kbd>Esc</kbd> 收起面板。
        </p>
      </div>
    </div>
  {/if}

  <ReaderBookmarks
    bookUrl={book.bookUrl}
    bind:show={showBookmarks}
    currentPos={currentPos}
    chapterTitle={chapters[currentPos]?.title || ''}
    hasContent={!!current}
    getRatio={() => currentRatio()}
    on:goto={(e) => gotoBookmark(e.detail)}
    on:toast={(e) => showToast(e.detail)}
  />

  <ReaderSearch
    bookUrl={book.bookUrl}
    bind:show={showSearch}
    on:goto={(e) => gotoSearchHit(e.detail)}
  />

  {#if showJump}
    <div class="panel-mask" on:click|self={() => (showJump = false)}>
      <div class="panel jump-panel" role="dialog" aria-label="跳转定位">
        <div class="panel-head">
          <strong>跳转定位</strong>
          <button class="rd-btn" on:click={() => (showJump = false)}>关闭</button>
        </div>

        <div class="panel-row">
          <span>章节</span>
          <div class="stepper">
            <input
              class="jump-num"
              type="number"
              min="1"
              max={chapters.length}
              value={jumpChapter}
              on:input={(e) => onJumpChapter(e.currentTarget.value)}
              aria-label="章节序号"
            />
            <span class="val">/ {chapters.length}</span>
          </div>
        </div>

        <div class="panel-row">
          <span>章内位置</span>
          <div class="slider-wrap">
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={jumpPct}
              on:input={(e) => {
                jumpPct = Number(e.currentTarget.value);
                syncJumpPage();
              }}
              aria-label="章内位置百分比"
            />
            <span class="val">{jumpPct}%</span>
          </div>
        </div>

        {#if isPaged && (Number(jumpChapter) || 1) - 1 === currentPos}
          <div class="panel-row">
            <span>页码</span>
            <div class="stepper">
              <input
                class="jump-num"
                type="number"
                min="1"
                max={pageCount}
                value={jumpPage}
                on:input={(e) => {
                  jumpPage = Number(e.currentTarget.value);
                  syncJumpPct();
                }}
                aria-label="页码"
              />
              <span class="val">/ {pageCount}（本章）</span>
            </div>
          </div>
        {/if}

        <div class="panel-row">
          <span>段落</span>
          <div class="stepper">
            <input
              class="jump-num"
              type="number"
              min="1"
              max={Math.max(1, paras.length)}
              placeholder="可选"
              bind:value={jumpPara}
              aria-label="段落序号"
            />
            <span class="val">/ {paras.length}</span>
          </div>
        </div>

        <p class="rd-muted small-note">
          位置沿用 0–1 的章内比例（界面按百分比显示）；翻页模式在同一章内按页码折算。段落序号按目标章节的段落数。
        </p>
        <div class="jump-actions">
          <button class="rd-btn solid" on:click={doJump}>跳转</button>
          <button class="rd-btn" on:click={() => (showJump = false)}>取消</button>
        </div>
      </div>
    </div>
  {/if}

  {#if showSwitch}
    <div class="panel-mask" on:click|self={() => (showSwitch = false)}>
      <div class="panel switch-panel">
        <div class="panel-head">
          <strong>换源</strong>
          <button class="rd-btn" on:click={() => (showSwitch = false)}>关闭</button>
        </div>
        <p class="rd-muted small-note">当前：《{info?.name || book.name}》 · 书源 {book.sourceId}</p>
        {#if switchLoading}
          <p class="rd-muted">正在其他书源搜索同名书籍…</p>
        {:else if switchErr}
          <p class="rd-error">{switchErr}</p>
        {:else if !candidates.length}
          <p class="rd-muted">没有找到其他书源的同名书籍。可重新搜索，或先在「发现」页确认书源可用。</p>
        {:else}
          <div class="switch-list">
            {#each candidates as c (c.book_url + '|' + (c.origin || ''))}
              <div class="switch-item">
                <div class="switch-main">
                  <b>{c.name || c.book_url}</b>
                  <span class="rd-muted">{c.author || '未知作者'} · {c.origin_name || c.origin || '未知书源'}</span>
                  {#if c.latest_chapter_title}<span class="rd-muted small-note">最新：{c.latest_chapter_title}</span>{/if}
                </div>
                <button class="rd-btn" on:click={() => useCandidate(c)}>用此书源</button>
              </div>
            {/each}
          </div>
        {/if}
        <div class="row" style="margin-top:10px">
          <button class="rd-btn" on:click={loadCandidates} disabled={switchLoading}>重新搜索</button>
        </div>
      </div>
    </div>
  {/if}

  {#if showExport}
    <ExportDialog
      bookUrl={book.bookUrl}
      name={info?.name || book.name || ''}
      on:close={() => (showExport = false)}
    />
  {/if}
</div>

<style>
  .rd-root {
    --rd-bg: #faf9f6;
    --rd-fg: #202124;
    --rd-muted: #7c7c80;
    --rd-panel: #ffffff;
    --rd-border: #e6e3dc;
    --rd-accent: #b3392f;
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    left: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: var(--rd-bg);
    color: var(--rd-fg);
    font-family: system-ui, -apple-system, 'PingFang SC', 'Microsoft YaHei', serif;
  }
  .theme-dark {
    --rd-bg: #15171b;
    --rd-fg: #c9ccd1;
    --rd-muted: #8b9096;
    --rd-panel: #1e2127;
    --rd-border: #2c313a;
    --rd-accent: #d8756b;
  }
  .theme-sepia {
    --rd-bg: #f5ecd9;
    --rd-fg: #2f2a22;
    --rd-muted: #6b6357;
    --rd-panel: #fbf6ea;
    --rd-border: #d8ccb6;
    --rd-accent: #8a5a2b;
  }
  .theme-eink {
    --rd-bg: #ffffff;
    --rd-fg: #000000;
    --rd-muted: #333333;
    --rd-panel: #ffffff;
    --rd-border: #cccccc;
    --rd-accent: #000000;
  }
  .rd-bar {
    position: relative;
    z-index: 22;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: calc(env(safe-area-inset-top) + 6px) 8px 6px;
    background: var(--rd-panel);
    border-bottom: 1px solid var(--rd-border);
    flex: 0 0 auto;
  }
  .rd-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
    font-size: 14px;
    text-align: center;
  }
  .rd-btn {
    background: transparent;
    color: var(--rd-fg);
    border: 1px solid var(--rd-border);
    border-radius: 8px;
    padding: 7px 12px;
    font-size: 13px;
    min-height: 38px;
    white-space: nowrap;
  }
  .rd-btn:hover { background: var(--rd-border); }
  .rd-btn.solid { background: var(--rd-accent); color: #fff; border-color: var(--rd-accent); }
  .rd-btn:disabled { opacity: 0.4; }
  .rd-scroll {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    -webkit-overflow-scrolling: touch;
    overscroll-behavior: contain;
    position: relative;
    user-select: text;
    -webkit-user-select: text;
    -webkit-touch-callout: default;
  }
  .rd-scroll.mode-paged {
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }
  .rd-scroll.mode-paged::-webkit-scrollbar { display: none; }
  .rd-content {
    margin: 0 auto;
    padding: 18px 18px calc(24px + env(safe-area-inset-bottom));
    user-select: text;
    -webkit-user-select: text;
  }
  .rd-content.paged {
    will-change: transform, opacity;
    backface-visibility: hidden;
  }
  .rd-content.paged .rd-meta,
  .rd-content.paged .rd-chapter-title,
  .rd-content.paged .rd-p,
  .rd-content.paged .rd-navrow {
    padding-left: 18px;
    padding-right: 18px;
  }
  .rd-content.paged .rd-meta { padding-top: 18px; }
  .rd-content.paged .rd-navrow { margin-bottom: 18px; }
  .flip-next { animation: rdFlipNext 0.32s ease; }
  .flip-prev { animation: rdFlipPrev 0.32s ease; }
  @keyframes rdFlipNext {
    0% { opacity: 0.4; transform: perspective(1100px) rotateY(7deg) translateX(16px); }
    100% { opacity: 1; transform: none; }
  }
  @keyframes rdFlipPrev {
    0% { opacity: 0.4; transform: perspective(1100px) rotateY(-7deg) translateX(-16px); }
    100% { opacity: 1; transform: none; }
  }
  .rd-meta {
    font-size: 12px;
    color: var(--rd-muted);
    margin-bottom: 14px;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  .dot { opacity: 0.6; }
  .replace-hint {
    font-size: 12px;
    color: var(--rd-muted);
    border: 1px solid var(--rd-border, #ccc);
    border-radius: 6px;
    padding: 4px 8px;
    margin: 0 0 14px;
  }
  .rd-chapter-title {
    font-size: 1.25em;
    line-height: 1.5;
    margin: 0 0 16px;
    font-weight: 600;
  }
  .rd-p {
    margin: 0 0 0.85em;
    text-indent: 2em;
    word-break: break-word;
  }
  /* 内嵌图片：桌面居中 + max-width；手机满宽（颜色交给服务端灰度转码） */
  .rd-img {
    display: block;
    margin: 0.8em 0;
    text-align: center;
    break-inside: avoid;
    page-break-inside: avoid;
  }
  .rd-img img {
    display: block;
    margin: 0 auto;
    max-width: 100%;
    height: auto;
    border-radius: 4px;
  }
  .rd-content.paged .rd-img {
    padding-left: 18px;
    padding-right: 18px;
  }
  .rd-img-ph {
    display: block;
    padding: 18px 12px;
    border: 1px dashed var(--rd-border);
    color: var(--rd-muted);
    font-size: 13px;
    text-align: center;
  }
  .rd-img-err {
    margin-top: 4px;
    font-size: 12px;
    word-break: break-all;
  }
  .rd-img-ph .rd-btn {
    margin-top: 10px;
  }
  :global(html[data-vp='sm']) .rd-img img {
    width: 100%;
  }
  .rd-navrow {
    display: flex;
    gap: 8px;
    margin-top: 28px;
    padding-top: 16px;
    border-top: 1px solid var(--rd-border);
  }
  .rd-navrow .rd-btn { flex: 1; }
  .rd-state {
    padding: 48px 24px;
    text-align: center;
    color: var(--rd-muted);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }
  .rd-state.slim { padding: 20px; }
  .rd-empty { padding: 28px 8px; text-align: center; }
  .rd-empty-title { font-size: 15px; font-weight: 600; margin: 0; }
  /* 长章节滚动：仅滚动模式启用渲染跳过，避免影响分页列宽测量 */
  .rd-scroll:not(.mode-paged) .rd-p {
    content-visibility: auto;
    contain-intrinsic-size: auto 1.9em;
  }
  .rd-error { color: #c0392b; font-size: 13px; margin: 0; }
  .theme-dark .rd-error { color: #e88; }
  .rd-muted { color: var(--rd-muted); }
  .spinner {
    width: 22px; height: 22px;
    border: 2px solid var(--rd-border);
    border-top-color: var(--rd-accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  /* 亮度遮罩：只调暗阅读区，不改系统亮度 */
  .dim-layer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    left: 0;
    background: #000;
    pointer-events: none;
    z-index: 21;
    transition: opacity 0.2s;
  }

  /* 分页底栏 / 自动滚动 / 提示 */
  /* 作为 flex 子项占位，避免浮层压住正文最后一行 */
  .rd-pager {
    position: relative;
    z-index: 23;
    flex: 0 0 auto;
    align-self: center;
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px auto 6px;
    margin-bottom: calc(6px + env(safe-area-inset-bottom));
    padding: 5px 8px;
    background: var(--rd-panel);
    border: 1px solid var(--rd-border);
    border-radius: 999px;
    box-shadow: 0 2px 10px rgba(0,0,0,0.14);
    font-size: 12px;
    color: var(--rd-muted);
  }
  .rd-pager .rd-btn { min-height: 32px; padding: 4px 10px; border: none; }
  .pager-num { min-width: 54px; text-align: center; font-variant-numeric: tabular-nums; }
  .auto-pill {
    position: fixed;
    right: 10px;
    bottom: calc(58px + env(safe-area-inset-bottom));
    z-index: 24;
    background: var(--rd-accent);
    color: #fff;
    border: none;
    border-radius: 999px;
    padding: 8px 14px;
    font-size: 12px;
    box-shadow: 0 2px 10px rgba(0,0,0,0.2);
  }
  .rd-toast {
    position: fixed;
    left: 50%;
    bottom: calc(110px + env(safe-area-inset-bottom));
    transform: translateX(-50%);
    z-index: 29;
    background: rgba(0,0,0,0.78);
    color: #fff;
    padding: 8px 14px;
    border-radius: 8px;
    font-size: 13px;
    max-width: 80vw;
    text-align: center;
  }

  .pay-notice {
    position: relative;
    z-index: 21;
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 0 auto;
    padding: 6px 10px;
    font-size: 13px;
    color: var(--rd-fg);
    background: var(--rd-panel);
    border-bottom: 1px solid var(--rd-border);
  }
  .pay-notice .pay-text { flex: 1; min-width: 0; }
  /* 提示条不在 .rd-bar 内，单独保证手机触控目标 44px */
  :global(html[data-vp='sm']) .pay-notice .rd-btn {
    min-height: 44px;
    min-width: 44px;
  }
  :global(html[data-theme='eink']) .pay-notice,
  :global(html[data-theme='eink']) .pay-notice .pay-text {
    color: #000;
    background: #fff;
    border-color: #333;
  }

  /* 底部设置面板 */
  .panel-mask { position: fixed; top: 0; right: 0; bottom: 0; left: 0; background: rgba(0,0,0,0.3); z-index: 28; display: flex; align-items: flex-end; }
  .panel {
    width: 100%;
    max-height: 86vh;
    overflow-y: auto;
    background: var(--rd-panel);
    border-top-left-radius: 16px;
    border-top-right-radius: 16px;
    padding: 14px 16px calc(18px + env(safe-area-inset-bottom));
    box-shadow: 0 -4px 18px rgba(0,0,0,0.18);
  }
  .panel-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; font-size: 14px; }
  .switch-panel { max-height: 72vh; display: flex; flex-direction: column; }
  .switch-list { overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
  .switch-item { display: flex; align-items: center; gap: 10px; border-top: 1px solid var(--rd-border); padding-top: 8px; }
  .switch-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .switch-main b { font-size: 14px; }
  .panel-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 9px 0; }
  .panel-row > span { font-size: 13px; color: var(--rd-muted); flex: 0 0 auto; }
  .stepper { display: flex; align-items: center; gap: 10px; }
  .stepper .val { min-width: 40px; text-align: center; font-variant-numeric: tabular-nums; }
  .slider-wrap { display: flex; align-items: center; gap: 10px; flex: 1; min-width: 0; }
  .slider-wrap input[type='range'] { flex: 1; min-width: 80px; accent-color: var(--rd-accent); padding: 0; }
  .seg.small { margin: 0; padding: 3px; border: 1px solid var(--rd-border); background: transparent; }
  .seg.small button { background: transparent; color: var(--rd-fg); padding: 4px 10px; min-height: 30px; font-size: 12px; border-radius: 6px; }
  .seg.small button.on { background: var(--rd-accent); color: #fff; }
  .small-note { margin: 10px 0 0; font-size: 12px; line-height: 1.6; }
  .err-actions, .jump-actions { display: flex; gap: 10px; justify-content: center; flex-wrap: wrap; margin-top: 10px; }
  .jump-panel { max-height: 80vh; overflow-y: auto; }
  .jump-num {
    width: 76px;
    padding: 6px 8px;
    border: 1px solid var(--rd-border);
    border-radius: 8px;
    background: var(--rd-panel);
    color: var(--rd-fg);
    font-size: 14px;
    font-variant-numeric: tabular-nums;
  }

  .row-actions { display: flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
</style>
