<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { api, describeFailure } from '../lib/api.js';

  export let book; // { bookUrl, sourceId, name, author, bookType }
  const dispatch = createEventDispatcher();

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
  let toast = '';
  let toastTimer = null;
  let saveTimer = null;
  let lastSaveAt = 0;
  let reqId = 0;
  // 已下载图片的 objectURL 缓存（最多 6 张，超出按插入顺序回收，控制内存）
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

  function imageErrorText(e) {
    if (e && e.code && CODE_MSG[e.code]) return CODE_MSG[e.code];
    return (e && e.message) || '图片加载失败';
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
      await showImage(target);
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

  // 预取下一张：只保留 1 张缓冲，KPW3 上也能明显减少翻页等待
  function prefetchNext() {
    const n = imgIndex + 1;
    if (n >= images.length) return;
    const src = images[n].url;
    if (blobCache.has(src)) return;
    const chapterUrl = currentChapter ? currentChapter.url : '';
    api
      .comicImage(book.sourceId || '', src, chapterUrl, book.bookUrl)
      .then((rec) => {
        if (blobCache.has(src) || images[n].url !== src) {
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

  function retryImage() {
    const src = images[imgIndex] && images[imgIndex].url;
    if (src) blobCache.delete(src);
    showImage(imgIndex);
  }

  function nextPage() {
    if (chapterLoading) return;
    if (imgIndex + 1 < images.length) showImage(imgIndex + 1);
    else goChapter(1, 'first');
  }

  function prevPage() {
    if (chapterLoading) return;
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
      await loadChapter(start, restore || 'first');
    } catch (e) {
      tocError = describeFailure(e, '读取目录');
    } finally {
      tocLoading = false;
    }
  }

  onMount(() => {
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
  </div>

  <div class="cm-stage">
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
      <img class="cm-img" src={currentImg.objectUrl} alt={chapterTitle || '漫画页'} />
    {:else}
      <div class="cm-state">暂无图片</div>
    {/if}

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
  </div>

  <div class="cm-bar">
    <span class="cm-count">{pageText}</span>
    <button class="cm-btn" disabled={noPrevChapter} on:click={() => goChapter(-1, 'last')}>上一章</button>
    <button class="cm-btn" disabled={noNextChapter} on:click={() => goChapter(1, 'first')}>下一章</button>
    <button class="cm-btn" on:click={() => (showToc = !showToc)}>目录</button>
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
    max-width: 78%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    vertical-align: top;
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
</style>
