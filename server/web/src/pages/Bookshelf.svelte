<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';

  const dispatch = createEventDispatcher();
  let books = [];
  let error = '';
  let loading = false;

  async function load() {
    loading = true;
    error = '';
    try {
      books = await api.bookshelf();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function open(b) {
    dispatch('open', { bookUrl: b.book_id, sourceId: b.source_id, name: b.title });
  }
  async function remove(b) {
    if (!confirm(`从书架移除《${b.title || b.book_id}》？`)) return;
    try {
      await api.removeFromShelf(b.book_id);
      await load();
    } catch (e) {
      error = e.message;
    }
  }

  onMount(load);
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">我的书架</h2>
  <div class="spacer"></div>
  <button class="ghost" on:click={load}>刷新</button>
</div>

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if books.length === 0 && !loading}
  <div class="empty">书架还是空的，去「发现」里找本书加入吧。</div>
{:else}
  <div class="grid cols3">
    {#each books as b}
      <div class="card book">
        {#if b.cover}
          <img class="cover" src={b.cover} alt="cover" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
        {:else}
          <div class="cover"></div>
        {/if}
        <div class="title">{b.title || b.book_id}</div>
        <div class="author">{b.author || ''}</div>
        <div class="row" style="margin-top:8px">
          <button style="flex:1" on:click={() => open(b)}>阅读</button>
          <button class="ghost" on:click={() => remove(b)}>移除</button>
        </div>
      </div>
    {/each}
  </div>
{/if}
