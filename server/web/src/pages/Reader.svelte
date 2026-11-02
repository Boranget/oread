<script>
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from '../lib/api.js';

  export let book; // { bookUrl, sourceId, name }
  const dispatch = createEventDispatcher();

  let info = null;
  let chapters = [];
  let volumes = {}; // index -> volume title
  let current = null; // BookContent
  let currentIndex = -1;
  let error = '';
  let loadingToc = false;
  let loadingContent = false;
  let showToc = false;

  onMount(async () => {
    error = '';
    try {
      // 加入书架并读取历史进度
      await api.addToShelf({
        book_id: book.bookUrl,
        source_id: book.sourceId,
        name: book.name,
      });
      const [b, toc, prog] = await Promise.all([
        api.bookInfo(book.sourceId, book.bookUrl),
        api.toc(book.sourceId, book.bookUrl),
        api.progress(book.bookUrl).catch(() => ({ chapter_index: 0 })),
      ]);
      info = b;
      chapters = toc;
      // 记录分卷标题，便于阅读器分组展示
      let vol = '';
      for (const c of chapters) {
        if (c.is_volume) vol = c.title || '';
        volumes[c.index] = vol;
      }
      const start = Math.max(0, (prog.chapter_index || 0));
      if (chapters.length) await openChapter(chapters[Math.min(start, chapters.length - 1)]);
    } catch (e) {
      error = e.message;
    }
  });

  async function openChapter(c) {
    if (c.is_volume) return;
    currentIndex = c.index;
    loadingContent = true;
    current = null;
    try {
      current = await api.content(book.sourceId, c.url);
      await api.setProgress({ book_id: book.bookUrl, chapter_index: c.index, position: 0 });
    } catch (e) {
      error = e.message;
    } finally {
      loadingContent = false;
      showToc = false;
      document.querySelector('.reader')?.scrollTo?.(0, 0);
    }
  }

  function paragraphs() {
    if (!current || !current.content) return [];
    return current.content.split('\n').map((s) => s.trim()).filter(Boolean);
  }
</script>

<div class="nav">
  <div class="tab" on:click={() => dispatch('close')}>‹ 返回</div>
  <div class="spacer"></div>
  <div class="right"><button class="ghost" on:click={() => (showToc = !showToc)}>目录</button></div>
</div>

<div class="container">
  {#if error}<p class="err">{error}</p>{/if}

  {#if info}
    <div class="card" style="margin-bottom:12px">
      <div class="row" style="align-items:flex-start">
        {#if info.cover_url}
          <img src={info.cover_url} alt="cover" referrerpolicy="no-referrer" style="width:84px;height:112px;object-fit:cover;border-radius:8px"
               on:error={(e) => (e.target.style.visibility = 'hidden')} />
        {/if}
        <div style="flex:1">
          <h2 style="margin:0 0 4px">{info.name || book.name}</h2>
          <p class="muted" style="margin:0">{info.author || '未知作者'}</p>
          {#if info.kind}<p class="muted" style="margin:4px 0">{info.kind}</p>{/if}
          {#if info.intro}<p style="margin:8px 0 0;font-size:13px;color:#555;max-height:120px;overflow:auto">{info.intro}</p>{/if}
        </div>
      </div>
    </div>
  {/if}

  {#if loadingToc}<p class="loading">加载目录…</p>{/if}

  {#if showToc}
    <div class="card">
      <h3>目录（共 {chapters.length} 章）</h3>
      <div style="max-height:60vh;overflow:auto">
        {#each chapters as c}
          <div
            class="chapter-item"
            class:active={c.index === currentIndex}
            class:vol={c.is_volume}
            style={c.is_volume ? 'font-weight:600;color:#555' : ''}
            on:click={() => openChapter(c)}
          >{c.title || '(无标题)'}</div>
        {/each}
      </div>
    </div>
  {:else}
    <div class="reader" style="padding-left:0;padding-right:0">
      {#if loadingContent}<p class="loading">加载正文…</p>{/if}
      {#if current}
        <h3>{current.title}</h3>
        {#each paragraphs() as p}<p>{p}</p>{/each}
        {#if !loadingContent}
          <div class="row" style="margin-top:24px">
            <button class="ghost" on:click={() => currentIndex > 0 && openChapter(chapters[currentIndex - 1])}>上一章</button>
            <div class="spacer"></div>
            <button class="ghost" on:click={() => currentIndex < chapters.length - 1 && openChapter(chapters[currentIndex + 1])}>下一章</button>
          </div>
        {/if}
      {/if}
    </div>
  {/if}
</div>
