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

  // ---- 多选 / 分页 ----
  // 选中集合按 book_id 保存，翻页、切换分组/排序都不会丢；批量接口逐项返回成功/失败。
  let selectMode = false;
  let selectedIds = [];
  const PAGE_SIZE = 24;
  let page = 1;
  let batchBusy = '';
  let batchMsg = '';
  let batchErr = '';
  let batchFailures = [];
  let moveTarget = '';

  // ---- 批量刷新目录 ----
  let tocBusy = false;
  let tocDone = 0;
  let tocTotal = 0;
  let tocLive = [];
  let tocResult = null;
  let tocError = '';
  const TOC_CLIENT_CONCURRENCY = 6; // 不超过后端上限 8

  const lastReadKey = (id) => `oread_lastread:${id}`;

  async function load() {
    loading = true;
    error = '';
    try {
      // GET /api/bookshelf 已返回 group / chapter_index / position / last_read_at，
      // 不再逐本请求 /bookshelf/progress。
      books = (await api.bookshelf()) || [];
      // 已不在书架的书从选中集合里剔除，避免批量操作打到不存在的书
      selectedIds = selectedIds.filter((id) => books.some((b) => b.book_id === id));
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

  // 分页：选中集合与当前页码无关，翻页后仍保留
  $: pageCount = Math.max(1, Math.ceil(shownView.length / PAGE_SIZE));
  $: safePage = Math.min(page, pageCount);
  $: pageView = shownView.slice((safePage - 1) * PAGE_SIZE, safePage * PAGE_SIZE);
  $: pageIds = pageView.map((v) => v.b.book_id);
  $: selectedOnPage = pageIds.filter((id) => selectedIds.includes(id)).length;

  function isSelected(id) {
    return selectedIds.includes(id);
  }

  function toggleSelect(id) {
    selectedIds = isSelected(id) ? selectedIds.filter((x) => x !== id) : [...selectedIds, id];
  }

  function selectPage() {
    selectedIds = [...new Set([...selectedIds, ...pageIds])];
  }

  function invertPage() {
    const rest = selectedIds.filter((id) => !pageIds.includes(id));
    selectedIds = [...rest, ...pageIds.filter((id) => !selectedIds.includes(id))];
  }

  function clearSelection() {
    selectedIds = [];
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
  }

  function toggleSelectMode() {
    selectMode = !selectMode;
    if (!selectMode) clearSelection();
  }

  function failureLines(results) {
    return (results || [])
      .filter((r) => !r.ok)
      .map((r) => {
        const book = books.find((b) => b.book_id === r.book_id);
        return `${book ? book.title || book.book_id : r.book_id}：${r.error || '失败'}`;
      });
  }

  function applyBatchResult(label, r) {
    const ok = Number(r && r.updated) || 0;
    const fail = Number(r && r.failed) || 0;
    batchMsg = `${label}：成功 ${ok} 本` + (fail ? `，失败 ${fail} 本` : '');
    batchFailures = failureLines(r && r.results).slice(0, 20);
  }

  async function batchRemove() {
    const n = selectedIds.length;
    if (!n) return;
    if (!confirm(`从书架移除选中的 ${n} 本书？\n不会删除阅读记录与正文缓存；重复删除是幂等的。`)) return;
    batchBusy = 'remove';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.shelfBatch('remove', selectedIds);
      applyBatchResult(`移除 ${n} 本`, r);
      if (!Number(r && r.failed)) selectedIds = [];
      await load();
    } catch (e) {
      batchErr = `批量移除失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  async function batchMove() {
    const n = selectedIds.length;
    if (!n) return;
    const target = moveTarget;
    const label = target ? `移入「${target}」` : '移出分组';
    if (!confirm(`将选中的 ${n} 本书${label}？`)) return;
    batchBusy = 'move';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.shelfBatch(target ? 'set_group' : 'remove_group', selectedIds, { group: target });
      applyBatchResult(`移动 ${n} 本`, r);
      await load();
    } catch (e) {
      batchErr = `批量移动失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  async function batchUpdate(enabled) {
    const n = selectedIds.length;
    if (!n) return;
    const word = enabled ? '启用' : '停用';
    if (
      !confirm(
        `将选中的 ${n} 本书${word}目录更新？` +
          (enabled ? '' : '\n停用后，批量「更新目录」会跳过这些书。')
      )
    )
      return;
    batchBusy = enabled ? 'enable' : 'disable';
    batchMsg = '';
    batchErr = '';
    batchFailures = [];
    try {
      const r = await api.shelfBatch('set_update', selectedIds, { enabled });
      applyBatchResult(`${word}更新 ${n} 本`, r);
      await load();
    } catch (e) {
      batchErr = `批量${word}更新失败：${e.message}`;
    } finally {
      batchBusy = '';
    }
  }

  // 前端按 6 个并发逐本调用 /book/toc/batch：每完成一本就更新进度，
  // 避免一次性等待全部书籍导致长时间白屏。
  async function batchToc() {
    const n = selectedIds.length;
    if (!n) return;
    if (
      !confirm(
        `刷新选中的 ${n} 本书目录？\n并发 ${Math.min(TOC_CLIENT_CONCURRENCY, n)} 本，逐本显示新增/删除章节。`
      )
    )
      return;
    const targets = books.filter((b) => selectedIds.includes(b.book_id));
    await refreshToc(targets);
  }

  // 真正的目录刷新执行器：手动「更新目录」与「自动刷新」共用。
  async function refreshToc(targets) {
    if (!targets.length || tocBusy) return;
    tocBusy = true;
    tocResult = null;
    tocError = '';
    tocDone = 0;
    tocTotal = targets.length;
    tocLive = [];
    const results = [];
    let cursor = 0;
    async function worker() {
      while (true) {
        const i = cursor++;
        if (i >= targets.length) return;
        const b = targets[i];
        try {
          const r = await api.tocBatch([b.book_id]);
          const o = r && Array.isArray(r.results) ? r.results[0] : null;
          results.push(
            o
              ? { ...o, title: b.title || b.book_id }
              : { book_url: b.book_id, title: b.title || b.book_id, status: 'failed', error: '无返回' }
          );
        } catch (e) {
          results.push({
            book_url: b.book_id,
            title: b.title || b.book_id,
            status: 'failed',
            error: e.message,
          });
        }
        tocDone = results.length;
        tocLive = results.slice();
      }
    }
    try {
      const workers = Array.from(
        { length: Math.min(TOC_CLIENT_CONCURRENCY, targets.length) },
        () => worker()
      );
      await Promise.all(workers);
      const sum = (k) => results.reduce((a, r) => a + (Number(r[k]) || 0), 0);
      tocResult = {
        results,
        updated: results.filter((r) => r.status === 'updated').length,
        failed: results.filter((r) => r.status === 'failed').length,
        skipped: results.filter((r) => r.status === 'skipped').length,
        added: sum('added'),
        removed: sum('removed'),
      };
      await load();
    } catch (e) {
      tocError = `批量更新目录失败：${e.message}`;
    } finally {
      tocBusy = false;
    }
  }

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
      // 后端在同一事务里改分组名并迁移 bookshelf / book_meta，避免中途失败留下半套数据
      await api.saveGroup({ id: g.group_id, name });
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

  // 自动刷新（对应设置页 auto_refresh；Legado 语义是“打开软件时自动更新书籍”）：
  // 打开书架即在本机检查一次，24 小时内最多触发一次，只刷新未停更的书。
  const AUTO_REFRESH_KEY = 'oread_autorefresh_last';
  const AUTO_REFRESH_INTERVAL_MS = 24 * 60 * 60 * 1000;

  async function maybeAutoRefresh() {
    if (tocBusy) return;
    try {
      const s = await api.settings();
      if (!s || !s.auto_refresh) return;
      const last = Number(localStorage.getItem(AUTO_REFRESH_KEY) || 0);
      if (last > 0 && Date.now() - last < AUTO_REFRESH_INTERVAL_MS) return;
      const targets = books.filter((b) => b.enable_update !== false);
      if (!targets.length) return;
      // 先落时间戳，避免多标签页同时打开书架时重复刷新
      localStorage.setItem(AUTO_REFRESH_KEY, String(Date.now()));
      await refreshToc(targets);
    } catch (e) {
      // 自动刷新属于后台行为，失败不打断书架浏览
    }
  }

  onMount(async () => {
    await load();
    loadGroups();
    maybeAutoRefresh();
  });
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">我的书架</h2>
  <div class="spacer"></div>
  <span class="muted" style="font-size:12px">{books.length} 本</span>
  <button class="ghost" on:click={toggleSelectMode}>{selectMode ? '退出多选' : '批量管理'}</button>
  <button class="ghost" on:click={openGroups}>分组管理</button>
  <button class="ghost" on:click={load} disabled={loading}>刷新</button>
</div>

<div class="row" style="margin-bottom:6px">
  <select
    bind:value={groupFilter}
    on:change={() => (page = 1)}
    style="flex:1;min-width:0"
    aria-label="分组筛选"
  >
    <option value="all">全部书籍</option>
    <option value="ungrouped">未分组</option>
    {#each groupOptions as g}
      <option value={g}>{g}</option>
    {/each}
  </select>
  <select
    bind:value={sortBy}
    on:change={() => (page = 1)}
    style="flex:1;min-width:0"
    aria-label="排序方式"
  >
    <option value="default">默认（最近阅读）</option>
    <option value="progress">阅读进度</option>
    <option value="recent">最近阅读</option>
    <option value="title">书名</option>
    <option value="author">作者</option>
  </select>
</div>
<p class="muted tiny-note">分组与最近阅读时间来自书架接口；本机打开过的书记录会作为补充。</p>

{#if selectMode}
  <div class="batch-bar">
    <div class="batch-line">
      <strong>已选 {selectedIds.length} 本</strong>
      <span class="muted">（本页 {selectedOnPage}/{pageView.length}，跨页保持）</span>
      <button class="ghost mini" on:click={selectPage} disabled={!pageView.length}>全选本页</button>
      <button class="ghost mini" on:click={invertPage} disabled={!pageView.length}>反选本页</button>
      <button class="ghost mini" on:click={clearSelection} disabled={!selectedIds.length}>清空</button>
    </div>
    <div class="batch-line">
      <button on:click={batchToc} disabled={!selectedIds.length || tocBusy || !!batchBusy}>
        {tocBusy ? '目录刷新中…' : '更新目录'}
      </button>
      <button class="ghost mini" on:click={() => batchUpdate(true)} disabled={!selectedIds.length || !!batchBusy || tocBusy}>
        启用更新
      </button>
      <button class="ghost mini" on:click={() => batchUpdate(false)} disabled={!selectedIds.length || !!batchBusy || tocBusy}>
        停用更新
      </button>
      <select bind:value={moveTarget} aria-label="目标分组" class="batch-group">
        <option value="">未分组</option>
        {#each groupOptions as g}
          <option value={g}>{g}</option>
        {/each}
      </select>
      <button class="ghost mini" on:click={batchMove} disabled={!selectedIds.length || !!batchBusy || tocBusy}>移到分组</button>
      <button class="danger mini" on:click={batchRemove} disabled={!selectedIds.length || !!batchBusy || tocBusy}>删除</button>
    </div>
  </div>
{/if}

{#if tocBusy || tocResult || tocError}
  <div class="toc-progress">
    {#if tocBusy}
      <div class="batch-line">
        <strong>更新目录：{tocDone}/{tocTotal}</strong>
        <progress value={tocDone} max={tocTotal || 1}></progress>
      </div>
      <ul class="toc-list">
        {#each tocLive as r}
          <li>{r.title || r.book_url}：{r.status === 'updated' ? `目录 ${r.before_total}→${r.after_total} 章（+${r.added}/-${r.removed}）` : r.status === 'skipped' ? `跳过：${r.error || ''}` : `失败：${r.error || ''}`}</li>
        {/each}
      </ul>
    {/if}
    {#if tocError}<p class="err">{tocError}</p>{/if}
    {#if tocResult}
      <p class={tocResult.failed ? 'err' : 'ok-line'}>
        目录刷新：成功 {tocResult.updated} 本，跳过 {tocResult.skipped} 本，失败 {tocResult.failed} 本；新增 {tocResult.added} 章，删除 {tocResult.removed} 章。
      </p>
      <ul class="toc-list">
        {#each tocResult.results.slice(0, 20) as r}
          <li>{r.title || r.book_url}：{r.status === 'updated' ? `${r.before_total}→${r.after_total} 章（+${r.added}/-${r.removed}）` : r.status === 'skipped' ? `跳过：${r.error || ''}` : `失败：${r.error || ''}`}</li>
        {/each}
      </ul>
      {#if tocResult.results.length > 20}
        <p class="muted tiny-note">仅显示前 20 本，共 {tocResult.results.length} 本。</p>
      {/if}
    {/if}
  </div>
{/if}

{#if batchMsg}<p class="ok-line">{batchMsg}</p>{/if}
{#if batchErr}<p class="err">{batchErr}</p>{/if}
{#if batchFailures.length}
  <ul class="fail-list">
    {#each batchFailures as line}<li>{line}</li>{/each}
  </ul>
{/if}

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if books.length === 0 && !loading}
  <div class="empty">书架还是空的，去「发现」里搜索并加入一本书吧。</div>
{:else if shown.length === 0 && !loading}
  <div class="empty">该分组下没有书籍。</div>
{:else}
  <div class="grid auto">
    {#each pageView as v (v.b.book_id)}
      {@const b = v.b}
      <div class="card book">
        {#if selectMode}
          <label class="sel-lab">
            <input type="checkbox" checked={isSelected(b.book_id)} on:change={() => toggleSelect(b.book_id)} />
            <span>选择</span>
          </label>
        {/if}
        <button class="cover-btn" on:click={() => open(b)} aria-label="阅读《{b.title || b.book_id}》">
          <div class="cover-wrap">
            <span class="cover-ph" aria-hidden="true">{(b.title || '?').slice(0, 2)}</span>
            {#if b.cover}
              <img class="cover" src={b.cover} alt="" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
            {/if}
            {#if Number(b.new_chapters) > 0}
              <span class="new-badge" title="有 {b.new_chapters} 章未读新章节">{Number(b.new_chapters) > 99 ? '99+' : b.new_chapters}</span>
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
          {#if b.enable_update === false}<span class="badge">停更</span>{/if}
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
  {#if pageCount > 1}
    <div class="pager">
      <button class="ghost mini" on:click={() => (page = Math.max(1, safePage - 1))} disabled={safePage <= 1}>上一页</button>
      <span class="muted">第 {safePage} / {pageCount} 页 · 共 {shownView.length} 本</span>
      <button class="ghost mini" on:click={() => (page = Math.min(pageCount, safePage + 1))} disabled={safePage >= pageCount}>下一页</button>
    </div>
  {/if}
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
  /* 批量工具栏：全部用块级/行内块，墨水屏不依赖 flex/grid */
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
  .batch-line strong { margin-right: 6px; }
  .batch-line .mini { padding: 6px 10px; font-size: 12px; }
  .batch-group { width: auto; min-width: 116px; display: inline-block; padding: 6px 8px; vertical-align: middle; }
  .batch-line progress { width: 100%; height: 10px; margin-top: 6px; display: block; }
  .toc-progress {
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--card);
    padding: 10px 12px;
    margin: 0 0 10px;
  }
  .toc-list, .fail-list { margin: 6px 0 0; padding-left: 20px; font-size: 12px; }
  .toc-list li, .fail-list li { line-height: 1.5; word-break: break-word; }
  .fail-list { color: #b3392f; }
  .pager { margin: 14px 0 4px; text-align: center; }
  .pager button { margin: 0 4px; }
  .card.book { position: relative; }
  .sel-lab {
    position: absolute;
    top: 8px;
    left: 8px;
    z-index: 2;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 2px 6px;
    font-size: 11px;
  }
  .sel-lab input { width: auto; margin: 0 4px 0 0; vertical-align: middle; }
  .sel-lab span { vertical-align: middle; }
  .new-badge {
    position: absolute;
    top: 6px;
    right: 6px;
    min-width: 20px;
    padding: 2px 5px;
    background: #b3392f;
    color: #fff;
    border-radius: 10px;
    font-size: 11px;
    line-height: 16px;
    text-align: center;
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
