<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, describeFailure } from './api.js';
  import BookFileEditor from './BookFileEditor.svelte';

  export let bookUrl = '';
  export let initialTab = 'meta';
  export let initialChapter = '';
  export let initialPage = '';
  const dispatch = createEventDispatcher();

  let loading = true;
  let err = '';
  let notice = '';
  let data = null;
  let kind = 'text';
  let revision = 0;
  let chapters = [];
  let tab = 'meta';
  let initialApplied = false;
  let busy = '';
  let form = emptyForm();

  let chapterRef = '';
  let chapterTitle = '';
  let paragraphsText = '';
  let chapterRevision = 0;
  let chapterLoading = false;
  let chapterErr = '';

  let coverPreview = '';
  let coverHref = '';
  let coverMime = '';
  let pageBusy = '';
  let thumbUrls = {};

  $: selectedChapter = chapters.find((c) => refOf(c) === chapterRef) || null;
  $: if (selectedChapter) loadThumbs();

  onMount(() => {
    load();
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      clearThumbs();
    };
  });

  function onKey(e) {
    if (e.key === 'Escape') close();
  }

  function close() {
    dispatch('close');
  }

  function emptyForm() {
    return {
      name: '',
      author: '',
      intro: '',
      language: '',
      publisher: '',
      pubdate: '',
      series_name: '',
      series_index: '',
      subjects: '',
      isbn: '',
    };
  }

  function refOf(c) {
    return c.url || `local:${bookUrl}#${c.id}`;
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  function splitList(text) {
    return String(text || '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
  }

  async function load() {
    loading = true;
    err = '';
    try {
      const res = await api.bookEdit(bookUrl);
      data = res;
      kind = res.kind === 'comic' ? 'comic' : 'text';
      revision = Number(res.revision) || 0;
      chapters = res.chapters || [];
      const m = res.meta || {};
      form = {
        name: m.title || '',
        author: m.author || '',
        intro: m.description || '',
        language: m.language || '',
        publisher: m.publisher || '',
        pubdate: m.pubdate || '',
        series_name: m.series || '',
        series_index: m.series_index == null ? '' : String(m.series_index),
        subjects: (m.subjects || []).join(', '),
        isbn: m.isbn || '',
      };
      coverHref = res.cover ? res.cover.href : '';
      coverPreview = res.cover && res.cover.data_url ? res.cover.data_url : '';
      coverMime = res.cover && res.cover.media_type ? res.cover.media_type : '';
      // 重新载入后保留仍在目录里的当前章节。
      if (!chapters.some((c) => refOf(c) === chapterRef)) chapterRef = '';
      // 首次载入时应用入口参数：从阅读器打开时直接定位到当前章节。
      if (!initialApplied) {
        initialApplied = true;
        if (initialTab === 'content' || initialTab === 'cover' || initialTab === 'files') tab = initialTab;
        if (initialChapter && chapters.some((c) => refOf(c) === initialChapter)) {
          chapterRef = initialChapter;
          await selectChapter();
        }
      }
    } catch (e) {
      err = describeFailure(e, '读取编辑信息');
    } finally {
      loading = false;
    }
  }

  function clearThumbs() {
    for (const url of Object.values(thumbUrls)) {
      if (url) URL.revokeObjectURL(url);
    }
    thumbUrls = {};
  }

  async function loadThumbs() {
    if (kind !== 'comic' || !data || !selectedChapter) {
      clearThumbs();
      return;
    }
    const pages = selectedChapter.pages || [];
    const wanted = new Set(pages.map((p) => p.href));
    for (const [href, url] of Object.entries(thumbUrls)) {
      if (!wanted.has(href) && url) URL.revokeObjectURL(url);
    }
    for (const page of pages) {
      if (thumbUrls[page.href]) continue;
      try {
        const res = await api.comicImage('', `oread-img://${data.book_id}/${page.href}`);
        thumbUrls = { ...thumbUrls, [page.href]: res.objectUrl };
      } catch {
        // 缩略图失败不影响换页；文件选择仍然可用。
      }
    }
  }

  async function selectChapter() {
    chapterErr = '';
    paragraphsText = '';
    chapterTitle = '';
    const c = chapters.find((x) => refOf(x) === chapterRef);
    if (!c) return;
    chapterTitle = c.title || '';
    if (kind === 'comic') return;
    chapterLoading = true;
    try {
      const res = await api.bookEditChapter(bookUrl, chapterRef);
      chapterRevision = Number(res.revision) || 0;
      chapterTitle = res.title || chapterTitle;
      paragraphsText = (res.paragraphs || []).join('\n');
    } catch (e) {
      chapterErr = describeFailure(e, '读取本章');
    } finally {
      chapterLoading = false;
    }
  }

  async function saveMeta() {
    err = '';
    notice = '';
    const rawIndex = String(form.series_index || '').trim();
    const index = rawIndex === '' ? null : Number(rawIndex);
    if (index != null && (!Number.isFinite(index) || index < 0)) {
      err = '系列序号必须是非负数';
      return;
    }
    busy = 'meta';
    try {
      await api.saveLibraryBookMeta({
        book_url: bookUrl,
        name: form.name,
        author: form.author,
        intro: form.intro,
        language: form.language,
        publisher: form.publisher,
        pubdate: form.pubdate,
        series_name: form.series_name,
        series_index: index,
        tags: splitList(form.subjects),
        identifiers: form.isbn.trim()
          ? [{ scheme: 'isbn', value: form.isbn.trim() }]
          : [],
      });
      await load();
      notice = '元数据已写入书目录';
      dispatch('saved');
    } catch (e) {
      err = describeFailure(e, '保存元数据');
    } finally {
      busy = '';
    }
  }

  async function saveChapter() {
    chapterErr = '';
    notice = '';
    const paragraphs = String(paragraphsText || '')
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean);
    if (!paragraphs.length) {
      chapterErr = '正文不能为空';
      return;
    }
    busy = 'chapter';
    try {
      const res = await api.saveBookChapter({
        book_url: bookUrl,
        chapter_url: chapterRef,
        revision: chapterRevision,
        paragraphs,
      });
      revision = Number(res.revision) || revision;
      chapterRevision = revision;
      notice = '本章正文已保存';
      dispatch('saved');
    } catch (e) {
      chapterErr = describeFailure(e, '保存正文');
      // 修订冲突：重新读一次，避免用户继续在旧内容上编辑。
      if (e && e.status === 409) await selectChapter();
    } finally {
      busy = '';
    }
  }

  function readFile(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ''));
      reader.onerror = () => reject(reader.error || new Error('读取文件失败'));
      reader.readAsDataURL(file);
    });
  }

  function mimeOf(dataUrl) {
    const m = /^data:([^;,]+)[;,]/.exec(String(dataUrl || ''));
    return m ? m[1] : '';
  }

  async function onCoverFile(e) {
    const file = e.currentTarget.files && e.currentTarget.files[0];
    e.currentTarget.value = '';
    if (!file) return;
    coverPreview = await readFile(file);
    coverMime = mimeOf(coverPreview) || file.type || '';
  }

  async function saveCover() {
    if (!coverPreview) return;
    err = '';
    notice = '';
    busy = 'cover';
    try {
      const res = await api.saveBookCover({
        book_url: bookUrl,
        revision,
        mime: coverMime || mimeOf(coverPreview) || 'image/jpeg',
        data: coverPreview,
      });
      revision = Number(res.revision) || revision;
      if (res.cover && res.cover.href) coverHref = res.cover.href;
      await load();
      notice = '封面已替换';
      dispatch('saved');
    } catch (e) {
      err = describeFailure(e, '替换封面');
    } finally {
      busy = '';
    }
  }

  async function onPageFile(e, page) {
    const file = e.currentTarget.files && e.currentTarget.files[0];
    e.currentTarget.value = '';
    if (!file) return;
    err = '';
    notice = '';
    pageBusy = page.href;
    try {
      const dataUrl = await readFile(file);
      const res = await api.replaceComicPage({
        book_url: bookUrl,
        chapter_url: chapterRef,
        href: page.href,
        revision,
        mime: mimeOf(dataUrl) || file.type || 'image/jpeg',
        data: dataUrl,
      });
      revision = Number(res.revision) || revision;
      clearThumbs();
      await load();
      notice = '本页已替换';
      dispatch('saved');
    } catch (e) {
      err = describeFailure(e, '替换漫画页');
    } finally {
      pageBusy = '';
    }
  }
</script>

<div class="be-overlay" role="presentation" on:click|self={close}>
  <div class="be-panel" role="dialog" aria-modal="true" aria-label="编辑馆藏">
    <div class="be-head">
      <b class="be-title">{form.name || bookUrl}</b>
      <span class="badge">{kind === 'comic' ? '漫画' : '文字书'}</span>
      <span class="muted tiny-note">修订 {revision}</span>
      <div class="spacer"></div>
      <button class="ghost" on:click={close}>关闭</button>
    </div>

    {#if loading}
      <p class="loading">加载中…</p>
    {:else if err && !data}
      <p class="err">{err}</p>
    {:else if data}
      <div class="seg be-tabs">
        <button class:on={tab === 'meta'} on:click={() => (tab = 'meta')}>元数据</button>
        <button class:on={tab === 'content'} on:click={() => (tab = 'content')}>
          {kind === 'comic' ? '页面' : '正文'}
        </button>
        {#if kind === 'text'}
          <button class:on={tab === 'cover'} on:click={() => (tab = 'cover')}>封面</button>
          <button class:on={tab === 'files'} on:click={() => (tab = 'files')}>文件</button>
        {/if}
      </div>

      {#if err}<p class="err">{err}</p>{/if}
      {#if notice}<p class="ok-line">{notice}</p>{/if}

      {#if tab === 'meta'}
        <div class="be-grid">
          <label class="be-field be-wide">
            <span>书名</span>
            <input bind:value={form.name} />
          </label>
          <label class="be-field">
            <span>作者</span>
            <input bind:value={form.author} />
          </label>
          <label class="be-field">
            <span>语言</span>
            <input bind:value={form.language} placeholder="zh" />
          </label>
          <label class="be-field">
            <span>出版社</span>
            <input bind:value={form.publisher} />
          </label>
          <label class="be-field">
            <span>出版日期</span>
            <input bind:value={form.pubdate} placeholder="YYYY-MM-DD" />
          </label>
          <label class="be-field">
            <span>系列</span>
            <input bind:value={form.series_name} />
          </label>
          <label class="be-field">
            <span>系列序号</span>
            <input bind:value={form.series_index} inputmode="decimal" />
          </label>
          <label class="be-field">
            <span>ISBN</span>
            <input bind:value={form.isbn} />
          </label>
          <label class="be-field be-wide">
            <span>标签（逗号分隔）</span>
            <input bind:value={form.subjects} />
          </label>
          <label class="be-field be-wide">
            <span>简介</span>
            <textarea rows="5" bind:value={form.intro}></textarea>
          </label>
        </div>
        <div class="be-actions">
          <button on:click={saveMeta} disabled={busy === 'meta'}>
            {busy === 'meta' ? '保存中…' : '保存元数据'}
          </button>
        </div>
      {:else if tab === 'content'}
        <div class="be-row">
          <select bind:value={chapterRef} on:change={selectChapter}>
            <option value="">选择章节…</option>
            {#each chapters.filter((c) => !c.is_volume) as c}
              <option value={refOf(c)}>{c.title}{c.cached ? '' : '（未缓存）'}</option>
            {/each}
          </select>
          <button class="ghost" on:click={selectChapter} disabled={!chapterRef || chapterLoading}>
            {chapterLoading ? '读取中…' : '读取'}
          </button>
        </div>

        {#if chapterErr}<p class="err">{chapterErr}</p>{/if}

        {#if kind === 'text'}
          {#if chapterRef && !chapterLoading}
            <p class="muted tiny-note">{chapterTitle} · 每行一段，空行会被忽略 · 修订 {chapterRevision}</p>
            <textarea class="be-body" rows="18" spellcheck="false" bind:value={paragraphsText}></textarea>
            <div class="be-actions">
              <button on:click={saveChapter} disabled={busy === 'chapter'}>
                {busy === 'chapter' ? '保存中…' : '保存本章'}
              </button>
            </div>
          {:else if !chapterRef}
            <p class="muted tiny-note">先选择一章。未缓存的章节需要先打开一次或等整本下载完成。</p>
          {/if}
        {:else if selectedChapter}
          <p class="muted tiny-note">{selectedChapter.title} · {selectedChapter.pages.length} 页 · 只接受 JPEG / WebP</p>
          <div class="be-pages">
            {#each selectedChapter.pages as page, i}
              <div class="be-page" class:be-page-current={initialPage && page.href === initialPage}>
                {#if thumbUrls[page.href]}
                  <img src={thumbUrls[page.href]} alt={`第 ${i + 1} 页`} />
                {:else}
                  <div class="be-page-ph">第 {i + 1} 页</div>
                {/if}
                <div class="muted tiny-note">
                  {page.width && page.height ? `${page.width}×${page.height} · ` : ''}{fmtBytes(page.bytes)}
                </div>
                <label class="be-file">
                  {pageBusy === page.href ? '替换中…' : '替换'}
                  <input
                    type="file"
                    accept="image/jpeg,image/webp"
                    disabled={pageBusy === page.href}
                    on:change={(e) => onPageFile(e, page)}
                  />
                </label>
              </div>
            {/each}
          </div>
        {:else}
          <p class="muted tiny-note">先选择一话，再逐页替换。</p>
        {/if}
      {:else if tab === 'cover'}
        {#if coverPreview}
          <img class="be-cover" src={coverPreview} alt="封面预览" />
        {:else}
          <p class="muted tiny-note">当前没有封面。</p>
        {/if}
        {#if coverHref}<p class="muted tiny-note">文件：OEBPS/{coverHref}</p>{/if}
        <div class="be-row">
          <input type="file" accept="image/jpeg,image/webp" on:change={onCoverFile} />
        </div>
        <div class="be-actions">
          <button on:click={saveCover} disabled={!coverPreview || busy === 'cover'}>
            {busy === 'cover' ? '保存中…' : '保存封面'}
          </button>
        </div>
        <p class="muted tiny-note">只接受 JPEG / WebP；替换后写入书目录的 content.opf，馆藏列表会立即使用新封面。</p>
      {:else if tab === 'files'}
        <BookFileEditor
          bookUrl={bookUrl}
          {revision}
          on:revision={(e) => (revision = e.detail)}
          on:saved={() => dispatch('saved')}
        />
      {/if}
    {/if}
  </div>
</div>

<style>
  .be-overlay {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    left: 0;
    z-index: 50;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 20px 12px;
    overflow: auto;
    background: rgba(0, 0, 0, 0.45);
  }
  .be-panel {
    width: min(980px, 100%);
    padding: 14px 16px 18px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--card, #fff);
    color: inherit;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
  }
  .be-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 8px;
  }
  .be-title {
    overflow-wrap: anywhere;
  }
  .be-tabs {
    margin: 8px 0 12px;
  }
  .be-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 10px 12px;
  }
  .be-field {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .be-field > span {
    font-size: 12px;
    color: var(--muted);
  }
  .be-field input,
  .be-field textarea,
  .be-body {
    width: 100%;
    box-sizing: border-box;
  }
  .be-wide {
    grid-column: 1 / -1;
  }
  .be-body {
    min-height: 260px;
    margin-top: 8px;
    font-family: inherit;
    line-height: 1.6;
    resize: vertical;
  }
  .be-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin: 8px 0;
  }
  .be-row select {
    flex: 1;
    min-width: 180px;
  }
  .be-actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 10px;
  }
  .be-cover {
    max-width: 220px;
    max-height: 320px;
    border: 1px solid var(--border);
    margin: 8px 0;
  }
  .be-pages {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
    gap: 10px;
    margin-top: 10px;
  }
  .be-page {
    display: grid;
    gap: 4px;
    justify-items: start;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    min-width: 0;
  }
  .be-page-current {
    border-color: var(--accent, #3b82f6);
    box-shadow: 0 0 0 2px var(--accent, #3b82f6);
  }
  .be-page img {
    max-width: 100%;
    max-height: 180px;
    border: 1px solid var(--border);
  }
  .be-page-ph {
    width: 100%;
    padding: 24px 0;
    text-align: center;
    border: 1px dashed var(--border);
    color: var(--muted);
  }
  .be-file {
    position: relative;
    display: inline-block;
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    font-size: 13px;
  }
  .be-file input {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    cursor: pointer;
  }
</style>
