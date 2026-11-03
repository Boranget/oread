<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import ExportDialog from '../lib/ExportDialog.svelte';

  const dispatch = createEventDispatcher();
  let books = [];
  let error = '';
  let loading = false;

  let groupFilter = 'all'; // all | ungrouped | <书架分组名>
  let sortBy = 'default'; // default | title | author | progress | recent

  // 导出 TXT：卡片入口，弹窗内先展示缓存/缺失章节再确认下载
  let exportBook = null;

  // 分组管理
  let groups = [];
  let showGroups = false;
  let groupBusy = false;
  let groupErr = '';
  let groupMsg = '';
  let newGroupName = '';
  let renameDraft = {};

  const lastReadKey = (id) => `oread_lastread:${id}`;

  async function load() {
    loading = true;
    error = '';
    try {
      // GET /api/bookshelf 已返回 group / chapter_index / position / last_read_at，
      // 不再逐本请求 /bookshelf/progress。
      books = (await api.bookshelf()) || [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function apiRecentAt(s) {
    if (!s) return 0;
    const iso = String(s).trim().replace(' ', 'T');
    const t = Date.parse(/Z|\+\d/i.test(iso) ? iso : iso + 'Z');
    return Number.isFinite(t) ? t : 0;
  }

  function readRecentAt(b) {
    const local = Number(localStorage.getItem(lastReadKey(b.book_id)) || 0);
    // 没读过时用 added_at 兜底，和列表默认排序 COALESCE(last_read_at, added_at) 一致
    return Math.max(local > 0 ? local : 0, apiRecentAt(b.last_read_at), apiRecentAt(b.added_at));
  }

  // 卡片文案要区分“最近阅读”和“加入书架”，避免把 added_at 说成阅读时间
  function activityOf(b) {
    const local = Number(localStorage.getItem(lastReadKey(b.book_id)) || 0);
    const read = Math.max(local > 0 ? local : 0, apiRecentAt(b.last_read_at));
    if (read) return { ts: read, label: '最近阅读' };
    const added = apiRecentAt(b.added_at);
    return added ? { ts: added, label: '加入' } : null;
  }

  function fmtRecent(ts) {
    if (!ts) return '';
    const diff = Date.now() - ts;
    const min = Math.floor(diff / 60000);
    if (min < 1) return '刚刚';
    if (min < 60) return `${min} 分钟前`;
    const hour = Math.floor(min / 60);
    if (hour < 24) return `${hour} 小时前`;
    const day = Math.floor(hour / 24);
    if (day < 30) return `${day} 天前`;
    return new Date(ts).toLocaleDateString();
  }

  $: groupOptions = [...new Set([...groups.map((g) => g.group_name), ...books.map((b) => b.group)].filter(Boolean))];

  // 后端 reading_progress 已是 (chapter_index + clamp(position,0,1))/total*100，
  // 直接展示，前端不再用 position 二次换算（否则重复计算）。
  function readPct(b) {
    const v = Number(b.reading_progress);
    return Number.isFinite(v) ? Math.min(100, Math.max(0, v)) : 0;
  }

  $: filtered = books.filter((b) => {
    const g = b.group || '';
    if (groupFilter === 'all') return true;
    if (groupFilter === 'ungrouped') return !g;
    return g === groupFilter;
  });

  $: shown = sortBy === 'default' ? filtered : [...filtered].sort((a, b) => {
    if (sortBy === 'title') return String(a.title || '').localeCompare(String(b.title || ''), 'zh');
    if (sortBy === 'author') return String(a.author || '').localeCompare(String(b.author || ''), 'zh');
    if (sortBy === 'progress') {
      return readPct(b) - readPct(a);
    }
    return readRecentAt(b) - readRecentAt(a);
  });

  // 书架接口已提供分组与最近阅读时间，本机记录只作为补充
  $: shownView = shown.map((b) => ({
    b,
    group: b.group || '',
    act: activityOf(b),
    pct: readPct(b),
  }));

  function open(b) {
    localStorage.setItem(lastReadKey(b.book_id), String(Date.now()));
    dispatch('open', {
      bookUrl: b.book_id,
      sourceId: b.source_id,
      name: b.title,
      author: b.author,
      coverUrl: b.cover,
    });
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

  async function loadGroups() {
    try {
      groups = (await api.groups()) || [];
    } catch (e) {
      groupErr = e.message;
    }
  }

  async function moveBook(b, name) {
    if ((b.group || '') === name) return;
    error = '';
    try {
      await api.moveToGroup([b.book_id], name);
      await load();
      groupMsg = name ? `已移入「${name}」` : '已移出分组';
    } catch (e) {
      error = e.message;
    }
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
      const ids = books.filter((b) => (b.group || '') === g.group_name).map((b) => b.book_id);
      await api.saveGroup({ id: g.group_id, name });
      // 书的 group 是字符串字段，改名后要把原组内书一起迁到新名字
      if (ids.length) await api.moveToGroup(ids, name);
      delete renameDraft[g.group_id];
      groupMsg = `已改名为「${name}」` + (ids.length ? `，${ids.length} 本书同步迁移` : '');
      await Promise.all([loadGroups(), load()]);
    } catch (e) {
      groupErr = e.message;
    } finally {
      groupBusy = false;
    }
  }

  async function removeGroup(g) {
    const inGroup = books.filter((b) => (b.group || '') === g.group_name);
    const ok = confirm(
      `删除分组「${g.group_name}」？\n书不会丢失，只会移出该分组。` +
        (inGroup.length ? `\n将先移出 ${inGroup.length} 本书。` : '')
    );
    if (!ok) return;
    groupBusy = true;
    groupErr = '';
    groupMsg = '';
    try {
      if (inGroup.length) await api.moveToGroup(inGroup.map((b) => b.book_id), '');
      await api.deleteGroup(g.group_id);
      groupMsg = `已删除「${g.group_name}」，书籍已移出分组`;
      await Promise.all([loadGroups(), load()]);
    } catch (e) {
      groupErr = e.message;
    } finally {
      groupBusy = false;
    }
  }

  function openGroups() {
    showGroups = true;
    groupErr = '';
    groupMsg = '';
    loadGroups();
  }

  onMount(() => {
    load();
    loadGroups();
  });
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">我的书架</h2>
  <div class="spacer"></div>
  <span class="muted" style="font-size:12px">{books.length} 本</span>
  <button class="ghost" on:click={openGroups}>分组管理</button>
  <button class="ghost" on:click={load} disabled={loading}>刷新</button>
</div>

<div class="row" style="margin-bottom:6px">
  <select bind:value={groupFilter} style="flex:1;min-width:0" aria-label="分组筛选">
    <option value="all">全部书籍</option>
    <option value="ungrouped">未分组</option>
    {#each groupOptions as g}
      <option value={g}>{g}</option>
    {/each}
  </select>
  <select bind:value={sortBy} style="flex:1;min-width:0" aria-label="排序方式">
    <option value="default">默认（最近阅读）</option>
    <option value="progress">阅读进度</option>
    <option value="recent">最近阅读</option>
    <option value="title">书名</option>
    <option value="author">作者</option>
  </select>
</div>
<p class="muted tiny-note">分组与最近阅读时间来自书架接口；本机打开过的书记录会作为补充。</p>

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if books.length === 0 && !loading}
  <div class="empty">书架还是空的，去「发现」里搜索并加入一本书吧。</div>
{:else if shown.length === 0 && !loading}
  <div class="empty">该分组下没有书籍。</div>
{:else}
  <div class="grid auto">
    {#each shownView as v (v.b.book_id)}
      {@const b = v.b}
      <div class="card book">
        <button class="cover-btn" on:click={() => open(b)} aria-label="阅读《{b.title || b.book_id}》">
          <div class="cover-wrap">
            <span class="cover-ph" aria-hidden="true">{(b.title || '?').slice(0, 2)}</span>
            {#if b.cover}
              <img class="cover" src={b.cover} alt="" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
            {/if}
          </div>
        </button>
        <div class="title" on:click={() => open(b)}>{b.title || b.book_id}</div>
        <div class="author">{b.author || '未知作者'}</div>
        <div class="shelf-meta">
          <span class="prog-line">
            {#if b.chapter_index}
              读到第 {b.chapter_index + 1} 章
            {:else}
              未开始阅读
            {/if}
          </span>
          {#if v.group}<span class="badge">{v.group}</span>{/if}
        </div>
        {#if v.pct > 0}
          <div class="pg-track" aria-hidden="true"><div class="pg-fill" style={`width:${v.pct}%`}></div></div>
          <div class="muted" style="font-size:11px">阅读进度 {v.pct.toFixed(0)}%</div>
        {/if}
        {#if v.act}
          <div class="muted" style="font-size:11px">{v.act.label}：{fmtRecent(v.act.ts)}</div>
        {/if}
        <select
          class="group-select"
          value={v.group}
          on:change={(e) => moveBook(b, e.target.value)}
          aria-label="《{b.title || b.book_id}》的分组"
        >
          <option value="">未分组</option>
          {#each groupOptions as g}
            <option value={g}>{g}</option>
          {/each}
        </select>
        <div class="card-actions">
          <button class="read-btn" on:click={() => open(b)}>阅读</button>
          <button class="ghost mini" on:click={() => (exportBook = b)}>导出</button>
          <button class="ghost mini" on:click={() => remove(b)}>移除</button>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if showGroups}
  <div class="gm-mask" on:click|self={() => (showGroups = false)}></div>
  <div class="gm-panel" role="dialog" aria-label="分组管理">
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
      <p class="muted">还没有分组。新建一个后，可在书籍卡片上用下拉把书移入。</p>
    {/if}
    {#each groups as g (g.group_id)}
      {@const count = books.filter((b) => (b.group || '') === g.group_name).length}
      <div class="gm-row">
        <div class="gm-name">
          <b>{g.group_name}</b>
          <span class="muted tiny-note">{count} 本</span>
        </div>
        <div class="row">
          <input placeholder={`改名为…（当前 ${g.group_name}）`} bind:value={renameDraft[g.group_id]} />
          <button class="ghost" on:click={() => renameGroup(g)} disabled={groupBusy}>改名</button>
          <button class="ghost" on:click={() => removeGroup(g)} disabled={groupBusy}>删除</button>
        </div>
      </div>
    {/each}
    <p class="muted tiny-note" style="margin-top:10px">
      删除分组不会删除书籍，会先把组内书移出；后端允许同名分组，这里按 id 区分。
    </p>
  </div>
{/if}

{#if exportBook}
  <ExportDialog
    bookUrl={exportBook.book_id}
    name={exportBook.title || ''}
    on:close={() => (exportBook = null)}
  />
{/if}

<style>
  .pg-track { height: 6px; margin-top: 6px; border: 1px solid var(--border); border-radius: 3px; overflow: hidden; }
  .pg-fill { height: 100%; background: #333; }
  .group-select { margin-top: 8px; width: 100%; font-size: 12px; padding: 6px 8px; min-height: 34px; }
  .gm-mask { position: fixed; top: 0; right: 0; bottom: 0; left: 0; background: rgba(0, 0, 0, 0.35); z-index: 40; }
  .gm-panel {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    z-index: 41;
    width: min(560px, calc(100% - 24px));
    max-height: min(640px, calc(100% - 40px));
    overflow-y: auto;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 16px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.18);
  }
  .gm-row { padding: 10px 0; border-top: 1px solid var(--border); }
  .gm-name { display: flex; align-items: baseline; gap: 8px; margin-bottom: 8px; }
  .gm-row .row { gap: 6px; }
  .gm-row input { flex: 1; min-width: 0; }
  /* 卡片操作：阅读独占一行，导出/移除并排，避免窄卡片里文字被挤成竖排 */
  .card-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
  .card-actions button {
    white-space: nowrap;
  }
  .card-actions .read-btn {
    flex: 1 1 100%;
  }
  .card-actions .mini {
    flex: 0 1 auto;
    padding: 8px 12px;
    font-size: 13px;
  }
</style>
