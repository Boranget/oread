<script>
  import { onMount } from 'svelte';
  import Login from './pages/Login.svelte';
  import Catalog from './pages/Catalog.svelte';
  import AppShell from './layout/AppShell.svelte';
  import { api, getToken, getRole, clearToken } from './lib/api.js';
  import { route, startRouter, navigate } from './lib/router.js';
  import { syncTheme, applyTheme } from './lib/theme.js';
  import {
    startSearchJobsPolling,
    stopSearchJobsPolling,
  } from './lib/searchJobsStore.js';

  let loggedIn = false;
  let role = 'user';
  // 阅读器通过 {bookUrl, sourceId, name, author, coverUrl, intro, bookType} 打开。
  let reader = null;

  // 首屏只留书架与图书馆；其余页面按需 import()，不进首屏包。
  const LAZY = {
    browse: () => import('./pages/Browse.svelte'),
    book: () => import('./pages/BookDetail.svelte'),
    stats: () => import('./pages/Stats.svelte'),
    me: () => import('./pages/Account.svelte'),
    admin: () => import('./pages/admin/Admin.svelte'),
    acquire: () => import('./pages/Acquire.svelte'),
  };
  let comps = {};
  let errs = {};
  let pending = {};

  let ComicReader = null;
  let comicError = '';
  let comicLoading = null;
  let Reader = null;
  let readerError = '';
  let readerLoading = null;

  $: page = pageOf($route.path);
  $: if (loggedIn && LAZY[page] && !((page === 'admin' || page === 'acquire') && role !== 'admin')) ensure(page);

  function pageOf(path) {
    if (!path || path === '/') return 'shelf';
    if (path === '/library') return 'library';
    if (path === '/shelf') return 'shelf';
    if (path === '/browse' || path === '/book' || path === '/stats' || path === '/me' || path === '/admin' || path === '/acquire') {
      return path.slice(1);
    }
    return 'notfound';
  }

  function ensure(id) {
    if (comps[id] || pending[id] || !LAZY[id]) return;
    pending[id] = true;
    errs[id] = '';
    LAZY[id]()
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
    const stopRouter = startRouter();
    window.addEventListener('error', onWindowError);
    window.addEventListener('unhandledrejection', onWindowRejection);
    if (getToken()) {
      loggedIn = true;
      role = getRole();
      syncTheme();
      // 任务列表与登录态解耦：关掉页面/重新登录后仍能看到后端仍在跑的任务。
      startSearchJobsPolling();
    }
    if (!location.hash) navigate('/shelf', { replace: true });
    return () => {
      stopRouter();
      window.removeEventListener('error', onWindowError);
      window.removeEventListener('unhandledrejection', onWindowRejection);
    };
  });

  function onLogin(r) {
    loggedIn = true;
    role = r || getRole();
    syncTheme();
    startSearchJobsPolling();
  }

  function onLogout() {
    clearToken();
    loggedIn = false;
    role = 'user';
    reader = null;
    applyTheme('light');
    stopSearchJobsPolling();
    navigate('/shelf', { replace: true });
  }

  function loadComicReader() {
    if (ComicReader || comicLoading) return comicLoading;
    comicError = '';
    comicLoading = import('./reader/ComicReader.svelte')
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
    readerLoading = import('./reader/Reader.svelte')
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

  function normalizeBook(raw) {
    if (!raw) return null;
    const type = Number(raw.book_source_type);
    return {
      bookUrl: raw.bookUrl || raw.book_url || raw.book_id || '',
      sourceId: raw.sourceId || raw.source_id || raw.origin || '',
      name: raw.name || raw.title || '',
      author: raw.author || '',
      coverUrl: raw.coverUrl || raw.cover_url || raw.cover || '',
      intro: raw.intro || '',
      bookType: raw.bookType || raw.type || (type === 2 ? 'image' : type === 1 ? 'text' : ''),
    };
  }

  let openSeq = 0;
  // 漫画与小说共用入口：搜索结果已带类型；书架/馆藏条目没有类型时，
  // 用 /api/comic/source 轻量探测书源类型（不发回源请求），失败则按小说打开。
  async function openReader(raw) {
    const book = normalizeBook(raw);
    if (!book || !book.bookUrl) return;
    const my = ++openSeq;
    const known = book.bookType;
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

  function closeReader() {
    reader = null;
  }
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
  <AppShell {role} onLogout={onLogout}>
    {#if page === 'library'}
      <Catalog {role} on:read={(e) => openReader(e.detail)} />
    {:else if page === 'shelf'}
      <Catalog {role} mode="shelf" on:read={(e) => openReader(e.detail)} />
    {:else if (page === 'admin' || page === 'acquire') && role !== 'admin'}
      <div class="card">
        <p class="err">需要管理员权限。</p>
        <a class="ghost-link" href="#/shelf">返回书架</a>
      </div>
    {:else if page === 'notfound'}
      <div class="card">
        <p>页面不存在。</p>
        <a class="ghost-link" href="#/shelf">返回书架</a>
      </div>
    {:else if comps[page]}
      <svelte:component
        this={comps[page]}
        {role}
        on:read={(e) => openReader(e.detail)}
        on:open={(e) => openReader(e.detail)}
        on:logout={onLogout}
      />
    {:else if errs[page]}
      <div class="card">
        <p class="err">{errs[page]}</p>
        <button class="ghost" on:click={() => ensure(page)}>重试</button>
      </div>
    {:else}
      <p class="loading">正在加载…</p>
    {/if}
  </AppShell>
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
</style>
