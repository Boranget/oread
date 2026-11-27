<script>
  import { route, href } from '../lib/router.js';
  import { searchJobs } from '../lib/searchJobsStore.js';

  export let role = 'user';
  export let onLogout = () => {};

  $: isAdmin = role === 'admin';
  $: path = $route.path;
  $: active = path === '/' || path.startsWith('/shelf')
    ? 'shelf'
    : path.startsWith('/library') || path.startsWith('/browse')
      ? 'library'
      : path.startsWith('/stats')
        ? 'stats'
        : path.startsWith('/admin')
          ? 'admin'
          : path.startsWith('/me')
            ? 'me'
            : '';

  $: mainNav = [
    { id: 'library', label: '图书馆', to: '/library' },
    { id: 'shelf', label: '书架', to: '/shelf' },
    { id: 'stats', label: '统计', to: '/stats' },
    { id: 'me', label: '我的', to: '/me' },
    ...(isAdmin ? [{ id: 'admin', label: '管理', to: '/admin' }] : []),
  ];
</script>

<div class="shell">
  <aside class="side">
    <a class="brand" href={href('/shelf')}>
      <span class="brand-name">Boranlib</span>
      <span class="brand-sub">个人图书馆</span>
    </a>

    <nav class="nav-main" aria-label="主导航">
      {#each mainNav as item (item.id)}
        <a class="nav-item" class:on={active === item.id} href={href(item.to)}>
          <span>{item.label}</span>
          {#if item.id === 'admin' && $searchJobs.active > 0}
            <span class="nav-badge" aria-label={`${$searchJobs.active} 个搜索任务运行中`}>{$searchJobs.active}</span>
          {/if}
        </a>
      {/each}
    </nav>

    <!-- 老设备入口独立于个人设置：只在这里出现，不占一级导航 -->
    <a class="side-extra" href="/k" rel="noopener">
      <span>Kindle / 老设备</span>
      <span class="side-extra-hint">极简阅读版 ↗</span>
    </a>

    <div class="side-spacer"></div>
    <button class="ghost logout" on:click={onLogout}>退出登录</button>
  </aside>

  <div class="main">
    <main class="content">
      <slot />
    </main>
  </div>

  <nav class="bottom" aria-label="主导航">
    {#each mainNav as item (item.id)}
      <a class="bottom-item" class:on={active === item.id} href={href(item.to)}>
        <span>{item.label}</span>
        {#if item.id === 'admin' && $searchJobs.active > 0}
          <span class="nav-badge">{$searchJobs.active}</span>
        {/if}
      </a>
    {/each}
    <button class="bottom-item bottom-logout" on:click={onLogout}>退出</button>
  </nav>
</div>

<style>
  .shell {
    min-height: 100vh;
    min-height: 100dvh;
    overflow-x: hidden;
  }

  /* ---------- 桌面侧栏 ---------- */
  .side {
    position: fixed;
    inset: 0 auto 0 0;
    width: 232px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 22px 14px 16px;
    background: var(--card);
    border-right: 1px solid var(--border);
    z-index: 5;
    overflow-y: auto;
  }
  .brand {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 2px 12px 18px;
    color: var(--accent);
    text-decoration: none;
  }
  .brand-name { font-weight: 700; font-size: 19px; letter-spacing: 0.2px; line-height: 1.2; }
  .brand-sub { color: var(--muted); font-size: 11px; font-weight: 400; letter-spacing: 0.4px; }

  .nav-main { display: flex; flex-direction: column; gap: 3px; }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 42px;
    padding: 9px 12px;
    border-radius: 9px;
    color: var(--fg);
    text-decoration: none;
    font-size: 14px;
  }
  .nav-item:hover { background: var(--soft-bg, #f0f0f2); }
  .nav-item.on { background: var(--accent); color: var(--on-accent, #fff); font-weight: 600; }
  .nav-item.on:hover { background: var(--accent-d); }
  .nav-badge {
    min-width: 16px;
    padding: 0 5px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--on-accent, #fff);
    font-size: 11px;
    line-height: 16px;
    text-align: center;
  }
  .nav-item.on .nav-badge { background: #fff; color: var(--accent); }

  .side-extra {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 14px;
    padding: 10px 12px;
    border: 1px dashed var(--border);
    border-radius: 9px;
    color: var(--fg);
    text-decoration: none;
    font-size: 13px;
  }
  .side-extra:hover { border-color: var(--accent); color: var(--accent); }
  .side-extra-hint { color: var(--muted); font-size: 11px; }

  .side-spacer { flex: 1; }
  .logout { width: 100%; }

  /* ---------- 主内容：没有顶部通栏，页面自己带标题与搜索 ---------- */
  .main {
    margin-left: 232px;
    min-height: 100vh;
    min-height: 100dvh;
    display: flex;
    flex-direction: column;
  }
  .content {
    width: 100%;
    max-width: 1320px;
    margin: 0 auto;
    padding: 26px 30px 56px;
    flex: 1;
  }

  /* 宽屏/全屏：内容居中并适度加宽，避免两侧留白过大 */
  @media (min-width: 1600px) {
    .side { width: 248px; padding-left: 18px; padding-right: 18px; }
    .main { margin-left: 248px; }
    .content { max-width: 1460px; padding: 30px 40px 64px; }
  }
  @media (min-width: 2000px) {
    .content { max-width: 1640px; }
  }

  .bottom { display: none; }

  @media (max-width: 860px) {
    .side { display: none; }
    .main { margin-left: 0; }
    .content { padding: 16px 14px calc(84px + env(safe-area-inset-bottom)); }
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
      min-height: 56px;
      padding: 0 4px;
      color: var(--muted);
      text-decoration: none;
      font-size: 13px;
      background: transparent;
      border: 0;
      border-radius: 0;
    }
    .bottom-item.on { color: var(--accent); font-weight: 600; }
    .bottom-item:hover { background: transparent; }
    .bottom-logout { color: var(--muted); border-left: 1px solid var(--border); }
  }
</style>
