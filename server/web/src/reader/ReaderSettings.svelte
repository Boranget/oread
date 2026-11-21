<script>
  import { createEventDispatcher } from 'svelte';
  import { FONT_OPTIONS, WIDTH_OPTIONS } from '../lib/readingPrefs.js';
  import { zhLabels } from '../lib/zhconvert.js';

  export let show = false;
  export let readMode = 'scroll';
  export let effFont = 18;
  export let effLine = 1.75;
  export let fontFamily = 'serif';
  export let theme = 'light';
  export let zhMode = 'off';
  export let useReplaceRules = true;
  export let isPaged = false;
  export let width = 'normal';
  export let brightness = 100;
  export let autoOn = false;
  export let autoSpeed = 3;

  const dispatch = createEventDispatcher();

  const MODES = ['scroll', 'paged', 'fade'];
  const themeName = { light: '浅色', dark: '深色', sepia: '护眼', eink: '墨水屏' };
  const modeName = { scroll: '滚动', paged: '翻页', fade: '仿真' };

  function close() {
    show = false;
  }
</script>

{#if show}
  <div class="panel-mask" on:click|self={close}>
    <div class="panel">
      <div class="panel-head">
        <strong>阅读设置</strong>
        <button class="rd-btn" on:click={close}>收起</button>
      </div>

      <div class="panel-row">
        <span>模式</span>
        <div class="seg small">
          {#each MODES as m}
            <button class:on={readMode === m} on:click={() => dispatch('mode', m)}>{modeName[m]}</button>
          {/each}
        </div>
      </div>

      <div class="panel-row">
        <span>字号</span>
        <div class="stepper">
          <button class="rd-btn" on:click={() => dispatch('font-step', -1)} disabled={effFont <= 12}>A−</button>
          <span class="val">{effFont}</span>
          <button class="rd-btn" on:click={() => dispatch('font-step', 1)} disabled={effFont >= 30}>A+</button>
        </div>
      </div>

      <div class="panel-row">
        <span>行距</span>
        <div class="stepper">
          <button class="rd-btn" on:click={() => dispatch('line-step', -0.1)} disabled={effLine <= 1.2}>−</button>
          <span class="val">{effLine.toFixed(1)}</span>
          <button class="rd-btn" on:click={() => dispatch('line-step', 0.1)} disabled={effLine >= 2.6}>+</button>
        </div>
      </div>

      <div class="panel-row">
        <span>字体</span>
        <div class="seg small">
          {#each FONT_OPTIONS as f}
            <button class:on={fontFamily === f.id} on:click={() => dispatch('font-family', f.id)}>{f.label}</button>
          {/each}
        </div>
      </div>

      <div class="panel-row">
        <span>定位</span>
        <button class="rd-btn" on:click={() => dispatch('jump')}>跳转章节 / 段</button>
      </div>

      <div class="panel-row">
        <span>配色</span>
        <div class="seg small">
          {#each ['light', 'dark', 'sepia', 'eink'] as t}
            <button class:on={theme === t} on:click={() => dispatch('theme', t)}>{themeName[t]}</button>
          {/each}
        </div>
      </div>

      <div class="panel-row">
        <span>字形</span>
        <div class="seg small">
          {#each ['off', 's', 't'] as m}
            <button class:on={zhMode === m} on:click={() => dispatch('zh', m)}>{zhLabels[m]}</button>
          {/each}
        </div>
      </div>

      <div class="panel-row">
        <span>替换规则</span>
        <button class="rd-btn" class:solid={useReplaceRules} on:click={() => dispatch('replace', !useReplaceRules)}>
          {useReplaceRules ? '已开启' : '已关闭'}
        </button>
      </div>

      {#if !isPaged}
        <div class="panel-row">
          <span>页宽</span>
          <div class="seg small">
            {#each WIDTH_OPTIONS as w}
              <button class:on={width === w.id} on:click={() => dispatch('width', w.id)}>{w.label}</button>
            {/each}
          </div>
        </div>
      {/if}

      <div class="panel-row">
        <span>亮度</span>
        <div class="slider-wrap">
          <input
            type="range"
            min="0"
            max="100"
            step="5"
            value={brightness}
            on:input={(e) => dispatch('brightness', e.currentTarget.value)}
          />
          <span class="val">{brightness}%</span>
        </div>
      </div>

      <div class="panel-row">
        <span>自动滚动</span>
        <div class="stepper">
          <button class="rd-btn" class:solid={autoOn} on:click={() => dispatch('toggle-auto')}>{autoOn ? '暂停' : '开始'}</button>
          <span class="val">{autoSpeed} 档</span>
        </div>
      </div>

      <div class="panel-row">
        <span>速度</span>
        <div class="slider-wrap">
          <input
            type="range"
            min="1"
            max="10"
            step="1"
            value={autoSpeed}
            on:input={(e) => dispatch('speed', e.currentTarget.value)}
          />
          <span class="val">{autoSpeed}</span>
        </div>
      </div>

      <div class="panel-row">
        <span>本书</span>
        <div class="row-actions">
          <button class="rd-btn" on:click={() => dispatch('bookmarks')}>书签</button>
          <button class="rd-btn" on:click={() => dispatch('search')}>书内搜索</button>
          <button class="rd-btn" on:click={() => dispatch('switch')}>换源</button>
        </div>
      </div>

      <p class="rd-muted small-note">
        左 1/3 上一页，右 1/3 下一页，中间显隐控制栏；上下滑到边界快速切章；长按正文可选中复制。
        字号/行距/配色/模式/页宽/字体族保存到账号设置，本机保留一份用于离线兜底；亮度、字形、速度仅存本机。
      </p>
      <p class="rd-muted small-note desk-only">
        快捷键 <kbd>←</kbd> <kbd>→</kbd> 翻页，<kbd>↑</kbd> <kbd>↓</kbd> 滚动，<kbd>空格</kbd> 暂停/开始自动滚动，<kbd>Esc</kbd> 收起面板。
      </p>
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
  .seg.small { margin: 0; padding: 3px; border: 1px solid var(--rd-border); background: transparent; }
  .seg.small button { background: transparent; color: var(--rd-fg); padding: 4px 10px; min-height: 30px; font-size: 12px; border-radius: 6px; }
  .seg.small button.on { background: var(--rd-accent); color: #fff; }
  .row-actions { display: flex; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
</style>
