<script>
  import { onMount } from 'svelte';
  import { api, getRole } from '../lib/api.js';

  // 书源写操作（导入/新增/编辑/启停/删除）需要 admin；普通用户给出明确只读态
  const isAdmin = getRole() === 'admin';

  let sources = [];
  let error = '';
  let notice = '';
  let busy = '';
  let loading = false;
  let filterStatus = 'all'; // all | enabled | disabled
  let filterTier = 'all'; // all | preferred | observing | quarantined
  let filterGroup = 'all';
  let keyword = '';

  // 三档标签只用黑白灰，墨水屏友好
  function sourceTier(s) {
    const t = (s && s.tier) || 'observing';
    return t === 'preferred' || t === 'quarantined' ? t : 'observing';
  }
  function tierText(t) {
    return t === 'preferred' ? '优选' : t === 'quarantined' ? '隔离' : '观察';
  }

  let selected = new Set();
  let fileInput = null;

  // 详情
  let detailOpen = false;
  let detailLoading = false;
  let detail = null;
  let detailError = '';
  let detailEmpty = false;
  let detailUrl = '';

  // 字段级编辑
  let EditorComponent = null; // 按需加载，避免编辑表单进入首屏主包
  let editorOpen = false;
  let editorSource = null;
  let editorSaving = false;
  let editorValidating = false;
  let editorValidation = null;
  let editorError = '';
  let editorUrl = '';
  let editorBaseline = null; // 打开时的详情快照，保存前读回比对

  // 单源校验（只查规则，check_url=false，不发远端请求）
  let validations = {}; // url -> { valid, errors, warnings }
  let validatingUrl = '';

  // 批量检查更新按需加载，避免把 diff/合并逻辑塞进首屏主包
  let UpdateChecker = null;
  let showUpdate = false;

  async function openUpdateChecker() {
    if (showUpdate) return;
    if (UpdateChecker) {
      showUpdate = true;
      return;
    }
    busy = '加载检查更新模块…';
    error = '';
    try {
      const mod = await import('../lib/SourceUpdateChecker.svelte');
      UpdateChecker = mod.default;
      showUpdate = true;
    } catch (e) {
      error = `加载检查更新模块失败：${e.message || e}`;
    } finally {
      busy = '';
    }
  }

  // 真实合集里存在没有名字的书源，列表要给出可辨认的占位
  function sname(s) { return (s && s.book_source_name) || '未命名书源'; }

  function newSource() {
    return {
      book_source_name: '',
      book_source_url: '',
      book_source_group: '',
      book_source_type: 0,
      enabled: true,
      search_url: '',
      book_source_comment: '',
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
  }

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      sources = (await api.sources()) || [];
      selected = new Set([...selected].filter((u) => sources.some((s) => s.book_source_url === u)));
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  $: groupOptions = [...new Set(sources.map((s) => s.book_source_group).filter(Boolean))];
  $: tierCounts = sources.reduce(
    (acc, s) => {
      acc[sourceTier(s)] += 1;
      return acc;
    },
    { preferred: 0, observing: 0, quarantined: 0 }
  );
  $: filtered = sources.filter((s) => {
    if (filterStatus === 'enabled' && !s.enabled) return false;
    if (filterStatus === 'disabled' && s.enabled) return false;
    if (filterTier !== 'all' && sourceTier(s) !== filterTier) return false;
    if (filterGroup !== 'all' && (s.book_source_group || '') !== filterGroup) return false;
    if (keyword.trim()) {
      const k = keyword.trim().toLowerCase();
      return (s.book_source_name || '').toLowerCase().includes(k) ||
        (s.book_source_url || '').toLowerCase().includes(k);
    }
    return true;
  });
  // 可搜索计数由后端 `searchable` 派生（enabled + 类型 + 搜索规则 + 非登录源）
  $: searchableCount = sources.filter((s) => s.searchable).length;
  // 全选只作用于可参与搜索的源；需登录源仍可见可编辑，但不进批量选择
  $: selectable = filtered.filter((s) => !s.login_required);
  $: allSelected = selectable.length > 0 && selectable.every((s) => selected.has(s.book_source_url));

  function toggleSelect(url) {
    const next = new Set(selected);
    next.has(url) ? next.delete(url) : next.add(url);
    selected = next;
  }
  function toggleSelectAll() {
    selected = allSelected
      ? new Set([...selected].filter((u) => !selectable.some((s) => s.book_source_url === u)))
      : new Set([...selected, ...selectable.map((s) => s.book_source_url)]);
  }
  function clearSelection() {
    selected = new Set();
  }

  function isEmptyDetail(d) {
    return !d || typeof d !== 'object' || Object.keys(d).length === 0 || !d.book_source_url;
  }

  async function openDetail(url) {
    detailUrl = url;
    detailOpen = true;
    detailLoading = true;
    detail = null;
    detailError = '';
    detailEmpty = false;
    try {
      const d = await api.sourceDetail(url);
      if (isEmptyDetail(d)) detailEmpty = true;
      else detail = d;
    } catch (e) {
      detailError = e.message;
    } finally {
      detailLoading = false;
    }
  }

  function closeDetail() {
    detailOpen = false;
    detail = null;
    detailError = '';
    detailEmpty = false;
  }

  function downloadJson(data, filename) {
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
    const a = document.createElement('a');
    const href = URL.createObjectURL(blob);
    a.href = href;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(href), 1000);
  }

  let exportTier = 'all'; // all | preferred

  async function exportAll() {
    error = '';
    busy = '正在导出…';
    try {
      const list = await api.exportSources(exportTier);
      const d = new Date();
      const stamp = `${d.getFullYear()}${String(d.getMonth() + 1).padStart(2, '0')}${String(d.getDate()).padStart(2, '0')}`;
      downloadJson(list, `oread-sources-${exportTier === 'preferred' ? 'preferred-' : ''}${stamp}.json`);
      notice = `已导出 ${(list || []).length} 个书源（${exportTier === 'preferred' ? '仅优选' : '全部'}）`;
    } catch (e) {
      error = '导出失败：' + e.message;
    } finally {
      busy = '';
    }
  }

  async function exportOne() {
    error = '';
    try {
      const list = await api.exportSources();
      const found = (list || []).find(
        (s) => s.bookSourceUrl === detailUrl || s.book_source_url === detailUrl
      );
      if (!found) {
        error = '导出失败：导出列表中找不到该书源';
        return;
      }
      const name = (found.bookSourceName || found.book_source_name || 'source').replace(/[\\/:*?"<>|]/g, '_');
      downloadJson(found, `${name}.json`);
      notice = '已导出该书源';
    } catch (e) {
      error = '导出失败：' + e.message;
    }
  }

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
        await load();
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

  async function importFromFile(e) {
    const file = e.target.files && e.target.files[0];
    e.target.value = '';
    if (!file) return;
    error = '';
    notice = '';
    busy = `正在导入 ${file.name}…`;
    try {
      const text = await file.text();
      let arr;
      try {
        arr = JSON.parse(text);
      } catch {
        throw new Error('文件不是合法 JSON');
      }
      const list = Array.isArray(arr) ? arr : [arr];
      const res = await api.importSources(list);
      await load();
      if (res && res.count !== undefined) {
        notice = `导入完成：共 ${res.count} 个，新增 ${res.inserted ?? 0}，更新 ${res.updated ?? 0}`;
      } else {
        notice = `导入完成：${list.length} 个`;
      }
    } catch (err) {
      error = '导入失败：' + err.message;
    } finally {
      busy = '';
    }
  }

  async function toggleOne(s) {
    error = '';
    busy = `正在${s.enabled ? '禁用' : '启用'}《${sname(s)}》…`;
    try {
      // 使用批量状态接口，避免读详情→整份回写的并发覆盖
      await api.setSourcesEnabled([s.book_source_url], !s.enabled);
      await load();
      notice = `已${s.enabled ? '禁用' : '启用'}《${sname(s)}》`;
    } catch (e) {
      error = e.message;
    } finally {
      busy = '';
    }
  }

  async function batchSet(enabled) {
    if (!selected.size) return;
    const urls = [...selected];
    error = '';
    notice = '';
    busy = `正在批量${enabled ? '启用' : '禁用'} ${urls.length} 个书源…`;
    try {
      const r = await api.setSourcesEnabled(urls, enabled);
      selected = new Set();
      await load();
      notice = `已${enabled ? '启用' : '禁用'} ${r?.updated ?? urls.length} 个书源` +
        (r?.missing?.length ? `；${r.missing.length} 个未找到` : '');
    } catch (e) {
      error = '批量操作失败：' + e.message;
    } finally {
      busy = '';
    }
  }

  // 按需加载编辑组件：只在打开新建/编辑时才拉取该 chunk
  async function ensureEditor() {
    if (!EditorComponent) {
      EditorComponent = (await import('../lib/SourceRulesEditor.svelte')).default;
    }
    return EditorComponent;
  }

  function snapshotOf(v) {
    return JSON.parse(JSON.stringify(v ?? null));
  }

  // 深度比较时按 key 排序，避免两次 GET 的字段顺序差异造成误报
  function canonicalJson(v) {
    if (v === undefined) return 'null';
    if (Array.isArray(v)) return '[' + v.map(canonicalJson).join(',') + ']';
    if (v && typeof v === 'object') {
      return '{' + Object.keys(v).sort().map((k) => JSON.stringify(k) + ':' + canonicalJson(v[k])).join(',') + '}';
    }
    return JSON.stringify(v);
  }

  function sameSource(a, b) {
    return canonicalJson(a) === canonicalJson(b);
  }

  async function openNew() {
    try {
      await ensureEditor();
    } catch (e) {
      error = '加载编辑组件失败：' + e.message;
      return;
    }
    editorSource = newSource();
    editorUrl = '';
    editorBaseline = null;
    editorValidation = null;
    editorError = '';
    editorOpen = true;
    closeDetail();
  }

  async function openEdit(url) {
    error = '';
    editorError = '';
    editorValidation = null;
    try {
      const [s] = await Promise.all([api.sourceDetail(url).catch(() => null), ensureEditor()]);
      if (isEmptyDetail(s)) {
        error = '书源详情为空（后端 detail 接口未返回内容），暂时无法编辑字段；可先删除后重新导入。';
        return;
      }
      editorSource = s;
      editorUrl = url;
      editorBaseline = snapshotOf(s);
      editorOpen = true;
      closeDetail();
    } catch (e) {
      error = '读取详情失败：' + e.message;
    }
  }

  function closeEditor() {
    if (editorSaving) return;
    editorOpen = false;
    editorSource = null;
    editorValidation = null;
    editorError = '';
    editorUrl = '';
    editorBaseline = null;
  }

  async function onEditorSave(e) {
    editorError = '';
    editorSaving = true;
    try {
      // POST /sources 是覆盖写：先读回，若打开后被其他标签页改动则要求确认
      if (editorUrl && editorBaseline) {
        const fresh = await api.sourceDetail(editorUrl).catch(() => null);
        if (isEmptyDetail(fresh)) {
          editorError = '该书源已不存在（可能已被删除），无法覆盖保存。';
          return;
        }
        if (!sameSource(editorBaseline, fresh)) {
          if (!confirm('该源已被修改，是否覆盖？')) {
            editorError = '已取消保存：该源在打开后被修改，请关闭后重新打开以获取最新内容。';
            return;
          }
        }
      }
      await api.saveSource(e.detail.source);
      editorOpen = false;
      editorSource = null;
      editorUrl = '';
      editorBaseline = null;
      await load();
      notice = '书源已保存';
    } catch (err) {
      editorError = err.message;
    } finally {
      editorSaving = false;
    }
  }

  async function onEditorValidate(e) {
    editorError = '';
    editorValidation = null;
    editorValidating = true;
    try {
      editorValidation = await api.validateSource(e.detail.source, false);
    } catch (err) {
      editorError = '校验请求失败：' + err.message;
    } finally {
      editorValidating = false;
    }
  }

  async function validateOne(s) {
    if (validatingUrl) return;
    validatingUrl = s.book_source_url;
    error = '';
    try {
      const d = await api.sourceDetail(s.book_source_url);
      if (isEmptyDetail(d)) throw new Error('书源详情为空，无法校验规则');
      const r = await api.validateSource(d, false);
      validations = {
        ...validations,
        [s.book_source_url]: r && typeof r === 'object'
          ? r
          : { valid: false, errors: ['校验接口未返回结果'], warnings: [] },
      };
    } catch (e) {
      validations = {
        ...validations,
        [s.book_source_url]: { valid: false, errors: [e.message || '校验请求失败'], warnings: [] },
      };
    } finally {
      validatingUrl = '';
    }
  }

  async function remove(url) {
    if (!confirm('删除该书源？此操作不可恢复。')) return;
    try {
      await api.deleteSource(url);
      await load();
      notice = '书源已删除';
    } catch (e) {
      error = e.message;
    }
  }
</script>

<div class="row" style="margin-bottom:10px">
  <h2 style="margin:0">书源管理</h2>
  <div class="spacer"></div>
  <span class="muted" style="font-size:12px">{sources.length} 个书源 · 可搜索 {searchableCount}</span>
</div>

{#if !isAdmin}
  <div class="warn-box readonly">
    <div class="warn-title">只读模式：书源由管理员维护</div>
    <div class="warn-item">当前账号不是管理员，可正常搜索、发现和阅读，但不能新增 / 导入 / 启用停用 / 删除书源。需要修改请联系管理员，或使用管理员账号登录。</div>
  </div>
{/if}

<div class="row" style="margin-bottom:10px">
  {#if isAdmin}
    <button class="ghost" on:click={() => fileInput.click()} disabled={!!busy}>导入 JSON</button>
  {/if}
  {#if isAdmin}
    <select bind:value={exportTier} style="width:auto" aria-label="导出范围">
      <option value="all">全部书源</option>
      <option value="preferred">只导优选</option>
    </select>
    <button class="ghost" on:click={exportAll} disabled={!!busy}>导出</button>
    <button class="ghost" on:click={openUpdateChecker} disabled={!!busy}>检查更新</button>
  {/if}
  <div class="spacer"></div>
  {#if isAdmin}
    <button on:click={openNew}>新建</button>
  {/if}
  <input class="file-input" type="file" accept=".json,application/json" bind:this={fileInput} on:change={importFromFile} />
</div>

<div class="row" style="margin-bottom:6px">
  <input placeholder="搜索名称 / URL" bind:value={keyword} style="flex:1;min-width:140px" />
  <select bind:value={filterStatus} style="width:auto" aria-label="启用状态筛选">
    <option value="all">全部状态</option>
    <option value="enabled">已启用</option>
    <option value="disabled">已禁用</option>
  </select>
  <select bind:value={filterGroup} style="width:auto" aria-label="分组筛选">
    <option value="all">全部分组</option>
    {#each groupOptions as g}
      <option value={g}>{g}</option>
    {/each}
  </select>
</div>

<!-- 三档汇总：点一下按档筛选，再点一次复位 -->
<div class="row tier-summary" style="margin-bottom:8px">
  <button
    class="tier-chip tier-preferred"
    class:active={filterTier === 'preferred'}
    on:click={() => (filterTier = filterTier === 'preferred' ? 'all' : 'preferred')}
  >优选 {tierCounts.preferred}</button>
  <button
    class="tier-chip tier-observing"
    class:active={filterTier === 'observing'}
    on:click={() => (filterTier = filterTier === 'observing' ? 'all' : 'observing')}
  >观察 {tierCounts.observing}</button>
  <button
    class="tier-chip tier-quarantined"
    class:active={filterTier === 'quarantined'}
    on:click={() => (filterTier = filterTier === 'quarantined' ? 'all' : 'quarantined')}
  >隔离 {tierCounts.quarantined}</button>
</div>

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

{#if isAdmin && selected.size}
  <div class="batch-bar">
    <span>已选 {selected.size} 个</span>
    <div class="spacer"></div>
    <button class="ghost" on:click={() => batchSet(true)} disabled={!!busy}>批量启用</button>
    <button class="ghost" on:click={() => batchSet(false)} disabled={!!busy}>批量禁用</button>
    <button class="ghost" on:click={clearSelection}>取消</button>
  </div>
{/if}

{#if busy}<p class="loading">{busy}</p>{/if}
{#if error}<p class="err">{error}</p>{/if}
{#if notice}<p class="ok-line">{notice}</p>{/if}

{#if showUpdate && UpdateChecker}
  <svelte:component this={UpdateChecker} {sources} />
{/if}

{#if loading}
  <p class="loading">加载中…</p>
{:else if sources.length === 0}
  <div class="empty">{isAdmin ? '还没有书源，点击「导入 JSON」或「新建」开始。' : '管理员还没有导入书源，导入后这里会显示列表。'}</div>
{:else if filtered.length === 0}
  <div class="empty">没有符合筛选条件的书源。</div>
{:else}
  <div class="row" style="justify-content:space-between;margin-bottom:8px">
    {#if isAdmin}
      <button class="ghost" on:click={toggleSelectAll}>{allSelected ? '取消全选' : '全选当前'}</button>
    {:else}
      <span></span>
    {/if}
    <span class="muted" style="font-size:12px">显示 {filtered.length} 个</span>
  </div>
  <div class="grid cols2">
    {#each filtered as s (s.book_source_url)}
      <div class="card" class:disabled-card={!s.enabled}>
        <div class="row" style="align-items:flex-start;gap:10px">
          {#if isAdmin}
            <input
              type="checkbox"
              style="width:auto;min-width:18px;height:18px;margin-top:2px"
              checked={selected.has(s.book_source_url)}
              on:change={() => toggleSelect(s.book_source_url)}
              aria-label="选择 {sname(s)}"
            />
          {/if}
          <div style="flex:1;min-width:0">
            <div style="font-weight:600">{sname(s)}</div>
            <div class="muted" style="word-break:break-all;font-size:12px">{s.book_source_url}</div>
            <div class="row" style="gap:4px;margin-top:6px">
              {#if s.book_source_group}<span class="badge">{s.book_source_group}</span>{/if}
              {#if s.type_name || s.type}<span class="badge">类型 {s.type_name || s.type}</span>{/if}
              <span class="badge" class:badge-off={!s.enabled}>{s.enabled ? '启用' : '禁用'}</span>
              {#if s.login_required}<span class="badge">需登录·已跳过</span>{/if}
              <span class="badge tier-badge tier-{sourceTier(s)}">{tierText(sourceTier(s))}</span>
              {#if s.weight}<span class="badge">权重 {s.weight}</span>{/if}
            </div>
          </div>
        </div>
        <div class="row" style="margin-top:10px">
          {#if isAdmin}
            <button class="ghost" on:click={() => openDetail(s.book_source_url)}>详情</button>
            <button class="ghost" on:click={() => validateOne(s)} disabled={validatingUrl === s.book_source_url}>
              {validatingUrl === s.book_source_url ? '校验中…' : '校验'}
            </button>
            <button class="ghost" on:click={() => toggleOne(s)} disabled={!!busy}>{s.enabled ? '禁用' : '启用'}</button>
            <div class="spacer"></div>
            <button class="danger" on:click={() => remove(s.book_source_url)}>删除</button>
          {/if}
        </div>
        {#if validations[s.book_source_url]}
          {@const v = validations[s.book_source_url]}
          <div class="val-box" class:val-bad={!v.valid}>
            <div class="val-head">
              校验{v.valid ? '通过' : '未通过'}
              {#if v.errors && v.errors.length}&nbsp;· {v.errors.length} 个错误{/if}
              {#if v.warnings && v.warnings.length}&nbsp;· {v.warnings.length} 条提示{/if}
            </div>
            {#if v.errors && v.errors.length}
              <div class="val-line">{v.errors[0]}</div>
              {#if v.errors.length > 1}
                <details class="val-detail">
                  <summary>其余错误（{v.errors.length - 1}）</summary>
                  {#each v.errors.slice(1) as msg}<div class="val-line">{msg}</div>{/each}
                </details>
              {/if}
            {/if}
            {#if v.warnings && v.warnings.length}
              <details class="val-detail">
                <summary>提示</summary>
                {#each v.warnings as msg}<div class="val-line">{msg}</div>{/each}
              </details>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<!-- 书源详情 -->
{#if detailOpen}
  <div class="modal-mask" role="presentation" on:click|self={closeDetail}>
    <div class="modal" role="dialog" aria-modal="true">
      <div class="row" style="margin-bottom:10px">
        <h2 style="margin:0;flex:1;min-width:0;word-break:break-all">{detailLoading ? '书源详情' : (detail?.book_source_name || '书源详情')}</h2>
        <button class="ghost" on:click={closeDetail}>关闭</button>
      </div>
      <div class="muted" style="font-size:12px;word-break:break-all;margin-bottom:10px">{detailUrl}</div>

      {#if detailLoading}
        <p class="loading">读取详情…</p>
      {:else if detailError}
        <div class="warn-box">
          <div class="warn-title">读取详情失败</div>
          <div class="warn-item">{detailError}</div>
        </div>
        <div class="row" style="margin-top:10px">
          <button class="ghost" on:click={() => openDetail(detailUrl)}>重试</button>
        </div>
      {:else if detailEmpty}
        <div class="warn-box">
          <div class="warn-title">后端暂未返回该书源详情</div>
          <div class="warn-item">detail 接口返回了空内容（可能书源只存了列表字段，或后端正在修复 content/rules 回填）。搜索、发现、阅读仍可能使用该书源；编辑完整 JSON 需等待后端修复。</div>
        </div>
        <div class="row" style="margin-top:10px">
          <button class="ghost" on:click={() => openDetail(detailUrl)}>重试</button>
          {#if isAdmin}<button class="ghost" on:click={exportOne}>导出该书源</button>{/if}
        </div>
      {:else}
        <div class="detail-grid">
          <div><span class="muted">质量档位</span><b>{tierText(sourceTier(detail))}</b></div>
          <div><span class="muted">质量分</span><b>{detail.score ?? 50}</b></div>
          <div><span class="muted">最近评分</span><b>{detail.last_eval_at ? new Date(detail.last_eval_at).toLocaleString() : '—'}</b></div>
          <div><span class="muted">分组</span><b>{detail.book_source_group || '—'}</b></div>
          <div><span class="muted">权重</span><b>{detail.weight ?? 0}</b></div>
          <div><span class="muted">自定义排序</span><b>{detail.custom_order ?? 0}</b></div>
          <div><span class="muted">状态</span><b>{detail.enabled ? '启用' : '禁用'}</b></div>
          <div><span class="muted">搜索规则</span><b>{detail.rule_search ? '有' : '无'}</b></div>
          <div><span class="muted">目录规则</span><b>{detail.rule_toc ? '有' : '无'}</b></div>
          <div><span class="muted">正文规则</span><b>{detail.rule_content ? '有' : '无'}</b></div>
          <div><span class="muted">发现规则</span><b>{detail.rule_explore ? '有' : '无'}</b></div>
        </div>
        {#if detail.evidence && detail.evidence.components}
          <div class="evidence-box">
            <div class="muted" style="font-size:12px;margin:8px 0 4px">评分构成（0–1 原始分，按配置权重折算成 0–100）</div>
            <div class="detail-grid">
              {#each [['connect', '连通性 25'], ['rule', '规则完整度 15'], ['hit', '命中率 30'], ['parse', '解析成功率 15'], ['ttfb', '速度 15']] as [k, label]}
                <div>
                  <span class="muted">{label}</span>
                  <b>{detail.evidence.components[k] != null ? Number(detail.evidence.components[k]).toFixed(2) : '—'}</b>
                </div>
              {/each}
            </div>
            {#if detail.evidence.reasons && detail.evidence.reasons.length}
              <div class="muted" style="font-size:12px;margin-top:6px;word-break:break-all">{detail.evidence.reasons.join(' · ')}</div>
            {/if}
          </div>
        {/if}
        {#if detail.search_url}<div class="muted" style="font-size:12px;margin:8px 0;word-break:break-all">搜索地址：{detail.search_url}</div>{/if}
        {#if detail.explore_url}<div class="muted" style="font-size:12px;margin:8px 0;word-break:break-all">发现地址：{detail.explore_url}</div>{/if}
        <pre class="json-view">{JSON.stringify(detail, null, 2)}</pre>
        <div class="row" style="margin-top:12px">
          {#if isAdmin}<button class="ghost" on:click={exportOne}>导出该书源</button>{/if}
          <div class="spacer"></div>
          {#if isAdmin}<button on:click={() => openEdit(detailUrl)}>编辑字段</button>{/if}
        </div>
      {/if}
    </div>
  </div>
{/if}

<!-- 字段级编辑 / 新建 -->
{#if editorOpen && EditorComponent}
  <div class="modal-mask" role="presentation" on:click|self={closeEditor}>
    <div class="modal" role="dialog" aria-modal="true">
      <svelte:component
        this={EditorComponent}
        source={editorSource}
        saving={editorSaving}
        validating={editorValidating}
        validation={editorValidation}
        saveError={editorError}
        on:save={onEditorSave}
        on:validate={onEditorValidate}
        on:cancel={closeEditor}
      />
    </div>
  </div>
{/if}

<style>
  .file-input { display: none; }
  .readonly { margin-bottom: 10px; }
  .batch-bar {
    position: sticky;
    top: 48px;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 8px 10px;
    margin-bottom: 10px;
    box-shadow: var(--shadow);
    font-size: 13px;
  }
  .disabled-card { opacity: 0.72; }
  /* 三档标签：黑底白字 / 描边 / 灰底，不使用彩色 */
  .tier-badge { border: 1px solid currentColor; }
  .tier-preferred { background: var(--fg); color: var(--card); border-color: var(--fg); }
  .tier-observing { background: transparent; color: var(--fg); }
  .tier-quarantined { background: var(--border); color: var(--muted); border-color: var(--border); }
  .tier-summary { gap: 6px; flex-wrap: wrap; }
  .tier-chip { padding: 3px 10px; border-radius: 999px; font-size: 12px; cursor: pointer; }
  .tier-chip.active { outline: 2px solid var(--fg); outline-offset: 1px; }
  .evidence-box {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 6px 8px;
    margin-top: 8px;
  }
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
  .val-box {
    margin-top: 8px;
    padding: 6px 8px;
    border: 1px solid #cfe6d6;
    border-radius: 8px;
    background: #f2faf4;
    font-size: 12px;
  }
  .val-bad {
    border-color: #e8c7a7;
    background: #fdf6ec;
  }
  .val-head {
    font-weight: 600;
  }
  .val-detail {
    margin-top: 4px;
  }
  .val-line {
    line-height: 1.5;
    word-break: break-word;
  }
  .detail-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 8px 14px;
    font-size: 13px;
    margin-bottom: 6px;
  }
  .detail-grid > div { display: flex; gap: 8px; align-items: baseline; }
  .json-view {
    max-height: 40vh;
    overflow: auto;
    background: #f7f7f8;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px;
    font-size: 11px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-all;
    margin: 8px 0 0;
  }
</style>
