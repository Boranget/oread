<script>
  import { onDestroy } from 'svelte';

  export let tocLoading = true;
  export let tocError = '';
  export let current = null;
  export let currentPos = -1;
  export let chapters = [];
  export let info = null;
  export let isPaged = false;
  export let pageIndex = 0;
  export let pageCount = 1;
  export let contentStyle = '';
  export let flipDir = 0;
  export let displayTitle = '';
  export let displayParas = [];
  export let contentLoading = false;
  export let contentError = '';
  export let chapterEmpty = false;
  export let emptyTitle = '';
  export let emptyHint = '';
  export let imgState = {};
  export let prevPos = -1;
  export let nextPos = -1;
  export let controlsVisible = true;
  export let dimOpacity = 0;
  export let autoOn = false;
  export let autoSpeed = 3;
  export let toast = '';

  export let bindScrollEl = () => {};
  export let onScroll = () => {};
  export let onReaderClick = () => {};
  export let onTouchStart = () => {};
  export let onTouchEnd = () => {};
  export let onTouchCancel = () => {};
  export let onWheel = () => {};
  export let loadAll = () => {};
  export let retryChapter = () => {};
  export let openChapter = () => {};
  export let currentRatio = () => 0;
  export let retryImage = () => {};
  export let onImgError = () => {};
  export let toggleSwitch = () => {};
  export let toggleToc = () => {};
  export let stopAuto = () => {};
  export let prevPage = () => {};
  export let nextPage = () => {};

  let el = null;
  // 只在滚动容器真正变化时回传。bindScrollEl 是父组件传入的函数 prop，
  // 函数 prop 在 Svelte 里每次 $set 都会被判定为“已变化”，若这里无条件调用，
  // 会形成 子更新→回传 el→父更新→重新 $set→子更新 的死循环，导致整页调度器卡死。
  let boundEl = null;
  $: if (el && el !== boundEl) {
    boundEl = el;
    bindScrollEl(el);
  }

  // 慢链路提示：加载超过 8s 仍未完成时明确告诉用户“还在等”，
  // 避免把域名/Funnel 抖动误认为阅读器卡死。计时器只用于文案，不参与请求控制。
  let slow = false;
  let slowTimer = null;
  $: loading = tocLoading || contentLoading;
  $: if (loading) {
    if (!slowTimer && typeof setTimeout !== 'undefined') {
      slowTimer = setTimeout(() => {
        slow = true;
      }, 8000);
    }
  } else {
    if (slowTimer) {
      clearTimeout(slowTimer);
      slowTimer = null;
    }
    slow = false;
  }
  onDestroy(() => {
    if (slowTimer) clearTimeout(slowTimer);
  });
</script>

  <div
    class="rd-scroll"
    class:mode-paged={isPaged}
    bind:this={el}
    on:scroll={onScroll}
    on:click={onReaderClick}
    on:touchstart={onTouchStart}
    on:touchend={onTouchEnd}
    on:touchcancel={onTouchCancel}
    on:wheel={onWheel}
  >
    {#if tocLoading}
      <div class="rd-state">
        <div class="spinner"></div>
        <p>正在打开书籍…</p>
        <p class="rd-muted">首次加载目录可能需要几秒</p>
        {#if slow}<p class="rd-muted">网络较慢，仍在等待书源响应…</p>{/if}
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
            <div class="rd-state">
              加载正文…
              {#if slow}<p class="rd-muted">网络较慢，仍在等待书源响应…</p>{/if}
            </div>
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


<style>
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
  :global(.theme-dark) .rd-error { color: #e88; }
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

  .err-actions { display: flex; gap: 10px; justify-content: center; flex-wrap: wrap; margin-top: 10px; }
</style>
