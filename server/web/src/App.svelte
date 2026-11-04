<script>
  import { onMount } from 'svelte';
  import Login from './pages/Login.svelte';
  import Bookshelf from './pages/Bookshelf.svelte';
  import Discover from './pages/Discover.svelte';
  import Reader from './pages/Reader.svelte';
  import { api, getToken, getRole, clearToken } from './lib/api.js';
  import { syncTheme, applyTheme } from './lib/theme.js';

  let loggedIn = false;
  let role = 'user';
  let tab = 'bookshelf';
  // 阅读器通过 {bookUrl, sourceId, name, author, coverUrl, intro} 打开；后四项是详情 seed
  let reader = null;

  // 首屏只留 搜索/书架/阅读器壳子。漫画阅读器（含 blob 图片缓存）与管理页按需 import()，
  // 不进首屏包；失败时给出可见错误与重试，不静默吞掉。
  let ComicReader = null;
  let comicError = '';
  let comicLoading = null;

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

  onMount(() => {
    if (getToken()) {
      loggedIn = true;
      role = getRole();
      syncTheme();
    }
  });

  function onLogin(r) {
    loggedIn = true;
    role = r || getRole();
    tab = 'bookshelf';
    syncTheme();
  }
  function onLogout() {
    clearToken();
    loggedIn = false;
    role = 'user';
    reader = null;
    tab = 'bookshelf';
    applyTheme('light');
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
    {:else}
      <Reader
        book={reader}
        on:close={closeReader}
        on:logout={onLogout}
        on:switch={(e) => openReader(e.detail)}
      />
    {/if}
  {/key}
{:else}
  <div class="nav">
    <div class="brand">Boranlib</div>
    <div class="tabs">
      {#each TABS as t}
        <div class="tab" class:active={tab === t.id} on:click={() => selectTab(t.id)}>{t.label}</div>
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
