<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';

  let rules = [];
  let error = '';
  let loading = false;
  let showEditor = false;
  let editing = null; // id 或 null
  let form = blank();

  // ---- 导入 ----
  let importBusy = false;
  let importMsg = '';
  let importProgress = '';
  let importResults = []; // {name, status: 'ok'|'skipped'|'failed', reason}

  // ---- 导出 ----
  let exportBusy = false;
  let exportMsg = '';

  // ---- 预览 ----
  let showPreview = false;
  let previewInput = '';
  let previewBook = '';
  let previewOrigin = '';
  let previewForTitle = false;
  let previewBusy = false;
  let previewErr = '';
  let previewRes = null;
  let selected = new Set();

  const SKIP_REASONS = {
    disabled: '规则已停用',
    scope_flag: '作用域开关不含此目标',
    empty_pattern: 'pattern 为空',
    scope_mismatch: '书名/书源不在 scope 内',
    excluded: '命中排除范围',
    pattern_too_long: '正则超过 512 字符',
  };

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
      // 预览默认选中全部规则，顺序与后端一致（order, id）
      selected = new Set((rules || []).map((r) => r.id));
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

  // ---- 导入：逐条提交，给出每一条的成功/跳过/失败原因 ----
  function parseRuleItems(raw) {
    const v = JSON.parse(raw);
    if (Array.isArray(v)) return v;
    if (v && Array.isArray(v.rules)) return v.rules;
    if (v && typeof v === 'object') return [v];
    throw new Error('规则包格式不正确：需要 JSON 数组或 {rules:[...]}');
  }

  async function onFilePick(ev) {
    const input = ev.target;
    const file = input.files && input.files[0];
    input.value = '';
    if (!file) return;
    error = '';
    importMsg = '';
    importResults = [];
    let items;
    try {
      items = parseRuleItems(await file.text());
    } catch (e) {
      error = '解析失败：' + (e.message || '不是合法的 JSON');
      return;
    }
    if (!items.length) {
      error = '规则包为空，没有可导入的规则';
      return;
    }
    importBusy = true;
    const results = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i] || {};
      const label = String(item.name || '').trim() || `第 ${i + 1} 条`;
      importProgress = `${i + 1} / ${items.length}`;
      if (!String(item.pattern || '').trim()) {
        results.push({ name: label, status: 'skipped', reason: 'pattern 为空' });
        importResults = results.slice();
        continue;
      }
      try {
        // 逐条发送：后端整包导入遇到坏规则会整体 400，这里按条提交才能精确到条目
        const res = await api.importReplaceRules(item);
        if (res && res.inserted > 0) {
          results.push({ name: label, status: 'ok', reason: '' });
        } else if (res && res.skipped > 0) {
          results.push({ name: label, status: 'skipped', reason: 'pattern 为空' });
        } else {
          results.push({ name: label, status: 'failed', reason: '后端未写入该条' });
        }
      } catch (e) {
        results.push({ name: label, status: 'failed', reason: e.message || '导入失败' });
      }
      importResults = results.slice();
    }
    importBusy = false;
    importProgress = '';
    const ok = results.filter((r) => r.status === 'ok').length;
    const skip = results.filter((r) => r.status === 'skipped').length;
    const fail = results.filter((r) => r.status === 'failed').length;
    importMsg = `导入完成：成功 ${ok} 条 / 跳过 ${skip} 条 / 失败 ${fail} 条`;
    await load();
  }

  // ---- 导出：Legado 原生格式，文件名带日期 ----
  async function exportRules() {
    error = '';
    exportMsg = '';
    exportBusy = true;
    try {
      const data = await api.exportReplaceRules();
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const d = new Date();
      const p = (n) => String(n).padStart(2, '0');
      const filename = `replace-rules-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}.json`;
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      const n = Array.isArray(data) ? data.length : 1;
      exportMsg = `已导出 ${n} 条规则：${filename}`;
    } catch (e) {
      error = e.message;
    } finally {
      exportBusy = false;
    }
  }

  // ---- 预览：只用后端 /api/replace-rules/preview，前端不做第二套替换 ----
  function toggleSelect(id) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selected = next;
  }
  function selectAll(on) {
    selected = on ? new Set(rules.map((r) => r.id)) : new Set();
  }

  function openPreview() {
    showPreview = true;
    previewErr = '';
    previewRes = null;
    if (!previewInput) {
      previewInput = '第12章 广告内容\n一秒记住【顶点小说】 www.example.com，更新快，无弹窗！\n萧炎抬头望向天空。';
    }
  }

  async function runPreview() {
    previewErr = '';
    previewRes = null;
    if (!previewInput.trim()) {
      previewErr = '请先输入一段样例文本';
      return;
    }
    const chosen = rules
      .filter((r) => selected.has(r.id))
      // 去掉本地自增 id；其它字段原样交给后端（snake_case 与 Legado camelCase 都支持）
      .map(({ id, ...rest }) => rest);
    if (!chosen.length) {
      previewErr = '请至少勾选一条规则';
      return;
    }
    previewBusy = true;
    try {
      previewRes = await api.previewReplaceRules({
        rules: chosen,
        input: previewInput,
        book: previewBook,
        origin: previewOrigin,
        for_title: previewForTitle,
      });
    } catch (e) {
      previewErr = e.message;
    } finally {
      previewBusy = false;
    }
  }

  function skipText(reason) {
    if (!reason) return '';
    if (reason.startsWith('invalid_regex')) return '正则无效：' + reason.slice('invalid_regex'.length).replace(/^\s*[:：]?\s*/, '');
    return SKIP_REASONS[reason] || reason;
  }
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">替换规则</h2>
  <div class="spacer"></div>
  <button on:click={openNew}>新建规则</button>
  <button class="ghost" on:click={openPreview}>预览</button>
  <button class="ghost" disabled={exportBusy} on:click={exportRules}>
    {exportBusy ? '导出中…' : '导出'}
  </button>
</div>

{#if error}<p class="err">{error}</p>{/if}
{#if exportMsg}<p class="muted">{exportMsg}</p>{/if}

<div class="io-box">
  <div class="io-title">导入（Legado 导出的规则 JSON）</div>
  <input type="file" accept=".json,application/json" disabled={importBusy} on:change={onFilePick} />
  {#if importBusy}<p class="muted">正在导入 {importProgress}…</p>{/if}
  {#if importMsg}<p class="muted">{importMsg}</p>{/if}
  {#if importResults.length}
    <ul class="io-results">
      {#each importResults as r, i}
        <li class="io-item {r.status}">
          <span class="io-name">{i + 1}. {r.name}</span>
          {#if r.status === 'ok'}
            <span class="io-tag ok">成功</span>
          {:else if r.status === 'skipped'}
            <span class="io-tag skip">跳过</span>
            <span class="muted">{r.reason}</span>
          {:else}
            <span class="io-tag fail">失败</span>
            <span class="muted">{r.reason}</span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

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

{#if showPreview}
  <div class="modal-mask" on:click|self={() => (showPreview = false)}>
    <div class="modal preview-modal">
      <div class="row">
        <h2 style="margin:0">规则预览</h2>
        <div class="spacer"></div>
        <button class="ghost" on:click={() => (showPreview = false)}>关闭</button>
      </div>
      <p class="muted">预览由后端真实接口执行：勾选的规则按列表顺序叠加，逐步展示每一步结果。</p>

      <div class="muted" style="font-size:12px;margin:8px 0 4px">样例文本</div>
      <textarea rows="5" bind:value={previewInput}></textarea>

      <div class="grid cols2" style="margin:8px 0">
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">书名（用于 scope 匹配，可选）</div>
          <input bind:value={previewBook} />
        </div>
        <div>
          <div class="muted" style="font-size:12px;margin-bottom:4px">书源域名（可选）</div>
          <input bind:value={previewOrigin} />
        </div>
      </div>
      <label class="row"><input type="checkbox" style="width:auto" bind:checked={previewForTitle} /><span>按“标题”作用域预览（缺省按正文）</span></label>

      <div class="row" style="margin:10px 0 4px">
        <span class="muted" style="font-size:12px">选择规则（{selected.size} / {rules.length}）</span>
        <div class="spacer"></div>
        <button class="ghost" on:click={() => selectAll(true)}>全选</button>
        <button class="ghost" on:click={() => selectAll(false)}>全不选</button>
      </div>
      <div class="pick-list">
        {#each rules as r}
          <label class="pick-item">
            <input type="checkbox" style="width:auto" checked={selected.has(r.id)} on:change={() => toggleSelect(r.id)} />
            <span>{r.name || r.pattern}{#if !r.is_enabled}<span class="badge">已禁用</span>{/if}</span>
          </label>
        {/each}
      </div>

      {#if previewErr}<p class="err">{previewErr}</p>{/if}
      <div class="row" style="margin-top:10px">
        <div class="spacer"></div>
        <button disabled={previewBusy} on:click={runPreview}>{previewBusy ? '预览中…' : '开始预览'}</button>
      </div>

      {#if previewRes}
        <div class="pv-block">
          <div class="pv-label">原始文本（{previewRes.input_length} 字符）</div>
          <pre class="pv-pre">{previewInput}</pre>
        </div>
        {#each previewRes.steps as s}
          <div class="pv-step">
            <div class="pv-head">
              <span>第 {s.index + 1} 步：{s.name || s.pattern}</span>
              {#if s.skipped_reason}
                <span class="io-tag skip">跳过</span>
              {:else}
                <span class="io-tag ok">{s.hits} 次命中</span>
              {/if}
            </div>
            {#if s.skipped_reason}
              <div class="muted">跳过原因：{skipText(s.skipped_reason)}</div>
            {:else}
              <pre class="pv-pre">{s.output}{#if s.output_truncated}<span class="muted">…（已截断）</span>{/if}</pre>
            {/if}
          </div>
        {/each}
        <div class="pv-block">
          <div class="pv-label">最终结果（{previewRes.output_length} 字符）</div>
          <pre class="pv-pre">{previewRes.final}{#if previewRes.final_truncated}<span class="muted">…（已截断）</span>{/if}</pre>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .io-box {
    display: block;
    border: 1px solid var(--border, #ddd);
    border-radius: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
  }
  .io-title { font-weight: 600; margin-bottom: 6px; }
  .io-results {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    max-height: 240px;
    overflow-y: auto;
  }
  .io-item {
    padding: 4px 0;
    border-bottom: 1px solid var(--border, #eee);
    font-size: 13px;
    word-break: break-all;
  }
  .io-name { margin-right: 8px; }
  .io-tag {
    display: inline-block;
    padding: 1px 6px;
    border: 1px solid currentColor;
    border-radius: 4px;
    font-size: 12px;
    margin-right: 6px;
  }
  .io-tag.ok { color: #1a7f37; }
  .io-tag.skip { color: #9a6700; }
  .io-tag.fail { color: #cf222e; }
  .preview-modal { max-width: 760px; width: 92vw; }
  .preview-modal textarea { width: 100%; box-sizing: border-box; font-family: inherit; }
  .pick-list {
    max-height: 180px;
    overflow-y: auto;
    border: 1px solid var(--border, #eee);
    border-radius: 6px;
    padding: 6px 8px;
  }
  .pick-item {
    display: block;
    padding: 3px 0;
    font-size: 13px;
    word-break: break-all;
  }
  .pv-block, .pv-step {
    margin-top: 10px;
    border: 1px solid var(--border, #eee);
    border-radius: 6px;
    padding: 8px 10px;
  }
  .pv-label { font-weight: 600; font-size: 13px; margin-bottom: 4px; }
  .pv-head { font-size: 13px; }
  .pv-head .io-tag { margin-left: 6px; }
  .pv-pre {
    margin: 6px 0 0;
    padding: 8px;
    background: var(--bg-soft, #f6f8fa);
    border-radius: 4px;
    font-family: ui-monospace, monospace;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 180px;
    overflow-y: auto;
  }
</style>
