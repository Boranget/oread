<script>
  import { createEventDispatcher } from 'svelte';
  import { api } from '../lib/api.js';
  import { route, href, buildPath } from '../lib/router.js';
  import BookEditor from '../lib/BookEditor.svelte';
  import ExportDialog from '../lib/ExportDialog.svelte';

  export let role = 'user';

  const dispatch = createEventDispatcher();
  const STATUS_LABEL = { want: '想读', reading: '在读', finished: '已读' };

  let book = null;
  let loading = true;
  let error = '';
  let busy = '';
  let notice = '';
  let showEditor = false;
  let showExport = false;
  let tocOpen = false;

  $: isAdmin = role === 'admin';
  $: bookUrl = $route.query.url || $route.query.book_url || '';
  let lastUrl = null;
  $: if (bookUrl !== lastUrl) {
    lastUrl = bookUrl;
    load();
  }

  $: status = (book && book.status) || '';
  $: pct = Math.max(0, Math.min(100, Number(book && book.reading_progress) || 0));
  $: total = Number(book && book.total_chapters) || 0;
  $: chapterNo = book && Number(book.chapter_index) > 0 ? Number(book.chapter_index) + 1 : 0;
  $: toc = (book && book.toc) || [];

  async function load() {
    if (!bookUrl) {
      book = null;
      loading = false;
      error = '缺少书籍参数';
      return;
    }
    loading = true;
    error = '';
    notice = '';
    try {
      const res = await api.libraryBook(bookUrl);
      book = res && res.book ? res.book : null;
      if (!book) error = '没有找到这本书';
    } catch (e) {
      error = e.message;
      book = null;
    } finally {
      loading = false;
    }
  }

  function read() {
    if (!book) return;
    dispatch('read', {
      bookUrl: book.book_url,
      sourceId: book.origin || '',
      name: book.name,
      author: book.author,
      coverUrl: book.cover_url,
      intro: book.intro,
      bookType: Number(book.book_source_type) === 2 ? 'image' : 'text',
    });
  }

  async function toggleStatus(value) {
    if (!book || busy) return;
    const next = status === value ? '' : value;
    busy = 'status';
    notice = '';
    try {
      await api.setReadingStatus([book.book_url], next);
      book = { ...book, status: next };
      notice = next ? `已标记为「${STATUS_LABEL[next]}」` : '已清除阅读状态';
    } catch (e) {
      notice = `设置状态失败：${e.message}`;
    } finally {
      busy = '';
    }
  }

  async function toggleShelf() {
    if (!book || busy) return;
    busy = 'shelf';
    notice = '';
    try {
      if (book.on_shelf) {
        await api.removeFromShelf(book.book_url);
        book = { ...book, on_shelf: false, group: '' };
        notice = '已移出书架；馆藏内容与阅读记录保留';
      } else {
        await api.addToShelf({
          book_id: book.book_url,
          source_id: book.origin || '',
          name: book.name,
          author: book.author,
          cover: book.cover_url,
        });
        book = { ...book, on_shelf: true };
        notice = '已加入书架';
      }
    } catch (e) {
      notice = `操作失败：${e.message}`;
    } finally {
      busy = '';
    }
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024 * 1024) return (v / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  function fmtDate(v) {
    if (!v) return '';
    const d = typeof v === 'number' ? new Date(v) : new Date(String(v).replace(' ', 'T') + (String(v).includes('Z') ? '' : 'Z'));
    if (Number.isNaN(d.getTime())) return String(v);
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }
</script>

<div class="page-head">
  <h2>书籍详情</h2>
  <div class="page-actions">
    <a class="ghost-link" href={href('/library')}>返回图书馆</a>
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>
</div>

{#if error}
  <div class="card">
    <p class="err">{error}</p>
    <button class="ghost" on:click={load}>重试</button>
  </div>
{:else if loading && !book}
  <p class="loading">加载中…</p>
{:else if book}
  {#if notice}<p class="ok-line">{notice}</p>{/if}

  <div class="card hero">
    <div class="cover-wrap">
      <span class="cover-ph">{(book.name || '?').slice(0, 2)}</span>
      {#if book.cover_url}
        <img
          class="cover"
          src={book.cover_url}
          alt=""
          referrerpolicy="no-referrer"
          on:error={(e) => (e.currentTarget.style.visibility = 'hidden')}
        />
      {/if}
    </div>

    <div class="info">
      <h3 class="book-title">{book.name || book.book_url}</h3>
      <div class="muted">{book.author || '未知作者'}</div>
      <div class="meta-line">
        {#if status}<span class="badge status-badge">{STATUS_LABEL[status] || status}</span>{/if}
        {#if book.on_shelf}<span class="badge">在书架</span>{/if}
        {#if book.group}<span class="badge">{book.group}</span>{/if}
        {#if book.language}<span class="badge">{book.language}</span>{/if}
        {#if book.series_name}
          <a class="badge link-badge" href={href(buildPath('/library', { series: book.series_name }))}>
            {book.series_name}{#if book.series_index != null} #{book.series_index}{/if}
          </a>
        {/if}
      </div>

      {#if total > 0}
        <div class="progress-line">
          <div class="pg-track" aria-hidden="true"><div class="pg-fill" style={`width:${pct}%`}></div></div>
          <div class="muted small">
            已读 {pct.toFixed(0)}%{#if chapterNo} · 第 {chapterNo} / {total} 章{:else if book.chapter_index != null} · 第 {Number(book.chapter_index) + 1} 章{/if}
          </div>
        </div>
      {:else if chapterNo}
        <div class="muted small">读到第 {chapterNo} 章</div>
      {/if}

      <div class="actions">
        <button on:click={read}>{pct > 0 ? '继续阅读' : '开始阅读'}</button>
        <button class="ghost" on:click={toggleShelf} disabled={!!busy}>
          {book.on_shelf ? '移出书架' : '加入书架'}
        </button>
        <button class="ghost" on:click={() => (showExport = true)}>导出</button>
        {#if isAdmin}
          <button class="ghost" on:click={() => (showEditor = true)}>编辑元数据</button>
        {/if}
      </div>

      <div class="status-row">
        <span class="muted small">阅读状态</span>
        {#each ['want', 'reading', 'finished'] as s}
          <button
            class="chip status-chip"
            class:on={status === s}
            on:click={() => toggleStatus(s)}
            disabled={!!busy}
          >{STATUS_LABEL[s]}</button>
        {/each}
        {#if status}
          <button class="ghost mini" on:click={() => toggleStatus(status)} disabled={!!busy}>清除</button>
        {/if}
      </div>
    </div>
  </div>

  <div class="detail-grid">
    <div class="card">
      <h3>元数据</h3>
      <dl class="meta-list">
        {#if book.publisher}<dt>出版社</dt><dd>{book.publisher}</dd>{/if}
        {#if book.pubdate}<dt>出版日期</dt><dd>{book.pubdate}</dd>{/if}
        {#if book.language}<dt>语言</dt><dd>{book.language}</dd>{/if}
        {#if book.series_name}<dt>系列</dt><dd>{book.series_name}{#if book.series_index != null} #{book.series_index}{/if}</dd>{/if}
        {#if book.origin_name}<dt>来源</dt><dd>{book.origin_name}</dd>{/if}
        {#if book.tier}<dt>层级</dt><dd>{book.tier === 'collection' ? '馆藏' : '临时缓存'}</dd>{/if}
        {#if book.cached_chapters != null}<dt>已缓存</dt><dd>{book.cached_chapters} / {book.total_chapter_num || '?'} 章</dd>{/if}
        {#if book.content_bytes}<dt>正文体积</dt><dd>{fmtBytes(book.content_bytes)}</dd>{/if}
        {#if book.image_bytes}<dt>图片体积</dt><dd>{fmtBytes(book.image_bytes)}</dd>{/if}
        {#if book.collected_at}<dt>入馆日期</dt><dd>{fmtDate(book.collected_at)}</dd>{/if}
        {#if book.added_at}<dt>加入书架</dt><dd>{fmtDate(book.added_at)}</dd>{/if}
      </dl>

      {#if (book.tags || []).length}
        <div class="sub-block">
          <div class="sub-title">标签</div>
          <div class="tag-list">
            {#each book.tags as tag}
              <a class="chip" href={href(buildPath('/library', { tag }))}>{tag}</a>
            {/each}
          </div>
        </div>
      {/if}

      {#if (book.identifiers || []).length}
        <div class="sub-block">
          <div class="sub-title">标识</div>
          <dl class="meta-list">
            {#each book.identifiers as id}
              <dt>{id.scheme}</dt><dd>{id.value}</dd>
            {/each}
          </dl>
        </div>
      {/if}
    </div>

    <div class="card">
      <div class="row" style="margin-bottom:8px">
        <h3 style="margin:0">目录</h3>
        <div class="spacer"></div>
        <span class="muted small">{toc.length} 章</span>
        {#if toc.length > 30}
          <button class="ghost mini" on:click={() => (tocOpen = !tocOpen)}>{tocOpen ? '收起' : '展开'}</button>
        {/if}
      </div>
      {#if !toc.length}
        <p class="muted">这本书还没有目录数据。</p>
      {:else}
        <ol class="toc" class:expanded={tocOpen}>
          {#each toc as ch, i}
            {#if tocOpen || i < 30}
              <li>
                <span class="toc-index">{i + 1}</span>
                <span class="toc-title">{ch.title || ch.url}</span>
                {#if i === Number(book.chapter_index)}<span class="badge">当前</span>{/if}
              </li>
            {/if}
          {/each}
        </ol>
        {#if !tocOpen && toc.length > 30}
          <p class="muted small">仅显示前 30 章，展开查看全部。</p>
        {/if}
      {/if}
    </div>
  </div>

  {#if book.intro}
    <div class="card intro-card">
      <h3>简介</h3>
      <p class="intro">{book.intro}</p>
    </div>
  {/if}
{/if}

{#if showEditor && book}
  <BookEditor
    bookUrl={book.book_url}
    on:close={() => (showEditor = false)}
    on:saved={() => {
      showEditor = false;
      load();
    }}
  />
{/if}

{#if showExport && book}
  <ExportDialog bookUrl={book.book_url} name={book.name || ''} on:close={() => (showExport = false)} />
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
  .hero { display: flex; gap: 18px; align-items: flex-start; margin-bottom: 14px; }
  .cover-wrap {
    position: relative;
    flex: 0 0 150px;
    height: 214px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--soft-bg, #eee);
  }
  .cover-ph {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: 28px;
    font-weight: 600;
  }
  .cover { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .info { flex: 1; min-width: 0; display: grid; gap: 8px; }
  .book-title { margin: 0; font-size: 20px; line-height: 1.35; overflow-wrap: anywhere; }
  .meta-line { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .link-badge { text-decoration: none; }
  .progress-line { display: grid; gap: 4px; max-width: 420px; }
  .pg-track { height: 8px; border: 1px solid var(--border); border-radius: 4px; overflow: hidden; }
  .pg-fill { height: 100%; background: var(--accent); }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 4px; }
  .status-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 4px; }
  .mini { min-height: 32px; padding: 4px 12px; font-size: 12px; }
  .status-chip { min-height: 32px; padding: 4px 14px; }
  .status-chip.on { background: var(--accent); color: var(--on-accent, #fff); border-color: var(--accent); }
  .detail-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-bottom: 14px; }
  .meta-list { display: grid; grid-template-columns: max-content 1fr; gap: 6px 14px; margin: 0; font-size: 13px; }
  .meta-list dt { color: var(--muted); }
  .meta-list dd { margin: 0; overflow-wrap: anywhere; }
  .sub-block { margin-top: 14px; }
  .sub-title { font-size: 12px; color: var(--muted); margin-bottom: 6px; }
  .tag-list { display: flex; gap: 6px; flex-wrap: wrap; }
  .toc { margin: 0; padding: 0; list-style: none; max-height: 360px; overflow-y: auto; }
  .toc.expanded { max-height: none; }
  .toc li { display: flex; align-items: baseline; gap: 8px; padding: 5px 0; border-bottom: 1px solid var(--border); font-size: 13px; }
  .toc li:last-child { border-bottom: 0; }
  .toc-index { color: var(--muted); font-size: 11px; min-width: 2.4em; text-align: right; font-variant-numeric: tabular-nums; }
  .toc-title { overflow-wrap: anywhere; }
  .intro-card { margin-bottom: 14px; }
  .intro { margin: 0; white-space: pre-wrap; line-height: 1.75; font-size: 14px; }
  .small { font-size: 12px; }
  @media (max-width: 760px) {
    .hero { flex-direction: column; }
    .cover-wrap { flex: 0 0 auto; width: 140px; height: 200px; }
    .detail-grid { grid-template-columns: 1fr; }
  }
</style>
