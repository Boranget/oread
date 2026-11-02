<script>
  import { onMount } from 'svelte';
  import Login from './pages/Login.svelte';
  import Bookshelf from './pages/Bookshelf.svelte';
  import Discover from './pages/Discover.svelte';
  import Reader from './pages/Reader.svelte';
  import Sources from './pages/Sources.svelte';
  import ReplaceRules from './pages/ReplaceRules.svelte';
  import Settings from './pages/Settings.svelte';
  import { getToken, getRole, clearToken } from './lib/api.js';

  let loggedIn = false;
  let role = 'user';
  let tab = 'bookshelf';
  // 阅读器通过 {bookUrl, sourceId, name} 打开
  let reader = null;

  const TABS = [
    { id: 'bookshelf', label: '书架' },
    { id: 'discover', label: '发现' },
    { id: 'replace', label: '替换规则' },
    { id: 'settings', label: '设置' },
  ];

  function isAdmin() { return role === 'admin'; }

  onMount(() => {
    if (getToken()) {
      loggedIn = true;
      role = getRole();
    }
  });

  function onLogin(r) {
    loggedIn = true;
    role = r || getRole();
    tab = 'bookshelf';
  }
  function onLogout() {
    clearToken();
    loggedIn = false;
    role = 'user';
    reader = null;
    tab = 'bookshelf';
  }
  function openReader(book) {
    reader = book;
  }
  function closeReader() { reader = null; }

  $: adminTabs = isAdmin() ? [{ id: 'sources', label: '书源管理' }, ...TABS] : TABS;
</script>

{#if !loggedIn}
  <Login on:loggedin={(e) => onLogin(e.detail)} />
{:else if reader}
  <Reader book={reader} on:close={closeReader} on:logout={onLogout} />
{:else}
  <div class="nav">
    <div class="brand">Legado Web</div>
    <div class="tabs">
      {#each adminTabs as t}
        <div class="tab" class:active={tab === t.id} on:click={() => (tab = t.id)}>{t.label}</div>
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
    {:else if tab === 'sources'}
      <Sources />
    {:else if tab === 'replace'}
      <ReplaceRules />
    {:else if tab === 'settings'}
      <Settings />
    {/if}
  </div>
{/if}
