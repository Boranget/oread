<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, describeFailure } from './api.js';

  export let bookUrl = '';
  export let revision = 0;
  const dispatch = createEventDispatcher();

  let loading = true;
  let err = '';
  let notice = '';
  let files = [];
  let filter = '';
  let activePath = '';
  let file = null;
  let text = '';
  let loadedText = '';
  let fileLoading = false;
  let saving = false;
  let rev = Number(revision) || 0;

  let showPreview = false;
  let previewBlocks = [];
  let previewTruncated = false;
  let previewBusy = false;
  let previewImages = {};

  let imagePreview = '';
  let imageBusy = false;

  let checking = false;
  let checkReport = null;

  let showReplace = false;
  let rq = '';
  let rr = '';
  let regex = false;
  let caseSensitive = true;
  let replaceBusy = false;
  let replaceResult = null;
  let lastDirty = null;

  $: if (revision > rev) rev = revision;
  $: filteredFiles = filterFiles(files, filter);
  $: dirty = !!(file && file.kind === 'text' && text !== loadedText);
  $: if (dirty !== lastDirty) {
    lastDirty = dirty;
    dispatch('dirty', dirty);
  }
  $: canEditText = !!(file && file.kind === 'text' && file.path !== 'mimetype');
  $: canReplaceImage = !!(
    file &&
    file.kind === 'image' &&
    /\.(jpe?g|webp)$/i.test(file.path)
  );

  onMount(() => {
    loadTree();
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      releaseImage();
      clearPreviewImages();
    };
  });

  function onKey(e) {
    if ((e.ctrlKey || e.metaKey) && (e.key === 's' || e.key === 'S')) {
      if (!canEditText || !dirty) return;
      e.preventDefault();
      save();
    }
  }

  function filterFiles(list, q) {
    const needle = String(q || '').trim().toLowerCase();
    if (!needle) return list;
    return list.filter((f) => f.path.toLowerCase().includes(needle));
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  function setRev(value) {
    const n = Number(value) || 0;
    if (n > rev) {
      rev = n;
      dispatch('revision', rev);
    }
  }

  async function loadTree() {
    loading = true;
    err = '';
    try {
      const res = await api.bookEditFiles(bookUrl);
      files = res.files || [];
      setRev(res.revision);
      if (activePath && !files.some((f) => f.path === activePath)) {
        activePath = '';
        file = null;
        text = '';
        loadedText = '';
      }
    } catch (e) {
      err = describeFailure(e, '读取文件树');
    } finally {
      loading = false;
    }
  }

  function releaseImage() {
    if (imagePreview) URL.revokeObjectURL(imagePreview);
    imagePreview = '';
  }

  function clearPreviewImages() {
    for (const url of Object.values(previewImages)) {
      if (url) URL.revokeObjectURL(url);
    }
    previewImages = {};
  }

  async function loadImagePreview(url) {
    releaseImage();
    if (!url) return;
    try {
      const res = await api.comicImage('', url);
      imagePreview = res.objectUrl;
    } catch {
      // 图片预览失败不影响替换；保留空占位。
    }
  }

  async function openFile(path) {
    if (dirty && !confirm('当前文件有未保存修改，确定切换？')) return;
    activePath = path;
    await readActive();
  }

  async function readActive() {
    if (!activePath) return;
    fileLoading = true;
    err = '';
    notice = '';
    try {
      const res = await api.bookEditFile(bookUrl, activePath);
      file = res;
      text = res.kind === 'text' ? res.text || '' : '';
      loadedText = text;
      setRev(res.revision);
      showPreview = false;
      previewBlocks = [];
      previewTruncated = false;
      clearPreviewImages();
      releaseImage();
      if (res.kind === 'image') await loadImagePreview(res.image_url);
    } catch (e) {
      err = describeFailure(e, '读取文件');
    } finally {
      fileLoading = false;
    }
  }

  async function save() {
    if (!canEditText) return;
    saving = true;
    err = '';
    notice = '';
    try {
      const res = await api.saveBookFile({
        book_url: bookUrl,
        path: activePath,
        revision: rev,
        content: text,
      });
      setRev(res.revision);
      loadedText = text;
      notice = `已保存 · 修订 ${res.revision}`;
      dispatch('saved');
      await loadTree();
    } catch (e) {
      err = describeFailure(e, '保存文件');
      if (e && e.status === 409) {
        err = '修订冲突：这本书已被其它操作修改，已刷新文件树，请重新打开文件后再改。';
        await loadTree();
      }
    } finally {
      saving = false;
    }
  }

  function readFile(fileObj) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ''));
      reader.onerror = () => reject(reader.error || new Error('读取文件失败'));
      reader.readAsDataURL(fileObj);
    });
  }

  async function onImageFile(e) {
    const picked = e.currentTarget.files && e.currentTarget.files[0];
    e.currentTarget.value = '';
    if (!picked) return;
    imageBusy = true;
    err = '';
    notice = '';
    try {
      const dataUrl = await readFile(picked);
      const res = await api.saveBookFileImage({
        book_url: bookUrl,
        path: activePath,
        revision: rev,
        data: dataUrl,
      });
      setRev(res.revision);
      notice = `图片已替换 · 修订 ${res.revision}`;
      dispatch('saved');
      await loadTree();
      if (res.image_url) await loadImagePreview(res.image_url);
      else await readActive();
    } catch (e) {
      err = describeFailure(e, '替换图片');
    } finally {
      imageBusy = false;
    }
  }

  async function removeActive() {
    if (!file) return;
    if (!confirm(`删除 ${file.path}？删除后需要自己清理 OPF manifest。`)) return;
    err = '';
    notice = '';
    try {
      const res = await api.deleteBookFile({
        book_url: bookUrl,
        path: activePath,
        revision: rev,
      });
      setRev(res.revision);
      notice = '文件已删除';
      dispatch('saved');
      activePath = '';
      file = null;
      text = '';
      loadedText = '';
      releaseImage();
      await loadTree();
    } catch (e) {
      err = describeFailure(e, '删除文件');
    }
  }

  async function togglePreview() {
    if (showPreview) {
      showPreview = false;
      return;
    }
    await runPreview();
  }

  async function runPreview() {
    if (!canEditText) return;
    previewBusy = true;
    err = '';
    try {
      const res = await api.previewBookFile({
        book_url: bookUrl,
        path: activePath,
        content: text,
      });
      previewBlocks = res.blocks || [];
      previewTruncated = !!res.truncated;
      showPreview = true;
      clearPreviewImages();
      const images = previewBlocks.filter((b) => b.type === 'image').slice(0, 40);
      for (const b of images) {
        if (!b.src || !String(b.src).startsWith('oread-img://')) continue;
        try {
          const r = await api.comicImage('', b.src);
          previewImages = { ...previewImages, [b.src]: r.objectUrl };
        } catch {
          // 单张图失败时保留原地址占位。
        }
      }
    } catch (e) {
      err = describeFailure(e, '预览');
    } finally {
      previewBusy = false;
    }
  }

  async function runCheck() {
    checking = true;
    err = '';
    try {
      const res = await api.checkBook(bookUrl);
      checkReport = res;
      setRev(res.revision);
    } catch (e) {
      err = describeFailure(e, 'Check book');
    } finally {
      checking = false;
    }
  }

  async function runReplace(dryRun) {
    if (!rq.trim()) {
      err = '查找内容不能为空';
      return;
    }
    if (
      !dryRun &&
      dirty &&
      !confirm('当前文件有未保存修改，替换后会重新载入文件并丢弃这些修改。继续？')
    ) {
      return;
    }
    replaceBusy = true;
    err = '';
    notice = '';
    try {
      const res = await api.replaceBookFiles({
        book_url: bookUrl,
        revision: rev,
        query: rq,
        replacement: rr,
        regex,
        case_sensitive: caseSensitive,
        dry_run: dryRun,
      });
      replaceResult = res;
      if (!dryRun) {
        setRev(res.revision);
        notice = `已替换 ${res.total} 处，涉及 ${res.changed_files.length} 个文件`;
        dispatch('saved');
        await loadTree();
        if (activePath) await readActive();
      }
    } catch (e) {
      err = describeFailure(e, dryRun ? '查找' : '替换');
    } finally {
      replaceBusy = false;
    }
  }
</script>

<div class="fe">
  <div class="fe-bar">
    <button class="ghost" on:click={loadTree} disabled={loading}>刷新文件树</button>
    <button class="ghost" class:on={showReplace} on:click={() => (showReplace = !showReplace)}>
      查找替换
    </button>
    <button class="ghost" on:click={runCheck} disabled={checking}>
      {checking ? '检查中…' : 'Check book'}
    </button>
    <div class="spacer"></div>
    <span class="muted tiny-note">修订 {rev}</span>
  </div>

  {#if err}<p class="err">{err}</p>{/if}
  {#if notice}<p class="ok-line">{notice}</p>{/if}

  {#if showReplace}
    <div class="fe-replace">
      <div class="fe-row">
        <input placeholder="查找" bind:value={rq} />
        <input placeholder="替换为（留空即删除）" bind:value={rr} />
      </div>
      <div class="fe-row">
        <label><input type="checkbox" bind:checked={regex} /> 正则</label>
        <label><input type="checkbox" bind:checked={caseSensitive} /> 区分大小写</label>
        <div class="spacer"></div>
        <button class="ghost" on:click={() => runReplace(true)} disabled={replaceBusy}>
          {replaceBusy ? '处理中…' : '预览匹配'}
        </button>
        <button
          on:click={() => runReplace(false)}
          disabled={replaceBusy || !replaceResult || replaceResult.applied || !replaceResult.total}
        >
          全部替换
        </button>
      </div>
      {#if replaceResult}
        <p class="muted tiny-note">
          {replaceResult.applied ? '已替换' : '匹配'} {replaceResult.total} 处，涉及
          {replaceResult.changed_files.length} 个文件{replaceResult.truncated ? '（预览已截断）' : ''}
        </p>
        {#if replaceResult.hits.length}
          <ul class="fe-hits">
            {#each replaceResult.hits as h}
              <li>
                <button class="fe-link" on:click={() => openFile(h.path)}>{h.path}:{h.line}</button>
                <code>{h.text}</code>
                <span class="muted tiny-note">→ {h.replacement}</span>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="fe-main">
    <aside class="fe-side">
      <input class="fe-filter" placeholder="筛选文件…" bind:value={filter} />
      {#if loading}
        <p class="muted tiny-note">加载中…</p>
      {:else}
        <ul class="fe-files">
          {#each filteredFiles as f}
            <li>
              <button class:on={f.path === activePath} title={f.path} on:click={() => openFile(f.path)}>
                <span class="fe-path">{f.path}</span>
                <span class="fe-size">{fmtBytes(f.size)}</span>
              </button>
            </li>
          {:else}
            <li class="muted tiny-note">没有匹配文件</li>
          {/each}
        </ul>
      {/if}
    </aside>

    <section class="fe-editor">
      {#if !activePath}
        <p class="muted tiny-note">
          从左侧选择文件。章节在 <code>OEBPS/chapters/</code>，样式在 <code>OEBPS/style.css</code>；
          保存前会校验 XML，OPF 只自动推进修订号。
        </p>
      {:else if fileLoading}
        <p class="muted tiny-note">读取中…</p>
      {:else if file}
        <div class="fe-file-head">
          <b class="fe-file-name">{file.path}</b>
          <span class="muted tiny-note">{file.media_type} · {fmtBytes(file.size)}</span>
          <div class="spacer"></div>
          {#if canEditText}
            <button class="ghost" on:click={togglePreview} disabled={previewBusy}>
              {previewBusy ? '预览中…' : showPreview ? '编辑源码' : '预览'}
            </button>
            <button on:click={save} disabled={!dirty || saving}>
              {saving ? '保存中…' : dirty ? '保存' : '已保存'}
            </button>
          {/if}
        </div>

        {#if file.kind === 'text'}
          {#if showPreview}
            <div class="fe-preview">
              {#each previewBlocks as b}
                {#if b.type === 'heading'}
                  <h4>{b.text}</h4>
                {:else if b.type === 'paragraph'}
                  <p>{b.text}</p>
                {:else if b.type === 'list'}
                  <ul class="fe-list">
                    {#each b.items || [] as it}
                      <li style={`margin-left:${(it.level || 0) * 1.2}em`}>{it.text}</li>
                    {/each}
                  </ul>
                {:else if b.type === 'code'}
                  <pre class="fe-pre">{b.text}</pre>
                {:else if b.type === 'image'}
                  {#if previewImages[b.src]}
                    <img src={previewImages[b.src]} alt={b.alt || ''} />
                  {:else}
                    <p class="muted tiny-note">[图片] {b.src}</p>
                  {/if}
                {/if}
              {:else}
                <p class="muted tiny-note">没有可预览的正文块。</p>
              {/each}
              {#if previewTruncated}<p class="muted tiny-note">预览已截断。</p>{/if}
            </div>
          {:else}
            <textarea
              class="fe-code"
              spellcheck="false"
              readonly={!canEditText}
              bind:value={text}
            ></textarea>
            {#if !canEditText}
              <p class="muted tiny-note">mimetype 是 EPUB 必需文件，只读。</p>
            {/if}
          {/if}
        {:else if file.kind === 'image'}
          {#if imagePreview}
            <img class="fe-image" src={imagePreview} alt={file.path} />
          {:else}
            <p class="muted tiny-note">图片预览不可用。</p>
          {/if}
          <div class="fe-row">
            {#if canReplaceImage}
              <label class="fe-file">
                {imageBusy ? '替换中…' : '替换图片'}
                <input
                  type="file"
                  accept="image/jpeg,image/webp"
                  disabled={imageBusy}
                  on:change={onImageFile}
                />
              </label>
              <span class="muted tiny-note">只接受 JPEG / WebP，扩展名必须与内容一致。</span>
            {:else}
              <span class="muted tiny-note">只有 .jpg / .jpeg / .webp 可以替换。</span>
            {/if}
          </div>
        {:else}
          <p class="muted tiny-note">二进制文件只读，可下载后自行替换（字体、音频等）。</p>
        {/if}

        <div class="fe-actions">
          <button class="ghost" on:click={removeActive}>删除文件</button>
        </div>
      {/if}
    </section>
  </div>

  {#if checkReport}
    <div class="fe-check">
      <div class="fe-row">
        <b>Check book</b>
        <span class="muted tiny-note">
          {checkReport.checked_files} 个文件 · {checkReport.errors} 错误 · {checkReport.warnings} 警告
        </span>
        <div class="spacer"></div>
        <button class="ghost" on:click={() => (checkReport = null)}>关闭</button>
      </div>
      {#if !checkReport.issues.length}
        <p class="ok-line">没有发现问题。</p>
      {:else}
        <ul class="fe-issues">
          {#each checkReport.issues as it}
            <li class:fe-issue-error={it.severity === 'error'} class:fe-issue-warn={it.severity !== 'error'}>
              <span class="fe-sev">{it.severity === 'error' ? '错误' : '警告'}</span>
              {#if it.path}
                <button class="fe-link" on:click={() => openFile(it.path)}>{it.path}</button>
              {/if}
              <span>{it.message}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .fe {
    display: grid;
    gap: 8px;
  }
  .fe-bar,
  .fe-row,
  .fe-file-head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .fe-main {
    display: grid;
    grid-template-columns: minmax(200px, 280px) minmax(0, 1fr);
    gap: 10px;
    align-items: start;
  }
  .fe-side {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
    max-height: 560px;
    overflow: auto;
  }
  .fe-filter {
    width: 100%;
    box-sizing: border-box;
    margin-bottom: 6px;
  }
  .fe-files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .fe-files button {
    display: flex;
    gap: 6px;
    width: 100%;
    justify-content: space-between;
    padding: 3px 6px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: inherit;
    text-align: left;
    font-size: 12px;
    cursor: pointer;
  }
  .fe-files button:hover {
    background: rgba(127, 127, 127, 0.12);
  }
  .fe-files button.on {
    background: rgba(59, 130, 246, 0.16);
  }
  .fe-path {
    overflow-wrap: anywhere;
  }
  .fe-size {
    color: var(--muted);
    white-space: nowrap;
  }
  .fe-editor {
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
  }
  .fe-file-name {
    overflow-wrap: anywhere;
    font-size: 13px;
  }
  .fe-code {
    width: 100%;
    box-sizing: border-box;
    min-height: 420px;
    margin-top: 8px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    line-height: 1.5;
    tab-size: 2;
    resize: vertical;
    white-space: pre;
    overflow: auto;
  }
  .fe-preview {
    margin-top: 8px;
    max-height: 520px;
    overflow: auto;
    padding: 4px 10px;
    line-height: 1.7;
  }
  .fe-preview h4 {
    margin: 14px 0 6px;
  }
  .fe-preview p {
    margin: 6px 0;
  }
  .fe-preview img {
    max-width: 100%;
    display: block;
    margin: 8px auto;
  }
  .fe-preview ul {
    margin: 6px 0;
    padding-left: 1.2em;
  }
  .fe-preview .fe-pre {
    margin: 8px 0;
    padding: 8px;
    overflow: auto;
    background: var(--panel-2, #f6f6f6);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .fe-image {
    max-width: 100%;
    max-height: 420px;
    display: block;
    margin: 8px 0;
    border: 1px solid var(--border);
  }
  .fe-actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 8px;
  }
  .fe-replace,
  .fe-check {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px;
  }
  .fe-replace input:not([type]) {
    flex: 1;
    min-width: 160px;
  }
  .fe-hits,
  .fe-issues {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: grid;
    gap: 4px;
    max-height: 260px;
    overflow: auto;
    font-size: 12px;
  }
  .fe-hits li,
  .fe-issues li {
    display: flex;
    gap: 6px;
    align-items: baseline;
    flex-wrap: wrap;
    padding: 3px 0;
    border-bottom: 1px dashed var(--border);
  }
  .fe-hits code {
    overflow-wrap: anywhere;
  }
  .fe-sev {
    flex: none;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    background: rgba(127, 127, 127, 0.16);
  }
  .fe-issue-error .fe-sev {
    background: rgba(220, 38, 38, 0.18);
    color: #b91c1c;
  }
  .fe-issue-warn .fe-sev {
    background: rgba(217, 119, 6, 0.18);
    color: #b45309;
  }
  .fe-link {
    border: 0;
    background: transparent;
    padding: 0;
    color: var(--accent, #2563eb);
    cursor: pointer;
    font-size: inherit;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .fe-file {
    position: relative;
    display: inline-block;
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
  }
  .fe-file input {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    cursor: pointer;
  }
  @media (max-width: 720px) {
    .fe-main {
      grid-template-columns: minmax(0, 1fr);
    }
    .fe-side {
      max-height: 220px;
    }
  }
</style>
