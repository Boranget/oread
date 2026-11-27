<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../../lib/api.js';
  import { route, navigate, href, buildPath } from '../../lib/router.js';
  import { searchJobs } from '../../lib/searchJobsStore.js';

  export let role = 'user';

  const dispatch = createEventDispatcher();
  const isAdmin = role === 'admin';

  const TABS = [
    { id: 'overview', label: '总览' },
    { id: 'collection', label: '馆藏维护' },
    { id: 'sources', label: '书源' },
    { id: 'replace', label: '替换规则' },
    { id: 'users', label: '用户' },
    { id: 'discover', label: '发现搜书' },
  ];

  const LOADERS = {
    collection: () => import('./CollectionAdmin.svelte'),
    sources: () => import('../Sources.svelte'),
    replace: () => import('../ReplaceRules.svelte'),
    users: () => import('./Users.svelte'),
    discover: () => import('../Discover.svelte'),
  };

  let comps = {};
  let pending = {};
  let errs = {};

  let stats = null;
  let home = null;
  let overviewErr = '';
  let overviewLoading = false;

  $: tab = TABS.some((t) => t.id === $route.query.tab) ? $route.query.tab : 'overview';
  $: if (isAdmin && tab !== 'overview') ensure(tab);

  onMount(() => {
    if (isAdmin) loadOverview();
  });

  function ensure(id) {
    if (comps[id] || pending[id] || !LOADERS[id]) return;
    pending[id] = true;
    errs[id] = '';
    LOADERS[id]()
      .then((mod) => {
        comps[id] = mod.default;
      })
      .catch((e) => {
        errs[id] = `页面加载失败：${e && e.message ? e.message : e}`;
      })
      .finally(() => {
        pending[id] = false;
      });
  }

  function setTab(id) {
    navigate(buildPath('/admin', { tab: id === 'overview' ? undefined : id }));
  }

  async function loadOverview() {
    overviewLoading = true;
    overviewErr = '';
    try {
      const [s, h] = await Promise.all([api.libraryStats(), api.libraryHome()]);
      stats = s;
      home = h;
    } catch (e) {
      overviewErr = e.message;
    } finally {
      overviewLoading = false;
    }
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024 * 1024) return (v / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }
</script>

<div class="page-head">
  <h2>管理</h2>
  <span class="page-count">仅管理员</span>
  <div class="page-actions">
    <a class="ghost-link" href={href('/library')}>返回图书馆</a>
    {#if tab === 'overview'}
      <button class="ghost" on:click={loadOverview} disabled={overviewLoading}>{overviewLoading ? '读取中…' : '刷新'}</button>
    {/if}
  </div>
</div>

{#if !isAdmin}
  <div class="card">
    <p class="err">需要管理员权限。</p>
    <a class="ghost-link" href={href('/')}>返回首页</a>
  </div>
{:else}
  <div class="seg admin-tabs">
    {#each TABS as t (t.id)}
      <button class:on={tab === t.id} on:click={() => setTab(t.id)}>
        {t.label}{#if t.id === 'discover' && $searchJobs.active > 0}<span class="tab-badge">{$searchJobs.active}</span>{/if}
      </button>
    {/each}
  </div>

  {#if tab === 'overview'}
    {#if overviewErr}<p class="err">{overviewErr}</p>{/if}
    {#if overviewLoading && !stats}
      <p class="loading">读取中…</p>
    {:else}
      <div class="overview-grid">
        <a class="card ov-card" href={href('/admin?tab=collection')}>
          <span class="ov-label">馆藏</span>
          <span class="ov-num">{stats ? stats.collection.books : '—'} 本</span>
          <span class="muted small">{stats ? `${stats.collection.chapters} 章 · ${fmtBytes(stats.collection.bytes)}` : ''}</span>
        </a>
        <a class="card ov-card" href={href('/admin?tab=collection')}>
          <span class="ov-label">临时缓存</span>
          <span class="ov-num">{stats ? stats.temp.books : '—'} 本</span>
          <span class="muted small">{stats ? `${fmtBytes(stats.temp.bytes)} · ${stats.temp.ttl_days} 天回收` : ''}</span>
        </a>
        <div class="card ov-card">
          <span class="ov-label">磁盘剩余</span>
          <span class="ov-num">{stats && stats.disk.free_mb != null ? fmtBytes(stats.disk.free_bytes) : '未知'}</span>
          <span class="muted small">{stats ? `安全阀 ${stats.disk.floor_mb} MB` : ''}</span>
        </div>
        <a class="card ov-card" href={href('/admin?tab=discover')}>
          <span class="ov-label">搜索任务</span>
          <span class="ov-num">{$searchJobs.active} 个运行中</span>
          <span class="muted small">共 {$searchJobs.jobs.length} 条记录</span>
        </a>
      </div>

      {#if home && home.stats}
        <div class="card my-stats">
          <div class="row" style="margin-bottom:8px">
            <b>我的阅读</b>
            <div class="spacer"></div>
            <a class="more" href={href('/library')}>去图书馆</a>
          </div>
          <div class="mini-stats">
            <a href={href('/library?status=reading')}><b>{home.stats.reading}</b><span>在读</span></a>
            <a href={href('/library?status=want')}><b>{home.stats.want}</b><span>想读</span></a>
            <a href={href('/library?status=finished')}><b>{home.stats.finished}</b><span>已读</span></a>
            <a href={href('/library')}><b>{home.stats.shelf}</b><span>在书架</span></a>
          </div>
        </div>
      {/if}

      {#if stats && stats.disk.download_paused}
        <div class="warn-box">
          <div class="warn-title">后台整本下载已暂停</div>
          <div class="warn-item">磁盘剩余空间低于安全阀（{stats.disk.floor_mb} MB）。阅读与临时缓存不受影响，空间恢复后采购单会自动继续。</div>
        </div>
      {/if}

      <div class="card">
        <h3>管理入口</h3>
        <div class="quick-links">
          <button class="ghost" on:click={() => setTab('collection')}>导入 EPUB / CBZ / 维护馆藏</button>
          <button class="ghost" on:click={() => setTab('sources')}>书源管理</button>
          <button class="ghost" on:click={() => setTab('replace')}>替换规则</button>
          <button class="ghost" on:click={() => setTab('users')}>用户</button>
          <button class="ghost" on:click={() => setTab('discover')}>发现搜书</button>
        </div>
      </div>
    {/if}
  {:else if comps[tab]}
    <svelte:component
      this={comps[tab]}
      {role}
      on:open={(e) => dispatch('read', e.detail)}
      on:read={(e) => dispatch('read', e.detail)}
    />
  {:else if errs[tab]}
    <div class="card">
      <p class="err">{errs[tab]}</p>
      <button class="ghost" on:click={() => { comps = { ...comps }; ensure(tab); }}>重试</button>
    </div>
  {:else}
    <p class="loading">正在加载…</p>
  {/if}
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
  .admin-tabs { overflow-x: auto; }
  .admin-tabs button { white-space: nowrap; }
  .tab-badge {
    display: inline-block;
    min-width: 16px;
    margin-left: 4px;
    padding: 0 5px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--on-accent, #fff);
    font-size: 11px;
    line-height: 16px;
    text-align: center;
  }
  .admin-tabs button.on .tab-badge { background: #fff; color: var(--accent); }
  .overview-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; margin-bottom: 12px; }
  .ov-card { display: grid; gap: 3px; padding: 14px 16px; color: var(--fg); text-decoration: none; }
  .ov-card:hover { border-color: var(--accent); }
  .ov-label { font-size: 12px; color: var(--muted); }
  .ov-num { font-size: 22px; font-weight: 700; line-height: 1.3; font-variant-numeric: tabular-nums; }
  .my-stats { margin-bottom: 12px; }
  .mini-stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(90px, 1fr)); gap: 10px; }
  .mini-stats a { display: grid; gap: 2px; padding: 8px 10px; border: 1px solid var(--border); border-radius: 8px; color: var(--fg); text-decoration: none; }
  .mini-stats a:hover { border-color: var(--accent); }
  .mini-stats b { font-size: 18px; font-variant-numeric: tabular-nums; }
  .mini-stats span { font-size: 12px; color: var(--muted); }
  .quick-links { display: flex; gap: 8px; flex-wrap: wrap; }
  .more { font-size: 12px; color: var(--accent); text-decoration: none; }
  .small { font-size: 12px; }
</style>
