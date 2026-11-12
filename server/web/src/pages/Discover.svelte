<script>
  import { createEventDispatcher, onDestroy, onMount } from 'svelte';
  import { api, describeSourceError, describeFailure } from '../lib/api.js';
  import { loadHistory, rememberKey } from '../lib/searchHistory.js';
  import DiscoverHistory from '../lib/DiscoverHistory.svelte';
  import DiscoverScopePicker from '../lib/DiscoverScopePicker.svelte';
  import DiscoverResults from '../lib/DiscoverResults.svelte';
  import SearchJobsPanel from '../lib/SearchJobsPanel.svelte';
  import {
    searchJobs,
    cancelSearchJob,
    removeSearchJob,
    refreshSearchJobs,
  } from '../lib/searchJobsStore.js';

  const dispatch = createEventDispatcher();
  let mode = 'search'; // search | explore | import
  let key = '';
  let results = [];
  let error = '';
  let notice = '';
  let searched = false;

  // 按链接打开（无需搜索）：选择书源 + 粘贴详情页 URL
  let importSourceId = '';
  let importUrl = '';
  let importing = false;

  // 两段式搜索：start 建 job → 轮询 result；拿到 books 先渲染，不等 done
  let jobId = '';
  let jobSeq = 0; // 递增序号：换词/换页/切模式时让旧响应失效
  let pollTimer = null;
  let inflight = null; // { ctrl, seq, page }
  let pollFailures = 0;
  let starting = false;
  let searching = false;
  let searchDone = false;
  let jobExpired = false;
  let jobStatus = ''; // 后端回显：running / done / cancelled / interrupted
  let paging = false;

  // 任务面板：busyId 防止取消/移除按钮重复点击
  let jobsBusyId = '';
  let jobsError = '';

  // 等待秒数 / 完成用时
  let searchElapsed = 0;
  let searchDuration = 0;
  let searchTimer = null;
  let searchStartedAt = 0;

  // 分页 / 统计（均来自 /api/search/result，不再本地累加）
  let page = 1;
  const PER_PAGE = 30;
  let hasMore = false;
  let total = 0;
  let sourceTotal = 0;
  let finishedSources = 0;
  let pendingSources = 0;
  let deadlineMs = 0;
  // 搜索范围回显（后端 /api/search/start 返回）
  // scope 由 sourceScope 决定：all（默认，全部可搜索源）/ curated（精选源）/ custom（指定源）。
  // 每次搜索都显式把 scope 与 source_ids 发给后端，避免“文案说全部、实际只跑精选”。
  function retryWithAll() {
    sourceScope = 'all';
    doSearch();
  }
  // 空态引导：切到「书源管理」（App.svelte 监听 oread:tab 事件）
  function goSources() {
    window.dispatchEvent(new CustomEvent('oread:tab', { detail: 'sources' }));
  }
  let requestedScope = 'all'; // 本次请求携带的 scope（用户意图）
  let searchScope = 'all'; // 后端实际使用的 scope
  let searchCoverage = 'expanded'; // curated / expanded
  let sourceMode = ''; // user / curated / curated_fallback / all_fallback / all（后端回显）
  let scopeFiltered = 0; // 精选收缩排除掉的可搜索源数量
  let curatedTotal = 0;
  let failedCount = 0;
  let failedSources = []; // [{ name, reason }]
  let skippedCount = 0; // 全局预算耗尽导致未执行/未完成的源
  let skippedSources = []; // [{ name, reason }]
  let resultsPage = 1; // 当前卡片实际对应的页码，过期时回退显示

  // 发现模式
  let sources = [];
  let sourcesReady = false; // /api/sources 是否已成功返回（区分“还没加载”与“确实 0 个源”）
  let selectedSource = '';
  let categories = [];
  let selectedCat = null;
  let catsLoading = false;
  let loading = false;
  let loadingMore = false;

  // 搜索范围：all=全部可搜索源（默认）；curated=精选源；custom=指定书源（source_ids 传 URL）
  let sourceScope = 'all';
  let pickedSourceIds = [];
  let scopeOpen = false;
  let requestSourceCount = 0; // 本次实际带上的 source_ids 数（0=未传）
  let ignoredSourceIds = []; // 后端回显的 ignored_source_ids
  let groupFilter = 'all';

  // 本次搜索的固定快照（用于结果页“只搜这些源重试”）
  let lastSearchKey = '';
  let lastSearchScopeIds = [];
  let resultScopeMode = 'all';
  let resultRequested = 0;

  // 失败源重试：独立 job，只带上次失败源的 source_ids，结果合并进当前页
  let retrying = false;
  let retrySeq = 0;
  let retryNote = '';
  let retryExtra = []; // 重试新增的结果，翻页时重新附加，避免翻页丢失

  // 可搜索源：与后端 searchable_sources 同一口径（enabled + 类型/规则/searchUrl + 非登录源 + 未隔离）
  // 计数、全选、可选状态都用它，不能只看 enabled。
  $: searchableSources = sources.filter((s) => s.searchable);
  $: groupOptions = [...new Set(sources.map((s) => s.book_source_group).filter(Boolean))];
  $: pickSources = groupFilter === 'all' ? sources : sources.filter((s) => (s.book_source_group || '') === groupFilter);

  // 书架状态（用于“已加入”标记）
  let shelfIds = new Set();

  // 已完成源 = 已结束 - 失败 - 预算未完成（两者也计入 finished）
  $: completedSources = Math.max(0, finishedSources - failedCount - skippedCount);

  onMount(async () => {
    loadHistory();
    // App 级已在轮询；进入发现页时立即刷新一次，避免看到过期状态。
    refreshSearchJobs();
    try {
      sources = await api.sources();
      sourcesReady = true;
      if (sources.length) {
        selectedSource = sources[0].book_source_url;
        if (!importSourceId) importSourceId = sources[0].book_source_url;
        await loadCategories();
      }
    } catch (e) {
      error = describeFailure(e, '读取书源列表');
    }
    try {
      const list = await api.bookshelf();
      shelfIds = new Set((list || []).map((b) => b.book_id));
    } catch {
      /* 书架读取失败不影响搜索 */
    }
    try {
      const c = await api.curatedSources();
      curatedTotal = Number(c.curated_total) || 0;
    } catch {
      /* 未生成精选集时忽略，搜索会自动回退全部源 */
    }
  });

  onDestroy(() => {
    clearInterval(searchTimer);
    jobSeq += 1;
    retrySeq += 1;
    cancelPolling();
  });

  function useHistory(q) {
    if (starting || searching) return;
    key = q;
    doSearch(q);
  }

  function startSearchClock() {
    clearInterval(searchTimer);
    searchStartedAt = Date.now();
    searchElapsed = 0;
    searchDuration = 0;
    searchTimer = setInterval(() => {
      searchElapsed = Math.floor((Date.now() - searchStartedAt) / 1000);
    }, 1000);
  }

  function stopSearchClock() {
    clearInterval(searchTimer);
    searchTimer = null;
    if (searchStartedAt) {
      searchDuration = Math.max(1, Math.round((Date.now() - searchStartedAt) / 1000));
    }
    searchStartedAt = 0;
  }

  function cancelPolling() {
    if (pollTimer) {
      clearTimeout(pollTimer);
      pollTimer = null;
    }
    if (inflight) {
      inflight.ctrl.abort();
      inflight = null;
    }
  }

  function schedulePoll(seq, delay) {
    if (seq !== jobSeq) return;
    if (pollTimer) clearTimeout(pollTimer);
    pollTimer = setTimeout(() => {
      pollTimer = null;
      fetchPage(seq, page);
    }, delay);
  }

  function applyResult(r) {
    // 同一 job 同一页的最新快照：轮询期间只替换数组内容，不清空，避免闪回空态
    // 失败源重试的新增结果按 book_url 去重后附在列表尾部，翻页时也会重新附加
    results = dedupeByUrl((r.books || []).concat(retryExtra));
    resultsPage = page;
    total = Number.isFinite(Number(r.total)) ? Number(r.total) : results.length;
    hasMore = !!r.has_more;
    sourceTotal = Number(r.source_total) || sourceTotal;
    const pending = Number(r.pending);
    finishedSources = Number.isFinite(Number(r.finished))
      ? Number(r.finished)
      : Number.isFinite(pending)
        ? Math.max(0, sourceTotal - pending)
        : finishedSources;
    pendingSources = Number.isFinite(pending) ? pending : Math.max(0, sourceTotal - finishedSources);
    searchDone = !!r.done;
    jobStatus = String(r.status || (searchDone ? 'done' : 'running'));
    deadlineMs = Number(r.deadline_ms) || deadlineMs;
    const rawFailed = Array.isArray(r.errors) ? r.errors : [];
    const budgetSkipped = (e) => e.kind === 'skipped' || e.kind === 'truncated';
    failedSources = rawFailed
      .filter((e) => !budgetSkipped(e))
      .map((e) => ({
        name: e.source_name || e.source || '未知书源',
        reason: describeSourceError(e.error),
        url: e.source || '',
      }));
    skippedSources = rawFailed
      .filter(budgetSkipped)
      .map((e) => ({
        name: e.source_name || e.source || '未知书源',
        reason: describeSourceError(e.error),
        url: e.source || '',
      }));
    failedCount = Number.isFinite(Number(r.source_failed))
      ? Math.max(0, Number(r.source_failed))
      : failedSources.length;
    skippedCount = Number.isFinite(Number(r.source_skipped))
      ? Math.max(0, Number(r.source_skipped))
      : skippedSources.length;
  }

  function finishJob() {
    searching = false;
    starting = false;
    paging = false;
    pollFailures = 0;
    cancelPolling();
    stopSearchClock();
    if (jobStatus === 'cancelled') {
      notice = '搜索已取消，已保留取消前的结果。';
    } else if (jobStatus === 'interrupted') {
      notice = '任务在服务重启时中断，这里只显示已保存的结果。';
    }
    // 任务结束后立即同步任务面板（状态/徽标/可移除）。
    refreshSearchJobs();
  }

  function handleExpired() {
    cancelPolling();
    searching = false;
    starting = false;
    paging = false;
    jobExpired = true;
    page = resultsPage || 1; // 结果仍停留在旧页，页码同步回退，避免“第 2 页显示第 1 页内容”
    stopSearchClock();
    refreshSearchJobs();
  }

  async function fetchPage(seq, pageNo) {
    if (seq !== jobSeq || !jobId) return;
    cancelPolling();
    const ctrl = new AbortController();
    inflight = { ctrl, seq, page: pageNo };
    try {
      const r = await api.searchResult(jobId, pageNo, PER_PAGE, ctrl.signal);
      if (seq !== jobSeq || pageNo !== page) return;
      pollFailures = 0;
      applyResult(r);
      if (searchDone) {
        finishJob();
        return;
      }
      schedulePoll(seq, 500); // 400–600ms 轮询，done / 404 立即停
    } catch (e) {
      if (seq !== jobSeq || pageNo !== page) return;
      if (e && e.aborted) return;
      if (e && e.status === 404) {
        handleExpired();
        return;
      }
      pollFailures += 1;
      if (pollFailures >= 3) {
        searching = false;
        starting = false;
        paging = false;
        stopSearchClock();
        error = describeFailure(e, '获取搜索结果');
        return;
      }
      schedulePoll(seq, 500 + 300 * pollFailures); // 连续失败时退避
    } finally {
      if (inflight && inflight.ctrl === ctrl) inflight = null;
      if (pageNo === page) paging = false;
    }
  }

  async function doSearch(preset) {
    const q = String(typeof preset === 'string' ? preset : key).trim();
    if (!q || starting) return;
    if (sourceScope === 'custom' && !pickedSourceIds.length) {
      error = '请至少选择一个书源，或把搜索范围切回「全部可搜索源」';
      return;
    }
    const requestedIds = sourceScope === 'custom' ? pickedSourceIds.slice() : [];
    // 新搜索不再取消旧任务：旧任务在后端继续跑，并出现在「搜索任务」里可管理。
    const previousRunning = searching && !!jobId;
    const seq = ++jobSeq;
    retrySeq += 1; // 作废仍在进行的失败源重试
    cancelPolling();
    jobId = '';
    jobExpired = false;
    starting = true;
    searching = false;
    searchDone = false;
    paging = false;
    searched = true;
    retrying = false;
    retryNote = '';
    retryExtra = [];
    page = 1;
    resultsPage = 1;
    results = []; // 新 job 属于新查询，清空旧结果
    total = 0;
    hasMore = false;
    sourceTotal = 0;
    finishedSources = 0;
    pendingSources = 0;
    deadlineMs = 0;
    requestedScope = sourceScope === 'curated' ? 'curated' : 'all';
    searchScope = requestedScope;
    searchCoverage = requestedScope === 'curated' ? 'curated' : 'expanded';
    sourceMode = '';
    scopeFiltered = 0;
    failedSources = [];
    failedCount = 0;
    skippedSources = [];
    skippedCount = 0;
    pollFailures = 0;
    jobStatus = '';
    jobsError = '';
    error = '';
    notice = previousRunning
      ? '上一个搜索仍在后台运行，可在「搜索任务」中查看进度或取消。'
      : '';
    requestSourceCount = requestedIds.length;
    ignoredSourceIds = [];
    rememberKey(q);
    lastSearchKey = q;
    lastSearchScopeIds = requestedIds.slice();
    resultScopeMode = sourceScope;
    resultRequested = requestedIds.length;
    startSearchClock();
    try {
      const r = await api.searchStart(q, requestedIds, requestedScope);
      if (seq !== jobSeq) return;
      jobId = String(r.job_id || '');
      if (!jobId) throw new Error('后端未返回 job_id');
      sourceTotal = Number(r.source_total) || 0;
      searchScope = String(r.scope || searchScope);
      searchCoverage = String(r.coverage || searchCoverage);
      sourceMode = String(r.source_mode || '');
      scopeFiltered = Number(r.scope_filtered) || 0;
      curatedTotal = Number(r.curated_total) || 0;
      requestSourceCount = Array.isArray(r.source_ids_requested) ? r.source_ids_requested.length : requestedIds.length;
      ignoredSourceIds = Array.isArray(r.ignored_source_ids) ? r.ignored_source_ids.map(String) : [];
      pendingSources = sourceTotal;
      deadlineMs = Number(r.deadline_ms) || 0;
      starting = false;
      searching = true;
      fetchPage(seq, 1);
    } catch (e) {
      if (seq !== jobSeq) return;
      starting = false;
      searching = false;
      stopSearchClock();
      error = describeFailure(e, '发起搜索');
    }
  }

  // 结果页“只搜这些源重试”：沿用本次关键词与本次范围
  function retryScope() {
    if (starting || searching) return;
    if (!lastSearchKey) return;
    if (resultScopeMode === 'custom') {
      if (!lastSearchScopeIds.length) {
        notice = '本次范围没有记录到具体书源，无法按范围重试；请重新选择书源后再搜。';
        return;
      }
      sourceScope = 'custom';
      pickedSourceIds = lastSearchScopeIds.slice();
      scopeOpen = true;
    } else {
      // 保持用户上次的范围（精选仍是精选，全部仍是全部）
      sourceScope = requestedScope === 'curated' ? 'curated' : 'all';
    }
    key = lastSearchKey;
    doSearch();
  }

  // 仅重试失败源：用上次关键词 + 失败源的 source_ids 新建 job，
  // 轮询完成后按 book_url 去重并入当前列表，并把新增项标记为“重试命中”。
  async function retryFailedSources() {
    if (retrying || starting || searching) return;
    const ids = [...failedSources, ...skippedSources].map((f) => f.url).filter(Boolean);
    if (!ids.length) {
      retryNote = '后端未返回失败源地址，无法只重试这些源；可整次重试，或到「书源管理」检查后再试。';
      return;
    }
    const q = lastSearchKey || key.trim();
    if (!q) return;
    const seq = ++retrySeq;
    retrying = true;
    retryNote = '';
    let collected = [];
    let lastErrors = [];
    try {
      // 显式 source_ids 会跳过 curated 收缩；scope 仍显式传 all，避免依赖后端默认值。
      const r = await api.searchStart(q, ids, 'all');
      if (seq !== retrySeq) return;
      const jid = String(r.job_id || '');
      if (!jid) throw new Error('后端未返回 job_id');
      let p = 1;
      let pages = 0;
      while (seq === retrySeq) {
        const rr = await api.searchResult(jid, p, PER_PAGE);
        if (seq !== retrySeq) return;
        collected = collected.concat((rr.books || []).map((b) => ({ ...b, retry: true })));
        lastErrors = Array.isArray(rr.errors)
          ? rr.errors.map((e) => ({
              name: e.source_name || e.source || '未知书源',
              reason: describeSourceError(e.error),
              url: e.source || '',
              kind: e.kind || 'site',
            }))
          : [];
        pages += 1;
        if (rr.done) {
          // 失败源也可能返回多页；最多再取 4 页，避免异常源拖住界面
          if (rr.has_more && pages < 5) {
            p += 1;
            continue;
          }
          break;
        }
        await new Promise((res) => setTimeout(res, 500));
      }
      if (seq !== retrySeq) return;
      const seen = new Set(results.map((b) => b.book_url));
      const fresh = [];
      for (const b of collected) {
        if (seen.has(b.book_url)) continue;
        seen.add(b.book_url);
        fresh.push(b);
      }
      retryExtra = dedupeByUrl(retryExtra.concat(fresh));
      results = dedupeByUrl(results.concat(fresh));
      total = Math.max(total, results.length);
      const budgetSkipped = (e) => e.kind === 'skipped' || e.kind === 'truncated';
      failedSources = lastErrors.filter((e) => !budgetSkipped(e));
      failedCount = failedSources.length;
      skippedSources = lastErrors.filter(budgetSkipped);
      skippedCount = skippedSources.length;
      if (fresh.length) {
        notice = `仅重试失败/未完成源：新增 ${fresh.length} 本（已按 book_url 去重）`;
      } else {
        retryNote = `仅重试 ${ids.length} 个失败/未完成源：没有新增结果` +
          (lastErrors.length ? `，仍有 ${lastErrors.length} 个源未成功，原因见上方列表。` : '，这些源均已正常返回但无匹配。');
      }
    } catch (e) {
      if (seq !== retrySeq) return;
      if (e && e.status === 404) {
        retryNote = '失败源重试任务已过期（后端 job 只保留 60 秒），请整次重试。';
      } else {
        retryNote = describeFailure(e, '仅重试失败源');
      }
    } finally {
      if (seq === retrySeq) retrying = false;
    }
  }

  // ---- 搜索任务面板：查看 / 取消 / 移除 ----

  // 重新挂载一个任务：运行中则继续轮询，已结束则直接取快照。
  function attachJob(job) {
    if (!job || !job.job_id) return;
    jobSeq += 1;
    retrySeq += 1;
    cancelPolling();
    clearInterval(searchTimer);
    searchTimer = null;
    mode = 'search';
    key = job.keyword || '';
    lastSearchKey = job.keyword || '';
    lastSearchScopeIds = [];
    resultScopeMode = job.scope === 'curated' ? 'curated' : 'all';
    resultRequested = 0;
    requestedScope = job.scope === 'curated' ? 'curated' : 'all';
    searchScope = requestedScope;
    searchCoverage = job.coverage || (requestedScope === 'curated' ? 'curated' : 'expanded');
    sourceMode = job.source_mode || '';
    curatedTotal = Number(job.curated_total) || curatedTotal;
    scopeFiltered = 0;
    requestSourceCount = 0;
    ignoredSourceIds = [];
    sourceTotal = Number(job.source_total) || 0;
    finishedSources = Number(job.finished) || 0;
    pendingSources = Number(job.pending) || 0;
    failedCount = Number(job.failed) || 0;
    failedSources = [];
    skippedCount = Number(job.skipped) || 0;
    skippedSources = [];
    jobId = job.job_id;
    jobExpired = false;
    jobStatus = String(job.status || '');
    page = 1;
    resultsPage = 1;
    results = [];
    total = 0;
    hasMore = false;
    searched = true;
    starting = false;
    searchDone = job.status !== 'running';
    searching = job.status === 'running';
    paging = false;
    pollFailures = 0;
    retryExtra = [];
    retryNote = '';
    retrying = false;
    error = '';
    notice = '';
    // 运行中的任务按创建时间还原已运行时长；已结束的用起止时间算总耗时。
    searchStartedAt = searching ? Number(job.created_at) || Date.now() : 0;
    searchElapsed = searchStartedAt
      ? Math.max(0, Math.floor((Date.now() - searchStartedAt) / 1000))
      : 0;
    searchDuration = 0;
    if (searching) {
      searchTimer = setInterval(() => {
        searchElapsed = Math.floor((Date.now() - searchStartedAt) / 1000);
      }, 1000);
    } else if (job.created_at && job.finished_at) {
      searchDuration = Math.max(
        1,
        Math.round((Number(job.finished_at) - Number(job.created_at)) / 1000)
      );
    }
    fetchPage(jobSeq, 1);
  }

  async function cancelJob(job) {
    if (!job || jobsBusyId) return;
    jobsBusyId = job.job_id;
    jobsError = '';
    try {
      await cancelSearchJob(job.job_id);
      // 正在查看的任务：立刻取一次最终状态（后端会把 status 写成 cancelled）。
      if (job.job_id === jobId) {
        await fetchPage(jobSeq, page);
      }
    } catch (e) {
      jobsError = describeFailure(e, '取消搜索任务');
    } finally {
      jobsBusyId = '';
    }
  }

  async function removeJob(job) {
    if (!job || jobsBusyId) return;
    jobsBusyId = job.job_id;
    jobsError = '';
    try {
      await removeSearchJob(job.job_id);
      // 移除的正是当前查看的任务：清空结果区，避免继续显示已删除任务。
      if (job.job_id === jobId) {
        jobSeq += 1;
        cancelPolling();
        clearInterval(searchTimer);
        searchTimer = null;
        jobId = '';
        jobStatus = '';
        searched = false;
        searching = false;
        starting = false;
        searchDone = false;
        jobExpired = false;
        paging = false;
        results = [];
        total = 0;
        hasMore = false;
        failedSources = [];
        failedCount = 0;
        skippedSources = [];
        skippedCount = 0;
        notice = '';
        error = '';
        page = 1;
        resultsPage = 1;
        searchElapsed = 0;
        searchDuration = 0;
        searchStartedAt = 0;
      }
    } catch (e) {
      jobsError = describeFailure(e, '移除搜索任务');
    } finally {
      jobsBusyId = '';
    }
  }

  // 翻页复用同一个 job_id，不新建 job
  async function changePage(target) {
    if (!jobId || jobExpired || paging || target < 1) return;
    if (target > page && !hasMore) return;
    const prevPage = page;
    page = target;
    paging = true;
    error = '';
    if (!searchDone) {
      fetchPage(jobSeq, target); // 轮询中立即取该页，并继续轮询
      return;
    }
    const seq = jobSeq;
    try {
      const r = await api.searchResult(jobId, target, PER_PAGE);
      if (seq !== jobSeq || target !== page) return;
      applyResult(r);
    } catch (e) {
      if (seq !== jobSeq) return;
      if (e && e.status === 404) {
        handleExpired();
      } else {
        page = prevPage;
        error = describeFailure(e, `加载第 ${target} 页`);
      }
    } finally {
      if (target === page) paging = false;
    }
  }

  function loadMore() {
    if (mode !== 'explore' || loading || loadingMore || !hasMore) return;
    page += 1;
    doExplore(false);
  }

  // 后端 /api/explore 的 url 参数必须是分类 URL（由 exploreUrl 展开），
  // 之前直接把书源 ID 传过去，只能命中“默认展开第一个分类”的兼容逻辑。
  async function loadCategories() {
    if (!selectedSource) return;
    catsLoading = true;
    categories = [];
    selectedCat = null;
    error = '';
    try {
      const list = await api.exploreCategories(selectedSource, 1);
      categories = list || [];
      if (categories.length) selectedCat = categories[0];
    } catch (e) {
      error = describeFailure(e, '读取分类');
    } finally {
      catsLoading = false;
    }
  }

  function switchMode(m) {
    if (mode === m) return;
    jobSeq += 1;
    retrySeq += 1;
    cancelPolling();
    jobId = '';
    starting = false;
    searching = false;
    searchDone = false;
    jobExpired = false;
    paging = false;
    mode = m;
    results = [];
    failedSources = [];
    failedCount = 0;
    skippedSources = [];
    skippedCount = 0;
    total = 0;
    hasMore = false;
    searched = false;
    error = '';
    page = 1;
    requestSourceCount = 0;
    ignoredSourceIds = [];
  }

  function dedupe(list) {
    const seen = new Set();
    return list.filter((b) => {
      const k = `${b.book_url}|${b.origin || ''}`;
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  }

  // 失败源重试合并用：严格按 book_url 去重（跨源重复也保留先到的）
  function dedupeByUrl(list) {
    const seen = new Set();
    return list.filter((b) => {
      if (seen.has(b.book_url)) return false;
      seen.add(b.book_url);
      return true;
    });
  }

  async function doExplore(reset = true) {
    if (!selectedCat) return;
    if (reset) {
      page = 1;
      results = [];
      error = '';
      hasMore = false;
      total = 0;
    }
    loading = reset;
    loadingMore = !reset;
    try {
      let cat = selectedCat;
      if (!reset) {
        // 书源的分页通过分类 URL 里的 {{page}} 生效，重新展开分类取同一标题的 URL
        const list = (await api.exploreCategories(selectedSource, page)) || [];
        cat = list.find((c) => c.title === selectedCat.title) || selectedCat;
      }
      const books = dedupe(await api.explore(cat.url, page));
      const merged = reset ? books : dedupe(results.concat(books));
      if (!reset && merged.length === results.length) {
        hasMore = false;
        notice = '该书源/分类没有更多结果（可能不支持翻页）';
      } else {
        hasMore = reset ? books.length >= 10 : true;
      }
      results = merged;
      total = merged.length;
      searched = true;
      failedSources = [];
      failedCount = 0;
      skippedSources = [];
      skippedCount = 0;
    } catch (e) {
      error = describeFailure(e, reset ? '发现' : `加载第 ${page} 页`);
      if (!reset && page > 1) page -= 1;
    } finally {
      loading = false;
      loadingMore = false;
    }
  }

  function open(r) {
    // 搜索结果的 name/author/cover/intro 作为 seed 一起带给详情与目录，
    // 避免正文规则取不到字段的源详情全空
    dispatch('open', {
      bookUrl: r.book_url,
      sourceId: r.origin,
      name: r.name,
      author: r.author,
      coverUrl: r.cover_url,
      intro: r.intro,
      // image / text：漫画与小说分流；缺省时 App 会再探测书源类型
      bookType: r.type,
    });
  }

  async function addShelf(r) {
    if (shelfIds.has(r.book_url)) return;
    try {
      await api.addToShelf({
        book_id: r.book_url,
        source_id: r.origin,
        name: r.name,
        author: r.author,
        cover: r.cover_url,
      });
      shelfIds = new Set([...shelfIds, r.book_url]);
      notice = `已加入书架：《${r.name || r.book_url}》`;
    } catch (e) {
      error = '加入书架失败：' + e.message;
    }
  }

  // 按链接打开：后端抓详情页并用书源规则解析，落库后直接进阅读页
  async function doImport() {
    const url = importUrl.trim();
    if (!importSourceId || !url) {
      error = '请选择书源并粘贴书籍详情页链接';
      return;
    }
    importing = true;
    error = '';
    notice = '';
    try {
      const r = await api.importByUrl(importSourceId, url);
      shelfIds = new Set([...shelfIds, r.book_url]);
      notice = r.created ? `已导入《${r.name}》，正在打开…` : `已打开已有书籍《${r.name}》`;
      importUrl = '';
      dispatch('open', {
        bookUrl: r.book_url,
        sourceId: r.source_id,
        name: r.name,
        author: r.author,
        coverUrl: r.cover_url,
        intro: r.intro,
        bookType: r.book_type,
      });
    } catch (e) {
      error = '按链接打开失败：' + e.message;
    } finally {
      importing = false;
    }
  }
</script>

<div class="seg" role="tablist">
  <button role="tab" aria-selected={mode === 'search'} class:on={mode === 'search'} on:click={() => switchMode('search')}>搜索</button>
  <button role="tab" aria-selected={mode === 'explore'} class:on={mode === 'explore'} on:click={() => switchMode('explore')}>发现</button>
  <button role="tab" aria-selected={mode === 'import'} class:on={mode === 'import'} on:click={() => switchMode('import')}>按链接打开</button>
</div>

{#if mode === 'search'}
  <div class="row">
    <input placeholder="输入书名 / 作者" bind:value={key} on:keydown={(e) => e.key === 'Enter' && doSearch()} />
    <button on:click={doSearch} disabled={starting || (sourceScope === 'custom' && !pickedSourceIds.length)}>搜索</button>
  </div>

  <SearchJobsPanel
    jobs={$searchJobs.jobs}
    active={$searchJobs.active}
    error={$searchJobs.error || jobsError}
    busyId={jobsBusyId}
    on:cancel={(e) => cancelJob(e.detail)}
    on:remove={(e) => removeJob(e.detail)}
    on:view={(e) => attachJob(e.detail)}
  />

  <DiscoverHistory busy={starting || searching} on:pick={(e) => useHistory(e.detail)} />

  <DiscoverScopePicker
    sources={sources}
    searchableSources={searchableSources}
    curatedTotal={curatedTotal}
    bind:sourceScope
    bind:pickedSourceIds
    bind:scopeOpen
    bind:groupFilter
    groupOptions={groupOptions}
    pickSources={pickSources}
    on:clearError={() => (error = '')}
  />

{:else if mode === 'explore'}
  <div class="row">
    <select bind:value={selectedSource} on:change={loadCategories} style="flex:1;min-width:0">
      {#each sources as s}
        <option value={s.book_source_url}>{s.book_source_name}{s.enabled ? '' : '（已禁用）'}</option>
      {/each}
    </select>
    <button class="ghost" on:click={loadCategories} disabled={catsLoading}>刷新分类</button>
  </div>
  {#if catsLoading}
    <p class="loading">读取分类…</p>
  {:else if categories.length}
    <div class="chips">
      {#each categories as c}
        <button class="chip" class:on={selectedCat && selectedCat.title === c.title} on:click={() => { selectedCat = c; results = []; searched = false; }}>
          {c.title}
        </button>
      {/each}
    </div>
    <div class="row" style="margin-top:8px">
      <button on:click={() => doExplore(true)} disabled={!selectedCat || loading}>加载「{selectedCat ? selectedCat.title : ''}」</button>
    </div>
  {:else if !error && sources.length}
    <p class="muted" style="margin-top:8px">该书源没有可展开的分类（exploreUrl 为空、为 @js 或解析为空）。</p>
  {/if}
{:else}
  <p class="muted" style="margin:0 0 8px">
    不用先搜索：选择书源后，粘贴该书源站点上的书籍详情页链接，后端会按书源规则解析并直接打开。
  </p>
  <div class="row">
    <select bind:value={importSourceId} style="flex:1 1 180px;min-width:0" aria-label="按链接打开的书源">
      {#each sources as s (s.book_source_url)}
        <option value={s.book_source_url}>{s.book_source_name}{s.enabled ? '' : '（已禁用）'}</option>
      {/each}
    </select>
    <input
      style="flex:2 1 260px;min-width:0"
      placeholder="粘贴书籍详情页链接（需与书源同域名）"
      bind:value={importUrl}
      on:keydown={(e) => e.key === 'Enter' && doImport()}
    />
    <button on:click={doImport} disabled={importing || !importSourceId || !importUrl.trim()}>
      {importing ? '导入中…' : '按链接打开'}
    </button>
  </div>
  {#if !sources.length && sourcesReady}
    <p class="muted" style="margin:8px 0 0">当前还没有书源，请先到「书源管理」导入并启用。</p>
  {/if}
  {#if error}<p class="err">{error}</p>{/if}
  {#if notice}<p class="ok-line">{notice}</p>{/if}
{/if}

{#if mode !== 'import'}
<DiscoverResults
  mode={mode}
  error={error}
  notice={notice}
  sources={sources}
  ignoredSourceIds={ignoredSourceIds}
  requestSourceCount={requestSourceCount}
  sourceTotal={sourceTotal}
  sourceMode={sourceMode}
  resultScopeMode={resultScopeMode}
  resultRequested={resultRequested}
  scopeFiltered={scopeFiltered}
  curatedTotal={curatedTotal}
  jobExpired={jobExpired}
  starting={starting}
  searching={searching}
  completedSources={completedSources}
  pendingSources={pendingSources}
  deadlineMs={deadlineMs}
  searchElapsed={searchElapsed}
  searchDuration={searchDuration}
  paging={paging}
  page={page}
  searched={searched}
  total={total}
  results={results}
  hasMore={hasMore}
  searchDone={searchDone}
  sourcesReady={sourcesReady}
  searchableCount={searchableSources.length}
  requestedScope={requestedScope}
  searchCoverage={searchCoverage}
  failedCount={failedCount}
  failedSources={failedSources}
  skippedCount={skippedCount}
  skippedSources={skippedSources}
  retrying={retrying}
  retryNote={retryNote}
  shelfIds={shelfIds}
  lastSearchKey={lastSearchKey}
  hasKey={!!key.trim()}
  loading={loading}
  loadingMore={loadingMore}
  on:retryAll={() => retryWithAll()}
  on:retryScope={() => retryScope()}
  on:retryFailed={() => retryFailedSources()}
  on:search={() => doSearch()}
  on:goSources={() => goSources()}
  on:page={(e) => changePage(e.detail)}
  on:loadMore={() => loadMore()}
  on:open={(e) => open(e.detail)}
  on:addShelf={(e) => addShelf(e.detail)}
/>
{/if}
