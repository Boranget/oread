<script>
  import { createEventDispatcher } from 'svelte';

  export let show = false;
  export let chapters = [];
  export let currentPos = -1;
  export let getRatio = () => 0;
  export let pageIndex = 0;
  export let pageCount = 1;
  export let isPaged = false;
  export let parasCount = 0;

  const dispatch = createEventDispatcher();

  let jumpChapter = 1;
  let jumpPct = 0;
  let jumpPage = 1;
  let jumpPara = '';
  let opened = false;

  const clamp = (v, min, max) => Math.min(max, Math.max(min, v));
  const clamp01 = (v) => clamp(Number(v) || 0, 0, 1);
  const num = (v, def) => {
    const n = Number(v);
    return Number.isFinite(n) ? n : def;
  };

  $: if (show && !opened) {
    opened = true;
    jumpChapter = currentPos + 1;
    jumpPct = Math.round(clamp01(getRatio()) * 100);
    jumpPage = pageIndex + 1;
    jumpPara = '';
  }
  $: if (!show) opened = false;

  // 换到另一章时回到章首（比例/页码清零），避免“跳到第 3 章”却落在中间的 8%
  function onJumpChapter(v) {
    jumpChapter = v;
    const idx = clamp(Math.round(num(v, currentPos + 1)) - 1, 0, Math.max(0, chapters.length - 1));
    if (idx !== currentPos) {
      jumpPct = 0;
      jumpPage = 1;
    }
  }

  // 翻页模式下“比例”和“页码”是同一位置的两种写法，改动任意一个同步另一个，
  // 避免用户拖了比例却被旧的页码覆盖。
  function syncJumpPage() {
    if (!isPaged || pageCount < 2) return;
    jumpPage = clamp(Math.round(1 + clamp01(num(jumpPct, 0) / 100) * (pageCount - 1)), 1, pageCount);
  }

  function syncJumpPct() {
    if (!isPaged || pageCount < 2) return;
    const p = clamp(Math.round(num(jumpPage, 1)), 1, pageCount);
    jumpPct = Math.round(((p - 1) / (pageCount - 1)) * 100);
  }

  function doJump() {
    const idx = clamp(Math.round(num(jumpChapter, currentPos + 1)) - 1, 0, Math.max(0, chapters.length - 1));
    let ratio = clamp01(num(jumpPct, 0) / 100);
    // 翻页模式下按页码折算成同一套 0..1 章内比例（同章才有页概念）
    if (isPaged && idx === currentPos && pageCount > 1) {
      ratio = (clamp(Math.round(num(jumpPage, pageIndex + 1)), 1, pageCount) - 1) / (pageCount - 1);
    }
    const para = Math.round(num(jumpPara, 0));
    show = false;
    dispatch('jump', { idx, ratio, para });
  }

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="panel-mask" on:click|self={close}>
    <div class="panel jump-panel" role="dialog" aria-label="跳转定位">
      <div class="panel-head">
        <strong>跳转定位</strong>
        <button class="rd-btn" on:click={close}>关闭</button>
      </div>

      <div class="panel-row">
        <span>章节</span>
        <div class="stepper">
          <input
            class="jump-num"
            type="number"
            min="1"
            max={chapters.length}
            value={jumpChapter}
            on:input={(e) => onJumpChapter(e.currentTarget.value)}
            aria-label="章节序号"
          />
          <span class="val">/ {chapters.length}</span>
        </div>
      </div>

      <div class="panel-row">
        <span>章内位置</span>
        <div class="slider-wrap">
          <input
            type="range"
            min="0"
            max="100"
            step="1"
            value={jumpPct}
            on:input={(e) => {
              jumpPct = Number(e.currentTarget.value);
              syncJumpPage();
            }}
            aria-label="章内位置百分比"
          />
          <span class="val">{jumpPct}%</span>
        </div>
      </div>

      {#if isPaged && (Number(jumpChapter) || 1) - 1 === currentPos}
        <div class="panel-row">
          <span>页码</span>
          <div class="stepper">
            <input
              class="jump-num"
              type="number"
              min="1"
              max={pageCount}
              value={jumpPage}
              on:input={(e) => {
                jumpPage = Number(e.currentTarget.value);
                syncJumpPct();
              }}
              aria-label="页码"
            />
            <span class="val">/ {pageCount}（本章）</span>
          </div>
        </div>
      {/if}

      <div class="panel-row">
        <span>段落</span>
        <div class="stepper">
          <input
            class="jump-num"
            type="number"
            min="1"
            max={Math.max(1, parasCount)}
            placeholder="可选"
            bind:value={jumpPara}
            aria-label="段落序号"
          />
          <span class="val">/ {parasCount}</span>
        </div>
      </div>

      <p class="rd-muted small-note">
        位置沿用 0–1 的章内比例（界面按百分比显示）；翻页模式在同一章内按页码折算。段落序号按目标章节的段落数。
      </p>
      <div class="jump-actions">
        <button class="rd-btn solid" on:click={doJump}>跳转</button>
        <button class="rd-btn" on:click={close}>取消</button>
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
  .rd-btn.solid { background: var(--rd-accent); color: #fff; border-color: var(--rd-accent); }
  .rd-btn:disabled { opacity: 0.4; }
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
  .panel-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 9px 0; }
  .panel-row > span { font-size: 13px; color: var(--rd-muted); flex: 0 0 auto; }
  .stepper { display: flex; align-items: center; gap: 10px; }
  .stepper .val { min-width: 40px; text-align: center; font-variant-numeric: tabular-nums; }
  .slider-wrap { display: flex; align-items: center; gap: 10px; flex: 1; min-width: 0; }
  .slider-wrap input[type='range'] { flex: 1; min-width: 80px; accent-color: var(--rd-accent); padding: 0; }

  .jump-actions { display: flex; gap: 10px; justify-content: center; flex-wrap: wrap; margin-top: 10px; }
  .jump-panel { max-height: 80vh; overflow-y: auto; }
  .jump-num {
    width: 76px;
    padding: 6px 8px;
    border: 1px solid var(--rd-border);
    border-radius: 8px;
    background: var(--rd-panel);
    color: var(--rd-fg);
    font-size: 14px;
    font-variant-numeric: tabular-nums;
  }
</style>
