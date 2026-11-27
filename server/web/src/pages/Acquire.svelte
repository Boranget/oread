<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';
  import { route, navigate, href, buildPath } from '../lib/router.js';
  import { searchJobs } from '../lib/searchJobsStore.js';
  import LocalImport from '../lib/LocalImport.svelte';

  export let role = 'user';
  const isAdmin = role === 'admin';
  const dispatch = createEventDispatcher();

  const TABS = [
    { id: 'overview', label: '总览' },
    { id: 'local', label: '本地导入' },
    { id: 'online', label: '在线书源' },
    { id: 'collection', label: '馆藏维护' },
  ];
  const ONLINE_TABS = [
    { id: 'discover', label: '发现搜书' },
    { id: 'sources', label: '书源管理' },
    { id: 'replace', label: '正文替换' },
  ];

  const LOADERS = {
    collection: () => import('./admin/CollectionAdmin.svelte'),
    discover: () => import('./Discover.svelte'),
    sources: () => import('./Sources.svelte'),
    replace: () => import('./ReplaceRules.svelte'),
  };

  let comps = {};
  let pending = {};
  let errs = {};

  let stats = null;
  let statsErr = '';
  let statsLoading = false;
  let lastImport = null;

  $: tab = TABS.some((t) => t.id === $route.query.tab) ? $route.query.tab : 'overview';
  $: sub = ONLINE_TABS.some((t) => t.id === $route.query.sub) ? $route.query.sub : 'discover';
  $: if (isAdmin && tab === 'online') ensure(sub);
  $: if (isAdmin && tab === 'collection') ensure('collection');

  onMount(() => {
    if (isAdmin) loadStats();
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
    const query = { tab: id === 'overview' ? undefined : id };
    if (id === 'online') query.sub = sub === 'discover' ? undefined : sub;
    navigate(buildPath('/acquire', query));
  }

  function setSub(id) {
    navigate(buildPath('/acquire', { tab: 'online', sub: id === 'discover' ? undefined : id }));
  }

  async function loadStats() {
    statsLoading = true;
    statsErr = '';
    try {
      stats = await api.libraryStats();
    } catch (e) {
      statsErr = e.message;
    } finally {
      statsLoading = false;
    }
  }

  function onImported(e) {
    lastImport = { ...(e.detail?.book || {}), errs: e.detail?.errs || 0 };
    loadStats();
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
  <h2>入馆</h2>
  <span class="page-count">图书馆 · 仅管理员</span>
  <div class="page-actions">
    <a class="ghost-link" href={href('/library')}>返回图书馆</a>
    {#if tab === 'overview'}
      <button class="ghost" on:click={loadStats} disabled={statsLoading}>{statsLoading ? '读取中…' : '刷新'}</button>
    {/if}
  </div>
</div>

{#if !isAdmin}
  <div class="card">
    <p class="err">入馆、书源与馆藏维护需要管理员权限。</p>
    <a class="ghost-link" href={href('/library')}>返回图书馆</a>
  </div>
{:else}
  <div class="seg acquire-tabs">
    {#each TABS as t (t.id)}
      <button class:on={tab === t.id} on:click={() => setTab(t.id)}>{t.label}</button>
    {/each}
  </div>

  {#if tab === 'overview'}
    {#if statsErr}<p class="err">{statsErr}</p>{/if}
    <div class="stat-grid">
      <div class="card stat-card">
        <span class="stat-label">永久馆藏</span>
        <span class="stat-num">{stats ? `${stats.collection.books} 本` : '—'}</span>
        <span class="muted tiny-note">{stats ? `${stats.collection.chapters} 章 · ${fmtBytes(stats.collection.bytes)}` : ''}</span>
      </div>
      <div class="card stat-card">
        <span class="stat-label">临时缓存</span>
        <span class="stat-num">{stats ? `${stats.temp.books} 本` : '—'}</span>
        <span class="muted tiny-note">{stats ? `${fmtBytes(stats.temp.bytes)} · ${stats.temp.ttl_days} 天回收` : ''}</span>
      </div>
      <div class="card stat-card">
        <span class="stat-label">磁盘剩余</span>
        <span class="stat-num">{stats && stats.disk.free_mb != null ? fmtBytes(stats.disk.free_bytes) : '未知'}</span>
        <span class="muted tiny-note">{stats ? `安全阀 ${stats.disk.floor_mb} MB` : ''}</span>
      </div>
      <div class="card stat-card">
        <span class="stat-label">搜索任务</span>
        <span class="stat-num">{$searchJobs.active} 个运行中</span>
        <span class="muted tiny-note">共 {$searchJobs.jobs.length} 条记录</span>
      </div>
    </div>

    {#if stats && stats.disk.download_paused}
      <div class="warn-box">
        <div class="warn-title">后台整本下载已暂停</div>
        <div class="warn-item">磁盘剩余空间低于安全阀（{stats.disk.floor_mb} MB）。阅读与临时缓存不受影响，空间恢复后采购单会自动继续。</div>
      </div>
    {/if}

    <div class="entry-grid">
      <button class="card entry-card" on:click={() => setTab('local')}>
        <span class="entry-kicker">本地文件</span>
        <b>导入 EPUB / CBZ</b>
        <span class="muted tiny-note">先解析查重，确认后写入馆藏；同一本书只保留一份内容。</span>
        <span class="entry-go">打开本地导入 →</span>
      </button>
      <button class="card entry-card" on:click={() => navigate(buildPath('/acquire', { tab: 'online' }))}>
        <span class="entry-kicker">在线书源</span>
        <b>发现、搜索与书源规则</b>
        <span class="muted tiny-note">搜索在线书源、按链接打开，或维护书源与正文替换规则。</span>
        <span class="entry-go">打开在线书源 →</span>
      </button>
      <button class="card entry-card" on:click={() => setTab('collection')}>
        <span class="entry-kicker">馆藏</span>
        <b>维护已入馆的书</b>
        <span class="muted tiny-note">查看占用与下载进度、编辑元数据、把临时缓存转为永久馆藏。</span>
        <span class="entry-go">打开馆藏维护 →</span>
      </button>
    </div>

    <div class="card acquire-note">
      <b>入馆规则</b>
      <p class="muted tiny-note">永久入馆只允许管理员操作。在线书源先产生临时缓存，确认内容完整后再转为馆藏；本地 EPUB / CBZ 直接进入永久馆藏。</p>
    </div>
  {:else if tab === 'local'}
    <LocalImport on:imported={onImported} />
    <div class="steps">
      <div class="card step">
        <span class="step-num">1</span>
        <b>上传解析</b>
        <span class="muted tiny-note">选择 EPUB 或 CBZ，服务端解压并读取元数据、目录与图片。</span>
      </div>
      <div class="card step">
        <span class="step-num">2</span>
        <b>查重确认</b>
        <span class="muted tiny-note">按文件、正文与书名作者查重；覆盖或新增都由你确认后才写入。</span>
      </div>
      <div class="card step">
        <span class="step-num">3</span>
        <b>写入馆藏</b>
        <span class="muted tiny-note">永久保存一份内容，书架、进度与阅读器立即可以打开。</span>
      </div>
    </div>
    {#if lastImport}
      <div class="card after-import">
        <div>
          <b>《{lastImport.title || '未命名'}》已写入馆藏</b>
          <p class="muted tiny-note">
            {#if lastImport.errs}体检发现 {lastImport.errs} 个错误，可在馆藏维护里打开编辑器运行 Check book。{:else}可以继续导入下一本，或去馆藏维护查看下载进度与元数据。{/if}
          </p>
        </div>
        <button class="ghost" on:click={() => setTab('collection')}>去馆藏维护</button>
      </div>
    {/if}
  {:else if tab === 'online'}
    <div class="seg sub-tabs">
      {#each ONLINE_TABS as t (t.id)}
        <button class:on={sub === t.id} on:click={() => setSub(t.id)}>
          {t.label}{#if t.id === 'discover' && $searchJobs.active > 0}<span class="tab-badge">{$searchJobs.active}</span>{/if}
        </button>
      {/each}
    </div>
    {#if comps[sub]}
      <svelte:component
        this={comps[sub]}
        {role}
        on:open={(e) => dispatch('open', e.detail)}
        on:read={(e) => dispatch('read', e.detail)}
      />
    {:else if errs[sub]}
      <div class="card">
        <p class="err">{errs[sub]}</p>
        <button class="ghost" on:click={() => { comps = { ...comps }; ensure(sub); }}>重试</button>
      </div>
    {:else}
      <p class="loading">正在加载…</p>
    {/if}
  {:else if tab === 'collection'}
    {#if comps.collection}
      <svelte:component this={comps.collection} {role} embedded={true} />
    {:else if errs.collection}
      <div class="card">
        <p class="err">{errs.collection}</p>
        <button class="ghost" on:click={() => { comps = { ...comps }; ensure('collection'); }}>重试</button>
      </div>
    {:else}
      <p class="loading">正在加载…</p>
    {/if}
  {/if}
{/if}

<style>
  .acquire-tabs { margin-bottom: 14px; overflow-x: auto; }
  .acquire-tabs button { white-space: nowrap; }
  .sub-tabs { margin-bottom: 12px; overflow-x: auto; }
  .sub-tabs button { white-space: nowrap; }
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
  .sub-tabs button.on .tab-badge { background: #fff; color: var(--accent); }
  .stat-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 12px;
    margin-bottom: 12px;
  }
  .stat-card { display: grid; gap: 3px; padding: 14px 16px; }
  .stat-label { font-size: 12px; color: var(--muted); }
  .stat-num { font-size: 22px; font-weight: 700; line-height: 1.3; font-variant-numeric: tabular-nums; }
  .entry-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
    gap: 12px;
    margin: 12px 0;
  }
  .entry-card {
    display: grid;
    gap: 5px;
    padding: 16px 18px;
    text-align: left;
    color: var(--fg);
    cursor: pointer;
  }
  .entry-card:hover { border-color: var(--accent); }
  .entry-card b { font-size: 15px; }
  .entry-kicker { font-size: 11px; letter-spacing: 0.4px; color: var(--muted); text-transform: uppercase; }
  .entry-card .tiny-note { margin: 0; line-height: 1.55; }
  .entry-go { margin-top: 6px; color: var(--accent); font-size: 12px; font-weight: 600; }
  .acquire-note { padding: 14px 16px; }
  .acquire-note .tiny-note { margin: 4px 0 0; line-height: 1.6; }
  .steps {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 12px;
    margin-top: 12px;
  }
  .step { display: grid; gap: 4px; padding: 14px 16px; }
  .step-num {
    width: 22px;
    height: 22px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--accent);
    border-radius: 999px;
    color: var(--accent);
    font-size: 12px;
    font-weight: 700;
  }
  .step .tiny-note { margin: 0; line-height: 1.55; }
  .after-import {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 16px;
    margin-top: 12px;
  }
  .after-import .tiny-note { margin: 4px 0 0; }
  @media (max-width: 560px) {
    .after-import { flex-direction: column; align-items: stretch; }
  }
</style>
