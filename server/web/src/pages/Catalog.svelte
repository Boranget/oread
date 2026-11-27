<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { route, navigate, href, buildPath } from '../lib/router.js';
  import BookCard from '../lib/BookCard.svelte';
  import BookCover from '../lib/BookCover.svelte';

  const dispatch = createEventDispatcher();
  function dispatchRead(book) {
    dispatch('read', book);
  }

  // library = 全部馆藏（找书/借阅）；shelf = 我的书架（已借阅的书 + 个人阅读状态）
  export let mode = 'library';
  export let role = 'user';
  $: isShelf = mode === 'shelf';
  $: isAdmin = role === 'admin';
  $: basePath = isShelf ? '/shelf' : '/library';

  const STATUS_TABS = [
    { id: 'all', label: '全部' },
    { id: 'none', label: '未标记' },
    { id: 'want', label: '想读' },
    { id: 'reading', label: '在读' },
    { id: 'finished', label: '已读' },
  ];
  const SORTS = [
    { id: 'recent', label: '最近阅读' },
    { id: 'added', label: '最近加入' },
    { id: 'title', label: '书名' },
    { id: 'author', label: '作者' },
    { id: 'series', label: '系列' },
    { id: 'progress', label: '进度' },
  ];
  const DIMS = [
    { key: 'author', label: '作者' },
    { key: 'series', label: '系列' },
    { key: 'tag', label: '标签' },
    { key: 'publisher', label: '出版社' },
    { key: 'language', label: '语言' },
  ];
  const PAGE_SIZE = 24;
  const STATUS_LABEL = { want: '想读', reading: '在读', finished: '已读' };

  let data = null;
  let loading = true;
  let error = '';
  let qInput = '';

  // 多选只作用于当前页，避免跨页选中后批量接口打到不在列表里的书。
  let selectMode = false;
  let selected = [];
  let batchBusy = '';
  let batchMsg = '';
  let batchErr = '';
  let batchFailures = [];
  let moveTarget = '';

  let groups = [];
  let showGroups = false;
  let groupBusy = false;
  let groupErr = '';
  let groupMsg = '';
  let newGroupName = '';
  let renameDraft = {};

  // 自动刷新（对应设置页 auto_refresh；Legado 语义是“打开软件时自动更新书籍”）。
  // 打开图书馆即在本机检查一次，24 小时内最多触发一次，只刷新未停更的书。
  const AUTO_REFRESH_KEY = 'oread_autorefresh_last';
  const AUTO_REFRESH_INTERVAL_MS = 24 * 60 * 60 * 1000;

  let view = 'grid';
  onMount(() => {
    try {
      const v = localStorage.getItem('oread_catalog_view');
      if (v === 'grid' || v === 'list') view = v;
    } catch {
      /* ignore */
    }
    maybeAutoRefresh();
  });

  async function maybeAutoRefresh() {
    try {
      const s = await api.settings();
      if (!s || !s.auto_refresh) return;
      const last = Number(localStorage.getItem(AUTO_REFRESH_KEY) || 0);
      if (last > 0 && Date.now() - last < AUTO_REFRESH_INTERVAL_MS) return;
      const shelf = (await api.bookshelf()) || [];
      const targets = shelf.filter((b) => b.enable_update !== false);
      if (!targets.length) return;
      // 先落时间戳，避免多标签页同时打开时重复刷新。
      localStorage.setItem(AUTO_REFRESH_KEY, String(Date.now()));
      for (const b of targets) {
        try {
          await api.tocBatch([b.book_id]);
        } catch {
          /* 单本失败不打断，下次打开再试 */
        }
      }
      await load();
    } catch {
      // 自动刷新属于后台行为，失败不打断浏览
    }
  }

  $: query = $route.query;
  // 个人阅读状态只在书架页生效；图书馆只负责全局馆藏。
  $: status = isShelf ? query.status || 'all' : 'all';
  $: page = Math.max(1, Number(query.page) || 1);
  $: sort = query.sort || 'recent';
  $: group = isShelf ? query.group || 'all' : 'all';
  $: q = query.q || '';
  $: shelfOnly = isShelf;
  $: dims = DIMS.map((d) => ({ ...d, value: query[d.key] || '' })).filter((d) => d.value);
  $: qInput = q;

  $: queryKey = JSON.stringify({
    mode,
    status,
    page,
    sort,
    group,
    q,
    author: query.author || '',
    series: query.series || '',
    tag: query.tag || '',
    publisher: query.publisher || '',
    language: query.language || '',
  });
  let lastQueryKey = '';
  $: if (queryKey !== lastQueryKey) {
    lastQueryKey = queryKey;
    selected = [];
    selectMode = false;
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    load();
  }

  $: items = (data && data.items) || [];
  $: total = Number(data && data.total) || 0;
  $: pages = Number(data && data.pages) || 0;
  $: selectedBooks = items.filter((b) => selected.includes(b.book_url));
  $: selectedOnShelf = selectedBooks.filter((b) => b.on_shelf);
  $: selectedOffShelf = selectedBooks.filter((b) => !b.on_shelf);
  $: groupOptions = [...new Set([...groups.map((g) => g.group_name), ...items.map((b) => b.group)].filter(Boolean))];

  async function load() {
    loading = true;
    error = '';
    try {
      data = await api.libraryCatalog({
        status,
        group,
        author: query.author,
        series: query.series,
        tag: query.tag,
        publisher: query.publisher,
        language: query.language,
        q,
        sort,
        page,
        limit: PAGE_SIZE,
        shelf: shelfOnly ? '1' : '',
      });
    } catch (e) {
      error = e.message;
      data = null;
    } finally {
      loading = false;
    }
  }

  function apply(patch, { resetPage = true } = {}) {
    const next = { ...$route.query, ...patch };
    if (resetPage) delete next.page;
    navigate(buildPath(basePath, next));
  }

  function submitSearch(e) {
    if (e) e.preventDefault();
    apply({ q: qInput.trim() });
  }

  function setStatus(id) {
    apply({ status: id === 'all' ? undefined : id });
  }

  function setSort(value) {
    apply({ sort: value === 'recent' ? undefined : value });
  }

  function setGroup(value) {
    apply({ group: value === 'all' ? undefined : value });
  }

  function clearDim(key) {
    apply({ [key]: undefined });
  }

  function clearAll() {
    navigate(basePath);
  }

  function goPage(p) {
    if (p < 1 || (pages && p > pages) || p === page) return;
    const next = { ...$route.query, page: p === 1 ? undefined : p };
    navigate(buildPath(basePath, next));
  }

  function toggleView() {
    view = view === 'grid' ? 'list' : 'grid';
    try {
      localStorage.setItem('oread_catalog_view', view);
    } catch {
      /* ignore */
    }
  }

  function toggleSelectMode() {
    selectMode = !selectMode;
    if (!selectMode) clearSelection();
  }

  function clearSelection() {
    selected = [];
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
  }

  function onSelect(e) {
    const { book, checked } = e.detail;
    const url = book.book_url;
    selected = checked ? [...new Set([...selected, url])] : selected.filter((x) => x !== url);
  }

  // 单本借阅 / 归还：借阅 = 加入当前账号书架，归还 = 移出书架（保留馆藏与阅读记录）
  function selectPage() {
    selected = items.map((b) => b.book_url);
  }

  function failureLines(results) {
    return (results || [])
      .filter((r) => !r.ok)
      .map((r) => {
        const book = items.find((b) => b.book_url === r.book_id);
        return `${book ? book.name || book.book_url : r.book_id}：${r.error || '失败'}`;
      });
  }

  function applyBatchResult(label, r) {
    const ok = Number(r && r.updated) || 0;
    const fail = Number(r && r.failed) || 0;
    batchMsg = `${label}：成功 ${ok} 本` + (fail ? `，失败 ${fail} 本` : '');
    batchFailures = failureLines(r && r.results).slice(0, 20);
  }

  async function batchStatus(value) {
    if (!selected.length) return;
    batchBusy = 'status';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.setReadingStatus(selected, value);
      batchMsg = value ? `已标记 ${r.updated} 本` : `已清除 ${r.updated} 本的状态`;
      await load();
    } catch (e) {
      batchErr = `批量设置状态失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  async function batchAddShelf() {
    const targets = selectedOffShelf;
    if (!targets.length) return;
    batchBusy = 'shelf';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    const failures = [];
    let ok = 0;
    for (const b of targets) {
      try {
        await api.addToShelf({
          book_id: b.book_url,
          source_id: b.origin || '',
          name: b.name,
          author: b.author,
          cover: b.cover_url,
        });
        ok += 1;
      } catch (e) {
        failures.push(`${b.name || b.book_url}：${e.message}`);
      }
    }
    batchMsg = `借阅：成功 ${ok} 本` + (failures.length ? `，失败 ${failures.length} 本` : '');
    batchFailures = failures.slice(0, 20);
    batchBusy = '';
    await load();
  }

  async function batchMove() {
    const targets = selectedOnShelf;
    if (!targets.length) return;
    const target = moveTarget;
    const label = target ? `移入「${target}」` : '移出分组';
    if (!confirm(`将选中的 ${targets.length} 本书${label}？`)) return;
    batchBusy = 'move';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.shelfBatch(
        target ? 'set_group' : 'remove_group',
        targets.map((b) => b.book_url),
        { group: target }
      );
      applyBatchResult(`移动 ${targets.length} 本`, r);
      await load();
    } catch (e) {
      batchErr = `批量移动失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  async function batchRemove() {
    const targets = selectedOnShelf;
    if (!targets.length) return;
    if (!confirm(`从书架归还选中的 ${targets.length} 本书？\n不会删除阅读记录与正文缓存；馆藏内容不受影响。`)) return;
    batchBusy = 'remove';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.shelfBatch('remove', targets.map((b) => b.book_url));
      applyBatchResult(`归还 ${targets.length} 本`, r);
      await load();
    } catch (e) {
      batchErr = `批量归还失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  async function loadGroups() {
    try {
      groups = (await api.groups()) || [];
    } catch (e) {
      groupErr = e.message;
    }
  }

  function openGroups() {
    showGroups = true;
    groupErr = '';
    groupMsg = '';
    loadGroups();
  }

  async function createGroup() {
    const name = newGroupName.trim();
    if (!name) return;
    groupBusy = true;
    groupErr = '';
    groupMsg = '';
    try {
      await api.addGroup({ name });
      newGroupName = '';
      groupMsg = `已新建「${name}」`;
      await Promise.all([loadGroups(), load()]);
    } catch (e) {
      groupErr = e.message;
    } finally {
      groupBusy = false;
    }
  }

  async function renameGroup(g) {
    const name = (renameDraft[g.group_id] || '').trim();
    if (!name || name === g.group_name) return;
    groupBusy = true;
    groupErr = '';
    groupMsg = '';
    try {
      await api.saveGroup({ id: g.group_id, name });
      delete renameDraft[g.group_id];
      groupMsg = `已改名为「${name}」`;
      await Promise.all([loadGroups(), load()]);
    } catch (e) {
      groupErr = e.message;
    } finally {
      groupBusy = false;
    }
  }

  async function removeGroup(g) {
    const inGroup = items.filter((b) => (b.group || '') === g.group_name);
    const ok = confirm(
      `删除分组「${g.group_name}」？\n书不会丢失，只会移出该分组。` +
        (inGroup.length ? `\n本页有 ${inGroup.length} 本书在该分组。` : '')
    );
    if (!ok) return;
    groupBusy = true;
    groupErr = '';
    groupMsg = '';
    try {
      if (inGroup.length) await api.moveToGroup(inGroup.map((b) => b.book_url), '');
      await api.deleteGroup(g.group_id);
      groupMsg = `已删除「${g.group_name}」`;
      await Promise.all([loadGroups(), load()]);
    } catch (e) {
      groupErr = e.message;
    } finally {
      groupBusy = false;
    }
  }
</script>

<div class="page-head">
  <h2>{isShelf ? '我的书架' : '图书馆'}</h2>
  <span class="page-count">{data ? (isShelf ? `${total} 本在书架` : `${total} 本`) : ''}</span>
  <div class="page-actions">
    {#if !isShelf}
      <a class="ghost-link" href={href('/browse')}>浏览维度</a>
      {#if isAdmin}
        <a class="ghost-link" href={href('/acquire')}>入馆 / 采购</a>
      {/if}
    {/if}
    <button class="ghost" on:click={toggleSelectMode}>{selectMode ? '退出多选' : '批量管理'}</button>
    {#if isShelf}
      <button class="ghost" on:click={openGroups}>分组管理</button>
    {/if}
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>
</div>

{#if isShelf}
  <div class="seg status-tabs">
    {#each STATUS_TABS as t (t.id)}
      <button class:on={status === t.id} on:click={() => setStatus(t.id)}>{t.label}</button>
    {/each}
  </div>
{/if}

<div class="toolbar">
  <form class="search-form" on:submit={submitSearch}>
    <input
      bind:value={qInput}
      placeholder={isShelf ? '搜索我的书架' : '搜索书名、作者、系列、标签'}
      aria-label={isShelf ? '搜索书架' : '搜索馆藏'}
    />
  </form>
  <select value={sort} on:change={(e) => setSort(e.currentTarget.value)} aria-label="排序">
    {#each SORTS as s (s.id)}
      <option value={s.id}>{s.label}</option>
    {/each}
  </select>
  {#if isShelf}
    <select value={group} on:change={(e) => setGroup(e.currentTarget.value)} aria-label="分组">
      <option value="all">全部分组</option>
      <option value="ungrouped">未分组</option>
      {#each groupOptions as g}
        <option value={g}>{g}</option>
      {/each}
    </select>
  {/if}
  <button class="ghost view-toggle" on:click={toggleView}>{view === 'grid' ? '列表' : '网格'}</button>
</div>

{#if dims.length || q}
  <div class="active-filters">
    {#if q}
      <button class="chip on" on:click={() => apply({ q: undefined })}>搜索：{q}<span class="x">×</span></button>
    {/if}
    {#each dims as d (d.key)}
      <button class="chip on" on:click={() => clearDim(d.key)}>{d.label}：{d.value}<span class="x">×</span></button>
    {/each}
    <button class="ghost mini" on:click={clearAll}>清除全部</button>
  </div>
{/if}

{#if selectMode}
  <div class="batch-bar">
    <div class="batch-line">
      <strong>已选 {selected.length} 本</strong>
      <span class="muted">（本页 {items.length} 本）</span>
      <button class="ghost mini" on:click={selectPage} disabled={!items.length}>全选本页</button>
      <button class="ghost mini" on:click={clearSelection} disabled={!selected.length}>清空</button>
    </div>
    {#if isShelf}
      <div class="batch-line">
        <span class="muted">状态</span>
        <button class="ghost mini" on:click={() => batchStatus('want')} disabled={!selected.length || !!batchBusy}>想读</button>
        <button class="ghost mini" on:click={() => batchStatus('reading')} disabled={!selected.length || !!batchBusy}>在读</button>
        <button class="ghost mini" on:click={() => batchStatus('finished')} disabled={!selected.length || !!batchBusy}>已读</button>
        <button class="ghost mini" on:click={() => batchStatus('')} disabled={!selected.length || !!batchBusy}>清除状态</button>
      </div>
    {/if}
    <div class="batch-line">
      {#if isShelf}
        <select bind:value={moveTarget} aria-label="目标分组" class="batch-group">
          <option value="">未分组</option>
          {#each groupOptions as g}
            <option value={g}>{g}</option>
          {/each}
        </select>
        <button class="ghost mini" on:click={batchMove} disabled={!selectedOnShelf.length || !!batchBusy}>移到分组</button>
        <button class="danger mini" on:click={batchRemove} disabled={!selectedOnShelf.length || !!batchBusy}>归还选中</button>
      {:else}
        <button class="ghost mini" on:click={batchAddShelf} disabled={!selectedOffShelf.length || !!batchBusy}>借阅选中</button>
      {/if}
    </div>
  </div>
{/if}

{#if batchMsg}<p class="ok-line">{batchMsg}</p>{/if}
{#if batchErr}<p class="err">{batchErr}</p>{/if}
{#if batchFailures.length}
  <ul class="fail-list">
    {#each batchFailures as line}<li>{line}</li>{/each}
  </ul>
{/if}

{#if error}
  <div class="card">
    <p class="err">{error}</p>
    <button class="ghost" on:click={load}>重试</button>
  </div>
{:else if loading && !data}
  <p class="loading">加载中…</p>
{:else if !items.length}
  <div class="card empty-card">
    {#if q || dims.length || status !== 'all' || group !== 'all' || shelfOnly}
      <b>没有符合条件的书</b>
      <p class="muted">换个筛选条件，或清除全部筛选后再看。</p>
      <button class="ghost" on:click={clearAll}>清除筛选</button>
    {:else}
      <b>馆藏还是空的</b>
      <p class="muted">馆藏是所有人共享的内容层。管理员导入 EPUB 后，这里会出现全部书籍。</p>
      {#if isAdmin}
        <a class="btn-link" href={href('/acquire?tab=local')}>去导入</a>
      {:else}
        <p class="muted">请联系管理员导入书籍。</p>
      {/if}
    {/if}
  </div>
{:else if view === 'grid'}
  <div class="grid auto">
    {#each items as book (book.book_url)}
      <BookCard
        {book}
        selectable={selectMode}
        selected={selected.includes(book.book_url)}
        on:select={onSelect}
        on:read={(e) => dispatchRead(e.detail)}
      />
    {/each}
  </div>
{:else}
  <div class="list">
    {#each items as book (book.book_url)}
      {@const pct = Math.max(0, Math.min(100, Number(book.reading_progress) || 0))}
      <div class="card list-row">
        {#if selectMode}
          <input
            type="checkbox"
            class="pick"
            checked={selected.includes(book.book_url)}
            on:change={(e) => onSelect({ detail: { book, checked: e.currentTarget.checked } })}
            aria-label="选择《{book.name || book.book_url}》"
          />
        {/if}
        <a class="thumb" href={href(`/book?url=${encodeURIComponent(book.book_url)}`)}>
          <span class="thumb-ph">{(book.name || '?').slice(0, 2)}</span>
          <BookCover url={book.cover_url} imgClass="cover" />
        </a>
        <div class="row-main">
          <a class="row-title" href={href(`/book?url=${encodeURIComponent(book.book_url)}`)}>{book.name || book.book_url}</a>
          <div class="muted small">
            {book.author || '未知作者'}
            {#if book.series_name} · {book.series_name}{#if book.series_index != null} #{book.series_index}{/if}{/if}
            {#if book.publisher} · {book.publisher}{/if}
          </div>
          <div class="row-meta">
            {#if book.status}<span class="badge">{STATUS_LABEL[book.status] || book.status}</span>{/if}
            {#if book.group}<span class="badge">{book.group}</span>{/if}
            {#if book.on_shelf}<span class="badge">在书架</span>{/if}
            <span class="muted small">{pct > 0 ? `已读 ${pct.toFixed(0)}%` : '未开始'}</span>
          </div>
        </div>
        <div class="row-actions">
          <button class="ghost mini" on:click={() => dispatchRead(book)}>阅读</button>
          <a class="ghost mini link-btn" href={href(`/book?url=${encodeURIComponent(book.book_url)}`)}>详情</a>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if pages > 1}
  <div class="pager">
    <button class="ghost mini" on:click={() => goPage(page - 1)} disabled={page <= 1}>上一页</button>
    <span class="muted">第 {page} / {pages} 页 · 共 {total} 本</span>
    <button class="ghost mini" on:click={() => goPage(page + 1)} disabled={page >= pages}>下一页</button>
  </div>
{/if}

{#if showGroups}
  <div class="modal-mask" on:click|self={() => (showGroups = false)}>
    <div class="modal" role="dialog" aria-modal="true" aria-label="分组管理">
      <div class="row" style="margin-bottom:12px">
        <b>分组管理</b>
        <div class="spacer"></div>
        <button class="ghost" on:click={() => (showGroups = false)}>关闭</button>
      </div>
      <div class="row" style="margin-bottom:10px">
        <input placeholder="新分组名" bind:value={newGroupName} on:keydown={(e) => e.key === 'Enter' && createGroup()} />
        <button on:click={createGroup} disabled={groupBusy || !newGroupName.trim()}>新建</button>
      </div>
      {#if groupErr}<p class="err">{groupErr}</p>{/if}
      {#if groupMsg}<p class="ok-line">{groupMsg}</p>{/if}
      {#if !groups.length}
        <p class="muted">还没有分组。新建后，可在批量管理里把书架上的书移入分组。</p>
      {/if}
      {#each groups as g (g.group_id)}
        {@const count = items.filter((b) => (b.group || '') === g.group_name).length}
        <div class="gm-row">
          <div class="gm-name">
            <b>{g.group_name}</b>
            <span class="muted small">本页 {count} 本</span>
          </div>
          <div class="row">
            <input placeholder={`改名为…（当前 ${g.group_name}）`} bind:value={renameDraft[g.group_id]} />
            <button class="ghost" on:click={() => renameGroup(g)} disabled={groupBusy}>改名</button>
            <button class="ghost" on:click={() => removeGroup(g)} disabled={groupBusy}>删除</button>
          </div>
        </div>
      {/each}
    </div>
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
  .status-tabs { overflow-x: auto; }
  .status-tabs button { white-space: nowrap; }
  .search-form { flex: 2 1 220px; }
  .search-form input { width: 100%; }
  .view-toggle { flex: 0 0 auto; }
  .active-filters { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 0 0 12px; }
  .active-filters .chip { display: inline-flex; align-items: center; gap: 4px; }
  .active-filters .x { font-size: 15px; line-height: 1; }
  .batch-bar {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--card);
    padding: 10px 12px;
    margin: 0 0 10px;
  }
  .batch-line { margin: 0 0 8px; line-height: 1.9; }
  .batch-line:last-child { margin-bottom: 0; }
  .batch-line > * { margin: 4px 6px 0 0; }
  .batch-group { width: auto; min-width: 116px; display: inline-block; padding: 6px 8px; vertical-align: middle; }
  .fail-list { margin: 6px 0 0; padding-left: 20px; font-size: 12px; color: var(--err, #b3392f); }
  .fail-list li { line-height: 1.5; word-break: break-word; }
  .empty-card { padding: 26px 20px; display: grid; gap: 10px; justify-items: start; }
  .empty-card p { margin: 0; max-width: 560px; line-height: 1.6; }
  .btn-link {
    display: inline-flex;
    align-items: center;
    padding: 8px 16px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--on-accent, #fff);
    text-decoration: none;
    font-size: 14px;
  }
  .list { display: grid; gap: 10px; }
  .list-row { display: flex; align-items: center; gap: 12px; padding: 10px 12px; }
  .pick { width: auto; margin: 0; }
  .mini { padding: 6px 10px; font-size: 12px; min-height: 32px; line-height: 1; }
  .thumb {
    position: relative;
    flex: 0 0 54px;
    height: 76px;
    border-radius: 6px;
    overflow: hidden;
    background: var(--soft-bg, #eee);
    display: block;
  }
  .thumb-ph {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: 15px;
    font-weight: 600;
  }
  .thumb img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .row-main { flex: 1; min-width: 0; display: grid; gap: 3px; }
  .row-title { color: var(--fg); text-decoration: none; font-weight: 600; overflow-wrap: anywhere; }
  .row-title:hover { color: var(--accent); }
  .row-meta { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .row-actions { display: flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
  .link-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 32px;
    padding: 6px 10px;
    border: 1px solid var(--accent);
    color: var(--accent);
    background: transparent;
    text-decoration: none;
    border-radius: 8px;
    line-height: 1;
  }
  .link-btn:hover { background: var(--accent-weak, rgba(179, 57, 47, 0.08)); }
  .small { font-size: 12px; }
  .pager { margin: 14px 0 4px; text-align: center; }
  .pager button { margin: 0 4px; }
  .gm-row { padding: 10px 0; border-top: 1px solid var(--border); }
  .gm-name { display: flex; align-items: baseline; gap: 8px; margin-bottom: 8px; }
  .gm-row .row { gap: 6px; }
  .gm-row input { flex: 1; min-width: 0; }
</style>
