<script>
  import { createEventDispatcher } from 'svelte';
  import { api } from './api.js';

  export let isAdmin = false;
  export let busy = '';

  const dispatch = createEventDispatcher();

  // ---- 体检语料（内置不可删改，只能停用；用户条目可改可删） ----
  let corpusOpen = false;
  let corpusLoading = false;
  let corpus = [];
  let corpusError = '';
  let corpusNotice = '';
  let editingCorpusId = '';
  let editingCorpus = null;
  const corpusCategories = ['玄幻', '都市', '历史', '科幻', '出版', '漫画', '其他'];
  let newCorpus = { title: '', expectedAuthor: '', category: '玄幻', type: 0 };

  // 体检是异步任务：POST 返回 run_id，前端每 1.5s 轮询进度
  let benchRunning = false;
  let benchJob = null;
  let benchError = '';

  async function runBenchNow() {
    if (benchRunning) return;
    benchRunning = true;
    benchError = '';
    benchJob = null;
    try {
      const r = await api.runBench('all', 5);
      await pollBench(r.run_id);
    } catch (e) {
      benchError = e.message || String(e);
      benchRunning = false;
    }
  }

  async function pollBench(runId) {
    try {
      for (;;) {
        const v = await api.benchRunStatus(runId);
        benchJob = v;
        if (v.status === 'completed' || v.status === 'failed') break;
        await new Promise((res) => setTimeout(res, 1500));
      }
      if (benchJob.status === 'failed') {
        benchError = benchJob.error || '体检失败';
      } else {
        dispatch('done');
      }
    } finally {
      benchRunning = false;
    }
  }

  function benchStatusText() {
    if (!benchJob) return '';
    if (benchJob.status === 'running')
      return `体检中：已跑 ${benchJob.sources_done}/${benchJob.sources_total} 个源…`;
    if (benchJob.status === 'failed') return '体检失败';
    const secs = ((benchJob.elapsed_ms || 0) / 1000).toFixed(1);
    return `完成：精选 ${benchJob.curated_total}（小说 ${benchJob.novel_total} / 漫画 ${benchJob.comic_total}），用时 ${secs}s`;
  }

  async function loadCorpus() {
    corpusLoading = true;
    corpusError = '';
    try {
      const r = await api.benchCorpus();
      corpus = (r && r.items) || [];
    } catch (e) {
      corpusError = e.message || String(e);
    } finally {
      corpusLoading = false;
    }
  }

  function toggleCorpusPanel(e) {
    corpusOpen = e.currentTarget.open;
    if (corpusOpen && !corpus.length) loadCorpus();
  }

  async function addCorpus() {
    corpusError = '';
    corpusNotice = '';
    if (!newCorpus.title.trim()) {
      corpusError = '书名不能为空';
      return;
    }
    try {
      const r = await api.addBenchCorpus(newCorpus);
      corpusNotice = `已添加「${r.item.title}」，将参与下次体检`;
      newCorpus = { title: '', expectedAuthor: '', category: newCorpus.category, type: newCorpus.type };
      await loadCorpus();
    } catch (e) {
      corpusError = e.message || String(e);
    }
  }

  function startCorpusEdit(c) {
    editingCorpusId = c.id;
    editingCorpus = {
      title: c.title,
      expectedAuthor: c.expected_author || '',
      category: c.category,
      type: c.source_type,
      enabled: c.enabled,
    };
  }

  function cancelCorpusEdit() {
    editingCorpusId = '';
    editingCorpus = null;
  }

  async function saveCorpusEdit(c) {
    corpusError = '';
    corpusNotice = '';
    try {
      await api.updateBenchCorpus(c.id, editingCorpus);
      corpusNotice = `已保存「${editingCorpus.title}」`;
      cancelCorpusEdit();
      await loadCorpus();
    } catch (e) {
      corpusError = e.message || String(e);
    }
  }

  async function removeCorpus(c) {
    if (!confirm(`删除语料「${c.title}」？删除后不再参与体检。`)) return;
    corpusError = '';
    corpusNotice = '';
    try {
      await api.deleteBenchCorpus(c.id);
      corpusNotice = `已删除「${c.title}」`;
      await loadCorpus();
    } catch (e) {
      corpusError = e.message || String(e);
    }
  }

  async function toggleCorpusEnabled(c) {
    corpusError = '';
    corpusNotice = '';
    try {
      await api.toggleBenchCorpus(c.id, !c.enabled);
      corpusNotice = `${c.enabled ? '已停用' : '已启用'}「${c.title}」`;
      await loadCorpus();
    } catch (e) {
      corpusError = e.message || String(e);
    }
  }
</script>

<!-- 体检语料：内置随版本发布、不可删改（只能停用）；用户条目可改可删，新增后参与下次体检 -->
<details class="bench-box" bind:open={corpusOpen} on:toggle={toggleCorpusPanel}>
  <summary>体检语料（{corpus.length} 条，内置 {corpus.filter((c) => c.builtin).length} / 用户 {corpus.filter((c) => !c.builtin).length}）</summary>
  <div class="bench-body">
    <p class="muted" style="margin:6px 0 8px;font-size:12px">
      体检用「常见书名」量源的匹配度与速度；内置语料不可删除/编辑（标题变动会让历史分数失去可比性），只能停用。
      新增的用户书名会在下一次体检抽样中生效。
    </p>
    {#if corpusLoading}
      <p class="loading">语料加载中…</p>
    {/if}
    {#if corpusError}<p class="err">{corpusError}</p>{/if}
    {#if corpusNotice}<p class="ok-line">{corpusNotice}</p>{/if}

    {#if isAdmin}
      <div class="bench-run">
        <button class="ghost" on:click={runBenchNow} disabled={benchRunning}>
          {benchRunning ? '体检中…' : '跑一次体检'}
        </button>
        {#if benchJob}<span class="muted">{benchStatusText()}</span>{/if}
        {#if benchError}<span class="err">{benchError}</span>{/if}
      </div>
    {/if}

    {#if isAdmin}
      <div class="bench-add">
        <input placeholder="书名（必填）" bind:value={newCorpus.title} />
        <input placeholder="作者（可空）" bind:value={newCorpus.expectedAuthor} />
        <select bind:value={newCorpus.category} aria-label="语料分类">
          {#each corpusCategories as c}<option value={c}>{c}</option>{/each}
        </select>
        <select bind:value={newCorpus.type} aria-label="语料类型">
          <option value={0}>小说</option>
          <option value={2}>漫画</option>
        </select>
        <button class="ghost" on:click={addCorpus} disabled={!!busy}>新增</button>
      </div>
      <p class="muted" style="margin:4px 0 8px;font-size:12px">新增书名将参与下次体检。</p>
    {/if}

    <table class="bench-table">
      <thead>
        <tr>
          <th>书名</th><th>作者</th><th>分类</th><th>类型</th><th>来源</th><th>状态</th><th>操作</th>
        </tr>
      </thead>
      <tbody>
        {#each corpus as c}
          <tr>
            {#if editingCorpusId === c.id}
              <td><input bind:value={editingCorpus.title} /></td>
              <td><input bind:value={editingCorpus.expectedAuthor} /></td>
              <td>
                <select bind:value={editingCorpus.category}>
                  {#each corpusCategories as cat}<option value={cat}>{cat}</option>{/each}
                </select>
              </td>
              <td>
                <select bind:value={editingCorpus.type}>
                  <option value={0}>小说</option>
                  <option value={2}>漫画</option>
                </select>
              </td>
              <td>{c.builtin ? '内置' : '用户'}</td>
              <td><label><input type="checkbox" bind:checked={editingCorpus.enabled} /> 启用</label></td>
              <td>
                <button class="ghost" on:click={() => saveCorpusEdit(c)}>保存</button>
                <button class="ghost" on:click={cancelCorpusEdit}>取消</button>
              </td>
            {:else}
              <td>{c.title}</td>
              <td>{c.expected_author || '—'}</td>
              <td>{c.category}</td>
              <td>{c.source_type === 2 ? '漫画' : '小说'}</td>
              <td>{c.builtin ? '内置（不可删除）' : '用户'}</td>
              <td>
                <label>
                  <input type="checkbox" checked={c.enabled} disabled={!isAdmin} on:change={() => toggleCorpusEnabled(c)} />
                  {c.enabled ? '启用' : '停用'}
                </label>
              </td>
              <td>
                {#if isAdmin}
                  {#if c.builtin}
                    <button class="ghost" disabled title="内置语料不可编辑">编辑</button>
                    <button class="ghost" disabled title="内置语料不可删除">删除</button>
                  {:else}
                    <button class="ghost" on:click={() => startCorpusEdit(c)}>编辑</button>
                    <button class="ghost" on:click={() => removeCorpus(c)}>删除</button>
                  {/if}
                {:else}
                  <span class="muted">只读</span>
                {/if}
              </td>
            {/if}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</details>

<style>
  /* 体检语料：块级 + 表格，墨水屏友好（不依赖 flex/grid） */
  .bench-box {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    margin-bottom: 10px;
    background: var(--card);
  }
  .bench-box > summary {
    cursor: pointer;
    font-size: 13px;
  }
  .bench-run {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin: 4px 0 10px;
    font-size: 12px;
  }
  .bench-add {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .bench-add input { flex: 1; min-width: 110px; }
  .bench-add select { width: auto; }
  .bench-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .bench-table th,
  .bench-table td {
    border-bottom: 1px solid var(--border);
    padding: 5px 6px;
    text-align: left;
    vertical-align: middle;
  }
  .bench-table th { font-weight: 600; color: var(--muted); }
  .bench-table input[type='text'],
  .bench-table input:not([type]) { width: 100%; box-sizing: border-box; }
  .bench-table input[type='checkbox'],
  .bench-add input[type='checkbox'] {
    width: auto;
    min-height: auto;
    margin-right: 4px;
    vertical-align: middle;
  }
  .bench-table button { padding: 2px 6px; font-size: 12px; margin-right: 4px; }
  .bench-table button:disabled { opacity: 0.45; cursor: not-allowed; }
</style>
