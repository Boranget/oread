<script>
  import { createEventDispatcher, tick } from 'svelte';

  export let show = false;
  export let chapters = [];
  export let currentPos = -1;

  const dispatch = createEventDispatcher();

  const ROW = 46; // 目录行高（px），虚拟滚动按固定行高计算
  const TOC_ORDER_KEY = 'oread_toc_order';

  let tocQuery = '';
  let tocEl = null;
  let tocScroll = 0;
  let tocViewH = 600;
  let opened = false;

  // 目录排序：URL 参数 toc_order 持久化（desc=倒序），本机 localStorage 作兜底
  function readTocOrder() {
    try {
      const q = new URLSearchParams(location.search).get('toc_order');
      if (q === 'desc' || q === 'asc') return q;
      return localStorage.getItem(TOC_ORDER_KEY) === 'desc' ? 'desc' : 'asc';
    } catch {
      return 'asc';
    }
  }

  let tocOrder = readTocOrder();

  function setTocOrder(v) {
    tocOrder = v === 'desc' ? 'desc' : 'asc';
    try {
      localStorage.setItem(TOC_ORDER_KEY, tocOrder);
    } catch {
      /* 私密模式忽略 */
    }
    try {
      const u = new URLSearchParams(location.search);
      if (tocOrder === 'desc') u.set('toc_order', 'desc');
      else u.delete('toc_order');
      const qs = u.toString();
      history.replaceState(null, '', location.pathname + (qs ? '?' + qs : '') + (location.hash || ''));
    } catch {
      /* 无 history 环境忽略 */
    }
  }

  $: chapterTotal = chapters.filter((c) => !c.is_volume).length;
  $: tocItems = (tocOrder === 'desc'
    ? chapters.map((c, i) => ({ c, i })).reverse()
    : chapters.map((c, i) => ({ c, i }))
  )
    .map((it, p) => ({ ...it, p }))
    .filter((it) => {
      const q = tocQuery.trim().toLowerCase();
      return !q || (it.c.title || '').toLowerCase().includes(q);
    });
  $: tocStart = Math.max(0, Math.floor(tocScroll / ROW) - 8);
  $: tocEnd = Math.min(tocItems.length, Math.ceil((tocScroll + tocViewH) / ROW) + 8);
  $: tocVisible = tocItems.slice(tocStart, tocEnd);

  // 打开目录时滚到当前章，不依赖父页的 toggle 时机
  $: if (show && !opened) {
    opened = true;
    tick().then(() => {
      if (!tocEl) return;
      tocViewH = tocEl.clientHeight || tocViewH;
      tocEl.scrollTop = Math.max(0, currentPos * ROW - tocViewH / 2);
    });
  }
  $: if (!show) opened = false;

  async function locateCurrent() {
    await tick();
    if (!tocEl) return;
    const idx = tocItems.findIndex((it) => it.i === currentPos);
    // 筛选把当前章滤掉时，清空筛选再定位，避免点了“当前章”没反应
    if (idx < 0 && tocQuery) {
      tocQuery = '';
      await tick();
      await locateCurrent();
      return;
    }
    if (idx < 0) return;
    tocEl.scrollTop = Math.max(0, idx * ROW - tocViewH / 2);
  }

  function onTocScroll(e) {
    tocScroll = e.target.scrollTop;
    tocViewH = e.target.clientHeight || tocViewH;
  }

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="toc-mask" on:click|self={close}></div>
  <aside class="toc-drawer" aria-label="目录">
    <div class="toc-head">
      <strong>
        目录（{chapterTotal} 章{#if tocQuery.trim()} · 命中 {tocItems.length}{/if}）
      </strong>
      <button class="rd-btn" on:click={close} aria-label="关闭目录">关闭</button>
    </div>
    <div class="toc-tools">
      <input
        placeholder="筛选章节（不区分大小写）"
        aria-label="筛选章节"
        bind:value={tocQuery}
        on:input={() => (tocScroll = 0)}
      />
      <button class="rd-btn" on:click={locateCurrent}>当前章</button>
      <button
        class="rd-btn"
        on:click={() => setTocOrder(tocOrder === 'desc' ? 'asc' : 'desc')}
        aria-label="切换目录排序"
        title="正序/倒序，URL 参数 toc_order 持久化"
      >{tocOrder === 'desc' ? '倒序' : '正序'}</button>
    </div>
    <div class="toc-list" bind:this={tocEl} on:scroll={onTocScroll}>
      <div style={`height:${tocItems.length * ROW}px;position:relative`}>
        {#each tocVisible as it (it.p)}
          <button
            class="toc-row"
            class:active={it.i === currentPos}
            class:vol={it.c.is_volume}
            style={`top:${it.p * ROW}px`}
            on:click={() => dispatch('choose', it.i)}
          >
            <span class="toc-title">{it.c.title || '(无标题)'}</span>
            {#if !it.c.is_volume && (it.c.is_vip || it.c.is_pay)}
              <span
                class="pay-badge"
                class:pay={!it.c.is_vip && it.c.is_pay}
                title={it.c.is_vip && it.c.is_pay ? 'VIP 且付费' : it.c.is_vip ? 'VIP 章节' : '付费章节'}
              >{it.c.is_vip ? 'V' : 'P'}</span>
              {#if it.c.is_vip && it.c.is_pay}<span class="pay-both">VIP·付费</span>{/if}
            {/if}
          </button>
        {/each}
      </div>
      {#if !tocItems.length}<div class="rd-state slim"><p class="rd-muted">没有匹配的章节</p></div>{/if}
    </div>
  </aside>
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
  .rd-muted { color: var(--rd-muted); }
  .rd-state {
    padding: 48px 24px;
    text-align: center;
    color: var(--rd-muted);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }
  .rd-state.slim { padding: 20px; }

  /* 目录侧栏 */
  .toc-mask { position: fixed; top: 0; right: 0; bottom: 0; left: 0; background: rgba(0,0,0,0.35); z-index: 25; }
  .toc-drawer {
    position: fixed;
    top: 0; left: 0; bottom: 0;
    width: 86vw;
    max-width: 380px;
    background: var(--rd-panel);
    color: var(--rd-fg);
    z-index: 26;
    display: flex;
    flex-direction: column;
    padding-top: 0;
    padding-top: env(safe-area-inset-top);
    box-shadow: 2px 0 12px rgba(0,0,0,0.18);
  }
  .toc-head {
    display: flex; align-items: center; justify-content: space-between;
    padding: 12px; border-bottom: 1px solid var(--rd-border); font-size: 14px;
  }
  .toc-tools { display: flex; gap: 8px; padding: 10px 12px; border-bottom: 1px solid var(--rd-border); }
  .toc-tools input { flex: 1; min-width: 0; background: transparent; color: var(--rd-fg); border: 1px solid var(--rd-border); padding: 8px 10px; font-size: 14px; border-radius: 8px; }
  .toc-list { flex: 1; overflow-y: auto; -webkit-overflow-scrolling: touch; position: relative; }
  .toc-row {
    position: absolute; left: 0; right: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    height: 46px;
    text-align: left;
    padding: 0 14px;
    overflow: hidden;
    background: transparent;
    color: var(--rd-fg);
    border: none;
    border-bottom: 1px solid var(--rd-border);
    border-radius: 0;
    font-size: 14px;
  }
  .toc-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .toc-row:hover { background: var(--rd-border); }
  .toc-row.active { color: var(--rd-accent); font-weight: 600; }
  .toc-row.vol { color: var(--rd-muted); font-weight: 600; }
  /* 付费/VIP 角标：V=VIP，P=付费；同时为真只显示 V 并另加文字说明 */
  .pay-badge {
    flex: 0 0 auto;
    min-width: 18px;
    height: 18px;
    padding: 0 4px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 700;
    line-height: 1;
    color: #fff;
    background: var(--rd-accent);
    border-radius: 4px;
  }
  .pay-badge.pay {
    background: var(--rd-muted);
  }
  .pay-both {
    flex: 0 0 auto;
    font-size: 10px;
    color: var(--rd-muted);
  }
  /* 墨水屏主题：纯文字 [V]/[P]，不用彩色块（只改配色，不动布局） */
  :global(html[data-theme='eink']) .pay-badge {
    background: none;
    color: #000;
    font-weight: 700;
  }
  :global(html[data-theme='eink']) .pay-badge::before { content: '['; }
  :global(html[data-theme='eink']) .pay-badge::after { content: ']'; }
</style>
