<script>
  import { onMount } from 'svelte';
  import { api, describeFailure, getRole, getToken, parseJwt, clearToken } from '../lib/api.js';
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

  // 字段与后端 /api/settings（user_settings 键值表）保持一致。
  // 正文净化与全局缓存维护不在个人设置里：前者属于内容生产流程，后者属于管理端运维。
  const DEFAULTS = {
    theme: 'light',
    font_size: 18,
    line_height: 1.75,
    auto_refresh: true,
    read_mode: 'scroll',
    page_width: PAGE_WIDTH_PX.normal,
    font_family: 'serif',
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
        theme: settings.theme,
        font_size: Number(settings.font_size),
        line_height: Number(settings.line_height),
        auto_refresh: !!settings.auto_refresh,
        read_mode: settings.read_mode,
        page_width: Number(settings.page_width) || DEFAULTS.page_width,
        font_family: fontTouched ? fontServerValue(fontId) : settings.font_family || 'serif',
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

<div class="me-page">
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
      <div class="form-actions">
        <button class="primary" on:click={changePassword} disabled={pwBusy}>
          {pwBusy ? '提交中…' : '修改密码'}
        </button>
      </div>
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

  <div class="row sticky-actions">
    <button class="ghost" on:click={resetDefaults} disabled={saving}>恢复默认</button>
    <div class="spacer"></div>
    <button on:click={save} disabled={saving}>{saving ? '保存中…' : '保存设置'}</button>
  </div>
  </div>
</div>

<style>
  .me-page { max-width: 960px; margin: 0 auto; }
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
  .form-actions { display: flex; gap: 8px; flex-wrap: wrap; margin: 10px 0 4px; }
</style>
