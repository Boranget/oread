<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';

  let sources = [];
  let error = '';
  let loading = false;
  let showEditor = false;
  let editUrl = null; // 编辑中的书源 url，null 表示新建
  let srcJson = '';
  let form = { name: '', url: '', group: '', enabled: true };

  const TEMPLATE = {
    book_source_name: '',
    book_source_url: '',
    book_source_group: '',
    enabled: true,
    search_url: '',
    rule_search: {
      book_list: '',
      name: '',
      author: '',
      book_url: '',
      cover_url: '',
      intro: '',
      last_chapter: '',
    },
  };

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      sources = await api.sources();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function syncFormFromJson() {
    try {
      const o = JSON.parse(srcJson);
      form = {
        name: o.book_source_name || '',
        url: o.book_source_url || '',
        group: o.book_source_group || '',
        enabled: o.enabled !== false,
      };
    } catch {
      /* 编辑中非法 JSON 不回写表单 */
    }
  }

  function onFormChange() {
    let o;
    try {
      o = JSON.parse(srcJson);
    } catch {
      o = {};
    }
    o.book_source_name = form.name;
    o.book_source_url = form.url;
    o.book_source_group = form.group;
    o.enabled = form.enabled;
    srcJson = JSON.stringify(o, null, 2);
  }

  function openNew() {
    editUrl = null;
    srcJson = JSON.stringify(TEMPLATE, null, 2);
    form = { name: '', url: '', group: '', enabled: true };
    showEditor = true;
  }

  async function openEdit(url) {
    error = '';
    try {
      const s = await api.sourceDetail(url);
      srcJson = JSON.stringify(s, null, 2);
      syncFormFromJson();
      editUrl = url;
      showEditor = true;
    } catch (e) {
      error = e.message;
    }
  }

  async function save() {
    error = '';
    let payload;
    try {
      payload = JSON.parse(srcJson);
    } catch (e) {
      error = 'JSON 解析失败：' + e.message;
      return;
    }
    try {
      await api.saveSource(payload);
      showEditor = false;
      await load();
    } catch (e) {
      error = e.message;
    }
  }

  async function remove(url) {
    if (!confirm('删除该书源？此操作不可恢复。')) return;
    try {
      await api.deleteSource(url);
      await load();
    } catch (e) {
      error = e.message;
    }
  }

  async function importJson() {
    const text = prompt('粘贴书源 JSON（数组或单个对象）：');
    if (!text) return;
    try {
      const arr = JSON.parse(text);
      const list = Array.isArray(arr) ? arr : [arr];
      await api.importSources(list);
      await load();
    } catch (e) {
      error = '导入失败：' + e.message;
    }
  }

  async function exportAll() {
    try {
      const list = await api.exportSources();
      const blob = new Blob([JSON.stringify(list, null, 2)], { type: 'application/json' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = 'legado-sources.json';
      a.click();
      URL.revokeObjectURL(a.href);
    } catch (e) {
      error = e.message;
    }
  }
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">书源管理</h2>
  <div class="spacer"></div>
  <button class="ghost" on:click={importJson}>导入</button>
  <button class="ghost" on:click={exportAll}>导出</button>
  <button on:click={openNew}>新建书源</button>
</div>

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if sources.length === 0 && !loading}
  <div class="empty">还没有书源，点击「新建书源」或「导入」开始。</div>
{:else}
  <div class="grid cols2">
    {#each sources as s}
      <div class="card">
        <div class="row" style="align-items:flex-start">
          <div style="flex:1">
            <div style="font-weight:600">{s.book_source_name}</div>
            <div class="muted">{s.book_source_url}</div>
            {#if s.book_source_group}<span class="badge" style="margin-top:4px;display:inline-block">{s.book_source_group}</span>{/if}
          </div>
          {#if !s.enabled}<span class="badge">已禁用</span>{/if}
        </div>
        <div class="row" style="margin-top:10px">
          <button class="ghost" on:click={() => openEdit(s.book_source_url)}>编辑</button>
          <button class="danger" on:click={() => remove(s.book_source_url)}>删除</button>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if showEditor}
  <div class="modal-mask" on:click|self={() => (showEditor = false)}>
    <div class="modal">
      <h2>{editUrl ? '编辑书源' : '新建书源'}</h2>
      <div class="grid cols3" style="margin-bottom:10px">
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">名称</div>
          <input bind:value={form.name} on:input={onFormChange} />
        </div>
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">URL（书源 ID）</div>
          <input bind:value={form.url} on:input={onFormChange} />
        </div>
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">分组</div>
          <input bind:value={form.group} on:input={onFormChange} />
        </div>
      </div>
      <label class="row" style="margin-bottom:10px">
        <input type="checkbox" style="width:auto" bind:checked={form.enabled} on:change={onFormChange} />
        <span>启用</span>
      </label>
      <div class="muted" style="font-size:12px;margin-bottom:4px">完整书源 JSON（可手动编辑）</div>
      <textarea bind:value={srcJson} on:input={syncFormFromJson} rows="18"></textarea>
      {#if error}<p class="err">{error}</p>{/if}
      <div class="row" style="margin-top:12px">
        <div class="spacer"></div>
        <button class="ghost" on:click={() => (showEditor = false)}>取消</button>
        <button on:click={save}>保存</button>
      </div>
    </div>
  </div>
{/if}
