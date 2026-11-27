<script>
  import { route, navigate, buildPath, href } from '../lib/router.js';
  import { searchJobs } from '../lib/searchJobsStore.js';

  export let role = 'user';
  export let onLogout = () => {};

  $: isAdmin = role === 'admin';
  $: path = $route.path;
  $: active = path === '/'
    ? 'home'
    : path.startsWith('/library') || path.startsWith('/browse')
      ? 'library'
      : path.startsWith('/shelf')
        ? 'shelf'
        : path.startsWith('/stats')
          ? 'stats'
          : path.startsWith('/admin')
            ? 'admin'
            : path.startsWith('/me')
              ? 'me'
              : '';

  $: mainNav = [
    { id: 'home', label: '首页', to: '/' },
    { id: 'library', label: '图书馆', to: '/library' },
    { id: 'shelf', label: '书架', to: '/shelf' },
    { id: 'stats', label: '统计', to: '/stats' },
    { id: 'me', label: '我的', to: '/me' },
    ...(isAdmin ? [{ id: 'admin', label: '管理', to: '/admin' }] : []),
  ];

  let q = '';

  function submitSearch(e) {
    e.preventDefault();
    const term = q.trim();
    if (!term) return;
    navigate(buildPath('/library', { q: term }));
  }
</script>

<div class="shell">
  <aside class="side">
    <a class="brand" href={href('/')}>Boranlib</a>
    <nav class="nav-main">
      {#each mainNav as item (item.id)}
        <a class="nav-item" class:on={active === item.id} href={href(item.to)}>
          <span>{item.label}</span>
          {#if item.id === 'admin' && $searchJobs.active > 0}
            <span class="nav-badge" aria-label={`${$searchJobs.active} 个搜索任务运行中`}>{$searchJobs.active}</span>
          {/if}
        </a>
      {/each}
    </nav>

    <div class="side-spacer"></div>
    <button class="ghost logout" on:click={onLogout}>退出登录</button>
  </aside>

  <div class="main">
    <header class="top">
      <a class="brand-m" href={href('/')}>Boranlib</a>
      <form class="search" on:submit={submitSearch}>
        <input bind:value={q} placeholder="搜索书名、作者、标签" aria-label="搜索馆藏" />
      </form>
      <div class="spacer"></div>
      <button class="ghost logout-d" on:click={onLogout}>退出</button>
    </header>

    <main class="content">
      <slot />
    </main>
  </div>

  <nav class="bottom">
    {#each mainNav as item (item.id)}
      <a class="bottom-item" class:on={active === item.id} href={href(item.to)}>
        <span>{item.label}</span>
        {#if item.id === 'admin' && $searchJobs.active > 0}
          <span class="nav-badge">{$searchJobs.active}</span>
        {/if}
      </a>
    {/each}
  </nav>
</div>

<style>
  .shell { min-height: 100vh; }
  .side {
    position: fixed;
    inset: 0 auto 0 0;
    width: 216px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 18px 12px 14px;
    background: var(--card);
    border-right: 1px solid var(--border);
    z-index: 5;
    overflow-y: auto;
  }
  .brand {
    display: block;
    padding: 2px 10px 14px;
    color: var(--accent);
    font-weight: 700;
    font-size: 17px;
    text-decoration: none;
    letter-spacing: 0.2px;
  }
  .nav-main { display: flex; flex-direction: column; gap: 2px; }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 9px 10px;
    border-radius: 8px;
    color: var(--fg);
    text-decoration: none;
    font-size: 14px;
  }
  .nav-item:hover { background: var(--soft-bg, #f0f0f2); }
  .nav-item.on { background: var(--accent); color: #fff; font-weight: 600; }
  .nav-item.on:hover { background: var(--accent-d); }
  .nav-badge {
    min-width: 16px;
    padding: 0 5px;
    border-radius: 999px;
    background: var(--accent);
    color: #fff;
    font-size: 11px;
    line-height: 16px;
    text-align: center;
  }
  .nav-item.on .nav-badge { background: #fff; color: var(--accent); }
  .side-spacer { flex: 1; }
  .logout { width: 100%; }

  .main {
    margin-left: 216px;
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .top {
    position: sticky;
    top: 0;
    z-index: 4;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 20px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
  }
  .brand-m { display: none; color: var(--accent); font-weight: 700; text-decoration: none; white-space: nowrap; }
  .search { flex: 0 1 340px; }
  .search input { width: 100%; }
  .content {
    width: 100%;
    max-width: 1120px;
    margin: 0 auto;
    padding: 18px 20px 48px;
    flex: 1;
  }
  .bottom { display: none; }

  @media (max-width: 860px) {
    .side { display: none; }
    .main { margin-left: 0; }
    .brand-m { display: block; }
    .top { padding: 8px 12px; }
    .search { flex: 1; }
    .logout-d { display: none; }
    .content { padding: 14px 12px 84px; }
    .bottom {
      display: flex;
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 6;
      background: var(--card);
      border-top: 1px solid var(--border);
      padding-bottom: env(safe-area-inset-bottom);
    }
    .bottom-item {
      flex: 1;
      display: flex;
      align-items: center;
      justify-content: center;
      gap: 4px;
      min-height: 54px;
      color: var(--muted);
      text-decoration: none;
      font-size: 13px;
    }
    .bottom-item.on { color: var(--accent); font-weight: 600; }
  }
</style>
