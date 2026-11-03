<script>
  import { onMount } from 'svelte';
  import Login from './pages/Login.svelte';
  import Bookshelf from './pages/Bookshelf.svelte';
  import Discover from './pages/Discover.svelte';
  import Reader from './pages/Reader.svelte';
  import Sources from './pages/Sources.svelte';
  import ReplaceRules from './pages/ReplaceRules.svelte';
  import Settings from './pages/Settings.svelte';
  import Stats from './pages/Stats.svelte';
  import { getToken, getRole, clearToken } from './lib/api.js';
  import { DEVICE_MODES, deviceLabels, devicePref, setDeviceMode } from './lib/device.js';

  let loggedIn = false;
  let role = 'user';
  let tab = 'bookshelf';
  // 阅读器通过 {bookUrl, sourceId, name, author, coverUrl, intro} 打开；后四项是详情 seed
  let reader = null;

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
</script>

{#if !loggedIn}
  <Login on:loggedin={(e) => onLogin(e.detail)} />
{:else if reader}
  <!-- 换源后 bookUrl/sourceId 变化，用 key 强制重建 Reader，避免沿用旧目录与缓存 -->
  {#key reader.sourceId + '|' + reader.bookUrl}
    <Reader
      book={reader}
      on:close={closeReader}
      on:logout={onLogout}
      on:switch={(e) => openReader(e.detail)}
    />
  {/key}
{:else}
  <div class="nav">
    <div class="brand">Boranlib</div>
    <div class="tabs">
      {#each TABS as t}
        <div class="tab" class:active={tab === t.id} on:click={() => (tab = t.id)}>{t.label}</div>
      {/each}
    </div>
    <div class="right">
      <select
        class="device-select"
        aria-label="显示模式"
        value={$devicePref}
        on:change={(e) => setDeviceMode(e.target.value)}
      >
        {#each DEVICE_MODES as m}
          <option value={m}>{deviceLabels[m]}</option>
        {/each}
      </select>
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
    {:else if tab === 'stats'}
      <Stats on:go={(e) => (tab = e.detail.tab)} />
    {:else if tab === 'replace'}
      <ReplaceRules />
    {:else if tab === 'settings'}
      <Settings />
    {/if}
  </div>
{/if}
