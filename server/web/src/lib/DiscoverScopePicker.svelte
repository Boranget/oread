<script>
  import { createEventDispatcher } from 'svelte';
  import { sourceName, sourceType } from '../lib/sourceName.js';

  export let sources = [];
  export let searchableSources = [];
  export let curatedTotal = 0;
  export let sourceScope = 'all';
  export let pickedSourceIds = [];
  export let scopeOpen = false;
  export let groupFilter = 'all';
  export let groupOptions = [];
  export let pickSources = [];

  const dispatch = createEventDispatcher();

  function togglePick(url) {
    pickedSourceIds = pickedSourceIds.includes(url)
      ? pickedSourceIds.filter((x) => x !== url)
      : [...pickedSourceIds, url];
  }

  function selectAllEnabled() {
    pickedSourceIds = searchableSources.map((s) => s.book_source_url);
  }

  // 按分组选择：只并入该分组下的可搜索源，保留已选的其他源
  function pickGroup() {
    if (groupFilter === 'all') return;
    const add = searchableSources
      .filter((s) => (s.book_source_group || '') === groupFilter)
      .map((s) => s.book_source_url);
    pickedSourceIds = [...new Set(pickedSourceIds.concat(add))];
  }

  function clearPicked() {
    pickedSourceIds = [];
  }
</script>

<div class="search-scope">
  <div class="row">
    <select bind:value={sourceScope} on:change={() => dispatch('clearError')} style="flex:1;min-width:0" aria-label="搜索范围">
      <option value="all">全部可搜索源（{searchableSources.length} 个）</option>
      <option value="curated">精选源{curatedTotal ? `（${curatedTotal} 个）` : ''}</option>
      <option value="custom">指定分类 / 书源（已选 {pickedSourceIds.length} 个）</option>
    </select>
    {#if sourceScope === 'custom'}
      <button class="ghost" on:click={() => (scopeOpen = !scopeOpen)} aria-expanded={scopeOpen}>
        {scopeOpen ? '收起书源' : '选择书源'}
      </button>
    {/if}
  </div>

  {#if sourceScope === 'custom' && scopeOpen}
    {#if groupOptions.length}
      <div class="row" style="margin-top:8px">
        <select bind:value={groupFilter} style="width:100%;min-width:0" aria-label="按分类筛选书源">
          <option value="all">全部分类</option>
          {#each groupOptions as g}
            <option value={g}>{g}</option>
          {/each}
        </select>
        <button class="ghost" on:click={pickGroup} disabled={groupFilter === 'all'}>选入本分类</button>
      </div>
    {/if}
    <div class="src-list" role="group" aria-label="选择参与搜索的书源">
      {#each pickSources as s (s.book_source_url)}
        <label class="src-item" class:off={!s.searchable}>
          <input
            type="checkbox"
            disabled={!s.searchable}
            checked={pickedSourceIds.includes(s.book_source_url)}
            on:change={() => togglePick(s.book_source_url)}
          />
          <span class="src-name">{sourceName(s)}</span>
          {#if s.book_source_group}<span class="src-type">{s.book_source_group}</span>{/if}
          {#if sourceType(s)}<span class="src-type">{sourceType(s)}</span>{/if}
          {#if !s.enabled}<span class="src-off">已禁用</span>{:else if !s.searchable}<span class="src-off">不可搜索</span>{/if}
        </label>
      {/each}
      {#if !sources.length}<p class="muted" style="margin:6px 0">还没有书源，请先到「书源管理」添加。</p>{:else if !pickSources.length}<p class="muted" style="margin:6px 0">该分类下没有书源。</p>{/if}
    </div>
    <div class="row" style="margin-top:6px">
      <button class="ghost" on:click={selectAllEnabled} disabled={!searchableSources.length}>全选可搜索源</button>
      <button class="ghost" on:click={clearPicked} disabled={!pickedSourceIds.length}>清空</button>
    </div>
  {/if}

  <p class="muted scope-hint">
    本次范围：{sourceScope === 'all'
      ? `全部可搜索源（${searchableSources.length} 个）`
      : sourceScope === 'curated'
        ? curatedTotal
          ? `精选源（${curatedTotal} 个；集合为空时自动回退）`
          : '精选源（集合尚未生成，搜索会自动回退）'
        : `已选 ${pickedSourceIds.length} 个源`}
    {#if sourceScope === 'custom' && !pickedSourceIds.length}&nbsp;· 至少选 1 个才能搜索{/if}
  </p>
</div>

<style>
  .search-scope {
    margin-top: 8px;
  }
  .scope-hint {
    margin: 6px 0 0;
    font-size: 12px;
  }
  .src-list {
    margin-top: 8px;
    max-height: 38vh;
    overflow: auto;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 8px;
    background: var(--card, #fff);
  }
  .src-item {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    font-size: 13px;
    cursor: pointer;
  }
  .src-item:last-child {
    border-bottom: 0;
  }
  .src-item.off {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .src-item input {
    width: 18px;
    height: 18px;
    flex: 0 0 auto;
  }
  .src-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .src-type {
    flex: 0 0 auto;
    font-size: 11px;
    padding: 1px 6px;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 10px;
    color: var(--muted, #666);
  }
  .src-off {
    flex: 0 0 auto;
    font-size: 11px;
    color: var(--muted, #666);
  }
  :global(html[data-vp='sm']) .src-item {
    min-height: 44px;
  }
</style>
