<script>
  import { createEventDispatcher } from 'svelte';
  import { api, describeFailure } from '../lib/api.js';

  export let show = false;
  export let title = '';
  export let author = '';
  export let sourceId = '';

  const dispatch = createEventDispatcher();

  let switchLoading = false;
  let switchErr = '';
  let candidates = [];
  let opened = false;

  $: if (show && !opened) {
    opened = true;
    if (!candidates.length && !switchLoading) loadCandidates();
  }
  $: if (!show) opened = false;

  async function loadCandidates() {
    switchLoading = true;
    switchErr = '';
    candidates = [];
    try {
      const list = await api.switchSource(title, author, sourceId);
      candidates = Array.isArray(list) ? list : [];
    } catch (e) {
      switchErr = describeFailure(e, '换源');
    } finally {
      switchLoading = false;
    }
  }

  function useCandidate(c) {
    // 换源后旧源的 seed 全部作废：只带候选自己的地址、书源与书名/作者/封面；
    // 候选缺地址或缺来源时拒绝切换，避免回退到旧源造成“看似换源、实际还在旧源”。
    const nextUrl = c.book_url || c.bookUrl || '';
    const nextSource = c.origin || c.source_id || '';
    if (!nextUrl || !nextSource) {
      switchErr = '该候选缺少书籍地址或书源标识，无法切换。请换一个书源或重新搜索。';
      return;
    }
    dispatch('choose', c);
  }

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="panel-mask" on:click|self={close}>
    <div class="panel switch-panel">
      <div class="panel-head">
        <strong>换源</strong>
        <button class="rd-btn" on:click={close}>关闭</button>
      </div>
      <p class="rd-muted small-note">当前：《{title}》 · 书源 {sourceId}</p>
      {#if switchLoading}
        <p class="rd-muted">正在其他书源搜索同名书籍…</p>
      {:else if switchErr}
        <p class="rd-error">{switchErr}</p>
      {:else if !candidates.length}
        <p class="rd-muted">没有找到其他书源的同名书籍。可重新搜索，或先在「发现」页确认书源可用。</p>
      {:else}
        <div class="switch-list">
          {#each candidates as c (c.book_url + '|' + (c.origin || ''))}
            <div class="switch-item">
              <div class="switch-main">
                <b>{c.name || c.book_url}</b>
                <span class="rd-muted">{c.author || '未知作者'} · {c.origin_name || c.origin || '未知书源'}</span>
                {#if c.latest_chapter_title}<span class="rd-muted small-note">最新：{c.latest_chapter_title}</span>{/if}
              </div>
              <button class="rd-btn" on:click={() => useCandidate(c)}>用此书源</button>
            </div>
          {/each}
        </div>
      {/if}
      <div class="row" style="margin-top:10px">
        <button class="rd-btn" on:click={loadCandidates} disabled={switchLoading}>重新搜索</button>
      </div>
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
  .switch-panel { max-height: 72vh; display: flex; flex-direction: column; }
  .switch-list { overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }
  .switch-item { display: flex; align-items: center; gap: 10px; border-top: 1px solid var(--rd-border); padding-top: 8px; }
  .switch-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .switch-main b { font-size: 14px; }
</style>
