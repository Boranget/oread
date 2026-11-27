<script>
  import { onMount } from 'svelte';
  import { api } from '../../lib/api.js';
  import { describeFailure } from '../../lib/apiErrors.js';

  // 只读运行指标：来自 /api/healthz（需登录）。不在这里做任何清理或改配置。
  let health = null;
  let loading = false;
  let err = '';

  onMount(load);

  async function load() {
    loading = true;
    err = '';
    try {
      health = await api.healthz();
    } catch (e) {
      err = describeFailure(e, '读取系统状态');
    } finally {
      loading = false;
    }
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024 * 1024) return (v / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  function pct(v) {
    return `${(Number(v || 0) * 100).toFixed(1)}%`;
  }
</script>

<div class="card">
  <div class="row" style="margin-bottom:10px">
    <h3 style="margin:0">系统状态</h3>
    <div class="spacer"></div>
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>

  {#if err}<p class="err">{err}</p>{/if}
  {#if loading && !health}
    <p class="loading">读取状态…</p>
  {:else if health}
    <div class="health-grid">
      <div><span class="muted">服务版本</span><b>{health.version ? `v${health.version}` : '未返回'}</b></div>
      <div><span class="muted">进程内存 RSS</span><b>{health.rss_mb == null ? '当前平台不可用' : health.rss_mb + ' MB'}</b></div>
      <div><span class="muted">累计写盘</span><b>{health.disk_write_mb == null ? '当前平台不可用' : health.disk_write_mb + ' MB'}</b></div>
      <div><span class="muted">打开的用户库</span><b>{health.open_user_dbs ?? 0}</b></div>
      <div><span class="muted">馆藏正文（永久）</span><b>{health.cache ? `${fmtBytes(health.cache.collection?.bytes ?? 0)} · ${health.cache.collection?.books ?? 0} 本` : '未返回'}</b></div>
      <div><span class="muted">临时缓存</span><b>{health.cache ? `${fmtBytes(health.cache.temp?.bytes ?? 0)} · ${health.cache.temp?.books ?? 0} 本` : '未返回'}</b></div>
      <div><span class="muted">图片缓存</span><b>{health.image_cache ? `${health.image_cache.mb ?? 0} / ${health.image_cache.limit_mb ?? '?'} MB · ${pct(health.image_cache.hit_rate)}` : '未返回'}</b></div>
      <div><span class="muted">正文缓存命中</span><b>{health.content_cache ? `${pct(health.content_cache.hit_rate)} · ${health.content_cache.hits ?? 0} 次` : '未返回'}</b></div>
      <div><span class="muted">HTTP 并发 / 超时</span><b>{health.http?.concurrency ?? '?'} / {health.http?.timeout_secs ?? '?'}s</b></div>
      <div><span class="muted">请求体上限</span><b>{health.http?.max_body_mb ?? '?'} MB</b></div>
      <div><span class="muted">JS 规则</span><b>{health.js?.enabled ? '开启' : '关闭'} · {health.js?.last_ms ?? 0} / {health.js?.max_ms ?? 0} ms</b></div>
      <div><span class="muted">JS 预算</span><b>{health.js?.budget_ms ?? '?'} ms</b></div>
      <div><span class="muted">搜索单源预算</span><b>{health.search ? `${health.search.per_source_secs}s / ${health.search.all_per_source_secs}s` : '未返回'}</b></div>
      <div><span class="muted">搜索整批预算</span><b>{health.search ? `${health.search.total_curated_secs}s / ${health.search.total_all_secs}s` : '未返回'}</b></div>
    </div>

    {#if health.cache?.temp?.expired_books > 0}
      <p class="muted field-hint">临时缓存中有 {health.cache.temp.expired_books} 本已超过 TTL，可在「馆藏维护 → 临时缓存」回收。</p>
    {/if}

    {#if health.cache?.books?.length}
      <div class="top-books">
        <div class="muted field-hint" style="margin-top:14px">占用最多的书（前 5 本）</div>
        <ul>
          {#each health.cache.books as b (b.book_url)}
            <li>
              <span class="name">{b.name || b.book_url}</span>
              <b>{fmtBytes(b.bytes)}</b>
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    <p class="muted field-hint">只读指标，来自 /api/healthz；不含书籍正文与账号内容。</p>
  {:else}
    <p class="muted">暂无状态数据。</p>
  {/if}
</div>

<style>
  .health-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; }
  .health-grid > div { display: flex; flex-direction: column; gap: 2px; }
  .health-grid span { font-size: 12px; }
  .health-grid b { font-size: 15px; font-variant-numeric: tabular-nums; }
  .field-hint { font-size: 11px; margin-top: 8px; line-height: 1.5; }
  .top-books ul { list-style: none; margin: 6px 0 0; padding: 0; display: grid; gap: 4px; font-size: 12px; }
  .top-books li { display: flex; justify-content: space-between; gap: 10px; }
  .top-books .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .top-books b { font-variant-numeric: tabular-nums; white-space: nowrap; }
</style>
