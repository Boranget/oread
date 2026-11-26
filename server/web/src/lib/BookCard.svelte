<script>
  import { createEventDispatcher } from 'svelte';
  import { href } from '../lib/router.js';

  export let book;
  export let compact = false;
  export let selectable = false;
  export let selected = false;

  const dispatch = createEventDispatcher();
  const STATUS_LABEL = { want: '想读', reading: '在读', finished: '已读' };

  $: url = String(book.book_url || '');
  $: detailHref = href(`/book?url=${encodeURIComponent(url)}`);
  $: title = book.name || book.title || url;
  $: author = book.author || '未知作者';
  $: status = book.status || '';
  $: pct = Math.max(0, Math.min(100, Number(book.reading_progress) || 0));
  $: series = book.series_name
    ? `${book.series_name}${book.series_index != null ? ' #' + book.series_index : ''}`
    : '';
</script>

<div class="card book-card" class:compact>
  {#if selectable}
    <label class="pick">
      <input
        type="checkbox"
        checked={selected}
        on:change={(e) => dispatch('select', { book, checked: e.currentTarget.checked })}
      />
      <span>选择</span>
    </label>
  {/if}

  <a class="cover-link" href={detailHref} aria-label="查看《{title}》详情">
    <span class="cover-wrap">
      <span class="cover-ph" aria-hidden="true">{(title || '?').slice(0, 2)}</span>
      {#if book.cover_url}
        <img
          class="cover"
          src={book.cover_url}
          alt=""
          loading="lazy"
          referrerpolicy="no-referrer"
          on:error={(e) => (e.currentTarget.style.visibility = 'hidden')}
        />
      {/if}
      {#if status}
        <span class="status-badge">{STATUS_LABEL[status] || status}</span>
      {/if}
    </span>
  </a>

  <a class="title" href={detailHref}>{title}</a>
  <div class="author">{author}</div>
  {#if series}<div class="series muted">{series}</div>{/if}

  {#if pct > 0}
    <div class="pg-track" aria-hidden="true"><div class="pg-fill" style={`width:${pct}%`}></div></div>
    <div class="muted tiny">已读 {pct.toFixed(0)}%</div>
  {:else}
    <div class="muted tiny">{book.on_shelf ? '在书架' : '未收藏'}</div>
  {/if}

  <div class="actions">
    <button class="ghost mini" on:click={() => dispatch('read', book)}>阅读</button>
    <a class="ghost mini detail-link" href={detailHref}>详情</a>
  </div>
</div>

<style>
  .book-card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px;
    cursor: default;
  }
  .cover-link { display: block; color: inherit; }
  .cover-wrap {
    position: relative;
    display: block;
    height: 190px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--soft-bg, #eee);
  }
  .compact .cover-wrap { height: 130px; }
  .cover-ph {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 24px;
    font-weight: 600;
    color: var(--muted);
  }
  .cover {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .status-badge {
    position: absolute;
    left: 6px;
    bottom: 6px;
    background: rgba(0, 0, 0, 0.65);
    color: #fff;
    border-radius: 999px;
    padding: 1px 8px;
    font-size: 11px;
    line-height: 18px;
  }
  .title {
    color: var(--fg);
    text-decoration: none;
    font-weight: 600;
    font-size: 13px;
    line-height: 1.35;
    min-height: 2.7em;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .title:hover { color: var(--accent); }
  .author, .series { font-size: 12px; color: var(--muted); }
  .series { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tiny { font-size: 11px; }
  .pg-track {
    height: 6px;
    margin-top: 4px;
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .pg-fill { height: 100%; background: var(--accent); }
  .actions { display: flex; gap: 6px; margin-top: 6px; }
  .actions > * { flex: 1 1 0; text-align: center; }
  .mini { padding: 6px 10px; font-size: 12px; min-height: 32px; }
  .detail-link {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    text-decoration: none;
    border-radius: 8px;
  }
  .pick {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 4px;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 2px 6px;
    font-size: 11px;
  }
  .pick input { width: auto; margin: 0; }
</style>
