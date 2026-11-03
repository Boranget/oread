<script>
  import { api } from './api.js';

  export let sources = [];

  // 检查更新（只读 diff）：按批调用 check-updates，最多 2 批并发，避免一次打满
  const UPDATE_CHUNK = 5;
  const UPDATE_CONCURRENCY = 2;

  let fileInput = null;
  let updateState = 'idle'; // idle | running | done
  let updateItems = []; // { idx, url, name, status, local, incoming, reason }
  let updateFailedChunks = []; // { offset, items, reason }
  let updateSeq = 0;
  let updateFileName = '';

  $: updateDone = updateItems.filter((x) => x.status !== 'pending').length;
  $: updateCounts = updateItems.reduce(
    (acc, x) => {
      if (x.status === 'updated') acc.updated += 1;
      else if (x.status === 'new') acc.new += 1;
      else if (x.status === 'local_newer') acc.local_newer += 1;
      else if (x.status === 'same') acc.same += 1;
      else if (x.status === 'error') acc.error += 1;
      return acc;
    },
    { updated: 0, new: 0, local_newer: 0, same: 0, error: 0 }
  );

  function updateUrlOf(item) {
    return String((item && (item.bookSourceUrl || item.book_source_url)) || '');
  }

  function updateNameOf(item, url) {
    const raw = item && (item.bookSourceName || item.book_source_name);
    if (raw) return String(raw);
    const local = sources.find((s) => s.book_source_url === url);
    return local ? local.book_source_name || local.book_source_url : '';
  }

  function updateLabel(status) {
    return {
      pending: '等待',
      updated: '有新版',
      new: '本地新增',
      local_newer: '本地较新',
      same: '相同',
      error: '失败',
    }[status] || status;
  }

  function fmtTs(t) {
    const n = Number(t);
    if (!Number.isFinite(n) || n <= 0) return '0';
    const d = new Date(n);
    if (Number.isNaN(d.getTime())) return String(t);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  // 把一批 check-updates 的响应落到对应行；第 N 条解析错误按批内序号映射
  function applyResponse(chunk, res) {
    const rows = updateItems.slice();
    const byUrl = new Map();
    for (const it of res.updated || []) {
      byUrl.set(it.book_source_url, { status: 'updated', local: it.local_update_time, incoming: it.incoming_update_time });
    }
    for (const it of res.new || []) {
      byUrl.set(it.book_source_url, { status: 'new', local: it.local_update_time, incoming: it.incoming_update_time });
    }
    for (const it of res.local_newer || []) {
      byUrl.set(it.book_source_url, { status: 'local_newer', local: it.local_update_time, incoming: it.incoming_update_time });
    }
    const errByIndex = new Map();
    for (const msg of res.errors || []) {
      const m = /^第\s*(\d+)\s*条[:：]\s*(\S[\s\S]*)$/.exec(String(msg));
      if (m) errByIndex.set(Number(m[1]) - 1, m[2]);
    }
    chunk.items.forEach((item, i) => {
      const idx = chunk.offset + i;
      const url = updateUrlOf(item);
      const row = { ...rows[idx] };
      row.name = updateNameOf(item, url) || row.name;
      row.url = url || row.url;
      if (errByIndex.has(i)) {
        row.status = 'error';
        row.reason = errByIndex.get(i);
      } else if (byUrl.has(url)) {
        row.status = byUrl.get(url).status;
        row.local = byUrl.get(url).local;
        row.incoming = byUrl.get(url).incoming;
        row.reason = '';
      } else {
        row.status = 'same';
        row.reason = '';
      }
      rows[idx] = row;
    });
    updateItems = rows;
  }

  function markChunkError(chunk, reason) {
    const rows = updateItems.slice();
    chunk.items.forEach((item, i) => {
      const idx = chunk.offset + i;
      rows[idx] = { ...rows[idx], status: 'error', reason };
    });
    updateItems = rows;
  }

  async function runChunks(chunks, seq) {
    let cursor = 0;
    const workers = Array.from(
      { length: Math.min(UPDATE_CONCURRENCY, chunks.length) },
      async () => {
        while (cursor < chunks.length) {
          const chunk = chunks[cursor];
          cursor += 1;
          if (seq !== updateSeq) return;
          try {
            const res = await api.checkSourceUpdates(chunk.items);
            if (seq !== updateSeq) return;
            applyResponse(chunk, res || {});
            updateFailedChunks = updateFailedChunks.filter((x) => x.offset !== chunk.offset);
          } catch (e) {
            if (seq !== updateSeq) return;
            const reason = (e && e.message) || '请求失败';
            markChunkError(chunk, `批次请求失败：${reason}`);
            updateFailedChunks = updateFailedChunks
              .filter((x) => x.offset !== chunk.offset)
              .concat([{ offset: chunk.offset, items: chunk.items, reason }]);
          }
        }
      }
    );
    await Promise.all(workers);
    if (seq === updateSeq) updateState = 'done';
  }

  async function checkUpdatesFile(e) {
    const file = e.target.files && e.target.files[0];
    e.target.value = '';
    if (!file) return;
    let parsed;
    try {
      parsed = JSON.parse(await file.text());
    } catch {
      parsed = null;
    }
    if (!parsed) {
      updateItems = [];
      updateState = 'done';
      updateFailedChunks = [];
      updateFileName = file.name;
      return;
    }
    const list = Array.isArray(parsed)
      ? parsed
      : parsed && Array.isArray(parsed.sources)
        ? parsed.sources
        : [parsed];
    if (!list.length) {
      updateItems = [];
      updateState = 'done';
      updateFailedChunks = [];
      updateFileName = file.name;
      return;
    }
    const seq = ++updateSeq;
    updateFileName = file.name;
    updateState = 'running';
    updateFailedChunks = [];
    updateItems = list.map((item, i) => {
      const url = updateUrlOf(item);
      return {
        idx: i,
        url,
        name: updateNameOf(item, url),
        status: 'pending',
        local: 0,
        incoming: 0,
        reason: '',
      };
    });
    const chunks = [];
    for (let i = 0; i < list.length; i += UPDATE_CHUNK) {
      chunks.push({ offset: i, items: list.slice(i, i + UPDATE_CHUNK) });
    }
    await runChunks(chunks, seq);
  }

  async function retryFailedChunks() {
    if (updateState === 'running' || !updateFailedChunks.length) return;
    const chunks = updateFailedChunks.slice();
    updateFailedChunks = [];
    updateState = 'running';
    const seq = ++updateSeq;
    const rows = updateItems.slice();
    for (const chunk of chunks) {
      chunk.items.forEach((item, i) => {
        const idx = chunk.offset + i;
        rows[idx] = { ...rows[idx], status: 'pending', reason: '' };
      });
    }
    updateItems = rows;
    await runChunks(chunks, seq);
  }
</script>

<div class="check-panel" aria-live="polite">
  <div class="check-head">
    <b>检查更新（只读）</b>
    <span class="muted">选择 Legado 书源包后逐条比对，不会写库。</span>
  </div>
  <div class="check-actions">
    <button on:click={() => fileInput.click()} disabled={updateState === 'running'}>选择书源包</button>
    <input class="file-input" type="file" accept=".json,application/json" bind:this={fileInput} on:change={checkUpdatesFile} />
    {#if updateFileName}<span class="muted check-file">{updateFileName}</span>{/if}
  </div>

  {#if updateState !== 'idle' && updateItems.length}
    <div class="check-progress-line">
      <b>进度：{updateDone} / {updateItems.length} 条</b>
      <span class="muted">
        {updateState === 'running' ? `进行中（每批 ${UPDATE_CHUNK} 条，最多 ${UPDATE_CONCURRENCY} 批并发）` : '已完成（只读，未写库）'}
      </span>
    </div>
    <progress class="check-progress" max={updateItems.length || 1} value={updateDone}></progress>
    <div class="check-summary">
      有新版 {updateCounts.updated} · 本地新增 {updateCounts.new} · 本地较新 {updateCounts.local_newer} · 相同 {updateCounts.same} · 失败 {updateCounts.error}
    </div>
    {#if updateFailedChunks.length}
      <div class="check-failed">
        有 {updateFailedChunks.length} 批请求失败；可只重试这些批次。
        <button class="ghost" on:click={retryFailedChunks} disabled={updateState === 'running'}>
          重试失败批次（{updateFailedChunks.length}）
        </button>
      </div>
    {/if}
    <details class="check-details">
      <summary>逐条结果（{updateItems.length} 条，失败 {updateCounts.error}）</summary>
      <div class="check-list">
        {#each updateItems as it (it.idx)}
          <div class="check-row">
            <div class="check-row-main">
              <span class="badge" class:badge-off={it.status === 'error'}>{updateLabel(it.status)}</span>
              <span class="check-name">{it.name || it.url || `第 ${it.idx + 1} 条`}</span>
            </div>
            {#if it.status === 'updated' || it.status === 'new' || it.status === 'local_newer'}
              <div class="check-sub muted">本地 {fmtTs(it.local)} → 包内 {fmtTs(it.incoming)}</div>
            {:else if it.status === 'error'}
              <details class="check-sub">
                <summary>失败原因</summary>
                <div class="check-reason">{it.reason || '未知错误'}</div>
              </details>
            {:else if it.url}
              <div class="check-sub muted">{it.url}</div>
            {/if}
          </div>
        {/each}
      </div>
    </details>
  {/if}
</div>

<style>
  .file-input {
    display: none;
  }
  /* 块级布局，不依赖 flex/grid/transform/transition/vw/vh */
  .check-panel {
    margin: 10px 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--card, #fff);
    font-size: 13px;
  }
  .check-head {
    margin-bottom: 6px;
  }
  .check-file {
    margin-left: 8px;
    font-size: 12px;
    word-break: break-all;
  }
  .check-actions {
    margin-bottom: 4px;
  }
  .check-progress-line {
    margin-bottom: 4px;
  }
  .check-progress {
    width: 100%;
    height: 14px;
  }
  .check-summary {
    margin: 6px 0;
    font-size: 12px;
  }
  .check-failed {
    margin: 6px 0;
    font-size: 12px;
  }
  .check-details {
    margin-top: 6px;
  }
  .check-list {
    max-height: 360px;
    overflow: auto;
    margin-top: 6px;
    border-top: 1px solid var(--border);
  }
  .check-row {
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .check-row-main {
    display: block;
  }
  .check-name {
    margin-left: 6px;
    word-break: break-all;
  }
  .check-sub {
    margin-top: 3px;
    font-size: 12px;
  }
  .check-reason {
    margin-top: 3px;
    color: #8a5a1f;
    word-break: break-word;
  }
  .badge-off {
    background: #fdecea;
    color: #b3392f;
  }
</style>
