<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';

  let rules = [];
  let error = '';
  let loading = false;
  let showEditor = false;
  let editing = null; // id 或 null
  let form = blank();

  function blank() {
    return {
      name: '',
      group: '',
      pattern: '',
      replacement: '',
      is_regex: true,
      is_enabled: true,
      scope_title: true,
      scope_content: true,
      exclude_scope: '',
    };
  }

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      rules = await api.replaceRules();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function openNew() {
    form = blank();
    editing = null;
    showEditor = true;
  }
  function openEdit(r) {
    form = { ...blank(), ...r, id: r.id };
    editing = r.id;
    showEditor = true;
  }

  async function save() {
    error = '';
    if (!form.pattern) {
      error = '替换规则（pattern）不能为空';
      return;
    }
    try {
      await api.saveReplaceRule(form);
      showEditor = false;
      await load();
    } catch (e) {
      error = e.message;
    }
  }
  async function remove(id) {
    if (!confirm('删除该替换规则？')) return;
    try {
      await api.deleteReplaceRule(id);
      await load();
    } catch (e) {
      error = e.message;
    }
  }
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">替换规则</h2>
  <div class="spacer"></div>
  <button on:click={openNew}>新建规则</button>
</div>

{#if error}<p class="err">{error}</p>{/if}
{#if loading}<p class="loading">加载中…</p>{/if}

{#if rules.length === 0 && !loading}
  <div class="empty">暂无替换规则。</div>
{:else}
  <div class="grid">
    {#each rules as r}
      <div class="card">
        <div class="row">
          <div style="flex:1">
            <div style="font-weight:600">{r.name || '(未命名)'}{#if r.group} <span class="badge">{r.group}</span>{/if}</div>
            <div class="muted" style="font-family:ui-monospace,monospace;word-break:break-all">{r.pattern} → {r.replacement}</div>
            <div class="muted" style="margin-top:4px">
              {r.is_regex ? '正则' : '普通'} · {r.scope_title ? '标题 ' : ''}{r.scope_content ? '正文' : ''}
              {#if !r.is_enabled}<span class="badge">已禁用</span>{/if}
            </div>
          </div>
          <div class="row">
            <button class="ghost" on:click={() => openEdit(r)}>编辑</button>
            <button class="danger" on:click={() => remove(r.id)}>删除</button>
          </div>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if showEditor}
  <div class="modal-mask" on:click|self={() => (showEditor = false)}>
    <div class="modal">
      <h2>{editing ? '编辑规则' : '新建规则'}</h2>
      <div class="grid cols2" style="margin-bottom:10px">
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">名称</div>
          <input bind:value={form.name} />
        </div>
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">分组</div>
          <input bind:value={form.group} />
        </div>
      </div>
      <div class="muted" style="font-size:12px;margin-bottom:4px">匹配（pattern）</div>
      <input bind:value={form.pattern} style="margin-bottom:10px" />
      <div class="muted" style="font-size:12px;margin-bottom:4px">替换为（replacement）</div>
      <input bind:value={form.replacement} style="margin-bottom:10px" />
      <div class="row">
        <label class="row"><input type="checkbox" style="width:auto" bind:checked={form.is_regex} /><span>正则</span></label>
        <label class="row"><input type="checkbox" style="width:auto" bind:checked={form.is_enabled} /><span>启用</span></label>
        <label class="row"><input type="checkbox" style="width:auto" bind:checked={form.scope_title} /><span>作用于标题</span></label>
        <label class="row"><input type="checkbox" style="width:auto" bind:checked={form.scope_content} /><span>作用于正文</span></label>
      </div>
      <div class="muted" style="font-size:12px;margin:10px 0 4px">排除范围（可选）</div>
      <input bind:value={form.exclude_scope} />
      {#if error}<p class="err">{error}</p>{/if}
      <div class="row" style="margin-top:12px">
        <div class="spacer"></div>
        <button class="ghost" on:click={() => (showEditor = false)}>取消</button>
        <button on:click={save}>保存</button>
      </div>
    </div>
  </div>
{/if}
