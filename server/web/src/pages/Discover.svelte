<script>
  import { createEventDispatcher, onDestroy, onMount } from 'svelte';
  import { api, describeSourceError, describeFailure } from '../lib/api.js';

  const dispatch = createEventDispatcher();
  let mode = 'search'; // search | explore
  let key = '';
  let results = [];
  let error = '';
  let notice = '';
  let searched = false;

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
  let paging = false;

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
  let strengthenSearch = false; // 勾选=加强搜索（scope=all，覆盖更多源）
  let requestedScope = 'curated'; // 本次请求携带的 scope（用户意图）
  let searchScope = 'curated'; // 后端实际使用的 scope
  let searchCoverage = 'curated'; // curated / expanded
  let curatedTotal = 0;
  let failedCount = 0;
  let failedSources = []; // [{ name, reason }]
  let resultsPage = 1; // 当前卡片实际对应的页码，过期时回退显示

  // 发现模式
  let sources = [];
  let selectedSource = '';
  let categories = [];
  let selectedCat = null;
  let catsLoading = false;
  let loading = false;
  let loadingMore = false;

  // 搜索范围：all=全部启用源；custom=指定书源（source_ids 传 URL）
  let sourceScope = 'all';
  let pickedSourceIds = [];
  let scopeOpen = false;
  let requestSourceCount = 0; // 本次实际带上的 source_ids 数（0=未传）
  let ignoredSourceIds = []; // 后端回显的 ignored_source_ids
  let groupFilter = 'all';

  // 搜索历史：本地持久化（localStorage），最多 12 条
  const HISTORY_KEY = 'oread_search_history';
  const HISTORY_MAX = 12;
  let history = [];

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

  $: enabledSources = sources.filter((s) => s.enabled);
  $: groupOptions = [...new Set(sources.map((s) => s.book_source_group).filter(Boolean))];
  $: pickSources = groupFilter === 'all' ? sources : sources.filter((s) => (s.book_source_group || '') === groupFilter);

  // 书架状态（用于“已加入”标记）
  let shelfIds = new Set();

  // 已完成源 = 已结束 - 失败（失败源也计入 finished）
  $: completedSources = Math.max(0, finishedSources - failedCount);

  onMount(async () => {
    history = readHistory();
    try {
      sources = await api.sources();
      if (sources.length) {
        selectedSource = sources[0].book_source_url;
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

  function readHistory() {
    try {
      const arr = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
      if (!Array.isArray(arr)) return [];
      return arr.filter((x) => typeof x === 'string' && x.trim()).slice(0, HISTORY_MAX);
    } catch {
      return [];
    }
  }

  function writeHistory(list) {
    history = list.slice(0, HISTORY_MAX);
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
    } catch {
      /* 本地存储不可用时只作用于当前会话 */
    }
  }

  function rememberKey(q) {
    writeHistory([q, ...history.filter((x) => x !== q)]);
  }

  function removeHistory(q) {
    writeHistory(history.filter((x) => x !== q));
  }

  function clearHistory() {
    writeHistory([]);
  }

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
    deadlineMs = Number(r.deadline_ms) || deadlineMs;
    const rawFailed = Array.isArray(r.errors) ? r.errors : [];
    failedSources = rawFailed.map((e) => ({
      name: e.source_name || e.source || '未知书源',
      reason: describeSourceError(e.error),
      url: e.source || '',
    }));
    failedCount = Number.isFinite(Number(r.source_failed))
      ? Math.max(0, Number(r.source_failed))
      : failedSources.length;
  }

  function finishJob() {
    searching = false;
    starting = false;
    paging = false;
    pollFailures = 0;
    cancelPolling();
    stopSearchClock();
  }

  function handleExpired() {
    cancelPolling();
    searching = false;
    starting = false;
    paging = false;
    jobExpired = true;
    page = resultsPage || 1; // 结果仍停留在旧页，页码同步回退，避免“第 2 页显示第 1 页内容”
    stopSearchClock();
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

  function sname(s) {
    return (s && s.book_source_name) || '未命名书源';
  }

  // 后端 type/type_name 正在加：有就显示，没有就跳过，不造字段
  function typeOf(s) {
    return (s && (s.type_name || s.type)) || '';
  }

  function togglePick(url) {
    pickedSourceIds = pickedSourceIds.includes(url)
      ? pickedSourceIds.filter((x) => x !== url)
      : [...pickedSourceIds, url];
  }

  function selectAllEnabled() {
    pickedSourceIds = enabledSources.map((s) => s.book_source_url);
  }

  // 按分组选择：只并入该分组下的启用书源，保留已选的其他源
  function pickGroup() {
    if (groupFilter === 'all') return;
    const add = enabledSources
      .filter((s) => (s.book_source_group || '') === groupFilter)
      .map((s) => s.book_source_url);
    pickedSourceIds = [...new Set(pickedSourceIds.concat(add))];
  }

  function clearPicked() {
    pickedSourceIds = [];
  }

  // ignored_source_ids 可能是 URL，也可能是 1-based 序号；能映射到书源名就显示名字
  function sourceLabel(id) {
    const s = sources.find((x) => x.book_source_url === id);
    if (s) {
      const t = typeOf(s);
      return t ? `${sname(s)}（${t}）` : sname(s);
    }
    return /^\d+$/.test(id) ? `第 ${id} 个源（序号）` : id;
  }

  async function doSearch(preset) {
    const q = String(typeof preset === 'string' ? preset : key).trim();
    if (!q || starting) return;
    if (sourceScope === 'custom' && !pickedSourceIds.length) {
      error = '请至少选择一个书源，或把搜索范围切回「全部启用源」';
      return;
    }
    const requestedIds = sourceScope === 'custom' ? pickedSourceIds.slice() : [];
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
    searchScope = strengthenSearch ? 'all' : 'curated';
    requestedScope = searchScope;
    searchCoverage = searchScope;
    curatedTotal = 0;
    failedSources = [];
    failedCount = 0;
    pollFailures = 0;
    error = '';
    notice = '';
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
      searchCoverage = String(r.coverage || searchScope);
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
      sourceScope = 'all';
    }
    key = lastSearchKey;
    doSearch();
  }

  // 仅重试失败源：用上次关键词 + 失败源的 source_ids 新建 job，
  // 轮询完成后按 book_url 去重并入当前列表，并把新增项标记为“重试命中”。
  async function retryFailedSources() {
    if (retrying || starting || searching) return;
    const ids = failedSources.map((f) => f.url).filter(Boolean);
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
      const r = await api.searchStart(q, ids);
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
      failedSources = lastErrors;
      failedCount = lastErrors.length;
      if (fresh.length) {
        notice = `仅重试失败源：新增 ${fresh.length} 本（已按 book_url 去重）`;
      } else {
        retryNote = `仅重试 ${ids.length} 个失败源：没有新增结果` +
          (lastErrors.length ? `，仍有 ${lastErrors.length} 个源失败，原因见上方列表。` : '，这些源均已正常返回但无匹配。');
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
</script>

<div class="seg" role="tablist">
  <button role="tab" aria-selected={mode === 'search'} class:on={mode === 'search'} on:click={() => switchMode('search')}>搜索</button>
  <button role="tab" aria-selected={mode === 'explore'} class:on={mode === 'explore'} on:click={() => switchMode('explore')}>发现</button>
</div>

{#if mode === 'search'}
  <div class="row">
    <input placeholder="输入书名 / 作者" bind:value={key} on:keydown={(e) => e.key === 'Enter' && doSearch()} />
    <button on:click={doSearch} disabled={starting || (sourceScope === 'custom' && !pickedSourceIds.length)}>搜索</button>
  </div>

  <label class="strengthen-toggle">
    <input type="checkbox" bind:checked={strengthenSearch} />
    <span>加强搜索（更慢，覆盖更多源）</span>
  </label>

  {#if history.length}
    <div class="hist" aria-label="搜索历史">
      <div class="hist-head">
        <span class="muted">最近搜索（{history.length}）</span>
        <button class="link-btn" on:click={clearHistory}>清空历史</button>
      </div>
      <div class="hist-list">
        {#each history as h (h)}
          <span class="hist-item">
            <button class="hist-word" title={h} on:click={() => useHistory(h)} disabled={starting || searching}>{h}</button>
            <button class="hist-del" aria-label="删除搜索历史 {h}" on:click={() => removeHistory(h)}>×</button>
          </span>
        {/each}
      </div>
    </div>
  {/if}

  <div class="search-scope">
    <div class="row">
      <select bind:value={sourceScope} on:change={() => (error = '')} style="flex:1;min-width:0" aria-label="搜索范围">
        <option value="all">全部可搜索源</option>
        <option value="custom">指定分类 / 书源（已选 {pickedSourceIds.length} 个）</option>
      </select>
      {#if sourceScope === 'custom'}
        <button class="ghost" on:click={() => (scopeOpen = !scopeOpen)} aria-expanded={scopeOpen}>
          {scopeOpen ? '收起书源' : '选择书源'}
        </button>
      {/if}
    </div>

    {#if sourceScope === 'custom' && scopeOpen}
      {#if groupOptions.length}
        <div class="row" style="margin-top:8px">
          <select bind:value={groupFilter} style="width:100%;min-width:0" aria-label="按分类筛选书源">
            <option value="all">全部分类</option>
            {#each groupOptions as g}
              <option value={g}>{g}</option>
            {/each}
          </select>
          <button class="ghost" on:click={pickGroup} disabled={groupFilter === 'all'}>选入本分类</button>
        </div>
      {/if}
      <div class="src-list" role="group" aria-label="选择参与搜索的书源">
        {#each pickSources as s (s.book_source_url)}
          <label class="src-item" class:off={!s.enabled}>
            <input
              type="checkbox"
              disabled={!s.enabled}
              checked={pickedSourceIds.includes(s.book_source_url)}
              on:change={() => togglePick(s.book_source_url)}
            />
            <span class="src-name">{sname(s)}</span>
            {#if s.book_source_group}<span class="src-type">{s.book_source_group}</span>{/if}
            {#if typeOf(s)}<span class="src-type">{typeOf(s)}</span>{/if}
            {#if !s.enabled}<span class="src-off">已禁用</span>{/if}
          </label>
        {/each}
        {#if !sources.length}<p class="muted" style="margin:6px 0">还没有书源，请先到「书源管理」添加。</p>{:else if !pickSources.length}<p class="muted" style="margin:6px 0">该分类下没有书源。</p>{/if}
      </div>
      <div class="row" style="margin-top:6px">
        <button class="ghost" on:click={selectAllEnabled} disabled={!enabledSources.length}>全选启用源</button>
        <button class="ghost" on:click={clearPicked} disabled={!pickedSourceIds.length}>清空</button>
      </div>
    {/if}

    <p class="muted scope-hint">
      本次范围：{sourceScope === 'all'
        ? strengthenSearch
          ? `全部启用源（${enabledSources.length} 个）`
          : `精选源${curatedTotal ? `（${curatedTotal} 个）` : ''}`
        : `已选 ${pickedSourceIds.length} 个源`}
      {#if sourceScope === 'custom' && !pickedSourceIds.length}&nbsp;· 至少选 1 个才能搜索{/if}
    </p>
  </div>
{:else}
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
{/if}

{#if error}<p class="err">{error}</p>{/if}
{#if notice}<p class="ok-line">{notice}</p>{/if}

{#if mode === 'search' && ignoredSourceIds.length}
  <div class="warn-box" role="alert">
    <div class="warn-title">已忽略 {ignoredSourceIds.length} 个书源：本次实际搜索 {sourceTotal} 个源</div>
    <div class="warn-item">
      本次请求 {requestSourceCount} 个源，后端只接受已启用且配置了搜索规则的书源。
      被忽略：{ignoredSourceIds.map(sourceLabel).join('、')}
    </div>
    {#if !sourceTotal}
      <div class="warn-item">没有可搜索的源，请到「书源管理」启用书源并补全搜索规则后重试。</div>
    {/if}
  </div>
{/if}

{#if mode === 'search' && sourceTotal && !error}
  <p class="scope-report">
    本次范围：<b>{sourceTotal}</b> 个源{resultScopeMode === 'custom' ? `（指定 ${resultRequested} 个）` : '（全部可搜索源）'}
    <button class="link-btn" on:click={retryScope} disabled={starting || searching || !lastSearchKey}>只搜这些源重试</button>
  </p>
{/if}

{#if mode === 'search' && jobExpired}
  <div class="warn-box" role="alert">
    <div class="warn-title">搜索任务已过期，结果可能不完整</div>
    <div class="warn-item">后端搜索任务只保留 60 秒、最多 8 个；当前 job 已不在服务端，无法继续取回剩余结果。</div>
    <div class="row" style="margin-top:8px">
      <button on:click={doSearch} disabled={starting || !key.trim()}>重新搜索</button>
    </div>
  </div>
{/if}

{#if mode === 'search' && (starting || searching)}
  <div class="search-wait" aria-live="polite">
    <div class="sw-line">
      <b>
        {#if starting}
          已发起搜索，正在创建任务并等待首个书源…
        {:else}
          已完成 {completedSources} / 共 {sourceTotal || '?'} 个源，慢源仍在返回…
        {/if}
      </b>
      {#if !starting}<span class="sw-time">已等待 {searchElapsed}s</span>{/if}
    </div>
    <div class="sw-sub">
      结果先到先显示：已完成的源会立即出现在下方，不需要等全部书源结束。
      {#if pendingSources}&nbsp;仍有 {pendingSources} 个源未返回。{/if}
      {#if deadlineMs}后端单源预算 {Math.round(deadlineMs / 1000)}s，超时源计入失败。{/if}
    </div>
  </div>
{/if}

{#if mode === 'search' && paging}
  <p class="loading">正在读取第 {page} 页…</p>
{/if}

{#if mode === 'search' && (starting || searching) && results.length === 0}
  <div class="grid auto" style="margin-top:12px" aria-hidden="true">
    {#each [0, 1, 2] as _i}
      <div class="card book skeleton">
        <div class="sk-cover"></div>
        <div class="sk-line w70"></div>
        <div class="sk-line w45"></div>
      </div>
    {/each}
  </div>
{/if}

{#if mode === 'explore' && loading}
  <p class="loading">加载中…</p>
{/if}

{#if mode === 'search' && searched}
  <div class="result-meta">
    <span>
      命中 <b>{total}</b> 本 · 本页 {results.length} 本
      {#if starting}
        &nbsp;· 正在创建搜索任务…
      {:else if searching}
        &nbsp;· 已完成 {completedSources} / 共 {sourceTotal || '?'} 个源 · 搜索中 · 已等待 {searchElapsed}s
      {:else if jobExpired}
        &nbsp;· 任务已过期，结果可能不完整
      {:else if sourceTotal}
        &nbsp;· 已完成 {completedSources} / 共 {sourceTotal} 个源{#if failedCount}（失败 {failedCount}）{/if}{#if searchDuration}&nbsp;· 用时 {searchDuration}s{/if}
      {/if}
      {#if requestedScope === 'curated'}
        &nbsp;· {searchCoverage === 'curated' ? '精选源' : '精选为空，已回退全部源'}
      {:else}
        &nbsp;· 加强搜索（全部源）
      {/if}
    </span>
  </div>
{/if}

{#if mode === 'search' && searchDone && total === 0}
  {#if requestedScope === 'curated' && searchCoverage === 'curated'}
    <p class="muted empty-hint">精选源没有结果，可勾选「加强搜索（更慢，覆盖更多源）」再试。</p>
  {:else if requestedScope === 'curated'}
    <p class="muted empty-hint">精选集尚未生成（或为空），已回退搜索全部源，仍无结果；可换个关键词再试。</p>
  {:else}
    <p class="muted empty-hint">没有找到结果，可换个关键词再试。</p>
  {/if}
{/if}

{#if failedCount || failedSources.length}
  <details class="warn-box">
    <summary class="warn-title">
      {failedCount || failedSources.length} 个书源失败{#if searching}（其余仍在搜索）{:else}，已返回其余结果{/if}
    </summary>
    <div class="warn-items">
      {#each failedSources as f}
        <div class="warn-item">{f.name}：{f.reason}</div>
      {/each}
      {#if !failedSources.length}
        <div class="warn-item">后端未返回失败明细，可重试或到「书源管理」检查这些书源。</div>
      {/if}
      <div class="warn-item muted">可先换个关键词或稍后重试；长期失败请到「书源管理」检查这些源的地址与启用状态。</div>
      <div class="row" style="margin-top:8px">
        <button class="ghost" on:click={retryFailedSources} disabled={retrying || starting || searching || !failedSources.some((f) => f.url)}>
          {retrying ? '正在仅重试失败源…' : '仅重试失败源'}
        </button>
        {#if !failedSources.some((f) => f.url)}<span class="muted">后端未返回失败源地址，无法按源重试</span>{/if}
      </div>
    </div>
  </details>
{/if}

{#if retryNote}<p class="retry-note" role="status">{retryNote}</p>{/if}

{#if results.length}
  <div class="grid auto" style="margin-top:12px">
    {#each results as r (r.book_url + '|' + (r.origin || ''))}
      <div class="card book">
        <button class="cover-btn" on:click={() => open(r)} aria-label="打开《{r.name || r.book_url}》">
          <div class="cover-wrap">
            <span class="cover-ph" aria-hidden="true">{(r.name || '?').slice(0, 2)}</span>
            {#if r.cover_url}
              <img class="cover" src={r.cover_url} alt="" referrerpolicy="no-referrer" on:error={(e) => (e.target.style.visibility = 'hidden')} />
            {/if}
          </div>
        </button>
        <div class="title" on:click={() => open(r)}>{r.name || r.book_url}</div>
        <div class="author">{r.author || '未知作者'}{#if r.origin_name} · {r.origin_name}{/if}{#if r.retry} <span class="retry-tag">重试命中</span>{/if}</div>
        {#if r.latest_chapter_title}<div class="author muted clamp1">{r.latest_chapter_title}</div>{/if}
        <div class="row" style="margin-top:8px">
          <button style="flex:1" on:click={() => open(r)}>阅读</button>
          <button class="ghost" disabled={shelfIds.has(r.book_url)} on:click={() => addShelf(r)}>{shelfIds.has(r.book_url) ? '已加入' : '加入书架'}</button>
        </div>
      </div>
    {/each}
  </div>
  <div class="row" style="justify-content:center;margin-top:16px">
    {#if mode === 'search'}
      {#if page > 1}<button class="ghost" on:click={() => changePage(page - 1)} disabled={paging || jobExpired}>上一页</button>{/if}
      <span class="muted">第 {page} 页{#if total}&nbsp;· 共 {total} 本{/if}</span>
      <button class="ghost" on:click={() => changePage(page + 1)} disabled={paging || !hasMore || jobExpired}>
        {paging ? '读取中…' : '下一页'}
      </button>
      {#if !hasMore && !searching && !starting}<span class="muted">没有更多了</span>{/if}
    {:else if hasMore}
      <button class="ghost" on:click={loadMore} disabled={loadingMore}>{loadingMore ? '加载中…' : `加载更多（第 ${page + 1} 页）`}</button>
    {:else}
      <span class="muted">没有更多了</span>
    {/if}
  </div>
{:else if mode === 'search'}
  {#if !searched}
    <div class="empty">输入关键词开始搜索</div>
  {:else if !starting && !searching && !paging}
    {#if failedSources.length || failedCount}
      <div class="empty">没有拿到结果，且有 {failedCount || failedSources.length} 个书源失败，原因见上方。<br />换关键词、稍后重试，或到「书源管理」检查这些书源。</div>
    {:else if !jobExpired && !error}
      <div class="empty">书源正常返回，但没有匹配的书。<br />换个关键词，或到「书源管理」确认书源是否启用。</div>
    {/if}
  {/if}
{:else if !loading && !searched}
  <div class="empty">选择书源与分类后开始发现</div>
{:else if !loading && searched}
  <div class="empty">没有找到结果。<br />换个分类，或到「书源管理」确认书源是否启用。</div>
{/if}

<style>
  .search-scope {
    margin-top: 8px;
  }
  .scope-hint {
    margin: 6px 0 0;
    font-size: 12px;
  }
  /* 加强搜索：块级 + 行内元素，墨水屏友好 */
  .strengthen-toggle {
    display: block;
    margin-top: 6px;
    font-size: 12px;
    color: var(--muted, #666);
    cursor: pointer;
  }
  .strengthen-toggle input {
    width: auto;
    min-height: auto;
    margin-right: 6px;
    vertical-align: middle;
  }
  .empty-hint {
    margin: 10px 0 0;
    font-size: 13px;
  }
  .scope-report {
    margin: 8px 0;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }
  /* 搜索历史：块级 + 行内块，墨水屏不依赖 flex/grid */
  .hist {
    margin-top: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 8px;
    background: var(--card, #fff);
  }
  .hist-head {
    font-size: 12px;
    margin-bottom: 6px;
  }
  .hist-head .link-btn {
    float: right;
  }
  .hist-item {
    display: inline-block;
    margin: 0 6px 6px 0;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 6px;
    background: var(--card, #fff);
  }
  .hist-word {
    background: transparent;
    color: var(--fg, #1d1d1f);
    border: 0;
    padding: 5px 8px;
    font-size: 12px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hist-word:hover {
    background: rgba(179, 57, 47, 0.08);
  }
  .hist-del {
    background: transparent;
    color: var(--muted, #666);
    border: 0;
    padding: 5px 8px;
    font-size: 12px;
  }
  .link-btn {
    background: transparent;
    color: var(--accent);
    border: 0;
    padding: 2px 6px;
    font-size: 12px;
    text-decoration: underline;
  }
  .link-btn:hover {
    background: transparent;
  }
  .retry-note {
    margin: 8px 0;
    padding: 8px 10px;
    border: 1px solid #e8c7a7;
    border-radius: 8px;
    background: #fdf6ec;
    color: #8a5a1f;
    font-size: 12px;
  }
  .retry-tag {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--accent);
    border-radius: 999px;
    font-size: 11px;
    color: var(--accent);
  }
  .src-list {
    margin-top: 8px;
    max-height: 38vh;
    overflow: auto;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 8px;
    background: var(--card, #fff);
  }
  .src-item {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border, #e3e3e6);
    font-size: 13px;
    cursor: pointer;
  }
  .src-item:last-child {
    border-bottom: 0;
  }
  .src-item.off {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .src-item input {
    width: 18px;
    height: 18px;
    flex: 0 0 auto;
  }
  .src-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .src-type {
    flex: 0 0 auto;
    font-size: 11px;
    padding: 1px 6px;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 10px;
    color: var(--muted, #666);
  }
  .src-off {
    flex: 0 0 auto;
    font-size: 11px;
    color: var(--muted, #666);
  }
  :global(html[data-vp='sm']) .src-item {
    min-height: 44px;
  }
</style>
