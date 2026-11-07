<script>
  import { createEventDispatcher } from 'svelte';
  import { api, describeFailure } from '../lib/api.js';

  export let bookUrl = '';
  export let show = false;

  const dispatch = createEventDispatcher();

  let searchQ = '';
  let searchLoading = false;
  let searchErr = '';
  let searchRes = null;

  $: searchMatchTotal = searchRes && Array.isArray(searchRes.hits)
    ? searchRes.hits.reduce((n, h) => n + (Number(h.match_count) || 0), 0)
    : 0;

  async function doBookSearch() {
    const q = searchQ.trim();
    if (!q) {
      searchErr = '请输入关键词';
      searchRes = null;
      return;
    }
    if (!bookUrl) {
      searchErr = '缺少书籍地址，无法搜索。请从书架重新打开这本书。';
      return;
    }
    searchLoading = true;
    searchErr = '';
    searchRes = null;
    try {
      searchRes = await api.bookSearch(bookUrl, q, 20);
    } catch (e) {
      searchErr = describeFailure(e, '书内搜索');
    } finally {
      searchLoading = false;
    }
  }

  // 片段来自正文原文，可能夹带 HTML；展示前只保留可读文本
  function snippetText(s) {
    return String(s || '')
      .replace(/<[^>]*>/g, ' ')
      .replace(/&nbsp;/g, ' ')
      .replace(/&lt;/g, '<')
      .replace(/&gt;/g, '>')
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, '&')
      .replace(/\s+/g, ' ')
      .trim();
  }

  function gotoHit(h) {
    dispatch('goto', {
      chapter_index: Number(h.chapter_index) || 0,
      query: (searchRes && searchRes.query) || searchQ,
    });
  }

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="panel-mask" on:click|self={close}>
    <div class="panel search-panel" role="dialog" aria-label="书内搜索">
      <div class="panel-head">
        <strong>书内搜索</strong>
        <button class="rd-btn" on:click={close}>关闭</button>
      </div>
      <div class="search-box">
        <input
          placeholder="输入关键词（仅检索已缓存章节）"
          aria-label="书内搜索关键词"
          bind:value={searchQ}
          on:keydown={(e) => e.key === 'Enter' && !searchLoading && doBookSearch()}
        />
        <button class="rd-btn solid" on:click={doBookSearch} disabled={searchLoading}>
          {searchLoading ? '搜索中…' : '搜索'}
        </button>
      </div>
      <p class="rd-muted small-note">只检索本机已缓存的正文；未缓存章节不会出现在结果里。</p>
      {#if searchErr}
        <p class="rd-error">{searchErr}</p>
      {:else if searchLoading}
        <p class="rd-muted">正在搜索已缓存章节…</p>
      {:else if searchRes}
        <div class="search-summary">
          <strong>命中 {searchMatchTotal} 处，共 {(searchRes.hits && searchRes.hits.length) || 0} 章</strong>
          <span class="rd-muted">
            已缓存 {searchRes.cached_chapters} / {searchRes.total_chapters} 章 · 耗时 {searchRes.elapsed_ms} ms
          </span>
        </div>
        {#if Number(searchRes.total_chapters) === 0}
          <p class="search-warn">{searchRes.note || '目录未缓存，无法判断是否覆盖全书；仅返回已缓存正文中的命中。'}</p>
        {:else if searchRes.complete}
          <p class="rd-muted">已覆盖全部 {searchRes.total_chapters} 章（均已缓存）。</p>
        {:else}
          <p class="search-warn">
            仅搜索了已缓存章节：已缓存 {searchRes.cached_chapters} / {searchRes.total_chapters} 章，
            {searchRes.uncached_chapters} 章未参与检索（complete=false）。
          </p>
        {/if}
        {#if searchRes.truncated}
          <p class="search-warn">命中超过本次上限，只显示前 {searchRes.limit} 条；可缩短关键词再搜。</p>
        {/if}
        {#if !(searchRes.hits && searchRes.hits.length)}
          <p class="rd-muted">没有命中。可换一个关键词；注意未缓存章节不参与检索。</p>
        {:else}
          <div class="search-hits">
            {#each searchRes.hits as h (h.chapter_index + '|' + h.offset)}
              <button class="search-hit" on:click={() => gotoHit(h)}>
                <span class="hit-title">{h.chapter_title || `第 ${Number(h.chapter_index) + 1} 章`}</span>
                <span class="rd-muted">第 {Number(h.chapter_index) + 1} 章 · 命中 {h.match_count} 处</span>
                <span class="hit-snippet">{snippetText(h.snippet)}</span>
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}

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
  .rd-error { color: #c0392b; font-size: 13px; margin: 0; }
  :global(.theme-dark) .rd-error { color: #e88; }
  .rd-muted { color: var(--rd-muted); }
  .small-note { margin: 10px 0 0; font-size: 12px; line-height: 1.6; }

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

  .search-box { display: flex; gap: 8px; margin-bottom: 4px; }
  .search-box input {
    flex: 1; min-width: 0; background: transparent; color: var(--rd-fg);
    border: 1px solid var(--rd-border); border-radius: 8px; padding: 8px 10px; font-size: 14px;
  }
  .search-summary { display: flex; flex-direction: column; gap: 2px; margin: 10px 0 6px; font-size: 13px; }
  .search-warn {
    border: 1px solid var(--rd-border); border-radius: 8px;
    padding: 8px 10px; font-size: 12px; line-height: 1.6;
  }
  .search-hits { display: flex; flex-direction: column; gap: 6px; max-height: 50vh; overflow-y: auto; }
  .search-hit {
    display: flex; flex-direction: column; gap: 3px; text-align: left;
    background: transparent; color: var(--rd-fg);
    border: 1px solid var(--rd-border); border-radius: 8px; padding: 8px 10px;
  }
  .hit-title { font-weight: 600; font-size: 13px; }
  .hit-snippet { font-size: 12px; color: var(--rd-muted); line-height: 1.5; }
</style>
