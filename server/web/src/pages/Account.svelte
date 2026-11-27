<script>
  import { onMount } from 'svelte';
  import { api, describeFailure, describeSourceError, getRole, getToken, parseJwt, clearToken } from '../lib/api.js';
  import { THEMES, THEME_LABELS, setTheme } from '../lib/theme.js';
  import {
    FONT_OPTIONS,
    PAGE_WIDTH_PX,
    WIDTH_OPTIONS,
    fontIdFromServer,
    fontServerValue,
    syncLayoutSettings,
    widthIdFromPx,
    widthPxFromId,
  } from '../lib/readingPrefs.js';

  // 字段与后端 /api/settings（user_settings 键值表）保持一致
  const DEFAULTS = {
    use_replace_rule: true,
    image_style: 0,
    theme: 'light',
    font_size: 18,
    line_height: 1.75,
    auto_refresh: true,
    read_mode: 'scroll',
    page_width: PAGE_WIDTH_PX.normal,
    font_family: 'serif',
    builtin_clean: false,
  };

  let settings = { ...DEFAULTS };
  let error = '';
  let notice = '';
  let loading = true;
  let saving = false;

  // 账户：用户名/角色来自本地 JWT claims；服务端权限判定始终以签名校验结果为准
  const claims = parseJwt(getToken());
  const accountName = String(claims.username || '');
  const accountRole = String(claims.role || getRole());

  let pwOld = '';
  let pwNew = '';
  let pwConfirm = '';
  let pwBusy = false;
  let pwErr = '';
  let pwNotice = '';

  function roleText(r) {
    return r === 'admin' ? '管理员' : '普通用户';
  }

  // 后端按 Unicode 字符数校验 6–128，这里用 [...s].length 保持一致
  function passwordProblem() {
    if (!pwOld) return '请输入当前密码';
    const n = [...String(pwNew || '')].length;
    if (n < 6 || n > 128) return `新密码需为 6–128 个字符（当前 ${n} 个）`;
    if (pwNew !== pwConfirm) return '两次输入的新密码不一致';
    return '';
  }

  async function changePassword() {
    pwErr = '';
    pwNotice = '';
    const problem = passwordProblem();
    if (problem) {
      pwErr = problem;
      return;
    }
    pwBusy = true;
    try {
      await api.changePassword(pwOld, pwNew);
      pwNotice = '密码已修改，旧密码与所有旧登录状态已失效；正在返回登录页…';
      pwOld = '';
      pwNew = '';
      pwConfirm = '';
      clearToken();
      setTimeout(() => location.reload(), 1500);
    } catch (e) {
      pwErr = describeFailure(e, '修改密码');
    } finally {
      pwBusy = false;
    }
  }

  // 页宽 / 字体族：服务端 user_settings 为准，本机 key 只做离线兜底
  let layoutBusy = '';
  // 只有用户主动点过字体选项才改写服务端值，避免把别的客户端写入的 kai/song 等枚举覆盖成 serif
  let fontTouched = false;
  $: widthId = widthIdFromPx(settings.page_width);
  $: fontId = fontIdFromServer(settings.font_family);

  async function saveLayoutField(patch, name) {
    layoutBusy = name;
    error = '';
    try {
      await api.saveSettings(patch);
      notice = `已同步${name}到账号`;
      setTimeout(() => (notice = ''), 1800);
    } catch (e) {
      error = describeFailure(e, `保存${name}`);
    } finally {
      layoutBusy = '';
    }
  }

  function pickWidth(id) {
    const px = widthPxFromId(id);
    if (!WIDTH_OPTIONS.some((w) => w.id === id)) return;
    settings = { ...settings, page_width: px };
    // 本机立即生效，同时保留离线兜底
    localStorage.setItem('oread_read_width', id);
    saveLayoutField({ page_width: px }, '页宽');
  }

  function pickFont(id) {
    if (!FONT_OPTIONS.some((f) => f.id === id)) return;
    fontTouched = true;
    settings = { ...settings, font_family: fontServerValue(id) };
    localStorage.setItem('oread_font_family', id);
    saveLayoutField({ font_family: fontServerValue(id) }, '字体族');
  }

  // 整本缓存：这里只做预取；全局占用、清理与回收统一在管理页
  let shelfBooks = [];
  let shelfLoading = false;
  let cacheBook = '';
  let cacheBusy = false;
  let cacheErr = '';
  let cacheNotice = '';
  let cacheRes = null;

  function fmtBytes(n) {
    const v = Number(n) || 0;
    if (v >= 1024 * 1024) return (v / (1024 * 1024)).toFixed(1) + ' MB';
    if (v >= 1024) return (v / 1024).toFixed(1) + ' KB';
    return v + ' B';
  }

  async function loadShelfBooks() {
    shelfLoading = true;
    try {
      shelfBooks = (await api.bookshelf()) || [];
      if (!cacheBook && shelfBooks.length) cacheBook = shelfBooks[0].book_id;
    } catch (e) {
      cacheErr = describeFailure(e, '读取书架');
    } finally {
      shelfLoading = false;
    }
  }

  async function cacheWholeBook() {
    if (!cacheBook) return;
    cacheBusy = true;
    cacheErr = '';
    cacheNotice = '';
    cacheRes = null;
    try {
      // 书架里的 source_id 直接带上，避免书未记录 origin 时后端报缺少 source
      const src = (shelfBooks.find((b) => b.book_id === cacheBook) || {}).source_id || '';
      cacheRes = await api.cacheAll(cacheBook, 200, src);
      cacheNotice =
        Number(cacheRes.attempted) === 0
          ? '这本书的章节已全部缓存，无需再抓取'
          : `缓存完成：成功 ${cacheRes.cached} 章`;
    } catch (e) {
      cacheErr = /目录未缓存/.test(String(e && e.message))
        ? '缓存整本失败：这本书的目录还没缓存。请先在阅读器打开这本书，加载目录后再试。'
        : describeFailure(e, '缓存整本');
    } finally {
      cacheBusy = false;
    }
  }

  // 后端白名单：read_mode 仅这三个值，其它值会被 400 拒绝
  const READ_MODES = [
    { id: 'scroll', label: '上下滚动' },
    { id: 'paged', label: '左右翻页' },
    { id: 'fade', label: '仿真翻页（淡入 + 简化 3D）' },
  ];

  // 主题：四项与全局主题共用同一份账号偏好，点击即生效并持久化
  function pickTheme(t) {
    if (!THEMES.includes(t)) return;
    settings = { ...settings, theme: t };
    setTheme(t);
  }

  onMount(async () => {
    error = '';
    loadShelfBooks();
    try {
      const s = await api.settings();
      const merged = { ...DEFAULTS, ...(s || {}) };
      // 非法枚举回退默认值，避免把脏值再提交给后端
      if (!READ_MODES.some((m) => m.id === merged.read_mode)) merged.read_mode = DEFAULTS.read_mode;
      if (!THEMES.includes(merged.theme)) merged.theme = DEFAULTS.theme;
      // 服务端优先；首次读取时把旧的本机页宽/字体迁移到账号（服务端仍为默认值才写）
      const layout = await syncLayoutSettings(s || {});
      merged.page_width = widthPxFromId(layout.width);
      // 服务端默认 serif 且迁移把本机值写上去时用迁移结果；否则保留原枚举（如 kai/song）
      const rawFont = String((s && s.font_family) || 'serif').trim();
      const migratedFont = fontServerValue(layout.fontFamily);
      merged.font_family =
        rawFont === 'serif' && migratedFont !== 'serif' ? migratedFont : rawFont || 'serif';
      settings = merged;
    } catch (e) {
      error = describeFailure(e, '读取设置');
    } finally {
      loading = false;
    }
  });

  async function save() {
    error = '';
    notice = '';
    saving = true;
    try {
      await api.saveSettings({
        use_replace_rule: !!settings.use_replace_rule,
        image_style: Number(settings.image_style) || 0,
        theme: settings.theme,
        font_size: Number(settings.font_size),
        line_height: Number(settings.line_height),
        auto_refresh: !!settings.auto_refresh,
        read_mode: settings.read_mode,
        page_width: Number(settings.page_width) || DEFAULTS.page_width,
        font_family: fontTouched ? fontServerValue(fontId) : settings.font_family || 'serif',
        builtin_clean: !!settings.builtin_clean,
      });
      notice = '已保存';
      setTimeout(() => (notice = ''), 1800);
    } catch (e) {
      // 400 时 api.js 会把后端 {"error":"..."} 原样透出，按提示修改即可
      error = describeFailure(e, '保存设置');
    } finally {
      saving = false;
    }
  }

  function resetDefaults() {
    settings = { ...DEFAULTS };
    fontTouched = true;
    localStorage.setItem('oread_read_width', 'normal');
    localStorage.setItem('oread_font_family', 'serif');
    notice = '已恢复表单默认值，点击「保存设置」生效';
  }
</script>

<div class="page-head">
  <h2>我的</h2>
  <span class="page-count">设置按账号保存</span>
</div>

{#if error}<p class="err">{error}</p>{/if}
{#if notice}<p class="ok-line">{notice}</p>{/if}
{#if loading}<p class="loading">读取设置…</p>{/if}

<div class="settings-wrap">
  <div class="card">
    <h3>账户</h3>
    <div class="account-head">
      <span class="muted">当前账号</span>
      <b>{accountName || '未记录'}</b>
      <span class="badge">{roleText(accountRole)}</span>
    </div>

    <div class="warn-box">
      <div class="warn-title">默认密码请立即修改</div>
      <div class="warn-item">首次部署的 admin 账号可能仍在使用部署默认密码。修改成功后旧密码立即失效，所有已登录设备都需要用新密码重新登录。</div>
    </div>

    <div class="field">
      <div class="field-label">修改密码</div>
      <input
        type="password"
        placeholder="当前密码"
        autocomplete="current-password"
        bind:value={pwOld}
      />
      <div style="height:8px"></div>
      <input
        type="password"
        placeholder="新密码（6–128 个字符）"
        autocomplete="new-password"
        bind:value={pwNew}
      />
      <div style="height:8px"></div>
      <input
        type="password"
        placeholder="再次输入新密码"
        autocomplete="new-password"
        bind:value={pwConfirm}
      />
      <div class="muted field-hint">密码按字符数校验 6–128；提交前会本地检查长度与两次输入是否一致。</div>
      {#if pwErr}<p class="err">{pwErr}</p>{/if}
      {#if pwNotice}<p class="ok-line">{pwNotice}</p>{/if}
      <div class="cache-actions">
        <button class="primary" on:click={changePassword} disabled={pwBusy}>
          {pwBusy ? '提交中…' : '修改密码'}
        </button>
      </div>
    </div>
  </div>

  <div class="card">
    <h3>正文净化</h3>
    <label class="switch-row">
      <span>
        <b>启用替换规则</b>
        <span class="muted">阅读正文时应用「替换规则」页里启用的规则</span>
      </span>
      <input type="checkbox" bind:checked={settings.use_replace_rule} />
    </label>

    <label class="switch-row">
      <span>
        <b>启用内置净化规则</b>
        <span class="muted">在替换规则之外，额外清理常见广告与推广行（默认关闭）</span>
      </span>
      <input type="checkbox" bind:checked={settings.builtin_clean} />
    </label>

    <div class="field">
      <div class="field-label">图片样式</div>
      <select bind:value={settings.image_style}>
        <option value={0}>原始（不处理）</option>
        <option value={1}>宽度自适应</option>
        <option value={2}>居中显示</option>
        <option value={3}>隐藏图片</option>
      </select>
    </div>
  </div>

  <div class="card">
    <h3>阅读排版</h3>
    <div class="field">
      <div class="field-label">
        字号
        <span class="val">{settings.font_size}px</span>
      </div>
      <input type="range" min="12" max="30" step="1" bind:value={settings.font_size} />
      <div class="range-scale"><span>12</span><span>30</span></div>
    </div>

    <div class="field">
      <div class="field-label">
        行距
        <span class="val">{Number(settings.line_height).toFixed(1)}</span>
      </div>
      <input type="range" min="1.2" max="2.6" step="0.1" bind:value={settings.line_height} />
      <div class="range-scale"><span>紧凑 1.2</span><span>宽松 2.6</span></div>
    </div>

    <div class="field">
      <div class="field-label">
        页宽
        {#if layoutBusy === '页宽'}<span class="val">同步中…</span>{/if}
      </div>
      <div class="seg" style="margin:0">
        {#each WIDTH_OPTIONS as w}
          <button class:on={widthId === w.id} on:click={() => pickWidth(w.id)}>{w.label}</button>
        {/each}
      </div>
      <div class="muted field-hint">保存到账号，换设备登录后同步；本机保留一份用于离线兜底。满屏按后端上限 2000px 保存。</div>
    </div>

    <div class="field">
      <div class="field-label">
        字体族
        {#if layoutBusy === '字体族'}<span class="val">同步中…</span>{/if}
      </div>
      <div class="seg" style="margin:0">
        {#each FONT_OPTIONS as f}
          <button class:on={fontId === f.id} on:click={() => pickFont(f.id)}>{f.label}</button>
        {/each}
      </div>
      <div class="muted field-hint">保存到账号，换设备登录后同步；本机保留一份用于离线兜底。等宽（mono）已由后端支持。</div>
    </div>

    <div class="field">
      <div class="field-label">配色主题</div>
      <div class="seg" style="margin:0">
        {#each THEMES as t}
          <button class:on={settings.theme === t} on:click={() => pickTheme(t)}>{THEME_LABELS[t]}</button>
        {/each}
      </div>
    </div>

    <div class="field">
      <div class="field-label">阅读模式</div>
      <select bind:value={settings.read_mode}>
        {#each READ_MODES as m}
          <option value={m.id}>{m.label}</option>
        {/each}
      </select>
      <div class="muted field-hint">阅读器内切换模式也会同步保存到这里；翻页模式按屏幕高度分页并保留阅读位置。</div>
    </div>
  </div>

  <div class="card">
    <div class="row" style="margin-bottom:10px">
      <h3 style="margin:0">书籍缓存</h3>
      <div class="spacer"></div>
      <button class="ghost" on:click={loadShelfBooks} disabled={shelfLoading}>{shelfLoading ? '读取中…' : '刷新书目'}</button>
    </div>
    {#if shelfBooks.length}
      <div class="field">
        <div class="field-label">选择书籍</div>
        <select bind:value={cacheBook}>
          {#each shelfBooks as b}
            <option value={b.book_id}>{b.title || b.book_id}</option>
          {/each}
        </select>
      </div>
      <div class="cache-actions">
        <button class="primary" on:click={cacheWholeBook} disabled={cacheBusy || !cacheBook}>
          {cacheBusy ? '处理中…' : '缓存整本'}
        </button>
      </div>
      <p class="muted field-hint">缓存整本按服务端预算分批抓取，可能需要几十秒，请保持页面打开。目录必须先缓存：在阅读器打开这本书即可。</p>
      <p class="muted field-hint">缓存占用、清理与过期回收属于全局维护，已移到「管理 → 馆藏维护 → 临时缓存」。</p>
    {:else if !shelfLoading}
      <p class="muted field-hint">书架为空。先在图书馆把书加入书架，再回来缓存整本。</p>
    {/if}
    {#if cacheErr}<p class="err">{cacheErr}</p>{/if}
    {#if cacheNotice}<p class="ok-line">{cacheNotice}</p>{/if}
    {#if cacheRes}
      <div class="cache-result">
        <p>
          <b>本次结果：</b>成功 {cacheRes.cached} 章 · 失败 {cacheRes.failed} 章
        </p>
        <p class="muted">
          已缓存 {cacheRes.already_cached + cacheRes.cached} / {cacheRes.total_chapters} 章 ·
          本次处理 {cacheRes.attempted} 章 · 耗时 {cacheRes.elapsed_ms} ms · 当前占用 {fmtBytes(cacheRes.used_bytes_after)}
        </p>
        {#if cacheRes.limited}
          <p class="cache-warn">
            本次只缓存了前 {cacheRes.limit} 章，还有未缓存章节；可再次点击「缓存整本」继续。
          </p>
        {/if}
        {#if cacheRes.failed > 0}
          <p class="muted">失败章节（最多列 5 条）：</p>
          <ul class="cache-fails">
            {#each (cacheRes.failures || []).slice(0, 5) as f}
              <li>第 {Number(f.chapter_index) + 1} 章{f.chapter_title ? ` ${f.chapter_title}` : ''}：{describeSourceError(f.error)}</li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}
  </div>

  <div class="card">
    <h3>Kindle / 老设备入口</h3>
    <p class="muted field-hint" style="margin-top:0">为电子墨水屏和低性能浏览器准备的极简页面：零 JS、需登录、服务端渲染，按页翻章。</p>
    <a class="k-link" href="/k" rel="noopener">打开 /k 入口</a>
  </div>

  <div class="row sticky-actions">
    <button class="ghost" on:click={resetDefaults} disabled={saving}>恢复默认</button>
    <div class="spacer"></div>
    <button on:click={save} disabled={saving}>{saving ? '保存中…' : '保存设置'}</button>
  </div>
</div>

<style>
  .settings-wrap h3 { margin-bottom: 12px; }
  .account-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 12px; }
  .account-head b { font-size: 15px; }
  .field { margin-bottom: 16px; }
  .field:last-child { margin-bottom: 0; }
  .field-label { display: flex; align-items: center; justify-content: space-between; font-size: 13px; color: var(--muted); margin-bottom: 6px; }
  .field-label .val { color: var(--fg); font-variant-numeric: tabular-nums; font-weight: 600; }
  .field input[type='range'] { width: 100%; accent-color: var(--accent); padding: 0; }
  .range-scale { display: flex; justify-content: space-between; font-size: 11px; color: var(--muted); margin-top: 2px; }
  .field-hint { font-size: 11px; margin-top: 6px; line-height: 1.5; }
  .switch-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 0; margin-bottom: 10px; }
  .switch-row span { display: flex; flex-direction: column; gap: 3px; font-size: 14px; }
  .switch-row span .muted { font-size: 12px; }
  .switch-row input { width: 20px; min-width: 20px; height: 20px; }
  .k-link {
    display: inline-block;
    padding: 8px 14px;
    border: 1px solid currentColor;
    border-radius: 8px;
    color: var(--text);
    text-decoration: none;
    font-size: 14px;
  }
  .k-link:hover { background: var(--card); }

  .cache-actions { display: flex; gap: 8px; flex-wrap: wrap; margin: 10px 0 4px; }
  .cache-result {
    border: 1px solid var(--border, #d8d8d8);
    padding: 10px 12px;
    margin-top: 10px;
    font-size: 13px;
    line-height: 1.7;
  }
  .cache-result p { margin: 0 0 6px; }
  .cache-warn { font-weight: 600; }
  .cache-fails { margin: 6px 0 0; padding-left: 18px; font-size: 12px; line-height: 1.6; }
</style>
