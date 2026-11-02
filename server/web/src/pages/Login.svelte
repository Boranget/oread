<script>
  import { createEventDispatcher } from 'svelte';
  import { api, setSession, parseJwt } from '../lib/api.js';

  const dispatch = createEventDispatcher();
  let username = '';
  let password = '';
  let error = '';
  let mode = 'login'; // login | register

  async function submit() {
    error = '';
    if (!username || !password) {
      error = '请输入用户名和密码';
      return;
    }
    try {
      const path = mode === 'login' ? api.login : api.register;
      const { token } = await path(username, password);
      const claims = parseJwt(token);
      setSession(token, claims.role || 'user');
      dispatch('loggedin', claims.role || 'user');
    } catch (e) {
      error = (mode === 'login' ? '登录失败：' : '注册失败：') + e.message;
    }
  }
</script>

<div class="container">
  <div class="card" style="max-width:380px;margin:8vh auto 0">
    <h2>{mode === 'login' ? '登录 Legado Web' : '注册新账号'}</h2>
    <p class="muted">普通用户与管理员共用入口；书源仅管理员可管理。</p>
    <input placeholder="用户名" bind:value={username} />
    <div style="height:8px"></div>
    <input
      type="password"
      placeholder="密码"
      bind:value={password}
      on:keydown={(e) => e.key === 'Enter' && submit()}
    />
    <div style="height:12px"></div>
    <button style="width:100%" on:click={submit}>{mode === 'login' ? '登录' : '注册并登录'}</button>
    {#if error}<p class="err">{error}</p>{/if}
    <div style="height:12px"></div>
    <p class="muted" style="text-align:center">
      {#if mode === 'login'}
        还没有账号？<a on:click={() => (mode = 'register')}>去注册</a>
      {:else}
        已有账号？<a on:click={() => (mode = 'login')}>去登录</a>
      {/if}
    </p>
  </div>
</div>
