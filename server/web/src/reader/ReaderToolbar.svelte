<script>
  import { createEventDispatcher } from 'svelte';

  export let visible = true;
  export let title = '';
  export let showToc = false;
  export let showBookmarks = false;
  export let showSwitch = false;
  export let showPayNotice = false;
  export let payKind = '';

  const dispatch = createEventDispatcher();
</script>

{#if visible}
  <header class="rd-bar">
    <button class="rd-btn" on:click={() => dispatch('back')} aria-label="返回书架">‹ 返回</button>
    <div class="rd-title">{title}</div>
    <button
      class="rd-btn"
      on:click={() => dispatch('toc')}
      aria-expanded={showToc}
      aria-label="打开目录">目录</button
    >
    <button
      class="rd-btn"
      on:click={() => dispatch('bookmarks')}
      aria-expanded={showBookmarks}
      aria-label="书签">书签</button
    >
    <button class="rd-btn" on:click={() => dispatch('export')} aria-label="导出 TXT">导出</button>
    <button
      class="rd-btn"
      on:click={() => dispatch('switch')}
      aria-expanded={showSwitch}
      aria-label="换源">换源</button
    >
    <button class="rd-btn" on:click={() => dispatch('settings')} aria-label="阅读设置">Aa</button>
  </header>
{/if}

{#if showPayNotice}
  <div class="pay-notice" role="note">
    <span class="pay-text">
      {payKind === 'vip' ? '目录标记 V（VIP）' : '目录标记 P（付费）'}：本章可能为付费或试读内容。
    </span>
    <button class="rd-btn" on:click={() => dispatch('dismiss-pay')} aria-label="关闭付费提示"
      >知道了</button
    >
  </div>
{/if}

<style>
  .rd-bar {
    position: relative;
    z-index: 22;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: calc(env(safe-area-inset-top) + 6px) 8px 6px;
    background: var(--rd-panel);
    border-bottom: 1px solid var(--rd-border);
    flex: 0 0 auto;
  }
  .rd-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
    font-size: 14px;
    text-align: center;
  }
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
  .pay-notice {
    position: relative;
    z-index: 21;
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 0 auto;
    padding: 6px 10px;
    font-size: 13px;
    color: var(--rd-fg);
    background: var(--rd-panel);
    border-bottom: 1px solid var(--rd-border);
  }
  .pay-notice .pay-text { flex: 1; min-width: 0; }
  /* 提示条不在 .rd-bar 内，单独保证手机触控目标 44px */
  :global(html[data-vp='sm']) .pay-notice .rd-btn {
    min-height: 44px;
    min-width: 44px;
  }
  :global(html[data-theme='eink']) .pay-notice,
  :global(html[data-theme='eink']) .pay-notice .pay-text {
    color: #000;
    background: #fff;
    border-color: #333;
  }
</style>
