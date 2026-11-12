<script>
  import { createEventDispatcher, onDestroy, onMount } from 'svelte';
  import {
    canCancel,
    canRemove,
    canView,
    jobProgress,
    jobProgressText,
    jobStatusLabel,
    jobStatusTone,
    jobTimeText,
  } from './searchJobsModel.js';

  export let jobs = [];
  export let active = 0;
  export let error = '';
  /** 正在执行取消/移除的任务 id：按钮统一置灰，避免重复点击。 */
  export let busyId = '';

  const dispatch = createEventDispatcher();
  let expanded = true;
  let now = Date.now();
  let tick = null;

  // 相对时间每分钟更新一次即可；运行中的进度由 store 轮询驱动。
  onMount(() => {
    tick = setInterval(() => (now = Date.now()), 60000);
    return () => clearInterval(tick);
  });
  onDestroy(() => {
    if (tick) clearInterval(tick);
  });

  // 出现新的运行中任务时自动展开，避免“搜完关掉页面再回来找不到”。
  $: if (active > 0) expanded = true;
</script>

{#if jobs.length}
  <div class="jobs" aria-label="搜索任务">
    <button class="head" aria-expanded={expanded} on:click={() => (expanded = !expanded)}>
      <span class="title">搜索任务{active > 0 ? ` · ${active} 个运行中` : ''}</span>
      <span class="count muted">{jobs.length} 个</span>
      <span class="chev">{expanded ? '收起' : '展开'}</span>
    </button>

    {#if expanded}
      <p class="hint">关闭页面后任务仍在后端运行；重新登录可在这里继续查看和管理。</p>
      {#if error}<p class="err">{error}</p>{/if}
      <ul class="list">
        {#each jobs as job (job.job_id)}
          {@const progress = jobProgress(job)}
          <li class="job">
            <div class="line">
              <span class="kw" title={job.keyword}>{job.keyword}</span>
              <span class="badge {jobStatusTone(job.status)}">{jobStatusLabel(job.status)}</span>
              <span class="prog">{jobProgressText(job)}</span>
            </div>
            <div
              class="bar"
              role="progressbar"
              aria-label={`「${job.keyword}」搜索进度`}
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={progress.percent}
            >
              <span style={`width:${progress.percent}%`}></span>
            </div>
            <div class="line sub">
              <span class="muted">
                {jobTimeText(job, now)}{job.scope === 'curated' ? ' · 精选源' : ''}
              </span>
              <span class="acts">
                {#if canCancel(job)}
                  <button
                    class="danger"
                    disabled={busyId === job.job_id}
                    on:click={() => dispatch('cancel', job)}
                  >取消</button>
                {/if}
                {#if canView(job)}
                  <button
                    disabled={busyId === job.job_id}
                    on:click={() => dispatch('view', job)}
                  >{job.status === 'running' ? '查看进度' : '查看结果'}</button>
                {/if}
                {#if canRemove(job)}
                  <button
                    class="ghost"
                    disabled={busyId === job.job_id}
                    on:click={() => dispatch('remove', job)}
                  >移除</button>
                {/if}
              </span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .jobs {
    margin-top: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 8px;
    background: var(--card, #fff);
  }
  .head {
    display: block;
    width: 100%;
    text-align: left;
    background: transparent;
    color: var(--fg, #1d1d1f);
    border: 0;
    padding: 2px 0;
    font-size: 13px;
  }
  .head:hover {
    background: transparent;
  }
  .title {
    font-weight: 600;
  }
  .count {
    margin-left: 6px;
    font-size: 12px;
  }
  .chev {
    float: right;
    color: var(--accent);
    font-size: 12px;
  }
  .hint {
    margin: 4px 0 6px;
    font-size: 12px;
    color: var(--muted, #666);
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .job {
    padding: 7px 0;
    border-top: 1px solid var(--border, #e3e3e6);
  }
  .job:first-child {
    border-top: 0;
  }
  .line {
    font-size: 12px;
    line-height: 1.5;
  }
  .kw {
    font-weight: 600;
    max-width: 45%;
    display: inline-block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: bottom;
  }
  .badge {
    margin-left: 6px;
    padding: 1px 6px;
    border-radius: 999px;
    border: 1px solid var(--border, #e3e3e6);
    font-size: 11px;
  }
  .badge.running {
    border-color: var(--accent);
    color: var(--accent);
  }
  .badge.ok {
    color: #1a7f37;
    border-color: #1a7f37;
  }
  .badge.warn {
    color: #9a6700;
    border-color: #9a6700;
  }
  .badge.muted {
    color: var(--muted, #666);
  }
  .prog {
    margin-left: 6px;
    color: var(--muted, #666);
  }
  .bar {
    height: 4px;
    margin: 4px 0 3px;
    border-radius: 2px;
    background: var(--border, #e3e3e6);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.2s linear;
  }
  .sub {
    color: var(--muted, #666);
  }
  .acts {
    float: right;
  }
  .acts button {
    margin-left: 6px;
    padding: 3px 9px;
    font-size: 12px;
  }
  .acts button.danger {
    color: #b3392f;
    border-color: #b3392f;
  }
  .acts button:disabled {
    opacity: 0.5;
  }
  .err {
    margin: 4px 0;
    color: #b3392f;
    font-size: 12px;
  }
</style>
