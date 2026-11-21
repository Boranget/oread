<script>
  import { createEventDispatcher } from 'svelte';
  import { api, describeFailure } from '../lib/api.js';

  export let bookUrl = '';
  export let show = false;
  export let currentPos = -1;
  export let chapterTitle = '';
  export let hasContent = false;
  export let getRatio = () => 0;

  const dispatch = createEventDispatcher();

  let bookmarks = [];
  let bmNote = '';
  let bmLoading = false;
  let bmErr = '';
  let bmBusy = false;
  let opened = false;

  // 打开面板时自动刷新，避免其它端新增/删除的书签不显示。
  $: if (show && !opened) {
    opened = true;
    loadBookmarks();
  }
  $: if (!show) opened = false;

  async function loadBookmarks() {
    if (!bookUrl) {
      bmErr = '缺少书籍地址，无法读取书签。请从书架重新打开这本书。';
      return;
    }
    bmLoading = true;
    bmErr = '';
    try {
      const list = await api.bookmarks(bookUrl);
      bookmarks = Array.isArray(list) ? list : [];
    } catch (e) {
      bmErr = describeFailure(e, '读取书签');
    } finally {
      bmLoading = false;
    }
  }

  async function addBookmark() {
    if (currentPos < 0) return;
    if (!hasContent) {
      dispatch('toast', '正文还没加载完，稍后再加书签');
      return;
    }
    bmBusy = true;
    bmErr = '';
    const ratio = Math.min(1, Math.max(0, Number(getRatio()) || 0));
    const note = bmNote.trim().slice(0, 2000);
    try {
      await api.addBookmark({
        book_id: bookUrl,
        chapter_index: currentPos,
        position: ratio,
        chapter_title: chapterTitle,
        ...(note ? { note } : {}),
      });
      bmNote = '';
      dispatch('toast', `已加书签：第 ${currentPos + 1} 章 · 章内 ${Math.round(ratio * 100)}%${note ? ' · 含备注' : ''}`);
      await loadBookmarks();
    } catch (e) {
      bmErr = describeFailure(e, '添加书签');
    } finally {
      bmBusy = false;
    }
  }

  async function removeBookmark(bm) {
    bmBusy = true;
    bmErr = '';
    try {
      await api.deleteBookmark(bm.id);
      bookmarks = bookmarks.filter((x) => x.id !== bm.id);
      dispatch('toast', '已删除书签');
    } catch (e) {
      bmErr = describeFailure(e, '删除书签');
    } finally {
      bmBusy = false;
    }
  }

  // 后端 created_at 是 SQLite UTC 的 "YYYY-MM-DD HH:MM:SS"，按本地时区展示
  function bookmarkTime(s) {
    const raw = String(s || '').trim();
    if (!raw) return '';
    const m = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}:\d{2})$/.exec(raw);
    const d = m ? new Date(`${m[1]}T${m[2]}Z`) : new Date(raw);
    if (Number.isNaN(d.getTime())) return raw;
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="panel-mask" on:click|self={close}>
    <div class="panel bm-panel" role="dialog" aria-label="书签">
      <div class="panel-head">
        <strong>书签（{bookmarks.length}）</strong>
        <button class="rd-btn" on:click={close}>关闭</button>
      </div>
      <div class="bm-tools">
        <button class="rd-btn solid" on:click={addBookmark} disabled={bmBusy || currentPos < 0}>加当前页书签</button>
        <button class="rd-btn" on:click={loadBookmarks} disabled={bmLoading}>刷新</button>
      </div>
      <input
        class="bm-note"
        maxlength="2000"
        placeholder="备注（可选，最多 2000 字）"
        aria-label="书签备注"
        bind:value={bmNote}
        on:keydown={(e) => e.key === 'Enter' && !bmBusy && addBookmark()}
      />
      <p class="rd-muted small-note">书签按账号保存；位置使用当前章节 + 0–1 的章内比例，和阅读进度同一套量纲。</p>
      {#if bmErr}<p class="rd-error">{bmErr}</p>{/if}
      {#if !bookmarks.length}
        <p class="rd-muted">{bmLoading ? '读取书签…' : '还没有书签。翻到想标记的位置，点「加当前页书签」。'}</p>
      {:else}
        <div class="bm-list">
          {#each bookmarks as bm (bm.id)}
            <div class="bm-item" class:current={bm.chapter_index === currentPos}>
              <button class="bm-main" on:click={() => dispatch('goto', bm)}>
                <strong>{bm.chapter_title || `第 ${Number(bm.chapter_index) + 1} 章`}</strong>
                <span class="rd-muted">
                  第 {Number(bm.chapter_index) + 1} 章 · 章内 {Math.round(Number(bm.position) * 100)}%{#if bm.chapter_index === currentPos} · 当前章{/if}
                </span>
                <span class="rd-muted bm-time">{bookmarkTime(bm.created_at)}</span>
                {#if bm.note}<span class="bm-note-text">{bm.note}</span>{/if}
              </button>
              <button class="rd-btn" on:click={() => removeBookmark(bm)} disabled={bmBusy}>删除</button>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .rd-btn {
    background: transparent;
    color: var(--rd-fg);
    border: 1px solid var(--rd-border);
    border-radius: 8px;
    padding: 7px 12px;
    font-size: 13px;
    min-height: 38px;
    white-space: nowrap;
  }
  .rd-btn:hover { background: var(--rd-border); }
  .rd-btn.solid { background: var(--rd-accent); color: #fff; border-color: var(--rd-accent); }
  .rd-btn:disabled { opacity: 0.4; }
  .rd-error { color: #c0392b; font-size: 13px; margin: 0; }
  :global(.theme-dark) .rd-error { color: #e88; }
  .rd-muted { color: var(--rd-muted); }
  .small-note { margin: 10px 0 0; font-size: 12px; line-height: 1.6; }

  /* 底部设置面板 */
  .panel-mask { position: fixed; top: 0; right: 0; bottom: 0; left: 0; background: rgba(0,0,0,0.3); z-index: 28; display: flex; align-items: flex-end; }
  .panel {
    width: 100%;
    max-height: 86vh;
    overflow-y: auto;
    background: var(--rd-panel);
    border-top-left-radius: 16px;
    border-top-right-radius: 16px;
    padding: 14px 16px calc(18px + env(safe-area-inset-bottom));
    box-shadow: 0 -4px 18px rgba(0,0,0,0.18);
  }
  .panel-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; font-size: 14px; }

  .bm-tools { display: flex; gap: 8px; margin-bottom: 4px; }
  .bm-note {
    width: 100%; margin: 6px 0 2px;
    background: transparent; color: var(--rd-fg);
    border: 1px solid var(--rd-border); border-radius: 8px; padding: 8px 10px; font-size: 13px;
  }
  .bm-note-text { font-size: 12px; color: var(--rd-fg); white-space: pre-wrap; word-break: break-word; }
  .bm-list { display: flex; flex-direction: column; gap: 6px; max-height: 52vh; overflow-y: auto; }
  .bm-item { display: flex; align-items: center; gap: 8px; border-top: 1px solid var(--rd-border); padding-top: 8px; }
  .bm-main {
    flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px;
    text-align: left; background: transparent; color: var(--rd-fg);
    border: none; padding: 4px 0;
  }
  .bm-main strong { font-size: 14px; }
  .bm-main span { font-size: 12px; }
  .bm-main .bm-time { font-size: 11px; }
  .bm-item.current .bm-main { border-left: 3px solid var(--rd-accent); padding-left: 8px; }
  .bm-item.current .bm-main strong { font-weight: 700; }
</style>
