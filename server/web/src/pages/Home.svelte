<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { href } from '../lib/router.js';
  import BookCard from '../lib/BookCard.svelte';

  export let role = 'user';

  const dispatch = createEventDispatcher();
  let data = null;
  let loading = true;
  let error = '';

  $: isAdmin = role === 'admin';
  $: stats = data?.stats || null;

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      data = await api.libraryHome();
    } catch (e) {
      error = e.message;
      data = null;
    } finally {
      loading = false;
    }
  }
</script>

<div class="page-head">
  <h2>我的图书馆</h2>
  <span class="page-count">{stats ? `${stats.collection} 本馆藏` : ''}</span>
  <div class="page-actions">
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>
</div>

{#if error}
  <div class="card">
    <p class="err">{error}</p>
    <button class="ghost" on:click={load}>重试</button>
  </div>
{:else if loading && !data}
  <p class="loading">加载中…</p>
{:else if data}
  <div class="stat-row">
    <a class="stat" href={href('/library')}>
      <span class="stat-num">{stats.collection}</span>
      <span class="stat-label">全部馆藏</span>
    </a>
    <a class="stat" href={href('/library?status=reading')}>
      <span class="stat-num">{stats.reading}</span>
      <span class="stat-label">在读</span>
    </a>
    <a class="stat" href={href('/library?status=want')}>
      <span class="stat-num">{stats.want}</span>
      <span class="stat-label">想读</span>
    </a>
    <a class="stat" href={href('/library?status=finished')}>
      <span class="stat-num">{stats.finished}</span>
      <span class="stat-label">已读</span>
    </a>
    <a class="stat" href={href('/library?shelf=1')}>
      <span class="stat-num">{stats.shelf}</span>
      <span class="stat-label">在书架</span>
    </a>
  </div>

  {#if stats.collection === 0}
    <div class="card empty-card">
      <b>馆藏还是空的</b>
      <p class="muted">
        馆藏是整个图书馆的内容层，所有账号共享；书架与阅读状态是每个人的。
        {#if isAdmin}先导入一本 EPUB，或从「发现」里把书入馆。{:else}请让管理员导入书籍后再来。{/if}
      </p>
      {#if isAdmin}
        <div class="empty-actions">
          <a class="btn-link" href={href('/admin?tab=collection')}>去导入</a>
          <a class="btn-link ghost-link" href={href('/admin?tab=discover')}>去发现</a>
        </div>
      {/if}
    </div>
  {:else}
    <section class="section">
      <div class="section-head">
        <h3>继续阅读</h3>
        <a class="more" href={href('/library?status=reading')}>全部在读</a>
      </div>
      {#if data.continue.length}
        <div class="strip">
          {#each data.continue as book (book.book_url)}
            <div class="strip-item">
              <BookCard {book} compact on:read={(e) => dispatch('read', e.detail)} />
            </div>
          {/each}
        </div>
      {:else}
        <p class="muted">还没有阅读进度。打开一本书读几页，这里会出现续读入口。</p>
      {/if}
    </section>

    <section class="section">
      <div class="section-head">
        <h3>最近加入</h3>
        <a class="more" href={href('/library?sort=added')}>查看全部</a>
      </div>
      {#if data.recent.length}
        <div class="grid auto">
          {#each data.recent as book (book.book_url)}
            <BookCard {book} on:read={(e) => dispatch('read', e.detail)} />
          {/each}
        </div>
      {:else}
        <p class="muted">暂无最近加入的书。</p>
      {/if}
    </section>

    {#if data.want.length}
      <section class="section">
        <div class="section-head">
          <h3>想读</h3>
          <a class="more" href={href('/library?status=want')}>全部想读</a>
        </div>
        <div class="grid auto">
          {#each data.want as book (book.book_url)}
            <BookCard {book} on:read={(e) => dispatch('read', e.detail)} />
          {/each}
        </div>
      </section>
    {/if}
  {/if}
{/if}

<style>
  .stat-row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 10px;
    margin-bottom: 20px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 14px;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: var(--fg);
    text-decoration: none;
  }
  .stat:hover { border-color: var(--accent); }
  .stat-num { font-size: 24px; font-weight: 700; line-height: 1.2; font-variant-numeric: tabular-nums; }
  .stat-label { font-size: 12px; color: var(--muted); }
  .section { margin-bottom: 26px; }
  .section-head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 10px; }
  .section-head h3 { margin: 0; font-size: 16px; }
  .more { margin-left: auto; font-size: 12px; color: var(--accent); text-decoration: none; }
  .strip {
    display: flex;
    gap: 12px;
    overflow-x: auto;
    padding-bottom: 6px;
    -webkit-overflow-scrolling: touch;
  }
  .strip-item { flex: 0 0 168px; }
  .empty-card { padding: 26px 20px; display: grid; gap: 10px; justify-items: start; }
  .empty-card p { margin: 0; max-width: 560px; line-height: 1.6; }
  .empty-actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .btn-link {
    display: inline-flex;
    align-items: center;
    padding: 8px 16px;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    text-decoration: none;
    font-size: 14px;
  }
  .ghost-link { background: transparent; color: var(--accent); border: 1px solid var(--accent); }
</style>
