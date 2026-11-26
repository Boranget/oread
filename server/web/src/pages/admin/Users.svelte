<script>
  import { onMount } from 'svelte';
  import { api, describeFailure, getRole, getToken, parseJwt } from '../../lib/api.js';

  export let role = 'user';

  const claims = parseJwt(getToken());
  const accountId = Number(claims.sub) || 0;
  const isAdmin = (role || getRole()) === 'admin';

  let users = [];
  let loading = false;
  let error = '';
  let busy = 0;

  $: enabledAdminCount = users.filter((u) => u.role === 'admin' && !u.disabled).length;

  onMount(load);

  function roleText(r) {
    return r === 'admin' ? '管理员' : '普通用户';
  }

  async function load() {
    if (!isAdmin) return;
    loading = true;
    error = '';
    try {
      const res = await api.adminUsers();
      users = (res && res.users) || [];
    } catch (e) {
      error = describeFailure(e, '读取用户列表');
    } finally {
      loading = false;
    }
  }

  async function toggleUser(u) {
    busy = u.id;
    error = '';
    try {
      await api.adminSetUserStatus(u.id, !u.disabled);
      await load();
    } catch (e) {
      error = describeFailure(e, u.disabled ? '启用账号' : '停用账号');
    } finally {
      busy = 0;
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
</script>

<div class="page-head">
  <h2>用户</h2>
  <span class="page-count">{users.length} 个账号</span>
  <div class="page-actions">
    <button class="ghost" on:click={load} disabled={loading}>{loading ? '读取中…' : '刷新'}</button>
  </div>
</div>

{#if !isAdmin}
  <div class="card"><p class="err">只有管理员可以管理用户。</p></div>
{:else}
  {#if error}<p class="err">{error}</p>{/if}
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
          <span class="user-meta user-role" data-label="角色">{roleText(u.role)}</span>
          <span class="user-meta user-state" class:user-off={u.disabled} data-label="状态">{u.disabled ? '已停用' : '启用中'}</span>
          <span class="user-meta user-login muted" data-label="最后登录">{fmtServerTime(u.last_login_at)}</span>
          <span class="user-op">
            <button
              class="ghost"
              on:click={() => toggleUser(u)}
              disabled={busy === u.id || u.id === accountId || (u.role === 'admin' && !u.disabled && enabledAdminCount <= 1)}
            >
              {busy === u.id ? '处理中…' : u.disabled ? '启用' : '停用'}
            </button>
          </span>
        </div>
      {/each}
    </div>
    <div class="muted field-hint">停用会立即让该账号所有已登录设备失效；重新启用后仍需重新登录。不能停用当前账号或最后一个启用中的管理员。</div>
  {:else if !loading}
    <p class="muted field-hint">暂无用户。</p>
  {/if}
{/if}

<style>
  .field-hint { font-size: 11px; margin-top: 8px; line-height: 1.5; }
</style>
