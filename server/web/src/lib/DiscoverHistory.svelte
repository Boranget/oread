<script>
  import { createEventDispatcher } from 'svelte';
  import { clearHistory, history, removeHistory } from '../lib/searchHistory.js';

  export let busy = false;

  const dispatch = createEventDispatcher();
</script>

{#if $history.length}
  <div class="hist" aria-label="搜索历史">
    <div class="hist-head">
      <span class="muted">最近搜索（{$history.length}）</span>
      <button class="link-btn" on:click={clearHistory}>清空历史</button>
    </div>
    <div class="hist-list">
      {#each $history as h (h)}
        <span class="hist-item">
          <button class="hist-word" title={h} on:click={() => dispatch('pick', h)} disabled={busy}>{h}</button>
          <button class="hist-del" aria-label="删除搜索历史 {h}" on:click={() => removeHistory(h)}>×</button>
        </span>
      {/each}
    </div>
  </div>
{/if}

<style>
  /* 搜索历史：块级 + 行内块，墨水屏不依赖 flex/grid */
  .hist {
    margin-top: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 8px;
    background: var(--card, #fff);
  }
  .hist-head {
    font-size: 12px;
    margin-bottom: 6px;
  }
  .hist-head .link-btn {
    float: right;
  }
  .hist-item {
    display: inline-block;
    margin: 0 6px 6px 0;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 6px;
    background: var(--card, #fff);
  }
  .hist-word {
    background: transparent;
    color: var(--fg, #1d1d1f);
    border: 0;
    padding: 5px 8px;
    font-size: 12px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hist-word:hover {
    background: rgba(179, 57, 47, 0.08);
  }
  .hist-del {
    background: transparent;
    color: var(--muted, #666);
    border: 0;
    padding: 5px 8px;
    font-size: 12px;
  }
  .link-btn {
    background: transparent;
    color: var(--accent);
    border: 0;
    padding: 2px 6px;
    font-size: 12px;
    text-decoration: underline;
  }
  .link-btn:hover {
    background: transparent;
  }
</style>
