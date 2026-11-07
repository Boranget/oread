<script>
  import { createEventDispatcher } from 'svelte';
  import { sourceLabel } from '../lib/sourceName.js';

  export let mode = 'search';
  export let error = '';
  export let notice = '';
  export let sources = [];
  export let ignoredSourceIds = [];
  export let requestSourceCount = 0;
  export let sourceTotal = 0;
  export let sourceMode = '';
  export let resultScopeMode = 'all';
  export let resultRequested = 0;
  export let scopeFiltered = 0;
  export let curatedTotal = 0;
  export let jobExpired = false;
  export let starting = false;
  export let searching = false;
  export let completedSources = 0;
  export let pendingSources = 0;
  export let deadlineMs = 0;
  export let searchElapsed = 0;
  export let searchDuration = 0;
  export let paging = false;
  export let page = 1;
  export let searched = false;
  export let total = 0;
  export let results = [];
  export let hasMore = false;
  export let searchDone = false;
  export let sourcesReady = false;
  export let searchableCount = 0;
  export let requestedScope = 'all';
  export let searchCoverage = 'expanded';
  export let failedCount = 0;
  export let failedSources = [];
  export let retrying = false;
  export let retryNote = '';
  export let shelfIds = new Set();
  export let lastSearchKey = '';
  export let hasKey = false;
  export let loading = false;
  export let loadingMore = false;

  const dispatch = createEventDispatcher();
</script>

{#if error}<p class="err">{error}</p>{/if}
{#if notice}<p class="ok-line">{notice}</p>{/if}

{#if mode === 'search' && ignoredSourceIds.length}
  <div class="warn-box" role="alert">
    <div class="warn-title">已忽略 {ignoredSourceIds.length} 个书源：本次实际搜索 {sourceTotal} 个源</div>
    <div class="warn-item">
      本次请求 {requestSourceCount} 个源，后端只接受可搜索源（已启用、类型支持、搜索规则与地址完整、未隔离）。
      被忽略：{ignoredSourceIds.map((id) => sourceLabel(sources, id)).join('、')}
    </div>
    {#if !sourceTotal}
      <div class="warn-item">没有可搜索的源，请到「书源管理」启用书源并补全搜索规则后重试。</div>
    {/if}
  </div>
{/if}

{#if mode === 'search' && sourceTotal && !error}
  <p class="scope-report">
    本次范围：<b>{sourceTotal}</b> 个源
    {#if sourceMode === 'user' || (!sourceMode && resultScopeMode === 'custom')}
      （用户指定 {resultRequested} 个，实际可执行 {sourceTotal} 个）
    {:else if sourceMode === 'curated'}
      精选源{#if scopeFiltered}（另有 {scopeFiltered} 个可搜索源未参与）{/if}
    {:else if sourceMode === 'curated_fallback'}
      （{curatedTotal ? '精选集暂无可用条目' : '尚未生成精选集'}，本次用兜底源；实际 {sourceTotal} 个源）
    {:else if sourceMode === 'all_fallback'}
      （{curatedTotal ? '精选集与常用源暂无可用条目' : '尚未生成精选集'}，已改用全部 {sourceTotal} 个源）
    {:else}
      （全部可搜索源）
    {/if}
    <button class="link-btn" on:click={() => dispatch('retryScope')} disabled={starting || searching || !lastSearchKey}>只搜这些源重试</button>
  </p>
{/if}

{#if mode === 'search' && jobExpired}
  <div class="warn-box" role="alert">
    <div class="warn-title">搜索任务已过期，结果可能不完整</div>
    <div class="warn-item">后端搜索任务只保留 60 秒；已完成任务最多 32 个，进行中任务另有 64 个硬上限。当前 job 已不在服务端，无法继续取回剩余结果。</div>
    <div class="row" style="margin-top:8px">
      <button on:click={() => dispatch('search')} disabled={starting || !hasKey}>重新搜索</button>
    </div>
  </div>
{/if}

{#if mode === 'search' && (starting || searching)}
  <div class="search-wait" aria-live="polite">
    <div class="sw-line">
      <b>
        {#if starting}
          已发起搜索，正在创建任务并等待首个书源…
        {:else}
          已完成 {completedSources} / 共 {sourceTotal || '?'} 个源，慢源仍在返回…
        {/if}
      </b>
      {#if !starting}<span class="sw-time">已等待 {searchElapsed}s</span>{/if}
    </div>
    <div class="sw-sub">
      结果先到先显示：已完成的源会立即出现在下方，不需要等全部书源结束。
      {#if pendingSources}&nbsp;仍有 {pendingSources} 个源未返回。{/if}
      {#if deadlineMs}后端单源预算 {Math.round(deadlineMs / 1000)}s，超时源计入失败。{/if}
    </div>
  </div>
{/if}

{#if mode === 'search' && paging}
  <p class="loading">正在读取第 {page} 页…</p>
{/if}

{#if mode === 'search' && (starting || searching) && results.length === 0}
  <div class="grid auto" style="margin-top:12px" aria-hidden="true">
    {#each [0, 1, 2] as _i}
      <div class="card book skeleton">
        <div class="sk-cover"></div>
        <div class="sk-line w70"></div>
        <div class="sk-line w45"></div>
      </div>
    {/each}
  </div>
{/if}

{#if mode === 'explore' && loading}
  <p class="loading">加载中…</p>
{/if}

{#if mode === 'search' && searched}
  <div class="result-meta">
    <span>
      命中 <b>{total}</b> 本 · 本页 {results.length} 本
      {#if starting}
        &nbsp;· 正在创建搜索任务…
      {:else if searching}
        &nbsp;· 已完成 {completedSources} / 共 {sourceTotal || '?'} 个源 · 搜索中 · 已等待 {searchElapsed}s
      {:else if jobExpired}
        &nbsp;· 任务已过期，结果可能不完整
      {:else if sourceTotal}
        &nbsp;· 已完成 {completedSources} / 共 {sourceTotal} 个源{#if failedCount}（失败 {failedCount}）{/if}{#if searchDuration}&nbsp;· 用时 {searchDuration}s{/if}
      {/if}
      {#if sourceMode === 'curated' || (!sourceMode && requestedScope === 'curated')}
        &nbsp;· 精选源{#if scopeFiltered}（另有 {scopeFiltered} 个可搜索源未参与）{/if}
      {:else if sourceMode === 'curated_fallback'}
        &nbsp;· {curatedTotal ? '精选集暂无可用条目，已回退' : '尚未生成精选集，已回退'}
      {:else if sourceMode === 'all_fallback'}
        &nbsp;· {curatedTotal ? '精选集与常用源暂无可用条目，已改用全部源' : '尚未生成精选集，已改用全部源'}
      {:else if sourceMode === 'user' || (!sourceMode && resultScopeMode === 'custom')}
        &nbsp;· 指定源
      {:else}
        &nbsp;· 全部可搜索源
      {/if}
    </span>
  </div>
{/if}

{#if mode === 'search' && searchDone && total === 0}
  {#if sourceTotal === 0 || (sourcesReady && searchableCount === 0)}
    <p class="muted empty-hint">
      当前还没有可用的书源，请先到<button class="ghost empty-retry" on:click={() => dispatch('goSources')}>书源管理</button>导入并启用书源。
    </p>
  {:else if requestedScope === 'curated' && searchCoverage === 'curated'}
    <p class="muted empty-hint">
      精选源没有结果，<button class="ghost empty-retry" on:click={() => dispatch('retryAll')}>用全部可搜索源重试</button>，或把上方范围切到「全部可搜索源」再试。
    </p>
  {:else if requestedScope === 'curated'}
    {#if curatedTotal}
      <p class="muted empty-hint">精选集暂无可用条目，已回退到常用/全部可搜索源，仍无结果；可换个关键词再试。</p>
    {:else}
      <p class="muted empty-hint">
        尚未生成精选集，本次已用兜底源，仍无结果；可到<button class="ghost empty-retry" on:click={() => dispatch('goSources')}>书源管理</button>点「跑一次体检」生成精选集后重试。
      </p>
    {/if}
  {:else}
    <p class="muted empty-hint">没有找到结果，可换个关键词再试。</p>
  {/if}
{/if}

{#if failedCount || failedSources.length}
  <details class="warn-box">
    <summary class="warn-title">
      {failedCount || failedSources.length} 个书源失败{#if searching}（其余仍在搜索）{:else}，已返回其余结果{/if}
    </summary>
    <div class="warn-items">
      {#each failedSources as f}
        <div class="warn-item">{f.name}：{f.reason}</div>
      {/each}
      {#if !failedSources.length}
        <div class="warn-item">后端未返回失败明细，可重试或到「书源管理」检查这些书源。</div>
      {/if}
      <div class="warn-item muted">可先换个关键词或稍后重试；长期失败请到「书源管理」检查这些源的地址与启用状态。</div>
      <div class="row" style="margin-top:8px">
        <button class="ghost" on:click={() => dispatch('retryFailed')} disabled={retrying || starting || searching || !failedSources.some((f) => f.url)}>
          {retrying ? '正在仅重试失败源…' : '仅重试失败源'}
        </button>
        {#if !failedSources.some((f) => f.url)}<span class="muted">后端未返回失败源地址，无法按源重试</span>{/if}
      </div>
    </div>
  </details>
{/if}

{#if retryNote}<p class="retry-note" role="status">{retryNote}</p>{/if}

{#if results.length}
  <div class="grid auto" style="margin-top:12px">
    {#each results as r (r.book_url + '|' + (r.origin || ''))}
      <div class="card book">
        <button class="cover-btn" on:click={() => dispatch('open', r)} aria-label="打开《{r.name || r.book_url}》">
          <div class="cover-wrap">
            <span class="cover-ph" aria-hidden="true">{(r.name || '?').slice(0, 2)}</span>
            {#if r.cover_url}
              <img class="cover" src={r.cover_url} alt="" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
            {/if}
          </div>
        </button>
        <div class="title" on:click={() => dispatch('open', r)}>{r.name || r.book_url}</div>
        <div class="author">{r.author || '未知作者'}{#if r.origin_name} · {r.origin_name}{/if}{#if r.retry} <span class="retry-tag">重试命中</span>{/if}</div>
        {#if r.latest_chapter_title}<div class="author muted clamp1">{r.latest_chapter_title}</div>{/if}
        <div class="row" style="margin-top:8px">
          <button style="flex:1" on:click={() => dispatch('open', r)}>阅读</button>
          <button class="ghost" disabled={shelfIds.has(r.book_url)} on:click={() => dispatch('addShelf', r)}>{shelfIds.has(r.book_url) ? '已加入' : '加入书架'}</button>
        </div>
      </div>
    {/each}
  </div>
  <div class="row" style="justify-content:center;margin-top:16px">
    {#if mode === 'search'}
      {#if page > 1}<button class="ghost" on:click={() => dispatch('page', page - 1)} disabled={paging || jobExpired}>上一页</button>{/if}
      <span class="muted">第 {page} 页{#if total}&nbsp;· 共 {total} 本{/if}</span>
      <button class="ghost" on:click={() => dispatch('page', page + 1)} disabled={paging || !hasMore || jobExpired}>
        {paging ? '读取中…' : '下一页'}
      </button>
      {#if !hasMore && !searching && !starting}<span class="muted">没有更多了</span>{/if}
    {:else if hasMore}
      <button class="ghost" on:click={() => dispatch('loadMore')} disabled={loadingMore}>{loadingMore ? '加载中…' : `加载更多（第 ${page + 1} 页）`}</button>
    {:else}
      <span class="muted">没有更多了</span>
    {/if}
  </div>
{:else if mode === 'search'}
  {#if !searched}
    <div class="empty">输入关键词开始搜索</div>
  {:else if !starting && !searching && !paging}
    {#if failedSources.length || failedCount}
      <div class="empty">没有拿到结果，且有 {failedCount || failedSources.length} 个书源失败，原因见上方。<br />换关键词、稍后重试，或到「书源管理」检查这些书源。</div>
    {:else if !jobExpired && !error}
      {#if sourceTotal === 0 || (sourcesReady && searchableCount === 0)}
        <div class="empty">当前还没有可用的书源，请先到「书源管理」导入并启用书源。<br /><button class="ghost" on:click={() => dispatch('goSources')}>去书源管理</button></div>
      {:else}
        <div class="empty">书源正常返回，但没有匹配的书。<br />换个关键词，或到「书源管理」确认书源是否启用。</div>
      {/if}
    {/if}
  {/if}
{:else if !loading && !searched}
  <div class="empty">选择书源与分类后开始发现</div>
{:else if !loading && searched}
  <div class="empty">没有找到结果。<br />换个分类，或到「书源管理」确认书源是否启用。</div>
{/if}

<style>
  /* 搜索范围下拉的说明文案 */
  .empty-hint {
    margin: 10px 0 0;
    font-size: 13px;
  }
  .empty-retry {
    width: auto;
    min-height: auto;
    padding: 2px 8px;
    margin: 0 4px;
    font-size: 13px;
  }
  .scope-report {
    margin: 8px 0;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }
  .link-btn {
    background: transparent;
    color: var(--accent);
    border: 0;
    padding: 2px 6px;
    font-size: 12px;
    text-decoration: underline;
  }
  .link-btn:hover {
    background: transparent;
  }
  .retry-note {
    margin: 8px 0;
    padding: 8px 10px;
    border: 1px solid #e8c7a7;
    border-radius: 8px;
    background: #fdf6ec;
    color: #8a5a1f;
    font-size: 12px;
  }
  .retry-tag {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--accent);
    border-radius: 999px;
    font-size: 11px;
    color: var(--accent);
  }
</style>
