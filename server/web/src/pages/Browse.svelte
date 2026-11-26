<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { href, buildPath } from '../lib/router.js';

  const DIMS = [
    { key: 'author', label: '作者', facet: 'authors' },
    { key: 'series', label: '系列', facet: 'series' },
    { key: 'tag', label: '标签', facet: 'tags' },
    { key: 'publisher', label: '出版社', facet: 'publishers' },
    { key: 'language', label: '语言', facet: 'languages' },
  ];

  let facets = null;
  let loading = true;
  let error = '';
  let dim = 'author';
  let filter = '';

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      facets = await api.libraryFacets();
    } catch (e) {
      error = e.message;
      facets = null;
    } finally {
      loading = false;
    }
  }

  $: current = DIMS.find((d) => d.key === dim) || DIMS[0];
  $: values = ((facets && facets[current.facet]) || []).filter((v) =>
    !filter.trim() ? true : String(v.value).toLowerCase().includes(filter.trim().toLowerCase())
  );
  $: total = values.reduce((sum, v) => sum + Number(v.count || 0), 0);

  function setDim(key) {
    dim = key;
    filter = '';
  }
</script>

<div class="page-head">
  <h2>浏览维度</h2>
  <span class="page-count">{values.length} 个{current.label}</span>
  <div class="page-actions">
    <a class="ghost-link" href={href('/library')}>返回图书馆</a>
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>
</div>

<div class="seg dim-tabs">
  {#each DIMS as d (d.key)}
    <button class:on={dim === d.key} on:click={() => setDim(d.key)}>{d.label}</button>
  {/each}
</div>

<div class="toolbar">
  <input class="grow" bind:value={filter} placeholder="在{current.label}里筛选…" aria-label="筛选{current.label}" />
</div>

{#if error}
  <div class="card">
    <p class="err">{error}</p>
    <button class="ghost" on:click={load}>重试</button>
  </div>
{:else if loading && !facets}
  <p class="loading">加载中…</p>
{:else if !values.length}
  <div class="card empty-card">
    {#if filter.trim()}
      <b>没有匹配的{current.label}</b>
      <p class="muted">换个关键词试试。</p>
    {:else}
      <b>还没有{current.label}数据</b>
      <p class="muted">导入带{current.label}元数据的书后，这里会自动出现索引。</p>
    {/if}
  </div>
{:else}
  <p class="muted summary">共 {values.length} 个{current.label}，覆盖 {total} 本书次（同一本书可能计入多个标签）。</p>
  <div class="facet-list">
    {#each values as v (v.value)}
      <a class="card facet" href={href(buildPath('/library', { [current.key]: v.value }))}>
        <span class="facet-name">{v.value}</span>
        <span class="facet-count">{v.count}</span>
      </a>
    {/each}
  </div>
{/if}

<style>
  .ghost-link {
    display: inline-flex;
    align-items: center;
    padding: 8px 14px;
    border: 1px solid var(--accent);
    border-radius: 8px;
    color: var(--accent);
    text-decoration: none;
    font-size: 14px;
  }
  .dim-tabs { overflow-x: auto; }
  .dim-tabs button { white-space: nowrap; }
  .summary { margin: 0 0 10px; font-size: 12px; }
  .facet-list { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; }
  .facet {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 12px 14px;
    color: var(--fg);
    text-decoration: none;
  }
  .facet:hover { border-color: var(--accent); }
  .facet-name { overflow-wrap: anywhere; }
  .facet-count { color: var(--muted); font-size: 12px; white-space: nowrap; font-variant-numeric: tabular-nums; }
  .empty-card { padding: 26px 20px; display: grid; gap: 10px; justify-items: start; }
  .empty-card p { margin: 0; }
</style>
