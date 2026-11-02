<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';

  const dispatch = createEventDispatcher();
  let mode = 'search'; // search | explore
  let key = '';
  let results = [];
  let error = '';
  let loading = false;

  // 发现模式
  let sources = [];
  let selectedSource = '';

  onMount(async () => {
    try {
      sources = await api.sources();
      if (sources.length) selectedSource = sources[0].book_source_url;
    } catch (e) {
      error = e.message;
    }
  });

  async function doSearch() {
    if (!key.trim()) return;
    loading = true;
    error = '';
    try {
      results = await api.search(key.trim(), 1);
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function doExplore() {
    if (!selectedSource) return;
    loading = true;
    error = '';
    try {
      results = await api.explore(selectedSource);
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function open(r) {
    dispatch('open', { bookUrl: r.book_url, sourceId: r.origin, name: r.name });
  }
</script>

<div class="row" style="margin-bottom:12px">
  <div class="tab" class:active={mode === 'search'} on:click={() => (mode = 'search')}>搜索</div>
  <div class="tab" class:active={mode === 'explore'} on:click={() => (mode = 'explore')}>发现</div>
</div>

{#if mode === 'search'}
  <div class="row">
    <input placeholder="输入书名 / 作者" bind:value={key} on:keydown={(e) => e.key === 'Enter' && doSearch()} />
    <button on:click={doSearch}>搜索</button>
  </div>
{:else}
  <div class="row">
    <select bind:value={selectedSource} style="flex:1">
      {#each sources as s}
        <option value={s.book_source_url}>{s.book_source_name}</option>
      {/each}
    </select>
    <button on:click={doExplore}>发现</button>
  </div>
{/if}

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if !loading && results.length === 0}
  <div class="empty">{mode === 'search' ? '输入关键词开始搜索' : '选择书源后点击「发现」'}</div>
{:else}
  <div class="grid cols3" style="margin-top:12px">
    {#each results as r}
      <div class="card book" on:click={() => open(r)}>
        {#if r.cover_url}
          <img class="cover" src={r.cover_url} alt="cover" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
        {:else}
          <div class="cover"></div>
        {/if}
        <div class="title">{r.name || r.book_url}</div>
        <div class="author">{r.author || ''}{#if r.origin_name} · {r.origin_name}{/if}</div>
        {#if r.latest_chapter_title}<div class="author muted">{r.latest_chapter_title}</div>{/if}
        <div class="row" style="margin-top:8px">
          <button style="flex:1" on:click|stopPropagation={() => open(r)}>详情</button>
        </div>
      </div>
    {/each}
  </div>
{/if}
