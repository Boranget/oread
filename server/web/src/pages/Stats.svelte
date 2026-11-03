<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';

  const dispatch = createEventDispatcher();
  const RANGES = [7, 30, 90];

  let days = RANGES.includes(Number(localStorage.getItem('oread_stats_days')))
    ? Number(localStorage.getItem('oread_stats_days'))
    : 7;
  let overview = null;
  let loading = true;
  let error = '';

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      overview = await api.statsOverview(days);
    } catch (e) {
      error = e.message;
      overview = null;
    } finally {
      loading = false;
    }
  }

  function setDays(d) {
    days = d;
    try {
      localStorage.setItem('oread_stats_days', String(d));
    } catch {
      /* ignore */
    }
    load();
  }

  function dayKey(d) {
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  }

  $: dailyMap = new Map(((overview && overview.daily) || []).map((d) => [d.day, d]));

  // 后端 daily 只返回有数据的日期，这里补成完整日期轴
  $: axis = (() => {
    const out = [];
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const start = new Date(today);
    start.setDate(today.getDate() - (days - 1));
    for (let i = 0; i < days; i++) {
      const d = new Date(start);
      d.setDate(start.getDate() + i);
      const key = dayKey(d);
      const row = dailyMap.get(key);
      out.push({ key, label: key.slice(5), seconds: (row && row.seconds) || 0, chars: (row && row.chars) || 0 });
    }
    return out;
  })();

  $: maxSec = Math.max(1, ...axis.map((a) => a.seconds));
  $: books = (overview && overview.books) || [];
  $: hasData = !!overview && ((overview.daily && overview.daily.length > 0) || books.length > 0 || overview.total_seconds > 0);
  $: rangeSeconds = (overview && overview.total_seconds) || 0;

  function barHeight(sec) {
    return sec > 0 ? Math.max(3, Math.round((sec / maxSec) * 100)) + '%' : '0%';
  }

  function showLabel(i) {
    if (days <= 7) return true;
    return i === 0 || i === axis.length - 1 || i % Math.ceil(days / 7) === 0;
  }

  function fmtDuration(sec) {
    const s = Math.max(0, Number(sec) || 0);
    if (s < 60) return s ? `${s} 秒` : '0';
    const min = Math.round(s / 60);
    if (min < 60) return `${min} 分钟`;
    const h = Math.floor(min / 60);
    const m = min % 60;
    return m ? `${h} 小时 ${m} 分` : `${h} 小时`;
  }

  function fmtChars(n) {
    const v = Math.max(0, Number(n) || 0);
    if (v >= 10000) return (v / 10000).toFixed(1) + ' 万字';
    return v.toLocaleString('zh-CN') + ' 字';
  }

  function pct(n) {
    return Math.min(100, Math.max(0, Number(n) || 0));
  }
</script>

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">阅读统计</h2>
  <div class="spacer"></div>
  <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
</div>

<div class="row" style="margin-bottom:12px">
  <div class="seg" style="margin:0;width:auto">
    {#each RANGES as r}
      <button class:on={days === r} on:click={() => setDays(r)}>{r} 天</button>
    {/each}
  </div>
  <span class="muted tiny-note">按账号统计，数据来自阅读页自动上报</span>
</div>

{#if error}
  <div class="card">
    <p class="err" style="margin:0 0 8px">{error}</p>
    <button class="ghost" on:click={load}>重试</button>
  </div>
{:else if loading && !overview}
  <p class="loading">加载中…</p>
{:else if !hasData}
  <div class="card stats-empty">
    <div class="empty-mark">—</div>
    <b>还没有阅读统计</b>
    <p class="muted">
      打开任意一本书读一会儿，系统会按分钟记录阅读时长与字数。连续天数从第一次上报开始计算，
      统计只保存在你的账号下。
    </p>
    <button on:click={() => dispatch('go', { tab: 'bookshelf' })}>去书架选一本</button>
  </div>
{:else}
  <div class="kpi-grid">
    <div class="card kpi">
      <div class="kpi-label">阅读时长</div>
      <div class="kpi-num">{fmtDuration(rangeSeconds)}</div>
      <div class="muted tiny-note">最近 {days} 天</div>
    </div>
    <div class="card kpi">
      <div class="kpi-label">阅读字数</div>
      <div class="kpi-num">{fmtChars(overview.total_chars)}</div>
      <div class="muted tiny-note">最近 {days} 天</div>
    </div>
    <div class="card kpi">
      <div class="kpi-label">连续阅读</div>
      <div class="kpi-num">{overview.streak_days || 0} 天</div>
      <div class="muted tiny-note">最长 {overview.longest_streak_days || 0} 天</div>
    </div>
    <div class="card kpi">
      <div class="kpi-label">今日阅读</div>
      <div class="kpi-num">{fmtDuration(overview.today_seconds)}</div>
      <div class="muted tiny-note">按本机日期统计</div>
    </div>
  </div>

  <div class="card chart-card">
    <div class="row" style="margin-bottom:6px">
      <b>最近 {days} 天阅读分布</b>
      <div class="spacer"></div>
      <span class="muted tiny-note">悬停查看当日时长</span>
    </div>
    {#if axis.every((a) => a.seconds === 0)}
      <p class="muted">这段时间还没有阅读时长记录。</p>
    {:else}
      <div class="chart" role="img" aria-label="最近 {days} 天阅读时长柱状图">
        {#each axis as a, i}
          <div class="bar-col" style="flex:1 0 {days > 30 ? 8 : 16}px">
            <div class="bar-wrap">
              <div class="bar" style="height:{barHeight(a.seconds)}" title="{a.key}：{fmtDuration(a.seconds)}"></div>
            </div>
            <div class="bar-date">{showLabel(i) ? a.label : ''}</div>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="card">
    <div class="row" style="margin-bottom:8px">
      <b>最近阅读书籍</b>
      <div class="spacer"></div>
      <span class="muted tiny-note">{books.length} 本有记录</span>
    </div>
    <p class="muted tiny-note" style="margin-bottom:8px">后端返回全量最近 50 本，不随上方 7/30/90 天区间变化。</p>
    {#if books.length === 0}
      <p class="muted">暂无书籍维度数据；阅读并保存进度后会出现在这里。</p>
    {:else}
      <div class="book-list">
        {#each books.slice(0, 12) as b (b.book_id)}
          <div class="stat-book">
            <div class="stat-book-main">
              <div class="stat-book-name">{b.name || b.book_id}</div>
              <div class="muted tiny-note">
                已读 {b.read_chapters || 0} / {b.total_chapters ? b.total_chapters : '?'} 章
                · {pct(b.completion).toFixed(1)}%
              </div>
            </div>
            <div class="meter" aria-hidden="true"><div style="width:{pct(b.completion)}%"></div></div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .kpi-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 12px;
    margin-bottom: 12px;
  }
  .kpi { padding: 14px 16px; }
  .seg button { white-space: nowrap; padding: 8px 14px; }
  .kpi-label { font-size: 12px; color: var(--muted); }
  .kpi-num { font-size: 26px; font-weight: 700; line-height: 1.3; margin: 2px 0; }
  .chart-card { margin-bottom: 12px; }
  .chart {
    display: flex;
    align-items: stretch;
    gap: 4px;
    height: 160px;
    padding-top: 6px;
    overflow-x: auto;
    justify-content: center;
  }
  .bar-col { display: flex; flex-direction: column; min-width: 8px; max-width: 64px; }
  .bar-wrap { flex: 1; display: flex; align-items: flex-end; }
  .bar {
    width: 100%;
    background: #333;
    border-radius: 2px 2px 0 0;
  }
  .bar-date { height: 18px; line-height: 18px; font-size: 10px; color: var(--muted); text-align: center; white-space: nowrap; }
  .book-list { display: grid; gap: 10px; }
  .stat-book { display: grid; gap: 4px; }
  .stat-book-main { display: flex; align-items: baseline; gap: 8px; }
  .stat-book-name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meter { height: 6px; border: 1px solid var(--border); background: transparent; }
  .meter > div { height: 100%; background: #333; }
  .stats-empty { text-align: center; padding: 28px 18px; display: grid; gap: 8px; justify-items: center; }
  .stats-empty .empty-mark { font-size: 28px; color: var(--muted); line-height: 1; }
  .stats-empty p { margin: 0; max-width: 460px; }
</style>
