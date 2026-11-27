<script>
  import { createEventDispatcher } from 'svelte';
  import { href } from '../lib/router.js';
  import BookCover from './BookCover.svelte';

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
      <BookCover url={book.cover_url} imgClass="cover" />
      {#if status}
        <span class="status-badge">{STATUS_LABEL[status] || status}</span>
      {/if}
    </span>
  </a>

  <a class="title" href={detailHref} title={title}>{title}</a>
  <div class="author" title={author}>{author}</div>
  <!-- 系列行始终占位：没有系列的书也保留同样高度，卡片底部才能对齐 -->
  <div class="series muted" aria-hidden={!series}>{series}</div>

  <!-- 进度区固定高度：有无进度都占两行（进度条 + 文案） -->
  <div class="progress">
    <div class="pg-track" aria-hidden="true"><div class="pg-fill" style={`width:${pct}%`}></div></div>
    <div class="muted tiny progress-label">
      {#if pct > 0}
        已读 {pct.toFixed(0)}%
      {:else}
        {book.on_shelf ? '在书架 · 未开始' : '未开始'}
      {/if}
    </div>
  </div>

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
    height: 100%;
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
  .author {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* 固定一行高度，空串也占位 */
  .series {
    min-height: 1.45em;
    line-height: 1.45;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tiny { font-size: 11px; }
  .progress { display: grid; gap: 2px; }
  .progress-label { min-height: 1.4em; line-height: 1.4; }
  .pg-track {
    height: 6px;
    margin-top: 4px;
    border: 1px solid var(--border);
    border-radius: 3px;
    overflow: hidden;
  }
  .pg-fill { height: 100%; background: var(--accent); }
  /* 按钮统一高度并吸到卡片底部，同一行的卡片才会对齐 */
  .actions {
    display: flex;
    align-items: stretch;
    gap: 6px;
    margin-top: auto;
    padding-top: 6px;
  }
  .actions > * {
    flex: 1 1 0;
    min-width: 0;
    min-height: 32px;
    padding: 6px 10px;
    font-size: 12px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .mini { padding: 6px 10px; font-size: 12px; min-height: 32px; }
  .detail-link {
    color: var(--accent);
    background: transparent;
    border: 1px solid var(--accent);
    text-decoration: none;
    border-radius: 8px;
  }
  .detail-link:hover { background: var(--accent-weak, rgba(179, 57, 47, 0.08)); }
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
