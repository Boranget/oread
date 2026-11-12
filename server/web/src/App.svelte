<script>
  import { onMount } from 'svelte';
  import Login from './pages/Login.svelte';
  import Bookshelf from './pages/Bookshelf.svelte';
  import Discover from './pages/Discover.svelte';
  import { api, getToken, getRole, clearToken } from './lib/api.js';
  import { syncTheme, applyTheme } from './lib/theme.js';
  import {
    searchJobs,
    startSearchJobsPolling,
    stopSearchJobsPolling,
  } from './lib/searchJobsStore.js';

  let loggedIn = false;
  let role = 'user';
  let tab = 'bookshelf';
  // 阅读器通过 {bookUrl, sourceId, name, author, coverUrl, intro} 打开；后四项是详情 seed
  let reader = null;

  // 首屏只留 登录/书架/发现。阅读器（含正文解析与图片缓存）与管理页按需 import()，
  // 不进首屏包；失败时给出可见错误与重试，不静默吞掉。
  let ComicReader = null;
  let comicError = '';
  let comicLoading = null;
  // 小说阅读器连同正文解析/图片缓存/统计等模块按需加载，不进首屏包。
  let Reader = null;
  let readerError = '';
  let readerLoading = null;

  const LAZY_TABS = {
    stats: () => import('./pages/Stats.svelte'),
    sources: () => import('./pages/Sources.svelte'),
    replace: () => import('./pages/ReplaceRules.svelte'),
    settings: () => import('./pages/Settings.svelte'),
  };
  let lazyComps = {};
  let lazyErrs = {};

  const TABS = [
    { id: 'bookshelf', label: '书架' },
    { id: 'discover', label: '发现' },
    { id: 'stats', label: '统计' },
    // 普通用户也可见，进入后是只读态，避免出现可点击但必然 403 的按钮
    { id: 'sources', label: '书源管理' },
    { id: 'replace', label: '替换规则' },
    { id: 'settings', label: '设置' },
  ];

  // 其它页面（如 Discover 的空态引导）用事件请求切换 tab
  function onTabRequest(e) {
    const id = e && e.detail;
    if (id && TABS.some((t) => t.id === id)) selectTab(id);
  }

  // ---- 渲染期错误兜底 ----
  // Svelte 的 flush() 抛错后不会复位调度标志，之后所有状态更新都不再执行，
  // 页面会表现成“永远加载中”。所以这里用原生 DOM 直接挂一条可见提示，
  // 不依赖 Svelte 渲染：即使调度器已经卡死，用户仍有“重新加载”的出口。
  let fatalEl = null;
  function showFatalBanner(detail) {
    if (fatalEl || typeof document === 'undefined') return;
    const bar = document.createElement('div');
    bar.setAttribute('role', 'alert');
    bar.style.cssText =
      'position:fixed;left:0;right:0;top:0;z-index:2147483647;display:flex;' +
      'gap:10px;align-items:center;flex-wrap:wrap;padding:10px 14px;' +
      'background:#7a1f1f;color:#fff;font:13px/1.5 system-ui,sans-serif;' +
      'box-shadow:0 2px 10px rgba(0,0,0,.35)';
    const text = document.createElement('span');
    text.style.cssText = 'flex:1;min-width:220px;word-break:break-word';
    text.textContent = `页面出现未处理错误，可能已停止更新：${detail}`;
    const reload = document.createElement('button');
    reload.textContent = '重新加载';
    reload.style.cssText =
      'padding:6px 12px;border:1px solid #fff;border-radius:6px;' +
      'background:transparent;color:#fff;cursor:pointer';
    reload.addEventListener('click', () => location.reload());
    const close = document.createElement('button');
    close.textContent = '关闭';
    close.style.cssText =
      'padding:6px 12px;border:1px solid rgba(255,255,255,.6);border-radius:6px;' +
      'background:transparent;color:#fff;cursor:pointer';
    close.addEventListener('click', () => {
      bar.remove();
      fatalEl = null;
    });
    bar.append(text, reload, close);
    document.body.appendChild(bar);
    fatalEl = bar;
  }

  function errorDetail(e) {
    if (!e) return '未知错误';
    if (e.message) return String(e.message);
    if (e.reason) return String(e.reason.message || e.reason);
    return String(e);
  }

  function onWindowError(e) {
    // 资源加载失败（img/script）没有 message，由对应组件自己处理，不弹全局条
    if (!e || !e.message) return;
    const at = e.filename ? ` (${e.filename}:${e.lineno || 0})` : '';
    showFatalBanner(errorDetail(e) + at);
  }

  function onWindowRejection(e) {
    showFatalBanner(errorDetail(e.reason));
  }

  onMount(() => {
    window.addEventListener('oread:tab', onTabRequest);
    window.addEventListener('error', onWindowError);
    window.addEventListener('unhandledrejection', onWindowRejection);
    if (getToken()) {
      loggedIn = true;
      role = getRole();
      syncTheme();
      // 任务列表与登录态解耦：关掉页面/重新登录后仍能看到后端仍在跑的任务。
      startSearchJobsPolling();
    }
    return () => {
      window.removeEventListener('oread:tab', onTabRequest);
      window.removeEventListener('error', onWindowError);
      window.removeEventListener('unhandledrejection', onWindowRejection);
    };
  });

  function onLogin(r) {
    loggedIn = true;
    role = r || getRole();
    tab = 'bookshelf';
    syncTheme();
    startSearchJobsPolling();
  }
  function onLogout() {
    clearToken();
    loggedIn = false;
    role = 'user';
    reader = null;
    tab = 'bookshelf';
    applyTheme('light');
    stopSearchJobsPolling();
  }

  function selectTab(id) {
    tab = id;
    const load = LAZY_TABS[id];
    if (!load || lazyComps[id]) return;
    lazyErrs[id] = '';
    load()
      .then((mod) => {
        lazyComps[id] = mod.default;
      })
      .catch((e) => {
        lazyErrs[id] = `页面加载失败：${e && e.message ? e.message : e}`;
      });
  }

  function loadComicReader() {
    if (ComicReader || comicLoading) return comicLoading;
    comicError = '';
    comicLoading = import('./pages/ComicReader.svelte')
      .then((mod) => {
        ComicReader = mod.default;
      })
      .catch((e) => {
        comicError = `漫画阅读器加载失败：${e && e.message ? e.message : e}`;
      })
      .finally(() => {
        comicLoading = null;
      });
    return comicLoading;
  }

  function loadReader() {
    if (Reader || readerLoading) return readerLoading;
    readerError = '';
    readerLoading = import('./pages/Reader.svelte')
      .then((mod) => {
        Reader = mod.default;
      })
      .catch((e) => {
        readerError = `阅读器加载失败：${e && e.message ? e.message : e}`;
      })
      .finally(() => {
        readerLoading = null;
      });
    return readerLoading;
  }

  let openSeq = 0;
  // 漫画与小说共用入口：搜索结果已带 type；书架条目没有类型时，
  // 用 /api/comic/source 轻量探测书源类型（不发回源请求），失败则按小说打开。
  async function openReader(book) {
    if (!book) return;
    const my = ++openSeq;
    const known = book.bookType || book.type;
    if (known === 'image' || known === 'text') {
      reader = { ...book, bookType: known };
      if (known === 'image') loadComicReader();
      else loadReader();
      return;
    }
    if (book.sourceId) {
      try {
        const st = await api.comicSource(book.sourceId, book.bookUrl);
        if (my !== openSeq) return;
        if (st && st.comic) {
          reader = { ...book, bookType: 'image' };
          loadComicReader();
          return;
        }
      } catch {
        /* 探测失败按小说打开，Reader 仍可正常报错 */
      }
    }
    if (my !== openSeq) return;
    reader = { ...book, bookType: 'text' };
    loadReader();
  }
  function closeReader() { reader = null; }
</script>

{#if !loggedIn}
  <Login on:loggedin={(e) => onLogin(e.detail)} />
{:else if reader}
  <!-- 换源后 bookUrl/sourceId 变化，用 key 强制重建 Reader，避免沿用旧目录与缓存 -->
  {#key reader.sourceId + '|' + reader.bookUrl + '|' + reader.bookType}
    {#if reader.bookType === 'image'}
      {#if ComicReader}
        <svelte:component this={ComicReader} book={reader} on:close={closeReader} />
      {:else if comicError}
        <div class="load-fail">
          <p class="err">{comicError}</p>
          <button class="ghost" on:click={loadComicReader}>重试</button>
          <button class="ghost" on:click={closeReader}>返回</button>
        </div>
      {:else}
        <p class="empty">正在加载漫画阅读器…</p>
      {/if}
    {:else if Reader}
      <svelte:component
        this={Reader}
        book={reader}
        on:close={closeReader}
        on:logout={onLogout}
        on:switch={(e) => openReader(e.detail)}
      />
    {:else if readerError}
      <div class="load-fail">
        <p class="err">{readerError}</p>
        <button class="ghost" on:click={loadReader}>重试</button>
        <button class="ghost" on:click={closeReader}>返回</button>
      </div>
    {:else}
      <p class="empty">正在加载阅读器…</p>
    {/if}
  {/key}
{:else}
  <div class="nav">
    <div class="brand">Boranlib</div>
    <div class="tabs">
      {#each TABS as t}
        <div class="tab" class:active={tab === t.id} on:click={() => selectTab(t.id)}>
          {t.label}{#if t.id === 'discover' && $searchJobs.active > 0}<span
            class="tab-badge"
            aria-label={`${$searchJobs.active} 个搜索任务运行中`}
          >{$searchJobs.active}</span>{/if}
        </div>
      {/each}
    </div>
    <div class="right">
      <button class="ghost" on:click={onLogout}>退出</button>
    </div>
  </div>

  <div class="container">
    {#if tab === 'bookshelf'}
      <Bookshelf on:open={ (e) => openReader(e.detail) } />
    {:else if tab === 'discover'}
      <Discover on:open={ (e) => openReader(e.detail) } />
    {:else if LAZY_TABS[tab]}
      {#if lazyComps[tab]}
        <svelte:component this={lazyComps[tab]} on:go={(e) => selectTab(e.detail.tab)} />
      {:else if lazyErrs[tab]}
        <div class="load-fail">
          <p class="err">{lazyErrs[tab]}</p>
          <button class="ghost" on:click={() => selectTab(tab)}>重试</button>
        </div>
      {:else}
        <p class="empty">正在加载…</p>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .load-fail {
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--card);
    margin: 16px;
  }
  .load-fail button { margin-right: 8px; margin-top: 4px; }
</style>
