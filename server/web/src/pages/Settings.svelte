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
    cache_limit_mb: 50,
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
  let accountName = String(claims.username || '');
  let accountRole = String(claims.role || getRole());
  let accountId = Number(claims.sub) || 0;
  const isAdmin = accountRole === 'admin';

  let pwOld = '';
  let pwNew = '';
  let pwConfirm = '';
  let pwBusy = false;
  let pwErr = '';
  let pwNotice = '';

  let users = [];
  let usersLoading = false;
  let usersErr = '';
  let userBusy = 0;
  $: enabledAdminCount = users.filter((u) => u.role === 'admin' && !u.disabled).length;

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

  async function loadUsers() {
    if (!isAdmin) return;
    usersLoading = true;
    usersErr = '';
    try {
      const res = await api.adminUsers();
      users = (res && res.users) || [];
    } catch (e) {
      usersErr = describeFailure(e, '读取用户列表');
    } finally {
      usersLoading = false;
    }
  }

  async function toggleUser(u) {
    userBusy = u.id;
    usersErr = '';
    try {
      await api.adminSetUserStatus(u.id, !u.disabled);
      await loadUsers();
    } catch (e) {
      usersErr = describeFailure(e, u.disabled ? '启用账号' : '停用账号');
    } finally {
      userBusy = 0;
    }
  }

  // 后端时间均为 UTC datetime('now') 字符串，换成本机时区展示；空值表示从未网页登录
  function fmtServerTime(s) {
    if (!s) return '从未登录';
    const d = new Date(String(s).replace(' ', 'T') + 'Z');
    if (Number.isNaN(d.getTime())) return String(s);
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
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

  // 整本缓存 / 清理缓存
  let shelfBooks = [];
  let shelfLoading = false;
  let cacheBook = '';
  let cacheBusy = false;
  let cacheErr = '';
  let cacheNotice = '';
  let cacheRes = null;
  let clearRes = null;
  let clearArmed = false;
  let clearTimer = null;
  // 全库清理：作用域为当前账号全部正文缓存，与按书清理一样两步确认
  let clearAllRes = null;
  let clearAllArmed = false;
  let clearAllTimer = null;

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
    clearRes = null;
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

  async function clearBookCache() {
    if (!cacheBook) return;
    // 两步确认，不依赖原生 confirm：墨水屏和移动端都不弹系统对话框
    if (!clearArmed) {
      clearArmed = true;
      clearTimeout(clearTimer);
      clearTimer = setTimeout(() => (clearArmed = false), 4000);
      return;
    }
    clearArmed = false;
    clearTimeout(clearTimer);
    cacheBusy = true;
    cacheErr = '';
    cacheNotice = '';
    cacheRes = null;
    clearRes = null;
    try {
      clearRes = await api.cacheClear(cacheBook);
      cacheNotice = `已清理 ${clearRes.deleted} 章缓存`;
    } catch (e) {
      cacheErr = describeFailure(e, '清理缓存');
    } finally {
      cacheBusy = false;
    }
  }

  async function clearAllCache() {
    // 两步确认，和按书清理同一套交互；4 秒内不点第二次则自动复位
    if (!clearAllArmed) {
      clearAllArmed = true;
      clearTimeout(clearAllTimer);
      clearAllTimer = setTimeout(() => (clearAllArmed = false), 4000);
      return;
    }
    clearAllArmed = false;
    clearTimeout(clearAllTimer);
    cacheBusy = true;
    cacheErr = '';
    cacheNotice = '';
    cacheRes = null;
    clearRes = null;
    clearAllRes = null;
    try {
      clearAllRes = await api.cacheClear('');
      cacheNotice = `已清理全部缓存：${clearAllRes.deleted} 章`;
    } catch (e) {
      cacheErr = describeFailure(e, '清理全部缓存');
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

  // 只读健康指标
  let health = null;
  let healthErr = '';
  let healthLoading = false;

  // 主题：四项与全局主题共用同一份账号偏好，点击即生效并持久化
  function pickTheme(t) {
    if (!THEMES.includes(t)) return;
    settings = { ...settings, theme: t };
    setTheme(t);
  }

  async function loadHealth() {
    healthLoading = true;
    healthErr = '';
    try {
      health = await api.healthz();
    } catch (e) {
      healthErr = e.message;
    } finally {
      healthLoading = false;
    }
  }

  onMount(async () => {
    error = '';
    loadHealth();
    loadShelfBooks();
    if (isAdmin) loadUsers();
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
        cache_limit_mb: Number(settings.cache_limit_mb),
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

<div class="row" style="margin-bottom:12px">
  <h2 style="margin:0">阅读设置</h2>
  <div class="spacer"></div>
  <span class="muted" style="font-size:12px">按账号保存</span>
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

  {#if isAdmin}
    <div class="card">
      <div class="row" style="margin-bottom:10px">
        <h3 style="margin:0">用户管理</h3>
        <div class="spacer"></div>
        <button class="ghost" on:click={loadUsers} disabled={usersLoading}>
          {usersLoading ? '读取中…' : '刷新'}
        </button>
      </div>
      {#if usersErr}<p class="err">{usersErr}</p>{/if}
      {#if users.length}
        <div class="user-table">
          <div class="user-row user-head">
            <span>用户名</span><span>角色</span><span>状态</span><span>最后登录</span><span></span>
          </div>
          {#each users as u (u.id)}
            <div class="user-row">
              <span class="user-name">
                {u.username}{#if u.id === accountId}<span class="muted">（当前）</span>{/if}
              </span>
              <span>{roleText(u.role)}</span>
              <span class:user-off={u.disabled}>{u.disabled ? '已停用' : '启用中'}</span>
              <span class="muted">{fmtServerTime(u.last_login_at)}</span>
              <span>
                <button
                  class="ghost"
                  on:click={() => toggleUser(u)}
                  disabled={userBusy === u.id || u.id === accountId || (u.role === 'admin' && !u.disabled && enabledAdminCount <= 1)}
                >
                  {userBusy === u.id ? '处理中…' : u.disabled ? '启用' : '停用'}
                </button>
              </span>
            </div>
          {/each}
        </div>
        <div class="muted field-hint">停用会立即让该账号所有已登录设备失效；重新启用后仍需重新登录。不能停用当前账号或最后一个启用中的管理员。</div>
      {:else if !usersLoading}
        <p class="muted field-hint">暂无用户。</p>
      {/if}
    </div>
  {/if}

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
    <h3>缓存与刷新</h3>
    <div class="field">
      <div class="field-label">正文缓存上限（MB）</div>
      <input type="number" min="10" max="1000" step="10" bind:value={settings.cache_limit_mb} />
      <div class="muted field-hint">由服务端按账号执行 LRU 淘汰；实际上限取本值与服务端部署上限的较小值。</div>
    </div>

    <label class="switch-row">
      <span>
        <b>自动刷新</b>
        <span class="muted">仅保存偏好；后端目前只存库，暂无自动刷新消费方</span>
      </span>
      <input type="checkbox" bind:checked={settings.auto_refresh} />
    </label>
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
        <button class="ghost" on:click={clearBookCache} disabled={cacheBusy || !cacheBook}>
          {clearArmed ? '再次点击确认清理' : '清理本书缓存'}
        </button>
      </div>
      <p class="muted field-hint">缓存整本按服务端预算分批抓取，可能需要几十秒，请保持页面打开。目录必须先缓存：在阅读器打开这本书即可。</p>
    {:else if !shelfLoading}
      <p class="muted field-hint">书架为空。先在书架加入书籍，再回来缓存整本。</p>
    {/if}
    <div class="cache-actions cache-global">
      <button class="ghost" on:click={clearAllCache} disabled={cacheBusy}>
        {clearAllArmed ? '再次点击确认清理全部' : '清理全部缓存（当前账号）'}
      </button>
      <span class="muted field-hint" style="margin:0">不影响书籍、书架和阅读进度，只删除已缓存的正文。</span>
    </div>
    {#if cacheErr}<p class="err">{cacheErr}</p>{/if}
    {#if cacheNotice}<p class="ok-line">{cacheNotice}</p>{/if}
    {#if cacheRes}
      <div class="cache-result">
        <p>
          <b>本次结果：</b>成功 {cacheRes.cached} 章 · 失败 {cacheRes.failed} 章 · 预算跳过 {cacheRes.skipped_budget} 章
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
        {#if cacheRes.skipped_budget > 0}
          <p class="cache-warn">缓存额度不足，跳过 {cacheRes.skipped_budget} 章；可调大正文缓存上限或先清理缓存后重试。</p>
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
    {#if clearRes}
      <p class="muted">已清理 {clearRes.deleted} 章，释放 {fmtBytes(clearRes.bytes_freed)}。</p>
    {/if}
    {#if clearAllRes}
      <p class="muted">
        全部清理完成（作用域 {clearAllRes.scope === 'user' ? '当前账号' : '单本'}）：删除 {clearAllRes.deleted} 章，释放 {fmtBytes(clearAllRes.bytes_freed)}。
      </p>
    {/if}
  </div>

  <div class="card">
    <div class="row" style="margin-bottom:10px">
      <h3 style="margin:0">服务状态</h3>
      <div class="spacer"></div>
      <button class="ghost" on:click={loadHealth} disabled={healthLoading}>{healthLoading ? '读取中…' : '刷新'}</button>
    </div>
    {#if healthErr}<p class="err">{healthErr}</p>{/if}
    {#if health}
      <div class="health-grid">
        <div><span class="muted">进程内存 RSS</span><b>{health.rss_mb == null ? '当前平台不可用' : health.rss_mb + ' MB'}</b></div>
        <div><span class="muted">打开的用户库</span><b>{health.open_user_dbs ?? 0}</b></div>
        <div><span class="muted">正文缓存</span><b>{health.cache ? `${health.cache.mb} MB · ${health.cache.rows} 行` : '未返回'}</b></div>
        <div><span class="muted">缓存上限</span><b>{health.cache_limit_mb ?? '?'} MB</b></div>
        <div><span class="muted">HTTP 并发 / 超时</span><b>{health.http?.concurrency ?? '?'} / {health.http?.timeout_secs ?? '?'}s</b></div>
        <div><span class="muted">请求体上限</span><b>{health.http?.max_body_mb ?? '?'} MB</b></div>
        <div><span class="muted">JS 规则</span><b>{health.js?.enabled ? '开启' : '关闭'} · {health.js?.last_ms ?? 0} / {health.js?.max_ms ?? 0} ms</b></div>
      </div>
      <p class="muted field-hint">只读指标，来自 /api/healthz；不含书籍与账号内容。</p>
    {:else if healthLoading}
      <p class="loading">读取状态…</p>
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
  .settings-wrap { max-width: none; display: grid; gap: 12px; }
  .settings-wrap h3 { margin-bottom: 12px; }
  .account-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 12px; }
  .account-head b { font-size: 15px; }
  .user-table { border: 1px solid var(--border, #d8d8d8); border-radius: 8px; overflow-x: auto; }
  .user-row {
    display: grid;
    grid-template-columns: minmax(110px, 1.2fr) 70px 70px minmax(130px, 1fr) 96px;
    gap: 8px;
    align-items: center;
    padding: 8px 10px;
    border-top: 1px solid var(--border, #d8d8d8);
    font-size: 13px;
  }
  .user-row:first-child { border-top: 0; }
  .user-head { color: var(--muted); font-size: 12px; background: var(--soft-bg, #fafafa); }
  .user-name { font-weight: 600; word-break: break-all; }
  .user-off { color: var(--muted); }
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
  .health-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; }
  .health-grid > div { display: flex; flex-direction: column; gap: 2px; }
  .health-grid span { font-size: 12px; }
  .health-grid b { font-size: 15px; font-variant-numeric: tabular-nums; }
  .sticky-actions { position: sticky; bottom: 0; background: var(--bg); padding: 10px 0 calc(10px + env(safe-area-inset-bottom)); }
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
  .cache-global { align-items: center; border-top: 1px solid var(--border, #d8d8d8); padding-top: 10px; }
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
