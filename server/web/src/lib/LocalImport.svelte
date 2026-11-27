<script>
  import { createEventDispatcher } from 'svelte';
  import { api } from './api.js';
  import { describeFailure } from './apiErrors.js';

  const dispatch = createEventDispatcher();

  // 本地 EPUB / CBZ 导入：inspect（上传 + 解压 + 查重）→ 用户确认 → commit。
  let fileInput = null;
  let importing = false;
  let committing = false;
  let importProgress = null;
  let importErr = '';
  let importToken = '';
  let importBook = null;
  let importDups = [];
  let importTarget = '';

  function pickBook() {
    importErr = '';
    fileInput?.click();
  }

  async function onBookChosen(ev) {
    const file = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!file) return;
    const isCbz = /\.cbz$/i.test(file.name);
    if (!isCbz && !/\.epub$/i.test(file.name)) {
      importErr = '请选择 .epub 或 .cbz 文件';
      return;
    }
    importErr = '';
    importing = true;
    importProgress = { loaded: 0, total: file.size || 0 };
    importToken = '';
    importBook = null;
    importDups = [];
    importTarget = '';
    try {
      const upload = isCbz ? api.libraryImportCbz : api.libraryImportEpub;
      const res = await upload(file, (loaded, total) => {
        importProgress = { loaded, total };
      });
      importToken = res.token;
      importBook = res.book;
      importDups = res.duplicates || [];
      importTarget = importDups[0]?.book_url || '';
    } catch (e) {
      if (!e.aborted) importErr = describeFailure(e, isCbz ? '导入 CBZ' : '导入 EPUB');
    } finally {
      importing = false;
      importProgress = null;
    }
  }

  function dupKind(kind) {
    if (kind === 'file') return '同一文件';
    if (kind === 'content') return '正文相同';
    return '书名作者相同';
  }

  async function commitImport(mode) {
    if (!importToken) return;
    committing = true;
    importErr = '';
    try {
      const res = await api.libraryImportCommit({
        token: importToken,
        mode,
        ...(mode === 'overwrite' ? { target_book_url: importTarget } : {}),
      });
      if (res.cancelled) {
        closeImport();
        return;
      }
      const errs = (res.check?.issues || []).filter((i) => i.severity === 'error').length;
      const book = importBook;
      dispatch('imported', { book, result: res, errs });
      closeImport();
    } catch (e) {
      importErr = describeFailure(e, '保存导入');
    } finally {
      committing = false;
    }
  }

  async function cancelImport() {
    const token = importToken;
    closeImport();
    if (token) {
      // 取消只是清理服务端暂存；失败也不打扰用户。
      api.libraryImportCommit({ token, mode: 'cancel' }).catch(() => {});
    }
  }

  function closeImport() {
    importToken = '';
    importBook = null;
    importDups = [];
    importTarget = '';
    importErr = '';
    importProgress = null;
  }

  function importPercent() {
    if (!importProgress?.total) return null;
    return Math.min(100, Math.round((importProgress.loaded * 100) / importProgress.total));
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024 * 1024) return (v / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }
</script>

<div class="card import-card">
  <div class="import-main">
    <h3>本地文件入馆</h3>
    <p class="muted tiny-note">
      EPUB 文字书或 CBZ 漫画。先上传解析并查重，确认后才写入馆藏；同一本书只保留一份内容。
    </p>
  </div>
  <input
    type="file"
    accept=".epub,.cbz,application/epub+zip,application/vnd.comicbook+zip"
    bind:this={fileInput}
    on:change={onBookChosen}
    hidden
  />
  <button on:click={pickBook} disabled={importing}>
    {importing ? '上传解析中…' : '选择 EPUB / CBZ'}
  </button>
</div>

{#if importing || importBook || importErr}
  <div class="modal-mask" on:click|self={() => (importBook ? cancelImport() : closeImport())}>
    <div class="modal" role="dialog" aria-modal="true" aria-label="导入书籍">
      <h3>导入书籍</h3>
      {#if importing}
        <p class="muted tiny-note">正在上传并解析，完成后会先查重，不会直接写入馆藏。</p>
        {#if importPercent() != null}
          <div class="meter" aria-hidden="true"><div style={`width:${importPercent()}%`}></div></div>
          <p class="muted tiny-note">
            {fmtBytes(importProgress.loaded)} / {fmtBytes(importProgress.total)}
          </p>
        {/if}
      {:else if importBook}
        <p>
          <b>{importBook.title || '未命名'}</b>
          {#if importBook.author}<span class="muted"> · {importBook.author}</span>{/if}
        </p>
        <p class="muted tiny-note">
          {#if importBook.kind === 'comic'}
            {importBook.chapter_count} 章 · {importBook.page_count} 页
          {:else}
            {importBook.chapter_count} 章 · {importBook.image_count} 张插图
            {#if importBook.has_cover} · 有封面{/if}
          {/if}
          {#if importBook.file_name} · {importBook.file_name}{/if}
        </p>
        {#if importBook.warnings?.length}
          <div class="warn-box">
            <div class="warn-title">导入提示</div>
            {#each importBook.warnings as w}
              <div class="warn-item">{w}</div>
            {/each}
          </div>
        {/if}
        {#if importDups.length}
          <div class="dup-box">
            <p>馆藏里已有相似的书：</p>
            <ul>
              {#each importDups as d}
                <li>
                  <label>
                    <input type="radio" bind:group={importTarget} value={d.book_url} />
                    <span class="badge">{dupKind(d.kind)}</span>
                    {d.name || d.book_url}{#if d.author}<span class="muted"> · {d.author}</span>{/if}
                  </label>
                </li>
              {/each}
            </ul>
            <p class="muted tiny-note">
              覆盖会沿用原书号与书架位置，修订号递增；选择「新增一本」则保留两本。
            </p>
          </div>
        {/if}
        <div class="import-actions">
          {#if importDups.length}
            <button on:click={() => commitImport('overwrite')} disabled={committing || !importTarget}>
              {committing ? '写入中…' : '覆盖选中的书'}
            </button>
            <button class="ghost" on:click={() => commitImport('new')} disabled={committing}>新增一本</button>
          {:else}
            <button on:click={() => commitImport('new')} disabled={committing}>
              {committing ? '写入中…' : '导入'}
            </button>
          {/if}
          <button class="ghost" on:click={cancelImport} disabled={committing}>取消</button>
        </div>
      {/if}
      {#if importErr}
        <p class="err">{importErr}</p>
        <div class="import-actions">
          <button class="ghost" on:click={closeImport}>关闭</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .import-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 16px 18px;
  }
  .import-main { min-width: 0; }
  .import-main h3 { margin: 0 0 4px; }
  .import-main .tiny-note { margin: 0; max-width: 620px; }
  .meter { height: 6px; border: 1px solid var(--border); background: transparent; }
  .meter div { height: 100%; background: var(--accent); }
  .dup-box {
    margin: 10px 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .dup-box ul { margin: 6px 0; padding-left: 18px; display: grid; gap: 6px; }
  .dup-box li { list-style: none; }
  .dup-box label { display: flex; align-items: baseline; gap: 6px; flex-wrap: wrap; cursor: pointer; }
  .import-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    flex-wrap: wrap;
    margin-top: 12px;
  }
  @media (max-width: 560px) {
    .import-card { flex-direction: column; align-items: stretch; }
  }
</style>
