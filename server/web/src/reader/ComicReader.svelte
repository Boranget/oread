<script>
  import { createEventDispatcher, onMount, onDestroy, tick } from 'svelte';
  import { api, describeFailure, getRole } from '../lib/api.js';
  import BookEditor from '../lib/BookEditor.svelte';

  export let book; // { bookUrl, sourceId, name, author, bookType }
  const dispatch = createEventDispatcher();
  const isAdmin = getRole() === 'admin';

  const clamp = (v, min, max) => Math.min(max, Math.max(min, v));

  let chapters = [];
  let currentPos = -1;
  let tocLoading = true;
  let tocError = '';
  let images = [];
  let imgIndex = 0;
  let chapterLoading = false;
  let chapterError = '';
  let imageLoading = false;
  let imageError = '';
  let currentImg = null;
  let showToc = false;
  let showEditor = false;
  let editorSaved = false;
  let toast = '';
  let toastTimer = null;
  let saveTimer = null;
  let lastSaveAt = 0;
  let reqId = 0;
  // 阅读模式：单页 / 连续滚动；缩放用 CSS 宽度（不用 transform，兼容老内核）
  let viewMode = 'page';
  let zoom = 1;
  let stageEl = null;
  let scrollRaf = 0;
  // 连续模式的图片状态：src → { status, objectUrl, error }
  let pageState = {};
  const MODE_KEY = 'boran_comic_mode';
  // 已下载图片的 objectURL 缓存（单页模式最多 6 张，超出按插入顺序回收，控制内存）
  const blobCache = new Map();

  const seed = {
    name: book.name || '',
    author: book.author || '',
    intro: book.intro || '',
    coverUrl: book.coverUrl || '',
  };

  $: currentChapter = currentPos >= 0 ? chapters[currentPos] || null : null;
  $: chapterTitle = currentChapter ? currentChapter.title || currentChapter.url : '';
  $: pageText = images.length ? `第 ${imgIndex + 1}/${images.length} 张` : '—';
  $: noPrevChapter = currentPos < 0 || nextReadable(currentPos, -1) < 0;
  $: noNextChapter = currentPos < 0 || nextReadable(currentPos, 1) < 0;

  const CODE_MSG = {
    upstream_not_found: '源站 404：这张图不存在或已失效',
    not_an_image: '源站返回了网页/非图片内容，已拦截（避免把 HTML 错误页当图片）',
    image_too_large: '图片超过单图大小上限，已拒绝',
    upstream_forbidden: '源站拒绝访问（防盗链/需要 Referer 或登录）',
    unsupported_image_format: '源站返回 WebP，墨水屏无法显示',
    upstream_timeout: '图片回源超时，请重试这张图',
    private_address: '图片地址指向内网，已拦截',
    invalid_url: '图片地址无效',
  };

  // 当前页在书目录里的相对路径（`oread-img://<book_id>/<href>` 去掉前缀）。
  function currentPageHref() {
    const url = (images[imgIndex] && images[imgIndex].url) || '';
    const m = /^oread-img:\/\/[^/]+\/(.+)$/.exec(url);
    return m ? m[1] : '';
  }

  function imageErrorText(e) {
    if (e && e.code && CODE_MSG[e.code]) return CODE_MSG[e.code];
    return (e && e.message) || '图片加载失败';
  }

  // ---------- 连续滚动 / 缩放 ----------

  function setViewMode(mode) {
    if (mode === viewMode) return;
    viewMode = mode;
    try {
      localStorage.setItem(MODE_KEY, mode);
    } catch {
      /* 隐私模式忽略 */
    }
    if (mode === 'scroll') {
      tick().then(() => {
        ensureWindow(imgIndex);
        scrollToPage(imgIndex);
      });
    } else if (images.length) {
      // 单页模式复用连续模式已下载的当前页，避免重复回源。
      const src = images[imgIndex] && images[imgIndex].url;
      const rec = src && pageState[src];
      if (src && rec && rec.status === 'ok') {
        putBlob(src, rec);
        const next = { ...pageState };
        delete next[src];
        pageState = next;
      }
      clearPageState();
      showImage(imgIndex);
    }
  }

  function zoomBy(delta) {
    zoom = clamp(Math.round((zoom + delta) * 2) / 2, 1, 3);
  }

  function clearPageState() {
    for (const rec of Object.values(pageState)) {
      if (rec && rec.objectUrl) {
        try {
          URL.revokeObjectURL(rec.objectUrl);
        } catch {
          /* ignore */
        }
      }
    }
    pageState = {};
  }

  function loadScrollPage(i) {
    if (i < 0 || i >= images.length) return;
    const src = images[i].url;
    const st = pageState[src];
    if (st && (st.status === 'loading' || st.status === 'ok')) return;
    const chapterUrl = currentChapter ? currentChapter.url : '';
    pageState = { ...pageState, [src]: { status: 'loading' } };
    api
      .comicImage(book.sourceId || '', src, chapterUrl, book.bookUrl)
      .then((rec) => {
        if (!images.some((im) => im.url === src)) {
          try {
            URL.revokeObjectURL(rec.objectUrl);
          } catch {
            /* ignore */
          }
          return;
        }
        pageState = {
          ...pageState,
          [src]: { status: 'ok', objectUrl: rec.objectUrl, width: rec.width, height: rec.height },
        };
      })
      .catch((e) => {
        pageState = { ...pageState, [src]: { status: 'error', error: imageErrorText(e) } };
      });
  }

  function retryScrollPage(i) {
    if (i < 0 || i >= images.length) return;
    const src = images[i].url;
    const next = { ...pageState };
    delete next[src];
    pageState = next;
    loadScrollPage(i);
  }

  // 连续模式只保留可视页附近若干张，翻远后回收 objectURL，翻回时按需重取。
  function ensureWindow(center) {
    if (viewMode !== 'scroll') return;
    const from = Math.max(0, center - 1);
    const to = Math.min(images.length - 1, center + 2);
    for (let i = from; i <= to; i++) loadScrollPage(i);
    evictScrollPages(center);
  }

  function evictScrollPages(center) {
    const keepFrom = Math.max(0, center - 3);
    const keepTo = Math.min(images.length - 1, center + 3);
    const keep = new Set(images.slice(keepFrom, keepTo + 1).map((im) => im.url));
    let changed = false;
    const next = {};
    for (const [src, rec] of Object.entries(pageState)) {
      if (keep.has(src) || rec.status !== 'ok') {
        next[src] = rec;
        continue;
      }
      try {
        URL.revokeObjectURL(rec.objectUrl);
      } catch {
        /* ignore */
      }
      changed = true;
    }
    if (changed) pageState = next;
  }

  // 连续模式下的键盘翻页：按页定位而不是走单页加载。
  function scrollPageBy(delta) {
    if (!images.length) return;
    const next = imgIndex + delta;
    if (next < 0) {
      goChapter(-1, 'last');
      return;
    }
    if (next >= images.length) {
      goChapter(1, 'first');
      return;
    }
    imgIndex = next;
    ensureWindow(next);
    scrollToPage(next);
    saveProgress();
  }

  function pageBoxStyle(im) {
    const w = Number(im && im.width) || 0;
    const h = Number(im && im.height) || 0;
    const pct = w > 0 && h > 0 ? (h / w) * 100 : 140;
    return `padding-bottom:${clamp(pct, 20, 400)}%`;
  }

  function scrollToPage(i) {
    if (!stageEl) return;
    const el = stageEl.querySelector(`[data-page="${i}"]`);
    if (el) stageEl.scrollTop = el.offsetTop;
  }

  function onStageScroll() {
    if (viewMode !== 'scroll' || !stageEl || scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = 0;
      if (!stageEl || viewMode !== 'scroll') return;
      const top = stageEl.scrollTop + 40;
      let best = 0;
      const nodes = stageEl.querySelectorAll('[data-page]');
      for (const el of nodes) {
        if (el.offsetTop <= top) best = Number(el.dataset.page) || 0;
      }
      if (best !== imgIndex) {
        imgIndex = best;
        saveProgress();
      }
      ensureWindow(best);
    });
  }

  function nextReadable(from, dir) {
    for (let i = from + dir; i >= 0 && i < chapters.length; i += dir) {
      if (!chapters[i].is_volume) return i;
    }
    return -1;
  }

  function showToast(msg) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ''), 1800);
  }

  function putBlob(src, rec) {
    blobCache.set(src, rec);
    while (blobCache.size > 6) {
      const first = blobCache.keys().next().value;
      if (first === src) break;
      const old = blobCache.get(first);
      blobCache.delete(first);
      try {
        URL.revokeObjectURL(old.objectUrl);
      } catch {
        /* ignore */
      }
    }
  }

  function imageRatio() {
    if (images.length <= 1) return 0;
    return imgIndex / (images.length - 1);
  }

  function saveProgress(force = false, keepalive = false) {
    if (!book.bookUrl || currentPos < 0) return;
    const now = Date.now();
    if (!force && now - lastSaveAt < 30000) return;
    lastSaveAt = now;
    api
      .setProgress(
        {
          book_id: book.bookUrl,
          chapter_index: currentPos,
          position: imageRatio(),
          total_chapters: chapters.length,
        },
        keepalive
      )
      .catch(() => {});
  }

  async function loadChapter(pos, mode) {
    if (pos < 0 || pos >= chapters.length) return;
    if (chapters[pos].is_volume) {
      pos = nextReadable(pos, 1);
      if (pos < 0) return;
    }
    const my = ++reqId;
    currentPos = pos;
    chapterLoading = true;
    chapterError = '';
    imageError = '';
    imageLoading = false;
    currentImg = null;
    clearPageState();
    zoom = 1;
    images = [];
    imgIndex = 0;
    showToc = false;
    try {
      const res = await api.comicChapter(
        book.sourceId || '',
        book.bookUrl,
        chapters[pos].url,
        pos + 1
      );
      if (my !== reqId) return;
      const list = (res && res.images) || [];
      if (!list.length) {
        chapterError = res && res.placeholder_suspected
          ? '本章疑似付费/会员占位图，未解析到可用图片。'
          : '本章没有解析到图片（目录规则或正文规则可能失效）。';
        return;
      }
      images = list;
      let target = 0;
      if (mode === 'last') target = images.length - 1;
      else if (mode && typeof mode.ratio === 'number') {
        target = Math.round(clamp(mode.ratio, 0, 1) * (images.length - 1));
      }
      if (res && res.truncated) showToast(`本章图片超过上限，仅显示前 ${images.length} 张`);
      if (viewMode === 'scroll') {
        imgIndex = target;
        // 先结束 loading 分支，页表才会渲染出来，tick 后才能定位。
        chapterLoading = false;
        await tick();
        scrollToPage(target);
        ensureWindow(target);
      } else {
        await showImage(target);
      }
      saveProgress(true);
    } catch (e) {
      if (my !== reqId) return;
      chapterError = describeFailure(e, '加载漫画章节');
    } finally {
      if (my === reqId) chapterLoading = false;
    }
  }

  async function showImage(i) {
    if (i < 0 || i >= images.length) return;
    imgIndex = i;
    imageError = '';
    const src = images[i].url;
    const cached = blobCache.get(src);
    if (cached) {
      currentImg = { ...cached, sourceUrl: src };
      imageLoading = false;
      saveProgress();
      prefetchNext();
      return;
    }
    const my = reqId;
    imageLoading = true;
    currentImg = null;
    try {
      const rec = await api.comicImage(
        book.sourceId || '',
        src,
        currentChapter ? currentChapter.url : '',
        book.bookUrl
      );
      if (my !== reqId) {
        try {
          URL.revokeObjectURL(rec.objectUrl);
        } catch {
          /* ignore */
        }
        return;
      }
      putBlob(src, rec);
      currentImg = { ...rec, sourceUrl: src };
    } catch (e) {
      if (my !== reqId) return;
      imageError = imageErrorText(e);
    } finally {
      if (my === reqId) imageLoading = false;
    }
    saveProgress();
    prefetchNext();
  }

  // 预取后面两张：单页模式翻页更顺，内存仍只保留少量 objectURL
  function prefetchNext() {
    if (viewMode !== 'page') return;
    const chapterUrl = currentChapter ? currentChapter.url : '';
    for (let n = imgIndex + 1; n <= Math.min(images.length - 1, imgIndex + 2); n++) {
      const src = images[n].url;
      if (blobCache.has(src)) continue;
      api
        .comicImage(book.sourceId || '', src, chapterUrl, book.bookUrl)
        .then((rec) => {
          if (blobCache.has(src) || !images.some((im) => im.url === src)) {
            try {
              URL.revokeObjectURL(rec.objectUrl);
            } catch {
              /* ignore */
            }
            return;
          }
          putBlob(src, rec);
        })
        .catch(() => {});
    }
  }

  function retryImage() {
    const src = images[imgIndex] && images[imgIndex].url;
    if (src) blobCache.delete(src);
    showImage(imgIndex);
  }

  function nextPage() {
    if (chapterLoading) return;
    if (viewMode === 'scroll') {
      scrollPageBy(1);
      return;
    }
    if (imgIndex + 1 < images.length) showImage(imgIndex + 1);
    else goChapter(1, 'first');
  }

  function prevPage() {
    if (chapterLoading) return;
    if (viewMode === 'scroll') {
      scrollPageBy(-1);
      return;
    }
    if (imgIndex > 0) showImage(imgIndex - 1);
    else goChapter(-1, 'last');
  }

  function goChapter(dir, mode) {
    const p = nextReadable(currentPos, dir);
    if (p < 0) {
      showToast(dir > 0 ? '已经是最后一章' : '已经是第一章');
      return;
    }
    loadChapter(p, mode);
  }

  function jumpChapter(pos) {
    if (pos !== currentPos) loadChapter(pos, 'first');
    else showToc = false;
  }

  function onKeydown(e) {
    if (e.key === 'ArrowRight' || e.key === 'PageDown' || e.key === ' ') {
      e.preventDefault();
      nextPage();
    } else if (e.key === 'ArrowLeft' || e.key === 'PageUp') {
      e.preventDefault();
      prevPage();
    } else if (e.key === 'Escape') {
      if (showToc) showToc = false;
      else close();
    }
  }

  function onPageHide() {
    saveProgress(true, true);
  }

  function close() {
    saveProgress(true, true);
    dispatch('close');
  }

  async function loadToc() {
    tocLoading = true;
    tocError = '';
    try {
      const list = await api.toc(book.sourceId || '', book.bookUrl, seed);
      chapters = Array.isArray(list) ? list : [];
      if (!chapters.length) {
        tocError = '目录为空：书源目录规则可能失效，请换源或稍后重试。';
        return;
      }
      let start = 0;
      let restore = null;
      try {
        const prog = await api.progress(book.bookUrl);
        if (prog && Number.isFinite(Number(prog.chapter_index))) {
          start = clamp(Number(prog.chapter_index), 0, chapters.length - 1);
        }
        if (prog && Number(prog.position) > 0) restore = { ratio: Number(prog.position) };
      } catch {
        /* 进度读取失败不影响阅读 */
      }
      if (chapters[start] && chapters[start].is_volume) {
        const p = nextReadable(start, 1);
        if (p >= 0) start = p;
      }
      // 先退出“加载目录”分支，章节页才能渲染并定位到进度页。
      tocLoading = false;
      await loadChapter(start, restore || 'first');
    } catch (e) {
      tocError = describeFailure(e, '读取目录');
    } finally {
      tocLoading = false;
    }
  }

  onMount(() => {
    try {
      const saved = localStorage.getItem(MODE_KEY);
      if (saved === 'scroll' || saved === 'page') viewMode = saved;
    } catch {
      /* 隐私模式忽略 */
    }
    loadToc();
    window.addEventListener('keydown', onKeydown);
    window.addEventListener('pagehide', onPageHide);
    return () => {
      window.removeEventListener('keydown', onKeydown);
      window.removeEventListener('pagehide', onPageHide);
    };
  });

  onDestroy(() => {
    clearTimeout(toastTimer);
    clearTimeout(saveTimer);
    if (scrollRaf) cancelAnimationFrame(scrollRaf);
    clearPageState();
    for (const rec of blobCache.values()) {
      try {
        URL.revokeObjectURL(rec.objectUrl);
      } catch {
        /* ignore */
      }
    }
    blobCache.clear();
  });
</script>

<div class="cm-root">
  <div class="cm-top">
    <span class="cm-back" role="button" tabindex="0" on:click={close} on:keydown={(e) => e.key === 'Enter' && close()}>
      ‹ 返回
    </span>
    <span class="cm-title">{book.name || '漫画'} · {chapterTitle}</span>
    <button class="cm-mode" on:click={() => setViewMode(viewMode === 'page' ? 'scroll' : 'page')}>
      {viewMode === 'page' ? '连续' : '单页'}
    </button>
  </div>

  <div
    class="cm-stage"
    class:scroll-mode={viewMode === 'scroll'}
    bind:this={stageEl}
    on:scroll={onStageScroll}
  >
    {#if tocLoading}
      <div class="cm-state">加载目录…</div>
    {:else if tocError}
      <div class="cm-state">
        <div>{tocError}</div>
        <button class="cm-btn" on:click={loadToc}>重试目录</button>
      </div>
    {:else if chapterLoading}
      <div class="cm-state">加载章节…</div>
    {:else if chapterError}
      <div class="cm-state">
        <div>{chapterError}</div>
        <button class="cm-btn" on:click={() => loadChapter(currentPos, 'first')}>重试本章</button>
      </div>
    {:else if viewMode === 'scroll'}
      <div class="cm-pages">
        {#each images as im, i}
          <div class="cm-page" data-page={i} style={`width:${zoom * 100}%`}>
            {#if pageState[im.url]?.status === 'ok'}
              <img
                class="cm-scroll-img"
                src={pageState[im.url].objectUrl}
                alt={`${chapterTitle || '漫画页'} ${i + 1}`}
                style={zoom > 1 ? 'max-width:none;' : ''}
              />
            {:else if pageState[im.url]?.status === 'error'}
              <button class="cm-page-retry" on:click={() => retryScrollPage(i)}>
                第 {i + 1} 张加载失败 · 重试
              </button>
            {:else}
              <div class="cm-page-ph" style={pageBoxStyle(im)}>
                <span>第 {i + 1} 张 · 加载中…</span>
              </div>
            {/if}
          </div>
        {/each}
        <div class="cm-end">
          <button class="cm-btn" disabled={noNextChapter} on:click={() => goChapter(1, 'first')}>
            下一章
          </button>
        </div>
      </div>
    {:else if imageLoading}
      <div class="cm-state">加载第 {imgIndex + 1}/{images.length} 张…</div>
    {:else if imageError}
      <div class="cm-state">
        <div>{imageError}</div>
        <div class="cm-hint">第 {imgIndex + 1}/{images.length} 张</div>
        <button class="cm-btn" on:click={retryImage}>重试这张</button>
        <button class="cm-btn" on:click={nextPage}>跳过这张</button>
      </div>
    {:else if currentImg}
      <img
        class="cm-img"
        class:zoomed={zoom > 1}
        style={`width:${zoom * 100}%;${zoom > 1 ? 'max-width:none;' : ''}`}
        src={currentImg.objectUrl}
        alt={chapterTitle || '漫画页'}
      />
    {:else}
      <div class="cm-state">暂无图片</div>
    {/if}

    {#if viewMode === 'page'}
      <div
        class="cm-hit cm-hit-prev"
        role="button"
        tabindex="-1"
        aria-label="上一张"
        on:click={prevPage}
      ></div>
      <div
        class="cm-hit cm-hit-next"
        role="button"
        tabindex="-1"
        aria-label="下一张"
        on:click={nextPage}
      ></div>
    {/if}
  </div>

  {#if images.length && !tocLoading && !chapterLoading && !chapterError}
    <div class="cm-zoom">
      <button class="cm-zbtn" disabled={zoom <= 1} on:click={() => zoomBy(-0.5)}>−</button>
      <span class="cm-zval">{Math.round(zoom * 100)}%</span>
      <button class="cm-zbtn" disabled={zoom >= 3} on:click={() => zoomBy(0.5)}>＋</button>
    </div>
  {/if}

  <div class="cm-bar">
    <span class="cm-count">{pageText}</span>
    <button class="cm-btn" disabled={noPrevChapter} on:click={() => goChapter(-1, 'last')}>上一章</button>
    <button class="cm-btn" disabled={noNextChapter} on:click={() => goChapter(1, 'first')}>下一章</button>
    <button class="cm-btn" on:click={() => (showToc = !showToc)}>目录</button>
    {#if isAdmin && currentChapter}
      <button
        class="cm-btn"
        on:click={() => {
          editorSaved = false;
          showEditor = true;
        }}>编辑</button
      >
    {/if}
    <button class="cm-btn" on:click={close}>退出</button>
  </div>

  {#if showToc}
    <div class="cm-toc">
      <div class="cm-toc-head">
        <span class="cm-toc-title">{book.name || '目录'}</span>
        <button class="cm-btn" on:click={() => (showToc = false)}>关闭</button>
      </div>
      <div class="cm-toc-list">
        {#each chapters as c, i}
          <div
            class="cm-toc-row"
            class:active={i === currentPos}
            class:volume={c.is_volume}
            role="button"
            tabindex="0"
            on:click={() => !c.is_volume && jumpChapter(i)}
            on:keydown={(e) => e.key === 'Enter' && !c.is_volume && jumpChapter(i)}
          >
            {i + 1}. {c.title || c.url}
          </div>
        {/each}
      </div>
    </div>
  {/if}

  {#if toast}
    <div class="cm-toast">{toast}</div>
  {/if}

  {#if showEditor && currentChapter}
    <BookEditor
      bookUrl={book.bookUrl}
      initialTab="content"
      initialChapter={currentChapter.url}
      initialPage={currentPageHref()}
      on:close={() => {
        showEditor = false;
        if (editorSaved && currentPos >= 0) {
          // 保持当前阅读位置：按比例回到原页（换页后页数不变）。
          const ratio = images.length > 1 ? imgIndex / (images.length - 1) : 0;
          loadChapter(currentPos, { ratio });
        }
      }}
      on:saved={() => (editorSaved = true)}
    />
  {/if}
</div>

<style>
  /* 基础布局只用块级 / inline-block / float / position:fixed，
     全程不出现 flex/grid/transform/transition/animation/calc/vw/vh，
     因此 KPW3 墨水屏不会因声明被静默丢弃而错位。 */
  .cm-root {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    left: 0;
    background: #111;
    color: #eee;
  }
  .cm-top {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 36px;
    line-height: 36px;
    padding: 0 10px;
    background: rgba(0, 0, 0, 0.66);
    z-index: 3;
    white-space: nowrap;
    overflow: hidden;
  }
  .cm-back {
    display: inline-block;
    margin-right: 10px;
    padding: 0 6px;
    cursor: pointer;
  }
  .cm-title {
    display: inline-block;
    font-size: 13px;
    color: #ddd;
    max-width: 62%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    vertical-align: top;
  }
  .cm-mode {
    position: absolute;
    right: 8px;
    top: 6px;
    height: 24px;
    line-height: 22px;
    padding: 0 8px;
    font-size: 12px;
    border: 1px solid #555;
    border-radius: 4px;
    background: #333;
    color: #eee;
    cursor: pointer;
  }
  .cm-stage {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 48px;
    left: 0;
    overflow: auto;
    text-align: center;
    z-index: 1;
  }
  .cm-img {
    display: block;
    width: 100%;
    height: auto;
    margin: 0 auto;
  }
  .cm-img.zoomed {
    max-width: none;
  }
  .cm-stage.scroll-mode {
    text-align: left;
  }
  .cm-pages {
    padding: 40px 0 8px;
  }
  .cm-page {
    display: block;
    margin: 0 auto;
  }
  .cm-scroll-img {
    display: block;
    width: 100%;
    height: auto;
  }
  .cm-page-ph {
    position: relative;
    display: block;
    width: 100%;
    background: #1c1c1c;
    border-bottom: 1px solid #333;
  }
  .cm-page-ph span {
    position: absolute;
    top: 50%;
    left: 0;
    right: 0;
    margin-top: -9px;
    text-align: center;
    color: #888;
    font-size: 13px;
  }
  .cm-page-retry {
    display: block;
    width: 100%;
    padding: 48px 12px;
    background: #222;
    color: #e88;
    border: 0;
    border-bottom: 1px solid #333;
    font-size: 13px;
    cursor: pointer;
  }
  .cm-end {
    padding: 16px 0 24px;
    text-align: center;
  }
  .cm-zoom {
    position: fixed;
    right: 8px;
    bottom: 56px;
    z-index: 3;
    background: rgba(0, 0, 0, 0.66);
    border: 1px solid #555;
    border-radius: 6px;
    padding: 2px 4px;
    white-space: nowrap;
  }
  .cm-zbtn {
    display: inline-block;
    width: 30px;
    height: 30px;
    line-height: 28px;
    padding: 0;
    font-size: 16px;
    border: 1px solid #555;
    border-radius: 4px;
    background: #333;
    color: #eee;
    cursor: pointer;
    vertical-align: middle;
  }
  .cm-zbtn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .cm-zval {
    display: inline-block;
    min-width: 42px;
    text-align: center;
    font-size: 12px;
    color: #ddd;
    vertical-align: middle;
  }
  .cm-state {
    display: block;
    padding: 80px 20px 20px;
    text-align: center;
    color: #ccc;
    font-size: 14px;
    line-height: 1.7;
  }
  .cm-hint {
    margin: 6px 0 12px;
    color: #999;
    font-size: 13px;
  }
  .cm-hit {
    position: absolute;
    top: 0;
    bottom: 0;
    z-index: 2;
    cursor: pointer;
  }
  .cm-hit-prev {
    left: 0;
    width: 50%;
  }
  .cm-hit-next {
    left: 50%;
    right: 0;
  }
  .cm-bar {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    height: 48px;
    line-height: 48px;
    padding: 0 4px;
    background: rgba(0, 0, 0, 0.8);
    border-top: 1px solid #444;
    text-align: center;
    white-space: nowrap;
    overflow: hidden;
    z-index: 3;
  }
  .cm-count {
    display: inline-block;
    margin: 0 6px;
    font-size: 13px;
    color: #ddd;
    vertical-align: top;
    line-height: 48px;
  }
  .cm-btn {
    display: inline-block;
    margin: 8px 3px;
    padding: 0 8px;
    height: 32px;
    line-height: 30px;
    font-size: 13px;
    border: 1px solid #555;
    border-radius: 4px;
    background: #333;
    color: #eee;
    cursor: pointer;
    vertical-align: top;
  }
  .cm-btn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .cm-toc {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    left: 0;
    background: #fff;
    color: #111;
    z-index: 4;
    overflow: auto;
  }
  .cm-toc-head {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 44px;
    line-height: 44px;
    padding: 0 10px;
    background: #fff;
    border-bottom: 1px solid #bbb;
    z-index: 5;
    white-space: nowrap;
  }
  .cm-toc-title {
    display: inline-block;
    width: 70%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    vertical-align: top;
  }
  .cm-toc-list {
    padding-top: 48px;
    padding-bottom: 12px;
  }
  .cm-toc-row {
    display: block;
    padding: 10px 12px;
    border-bottom: 1px solid #ddd;
    font-size: 14px;
    cursor: pointer;
  }
  .cm-toc-row.active {
    background: #111;
    color: #fff;
  }
  .cm-toc-row.volume {
    color: #777;
  }
  .cm-toast {
    position: fixed;
    left: 18%;
    right: 18%;
    bottom: 64px;
    padding: 6px 8px;
    background: #000;
    color: #fff;
    border: 1px solid #666;
    text-align: center;
    font-size: 13px;
    z-index: 5;
  }

  /* 桌面：图片限宽居中；点击热区按 30% / 70% 分（更符合鼠标习惯） */
  :global(html[data-vp='lg']) .cm-img {
    max-width: 960px;
  }
  :global(html[data-vp='lg']) .cm-scroll-img {
    max-width: 960px;
    margin: 0 auto;
  }
  :global(html[data-vp='lg']) .cm-hit-prev {
    width: 30%;
  }
  :global(html[data-vp='lg']) .cm-hit-next {
    left: 30%;
  }

  /* 墨水屏主题：纯黑白（只改配色，不动布局） */
  :global(html[data-theme='eink']) .cm-root {
    background: #fff;
    color: #000;
  }
  :global(html[data-theme='eink']) .cm-top,
  :global(html[data-theme='eink']) .cm-bar,
  :global(html[data-theme='eink']) .cm-toast {
    background: #fff;
    color: #000;
    border-color: #333;
  }
  :global(html[data-theme='eink']) .cm-title,
  :global(html[data-theme='eink']) .cm-count,
  :global(html[data-theme='eink']) .cm-state,
  :global(html[data-theme='eink']) .cm-hint {
    color: #000;
  }
  :global(html[data-theme='eink']) .cm-mode,
  :global(html[data-theme='eink']) .cm-zbtn {
    background: #fff;
    color: #000;
    border-color: #333;
  }
  :global(html[data-theme='eink']) .cm-zoom {
    background: #fff;
    border-color: #333;
  }
  :global(html[data-theme='eink']) .cm-zval {
    color: #000;
  }
  :global(html[data-theme='eink']) .cm-page-ph,
  :global(html[data-theme='eink']) .cm-page-retry {
    background: #fff;
    color: #000;
    border-color: #999;
  }
  :global(html[data-theme='eink']) .cm-page-ph span {
    color: #333;
  }
</style>
