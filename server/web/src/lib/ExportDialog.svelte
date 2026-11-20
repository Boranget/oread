<script>
  // 导出弹窗：TXT / EPUB（已缓存正文）或 CBZ（已落盘漫画页）。
  // 先拉对应的 export/info 展示缓存状态，用户确认后再 fetch 下载。
  // 不能直接用 <a download> 直链——那样带不上 Authorization: Bearer。
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, describeFailure } from './api.js';

  export let bookUrl = '';
  export let name = '';

  const dispatch = createEventDispatcher();

  let format = 'txt'; // txt | epub | cbz
  let info = null;
  let epubInfo = null;
  let cbzInfo = null;
  let loading = false;
  let infoErr = '';
  let exporting = false;
  let err = '';
  let done = null;

  const ILLEGAL = /[/\\:*?"<>|]/g;

  // 文件名用「书名.txt / 书名.epub / 书名.cbz」；后端 sanitize 规则在本地再兜一层
  function downloadName() {
    const current = format === 'cbz' ? cbzInfo : format === 'epub' ? epubInfo : info;
    const ext = format === 'cbz' ? '.cbz' : format === 'epub' ? '.epub' : '.txt';
    const raw = (current && current.name) || name || 'book';
    const cleaned = raw
      .replace(/[\u0000-\u001f\u007f]/g, '_')
      .replace(ILLEGAL, '_')
      .trim()
      .replace(/\.+$/, '')
      .trim()
      .slice(0, 60);
    return (cleaned || 'book') + ext;
  }

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v < 1024) return v + ' B';
    if (v < 1024 * 1024) return (v / 1024).toFixed(1) + ' KB';
    return (v / 1048576).toFixed(2) + ' MB';
  }

  async function loadInfo() {
    if (!bookUrl) {
      infoErr = '缺少书籍地址，请从书架重新打开这本书';
      return;
    }
    loading = true;
    infoErr = '';
    try {
      info = await api.exportInfo(bookUrl);
    } catch (e) {
      if (e && e.status === 409 && /目录未缓存|book\/toc/.test(String(e.message || ''))) {
        infoErr = '这本书还没有缓存目录，暂时无法导出。请先在阅读器打开这本书（拉取目录）后再试。';
      } else {
        infoErr = describeFailure(e, '读取导出信息');
      }
    } finally {
      loading = false;
    }
  }

  async function loadCbzInfo() {
    if (!bookUrl) {
      infoErr = '缺少书籍地址，请从书架重新打开这本书';
      return;
    }
    loading = true;
    infoErr = '';
    try {
      cbzInfo = await api.exportCbzInfo(bookUrl);
    } catch (e) {
      if (e && e.status === 409 && /目录未缓存|book\/toc/.test(String(e.message || ''))) {
        infoErr = '这本书还没有缓存目录，暂时无法导出。请先在阅读器打开这本书（拉取目录）后再试。';
      } else {
        infoErr = describeFailure(e, '读取漫画导出信息');
      }
    } finally {
      loading = false;
    }
  }

  async function loadEpubInfo() {
    if (!bookUrl) {
      infoErr = '缺少书籍地址，请从书架重新打开这本书';
      return;
    }
    loading = true;
    infoErr = '';
    try {
      epubInfo = await api.exportEpubInfo(bookUrl);
    } catch (e) {
      if (e && e.status === 409 && /目录未缓存|book\/toc/.test(String(e.message || ''))) {
        infoErr = '这本书还没有缓存目录，暂时无法导出。请先在阅读器打开这本书（拉取目录）后再试。';
      } else {
        infoErr = describeFailure(e, '读取 EPUB 导出信息');
      }
    } finally {
      loading = false;
    }
  }

  function setFormat(f) {
    if (exporting || format === f) return;
    format = f;
    err = '';
    done = null;
    if (f === 'cbz' && !cbzInfo) loadCbzInfo();
    if (f === 'epub' && !epubInfo) loadEpubInfo();
    if (f === 'txt' && !info) loadInfo();
  }

  function refresh() {
    if (format === 'cbz') loadCbzInfo();
    else if (format === 'epub') loadEpubInfo();
    else loadInfo();
  }

  // 401 / 404 / 网络中断分开说明，避免一句“导出失败”让人无从下手
  function describeExportError(e) {
    const status = e && e.status;
    const msg = String((e && e.message) || '');
    if (status === 401) return '导出失败：登录已失效，请重新登录后再导出。';
    if (status === 404) {
      return '导出失败：后端找不到这本书（HTTP 404）。请回书架确认书还在，或重新打开书籍目录后再试。';
    }
    if (status === 0) {
      if (/超时/.test(msg)) {
        return '导出失败：请求超时。整本导出耗时可能过长，请稍后重试；也可先减少缓存章节。';
      }
      return '导出失败：网络中断，导出请求没有完成。请检查网络后重试。';
    }
    if (status === 400 || status === 422 || status === 409) {
      if (/目录未缓存|book\/toc/.test(msg)) {
        return '导出失败：这本书还没有缓存目录。请先在阅读器打开目录（或重新打开这本书）后再导出。';
      }
      if (/没有图片章节|没有已落盘/.test(msg)) {
        return '导出失败：' + msg + '。请先在阅读器缓存漫画页，或改用 TXT / EPUB。';
      }
      if (/没有可导出的正文章节/.test(msg)) {
        return '导出失败：' + msg + '。纯漫画书请改用 CBZ；文字书请先在阅读器打开章节。';
      }
      return '导出失败：' + msg + '。请按提示处理后重试。';
    }
    if (status >= 500) {
      return '导出失败：服务端出错（HTTP ' + status + '）。请稍后重试。';
    }
    return describeFailure(e, '导出');
  }

  async function doExport() {
    if (exporting) return;
    exporting = true;
    err = '';
    done = null;
    let objectUrl = '';
    try {
      const r =
        format === 'cbz'
          ? await api.exportCbz(bookUrl)
          : format === 'epub'
            ? await api.exportEpub(bookUrl)
            : await api.exportTxt(bookUrl);
      const filename = downloadName();
      objectUrl = URL.createObjectURL(r.blob);
      const a = document.createElement('a');
      a.href = objectUrl;
      a.download = filename;
      a.rel = 'noopener';
      a.style.display = 'none';
      document.body.appendChild(a);
      a.click();
      a.remove();
      // 等浏览器接管下载后再释放，避免部分浏览器取消保存
      const urlToRevoke = objectUrl;
      setTimeout(() => URL.revokeObjectURL(urlToRevoke), 1000);
      objectUrl = '';
      done = {
        filename,
        bytes: r.bytes,
        blobBytes: r.blob.size,
        headers: r.headers,
        mismatch: !!(r.headers.fileBytes && r.blob.size !== r.headers.fileBytes),
      };
    } catch (e) {
      err = describeExportError(e);
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    } finally {
      exporting = false;
    }
  }

  onMount(loadInfo);
</script>

<div class="modal-mask" on:click|self={() => dispatch('close')}>
  <div class="modal export-modal" role="dialog" aria-label="导出书籍">
    <div class="row" style="margin-bottom:8px">
      <strong style="font-size:15px">导出</strong>
      <div class="spacer"></div>
      <button class="ghost" on:click={() => dispatch('close')}>关闭</button>
    </div>

    <div class="seg" role="tablist">
      <button class:on={format === 'txt'} on:click={() => setFormat('txt')}>TXT 正文</button>
      <button class:on={format === 'epub'} on:click={() => setFormat('epub')}>EPUB 电子书</button>
      <button class:on={format === 'cbz'} on:click={() => setFormat('cbz')}>CBZ 漫画</button>
    </div>

    {#if loading}
      <p class="loading">正在读取缓存状态…</p>
    {:else if infoErr}
      <p class="err">{infoErr}</p>
      <div class="row">
        <button on:click={refresh}>重试</button>
      </div>
    {:else if format === 'txt' && info}
      <p class="exp-name">《{info.name}》{info.author && info.author !== '未知' ? ' · ' + info.author : ''}</p>
      {#if info.source}<p class="muted tiny-note">书源：{info.source}</p>{/if}

      <p class="exp-line">
        已缓存 <b>{info.cached_chapters}</b>/{info.total_chapters} 章，缺 {info.missing_chapters} 章
        · 已缓存正文 {fmtBytes(info.cached_bytes)}
      </p>

      {#if info.complete}
        <p class="ok-line">全部章节已缓存，导出文件就是全本。</p>
      {:else}
        <div class="warn-box">
          <div class="warn-title">缺 {info.missing_chapters} 章，导出的不是全本</div>
          <div>
            缺失章节在 TXT 里会写成【未缓存】占位并附上章节地址，不会静默丢章。
            想导出全本，请先在阅读器打开这些章节，或执行「缓存全部」后再来导出。
          </div>
        </div>
      {/if}
    {:else if format === 'epub' && epubInfo}
      <p class="exp-name">《{epubInfo.name}》{epubInfo.author && epubInfo.author !== '未知' ? ' · ' + epubInfo.author : ''}</p>
      {#if epubInfo.source}<p class="muted tiny-note">书源：{epubInfo.source}</p>{/if}

      <p class="exp-line">
        正文 <b>{epubInfo.text_chapters}</b> 章 · 已缓存 <b>{epubInfo.cached_chapters}</b>/
        {epubInfo.text_chapters} 章，缺 {epubInfo.missing_chapters} 章
        · 正文 {fmtBytes(epubInfo.cached_bytes)}
      </p>

      {#if epubInfo.text_chapters === 0}
        <div class="warn-box">
          <div class="warn-title">这本书没有正文章节</div>
          <div>EPUB 只适用于文字书；漫画书请切换到「CBZ 漫画」。</div>
        </div>
      {:else if epubInfo.complete}
        <p class="ok-line">全部正文章节已缓存，EPUB 含完整内容。</p>
      {:else}
        <div class="warn-box">
          <div class="warn-title">缺 {epubInfo.missing_chapters} 章，EPUB 不完整</div>
          <div>
            缺失章节在 EPUB 里会写成占位段落，不会静默丢章。
            想导出全本，请先在阅读器打开这些章节，或执行「缓存全部」后再来导出。
          </div>
        </div>
      {/if}
    {:else if format === 'cbz' && cbzInfo}
      <p class="exp-name">《{cbzInfo.name}》{cbzInfo.author && cbzInfo.author !== '未知' ? ' · ' + cbzInfo.author : ''}</p>
      {#if cbzInfo.source}<p class="muted tiny-note">书源：{cbzInfo.source}</p>{/if}

      <p class="exp-line">
        已落盘 <b>{cbzInfo.available_pages}</b>/{cbzInfo.total_pages} 页
        · {cbzInfo.image_chapters} 章 · 图片 {fmtBytes(cbzInfo.available_bytes)}
      </p>

      {#if cbzInfo.total_pages === 0}
        <div class="warn-box">
          <div class="warn-title">这本书没有图片章节</div>
          <div>CBZ 只适用于漫画/图片书源；文字书请切换到「TXT 正文」。</div>
        </div>
      {:else if cbzInfo.complete}
        <p class="ok-line">全部漫画页已落盘，CBZ 含完整页集。</p>
      {:else}
        <div class="warn-box">
          <div class="warn-title">缺 {cbzInfo.missing_pages} 页，CBZ 不完整</div>
          <div>
            CBZ 只打包 library_images 里已有字节的页，不生成占位图。
            想导出全本，请先在阅读器缓存漫画页，或把书加入馆藏后等待后台下载完成。
          </div>
        </div>
      {/if}
    {/if}

    {#if exporting}
      <div class="exp-progress">
        <span class="exp-spin" aria-hidden="true"></span>
        <span>正在导出…（{format === 'cbz' ? '逐页打包' : '逐章写盘'}，内容较多时需要一会儿）</span>
      </div>
    {/if}

    {#if err}<p class="err">{err}</p>{/if}

    {#if done}
      <p class="ok-line">已开始下载《{done.filename}》，{fmtBytes(done.bytes)}（{done.bytes} 字节）</p>
      <p class="muted tiny-note">
        字节对账：响应头 x-file-bytes {done.headers.fileBytes} · 实际收包 {done.blobBytes}
        {#if done.mismatch}
          （不一致，可能被代理截断，请核对文件完整性）
        {:else}
          （一致）
        {/if}
      </p>
      {#if format === 'cbz'}
        {#if done.headers.writtenPages !== undefined && done.headers.writtenPages !== done.headers.availablePages}
          <p class="muted tiny-note">
            写入 {done.headers.writtenPages} 页 / 已落盘 {done.headers.availablePages} 页（导出期间缓存有并发变动）
          </p>
        {/if}
      {:else if done.headers.written !== undefined && done.headers.written !== done.headers.cached}
        <p class="muted tiny-note">
          写入 {done.headers.written} 章 / 已缓存 {done.headers.cached} 章（导出期间缓存有并发变动）
        </p>
      {/if}
    {/if}

    {#if format === 'txt' && info}
      <div class="row" style="margin-top:12px">
        <button on:click={doExport} disabled={exporting} style="flex:1">
          {exporting ? '正在导出…' : done ? '再次导出' : '导出 TXT'}
        </button>
        <button class="ghost" on:click={loadInfo} disabled={exporting}>刷新缓存状态</button>
      </div>
      <p class="muted tiny-note" style="margin-top:10px">
        导出只读取已缓存正文，不会临时抓取网络；文件按「书名.txt」保存。
      </p>
    {:else if format === 'epub' && epubInfo}
      <div class="row" style="margin-top:12px">
        <button
          on:click={doExport}
          disabled={exporting || epubInfo.text_chapters === 0}
          style="flex:1"
        >
          {exporting ? '正在导出…' : done ? '再次导出' : '导出 EPUB'}
        </button>
        <button class="ghost" on:click={loadEpubInfo} disabled={exporting}>刷新缓存状态</button>
      </div>
      <p class="muted tiny-note" style="margin-top:10px">
        正文转成 XHTML，附目录与元数据（EPUB 3）；文件按「书名.epub」保存。
      </p>
    {:else if format === 'cbz' && cbzInfo}
      <div class="row" style="margin-top:12px">
        <button
          on:click={doExport}
          disabled={exporting || cbzInfo.available_pages === 0}
          style="flex:1"
        >
          {exporting ? '正在导出…' : done ? '再次导出' : '导出 CBZ'}
        </button>
        <button class="ghost" on:click={loadCbzInfo} disabled={exporting}>刷新缓存状态</button>
      </div>
      <p class="muted tiny-note" style="margin-top:10px">
        漫画页原样打包（不重编码、不压缩），附 ComicInfo.xml；文件按「书名.cbz」保存。
      </p>
    {/if}
  </div>
</div>

<style>
  .export-modal {
    max-width: 520px;
  }
  .exp-name {
    margin: 4px 0 10px;
    font-size: 15px;
    font-weight: 600;
    word-break: break-word;
  }
  .exp-line {
    margin: 6px 0;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }
  .exp-progress {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 10px 0;
    font-size: 13px;
  }
  .exp-spin {
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    border: 2px solid var(--border, #e3e3e6);
    border-top-color: var(--accent, #b3392f);
    border-radius: 50%;
    animation: expSpin 0.8s linear infinite;
  }
  @keyframes expSpin {
    to {
      transform: rotate(360deg);
    }
  }
  /* 墨水屏主题：纯黑白（只改配色，不动布局） */
  :global(html[data-theme='eink']) .ok-line,
  :global(html[data-theme='eink']) .err {
    color: #000 !important;
  }
</style>
