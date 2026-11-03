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
  let filterGroup = 'all';
  let keyword = '';

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
  $: filtered = sources.filter((s) => {
    if (filterStatus === 'enabled' && !s.enabled) return false;
    if (filterStatus === 'disabled' && s.enabled) return false;
    if (filterGroup !== 'all' && (s.book_source_group || '') !== filterGroup) return false;
    if (keyword.trim()) {
      const k = keyword.trim().toLowerCase();
      return (s.book_source_name || '').toLowerCase().includes(k) ||
        (s.book_source_url || '').toLowerCase().includes(k);
    }
    return true;
  });
  $: allSelected = filtered.length > 0 && filtered.every((s) => selected.has(s.book_source_url));

  function toggleSelect(url) {
    const next = new Set(selected);
    next.has(url) ? next.delete(url) : next.add(url);
    selected = next;
  }
  function toggleSelectAll() {
    selected = allSelected
      ? new Set([...selected].filter((u) => !filtered.some((s) => s.book_source_url === u)))
      : new Set([...selected, ...filtered.map((s) => s.book_source_url)]);
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

  async function exportAll() {
    error = '';
    busy = '正在导出…';
    try {
      const list = await api.exportSources();
      const d = new Date();
      const stamp = `${d.getFullYear()}${String(d.getMonth() + 1).padStart(2, '0')}${String(d.getDate()).padStart(2, '0')}`;
      downloadJson(list, `oread-sources-${stamp}.json`);
      notice = `已导出 ${(list || []).length} 个书源`;
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
  <span class="muted" style="font-size:12px">{sources.length} 个书源</span>
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
  <button class="ghost" on:click={exportAll} disabled={!!busy}>导出全部</button>
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
              {#if s.weight}<span class="badge">权重 {s.weight}</span>{/if}
            </div>
          </div>
        </div>
        <div class="row" style="margin-top:10px">
          <button class="ghost" on:click={() => openDetail(s.book_source_url)}>详情</button>
          {#if isAdmin}
            <button class="ghost" on:click={() => toggleOne(s)} disabled={!!busy}>{s.enabled ? '禁用' : '启用'}</button>
            <div class="spacer"></div>
            <button class="danger" on:click={() => remove(s.book_source_url)}>删除</button>
          {/if}
        </div>
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
          <button class="ghost" on:click={exportOne}>导出该书源</button>
        </div>
      {:else}
        <div class="detail-grid">
          <div><span class="muted">分组</span><b>{detail.book_source_group || '—'}</b></div>
          <div><span class="muted">权重</span><b>{detail.weight ?? 0}</b></div>
          <div><span class="muted">自定义排序</span><b>{detail.custom_order ?? 0}</b></div>
          <div><span class="muted">状态</span><b>{detail.enabled ? '启用' : '禁用'}</b></div>
          <div><span class="muted">搜索规则</span><b>{detail.rule_search ? '有' : '无'}</b></div>
          <div><span class="muted">目录规则</span><b>{detail.rule_toc ? '有' : '无'}</b></div>
          <div><span class="muted">正文规则</span><b>{detail.rule_content ? '有' : '无'}</b></div>
          <div><span class="muted">发现规则</span><b>{detail.rule_explore ? '有' : '无'}</b></div>
        </div>
        {#if detail.search_url}<div class="muted" style="font-size:12px;margin:8px 0;word-break:break-all">搜索地址：{detail.search_url}</div>{/if}
        {#if detail.explore_url}<div class="muted" style="font-size:12px;margin:8px 0;word-break:break-all">发现地址：{detail.explore_url}</div>{/if}
        <pre class="json-view">{JSON.stringify(detail, null, 2)}</pre>
        <div class="row" style="margin-top:12px">
          <button class="ghost" on:click={exportOne}>导出该书源</button>
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
  .badge-off { background: #fdecea; color: #b3392f; }
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
