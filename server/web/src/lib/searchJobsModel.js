// 搜索任务展示层纯函数：状态文案、进度、时间与可执行动作。
//
// 单独抽出便于 node:test 直接覆盖，也避免 Discover/面板各自实现一套口径。

/** 任务状态：running（后端仍在跑）/ done / cancelled / interrupted（进程重启）。 */
export function isRunning(job) {
  return !!job && job.status === 'running';
}

export function jobStatusLabel(status) {
  switch (status) {
    case 'running':
      return '运行中';
    case 'done':
      return '已完成';
    case 'cancelled':
      return '已取消';
    case 'interrupted':
      return '已中断';
    default:
      return '未知';
  }
}

/** 状态对应的样式类（由组件映射到颜色）。 */
export function jobStatusTone(status) {
  switch (status) {
    case 'running':
      return 'running';
    case 'done':
      return 'ok';
    case 'cancelled':
      return 'warn';
    case 'interrupted':
      return 'muted';
    default:
      return 'muted';
  }
}

function num(v) {
  const n = Number(v);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/**
 * 进度：已处理源 = 已完成 + 取消。
 *
 * 取消时后端把剩余源计入 cancelled（finished 不变），因此两者相加才是“已处理”。
 * skipped（预算耗尽）在收尾时也会累加 finished，不需要单独相加。
 */
export function jobProgress(job) {
  const total = num(job && job.source_total);
  const finished = Math.min(num(job && job.finished), total || Infinity);
  const cancelled = Math.min(num(job && job.cancelled), total || Infinity);
  const processed = Math.min(total || finished + cancelled, finished + cancelled);
  const percent =
    total > 0 ? Math.round((processed / total) * 100) : isRunning(job) ? 0 : 100;
  return { total, finished, cancelled, processed, percent };
}

/** 一行进度文案：`30/147 源 · 失败 2 · 取消 3`。 */
export function jobProgressText(job) {
  const { total, finished, cancelled } = jobProgress(job);
  if (total === 0) return '没有可搜索的书源';
  const parts = [`${finished}/${total} 源`];
  const failed = num(job && job.failed);
  if (failed > 0) parts.push(`失败 ${failed}`);
  if (cancelled > 0) parts.push(`取消 ${cancelled}`);
  const skipped = num(job && job.skipped);
  if (skipped > 0) parts.push(`预算跳过 ${skipped}`);
  return parts.join(' · ');
}

/** 相对时间：刚刚 / N 分钟前 / N 小时前 / 本地日期。 */
export function formatRelativeTime(ms, now = Date.now()) {
  const t = Number(ms);
  if (!Number.isFinite(t) || t <= 0) return '';
  const diff = Math.max(0, Number(now) - t);
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  const d = new Date(t);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 任务时间文案：运行中看开始时间，结束后看结束时间。 */
export function jobTimeText(job, now = Date.now()) {
  if (!job) return '';
  const at = isRunning(job) ? job.created_at : job.finished_at || job.created_at;
  const rel = formatRelativeTime(at, now);
  if (!rel) return '';
  return isRunning(job) ? `开始于 ${rel}` : `结束于 ${rel}`;
}

export function canCancel(job) {
  return isRunning(job);
}

/** 运行中的任务必须先取消，避免“删了还在后台跑”。 */
export function canRemove(job) {
  return !!job && !isRunning(job);
}

/** 任何任务都能查看；运行中的任务会重新挂载并继续轮询。 */
export function canView(job) {
  return !!job;
}

export function countActive(jobs) {
  return (Array.isArray(jobs) ? jobs : []).filter(isRunning).length;
}
