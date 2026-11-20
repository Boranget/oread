<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { api, getRole } from '../lib/api.js';
  import { describeFailure } from '../lib/apiErrors.js';

  const dispatch = createEventDispatcher();
  // 入馆 / 移出 / 删除 / 回收临时缓存都要管理员；浏览与占用所有登录账号可读。
  const isAdmin = getRole() === 'admin';

  const TIERS = [
    { id: 'collection', label: '馆藏' },
    { id: 'temp', label: '临时缓存' },
    { id: 'all', label: '全部' },
  ];
  const SORTS = [
    { id: 'accessed', label: '最近访问' },
    { id: 'collected', label: '入馆时间' },
  ];
  const STATUS_LABEL = {
    queued: '排队中',
    running: '下载中',
    done: '已完成',
    partial: '部分完成',
    failed: '失败',
    cancelled: '已取消',
  };

  let tier = 'collection';
  let sort = 'accessed';
  let q = '';
  let stats = null;
  let statsErr = '';
  let items = [];
  let nextCursor = null;
  let loading = false;
  let loadingMore = false;
  let error = '';
  let notice = '';
  let busyUrl = '';
  let estimatingUrl = '';
  let armedUrl = '';
  let armedTimer = null;
  let jobs = [];
  let jobsErr = '';
  let jobsLoading = false;
  let pollTimer = null;

  onMount(() => {
    loadStats();
    reload();
    if (isAdmin) loadJobs();
    // 采购单在服务端后台跑：页面停留时轮询进度即可，不依赖前端保持下载。
    pollTimer = setInterval(() => {
      if (isAdmin && hasActiveJobs()) loadJobs();
    }, 5000);
    return () => clearInterval(pollTimer);
  });

  onDestroy(() => {
    clearInterval(pollTimer);
    clearTimeout(armedTimer);
  });

  function hasActiveJobs() {
    return jobs.some((j) => j.status === 'queued' || j.status === 'running');
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024 * 1024) return (v / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  function fmtDate(ts) {
    const n = Number(ts) || 0;
    if (!n) return '—';
    const d = new Date(n);
    const p = (x) => String(x).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }

  function tierLabel(t) {
    return t === 'collection' ? '馆藏' : '临时';
  }

  function isComic(b) {
    if (Number(b.book_source_type) === 2) return true;
    const kind = String(b.kind || '').toLowerCase();
    return kind === 'comic' || kind === 'manga' || kind === '漫画';
  }

  function fmtSpace(v) {
    return v == null ? '未知' : fmtBytes(v);
  }

  function jobDone(j) {
    const done = Number(j.done || 0) + Number(j.skipped || 0);
    const total = Number(j.total || 0);
    return total ? Math.min(total, done) : done;
  }

  function jobPercent(j) {
    if (!j.total) return null;
    return Math.min(100, Math.round((jobDone(j) * 100) / j.total));
  }

  async function loadStats() {
    statsErr = '';
    try {
      stats = await api.libraryStats();
    } catch (e) {
      statsErr = describeFailure(e, '读取馆藏占用');
    }
  }

  async function loadJobs() {
    if (!isAdmin) return;
    jobsLoading = true;
    jobsErr = '';
    try {
      const res = await api.libraryJobs({ limit: 100 });
      jobs = res.items || [];
    } catch (e) {
      jobsErr = describeFailure(e, '读取采购单');
    } finally {
      jobsLoading = false;
    }
  }

  async function reload() {
    items = [];
    nextCursor = null;
    await loadPage(true);
    loadStats();
  }

  async function loadPage(reset) {
    if (reset) {
      loading = true;
      error = '';
      notice = '';
    } else {
      loadingMore = true;
    }
    try {
      const res = await api.libraryBooks({
        tier,
        sort,
        q,
        cursor: reset ? '' : nextCursor || '',
        limit: 50,
      });
      const page = res.items || [];
      items = reset ? page : items.concat(page);
      nextCursor = res.next_cursor || null;
    } catch (e) {
      error = describeFailure(e, reset ? '读取馆藏' : '加载更多');
    } finally {
      loading = false;
      loadingMore = false;
    }
  }

  function setTier(t) {
    if (tier === t) return;
    tier = t;
    reload();
  }

  function setSort(s) {
    if (sort === s) return;
    sort = s;
    reload();
  }

  function search() {
    reload();
  }

  function disarm() {
    armedUrl = '';
    clearTimeout(armedTimer);
  }

  async function collect(b) {
    if (!isAdmin) return;
    error = '';
    notice = '';
    disarm();
    // 漫画先抽样预估：只是提示，不硬拦入馆；小说走原流程。
    if (isComic(b)) {
      estimatingUrl = b.book_url;
      try {
        const est = await api.libraryComicEstimate(b.book_url, b.origin || '');
        if (est.comic !== false) {
          const low = fmtSpace(est.estimated_low_bytes ?? est.estimated_bytes);
          const high = fmtSpace(est.estimated_high_bytes ?? est.estimated_bytes);
          const free = fmtSpace(est.disk_free_bytes);
          const enough = est.enough_space === false ? '\n空间可能不足，建议先清理。' : '';
          const body = low === '未知'
            ? '无法取得足够样本，体积只能按实际下载计算。'
            : `预计占 ${low}～${high}，当前剩余 ${free}。`;
          if (!window.confirm(`${body}${enough}\n\n抽样估算，以实际下载为准。确定入馆吗？`)) return;
        }
      } catch (e) {
        if (!window.confirm(`体积预检失败：${describeFailure(e, '体积预估')}\n仍要入馆吗？`)) return;
      } finally {
        estimatingUrl = '';
      }
    }
    busyUrl = b.book_url;
    try {
      const res = await api.libraryCollect(b.book_url, b.origin || '');
      notice = res.queued
        ? '已入馆，后台整本下载已排队'
        : '已入馆；这本书缺少书源记录，未能排队下载，可在书架打开一次后再入馆';
      await reload();
      if (isAdmin) loadJobs();
    } catch (e) {
      error = describeFailure(e, '入馆');
    } finally {
      busyUrl = '';
    }
  }

  async function release(b) {
    if (!isAdmin) return;
    busyUrl = b.book_url;
    error = '';
    notice = '';
    disarm();
    try {
      await api.libraryRelease(b.book_url);
      notice = '已移出馆藏，转为临时缓存（按 TTL 回收或手动删除）';
      await reload();
      if (isAdmin) loadJobs();
    } catch (e) {
      error = describeFailure(e, '移出馆藏');
    } finally {
      busyUrl = '';
    }
  }

  async function removeBook(b) {
    if (!isAdmin) return;
    if (armedUrl !== b.book_url) {
      armedUrl = b.book_url;
      clearTimeout(armedTimer);
      armedTimer = setTimeout(() => (armedUrl = ''), 4000);
      return;
    }
    disarm();
    busyUrl = b.book_url;
    error = '';
    notice = '';
    try {
      const res = await api.libraryDelete(b.book_url);
      notice = `已删除：${res.deleted_chapters} 章目录 / ${res.deleted_contents} 章正文，释放 ${fmtBytes(res.bytes_freed)}`;
      await reload();
      if (isAdmin) loadJobs();
    } catch (e) {
      error = describeFailure(e, '删除馆藏');
    } finally {
      busyUrl = '';
    }
  }

  async function cleanTemp() {
    if (!isAdmin) return;
    busyUrl = '*';
    error = '';
    notice = '';
    try {
      const res = await api.libraryCleanTemp();
      notice = res.books
        ? `已回收 ${res.books} 本过期临时缓存（${res.contents} 章），释放 ${fmtBytes(res.bytes_freed)}`
        : '没有过期的临时缓存';
      await reload();
    } catch (e) {
      error = describeFailure(e, '回收临时缓存');
    } finally {
      busyUrl = '';
    }
  }

  async function cancelJob(j) {
    if (!isAdmin) return;
    error = '';
    notice = '';
    try {
      const res = await api.libraryJobCancel(j.id);
      notice = res.cancelled ? '已取消该采购单' : '采购单已结束，无需取消';
      loadJobs();
    } catch (e) {
      error = describeFailure(e, '取消采购单');
    }
  }
</script>

<div class="page-head">
  <h2>馆藏</h2>
  <div class="page-actions">
    <button class="ghost" on:click={() => { loadStats(); loadPage(true); }} disabled={loading}>刷新</button>
  </div>
</div>

{#if statsErr}
  <p class="err">{statsErr}</p>
{/if}

{#if stats}
  <div class="lib-stats">
    <div class="card lib-stat">
      <div class="lib-stat-label">馆藏（永久）</div>
      <div class="lib-stat-num">{stats.collection.books} 本</div>
      <div class="muted tiny-note">{stats.collection.chapters} 章 · {fmtBytes(stats.collection.bytes)}</div>
    </div>
    <div class="card lib-stat">
      <div class="lib-stat-label">临时缓存</div>
      <div class="lib-stat-num">{stats.temp.books} 本</div>
      <div class="muted tiny-note">{stats.temp.chapters} 章 · {fmtBytes(stats.temp.bytes)} · {stats.temp.ttl_days} 天未访问回收</div>
    </div>
    <div class="card lib-stat">
      <div class="lib-stat-label">磁盘剩余</div>
      <div class="lib-stat-num">{stats.disk.free_mb == null ? '未知' : fmtBytes(stats.disk.free_bytes)}</div>
      <div class="muted tiny-note">安全阀 {stats.disk.floor_mb} MB</div>
    </div>
  </div>
  {#if stats.disk.download_paused}
    <div class="warn-box">
      <div class="warn-title">后台整本下载已暂停</div>
      <div class="warn-item">磁盘剩余空间低于安全阀（{stats.disk.floor_mb} MB）。阅读与临时缓存不受影响，空间恢复后采购单会自动继续。</div>
    </div>
  {/if}
  {#if Number(stats.temp.expired_books) > 0}
    <p class="muted tiny-note">有 {stats.temp.expired_books} 本临时缓存已超过 TTL，等待定期回收或手动回收。</p>
  {/if}
{/if}

<div class="seg lib-tabs">
  {#each TIERS as t}
    <button class:on={tier === t.id} on:click={() => setTier(t.id)}>{t.label}</button>
  {/each}
</div>

<div class="toolbar">
  <input class="grow" placeholder="搜索书名或作者" bind:value={q} on:keydown={(e) => e.key === 'Enter' && search()} />
  <select value={sort} on:change={(e) => setSort(e.currentTarget.value)} aria-label="排序">
    {#each SORTS as s}
      <option value={s.id}>{s.label}</option>
    {/each}
  </select>
  <button class="ghost" on:click={search} disabled={loading}>搜索</button>
  {#if isAdmin && tier !== 'collection'}
    <button class="ghost" on:click={cleanTemp} disabled={busyUrl === '*'}>回收过期临时缓存</button>
  {/if}
</div>

{#if !isAdmin}
  <div class="warn-box readonly">
    <div class="warn-title">只读模式：入馆与移出需要管理员</div>
    <div class="warn-item">你可以浏览馆藏、查看占用与下载进度；入馆、移出馆藏、删除整本、回收临时缓存仅管理员可操作。</div>
  </div>
{/if}

{#if error}<p class="err">{error}</p>{/if}
{#if notice}<p class="ok-line">{notice}</p>{/if}

{#if loading && items.length === 0}
  <p class="loading">加载中…</p>
{:else if items.length === 0}
  <div class="empty">
    {tier === 'collection' ? '馆藏还是空的。在书架或发现页把书入馆后，服务端会后台下载整本。' : '没有符合条件的书。'}
  </div>
{:else}
  <div class="lib-list">
    {#each items as b (b.book_url)}
      <div class="card lib-book">
        <div class="lib-book-main">
          <div class="lib-book-head">
            <b class="lib-book-name">{b.name || b.book_url}</b>
            <span class="badge">{tierLabel(b.tier)}</span>
            {#if b.origin_name}<span class="muted tiny-note">{b.origin_name}</span>{/if}
          </div>
          <div class="muted tiny-note">
            {b.author || '佚名'} · 已缓存 {b.cached_chapters} / {b.total_chapter_num || '?'} 章 · 正文 {fmtBytes(b.content_bytes)}{#if b.image_bytes} · 图片 {fmtBytes(b.image_bytes)}{/if}
            · 最近访问 {fmtDate(b.last_accessed_at)}
            {#if b.tier === 'collection' && b.collected_at} · 入馆 {fmtDate(b.collected_at)}{/if}
          </div>
          {#if b.download_percent != null}
            <div class="lib-meter" aria-hidden="true"><div style={`width:${b.download_percent}%`}></div></div>
          {/if}
        </div>
        {#if isAdmin}
          <div class="lib-book-actions">
            {#if b.tier === 'collection'}
              <button class="ghost" on:click={() => release(b)} disabled={!!busyUrl}>移出馆藏</button>
            {:else}
              <button on:click={() => collect(b)} disabled={!!busyUrl || estimatingUrl === b.book_url}>{estimatingUrl === b.book_url ? '预估体积…' : '入馆'}</button>
              <button class="ghost" on:click={() => removeBook(b)} disabled={!!busyUrl}>
                {armedUrl === b.book_url ? '再次点击确认删除' : '删除'}
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
  {#if nextCursor}
    <div class="lib-more">
      <button class="ghost" on:click={() => loadPage(false)} disabled={loadingMore}>
        {loadingMore ? '加载中…' : '加载更多'}
      </button>
    </div>
  {/if}
{/if}

{#if isAdmin}
  <div class="card lib-jobs">
    <div class="row" style="margin-bottom:8px">
      <b>采购单（后台整本下载）</b>
      <div class="spacer"></div>
      <button class="ghost" on:click={loadJobs} disabled={jobsLoading}>{jobsLoading ? '读取中…' : '刷新'}</button>
    </div>
    {#if jobsErr}<p class="err">{jobsErr}</p>{/if}
    {#if jobs.length === 0}
      <p class="muted tiny-note">还没有采购单。入馆一本书后，服务端会排队下载整本。</p>
    {:else}
      <div class="lib-job-list">
        {#each jobs as j (j.id)}
          <div class="lib-job">
            <div class="lib-job-main">
              <div class="lib-book-head">
                <b class="lib-book-name">{j.book_url}</b>
                <span class="badge">{STATUS_LABEL[j.status] || j.status}</span>
              </div>
              <div class="muted tiny-note">
                {j.done} 新下载 · 已完成 {jobDone(j)} / {j.total || '?'} 章 · 跳过 {j.skipped} · 失败 {j.failed}
                {#if j.error} · {j.error}{/if}
              </div>
              {#if jobPercent(j) != null}
                <div class="lib-meter" aria-hidden="true"><div style={`width:${jobPercent(j)}%`}></div></div>
              {/if}
            </div>
            {#if j.status === 'queued' || j.status === 'running'}
              <button class="ghost" on:click={() => cancelJob(j)}>取消</button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .lib-stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 12px;
    margin-bottom: 12px;
  }
  .lib-stat { padding: 14px 16px; }
  .lib-stat-label { font-size: 12px; color: var(--muted); }
  .lib-stat-num { font-size: 22px; font-weight: 700; line-height: 1.35; margin: 2px 0; }
  .lib-tabs { margin-top: 4px; }
  .lib-list { display: grid; gap: 10px; }
  .lib-book {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 14px;
    margin: 0;
  }
  .lib-book-main { flex: 1; min-width: 0; display: grid; gap: 4px; }
  .lib-book-head { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; min-width: 0; }
  .lib-book-name { overflow-wrap: anywhere; }
  .lib-book-actions { display: flex; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
  .lib-meter { height: 6px; border: 1px solid var(--border); background: transparent; }
  .lib-meter div { height: 100%; background: var(--accent); }
  .lib-more { display: flex; justify-content: center; margin: 12px 0; }
  .lib-jobs { margin-top: 16px; }
  .lib-job-list { display: grid; gap: 10px; }
  .lib-job {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .lib-job:first-child { border-top: 0; }
  .lib-job-main { flex: 1; min-width: 0; display: grid; gap: 4px; }
  .readonly { margin-bottom: 12px; }
  @media (max-width: 560px) {
    .lib-book, .lib-job { flex-direction: column; }
    .lib-book-actions { justify-content: flex-start; }
  }
</style>
