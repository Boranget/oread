<script>
  import { onMount } from 'svelte';
  import { api } from '../lib/api.js';

  let settings = { use_replace_rule: true, image_style: 0 };
  let error = '';
  let saved = false;

  onMount(load);

  async function load() {
    error = '';
    try {
      settings = await api.settings();
    } catch (e) {
      error = e.message;
    }
  }
  async function save() {
    error = '';
    saved = false;
    try {
      await api.saveSettings(settings);
      saved = true;
      setTimeout(() => (saved = false), 1500);
    } catch (e) {
      error = e.message;
    }
  }
</script>

<h2>阅读设置</h2>
{#if error}<p class="err">{error}</p>{/if}

<div class="card" style="max-width:480px">
  <label class="row" style="margin-bottom:14px">
    <input type="checkbox" style="width:auto" bind:checked={settings.use_replace_rule} />
    <span>启用替换规则（净化正文）</span>
  </label>

  <div class="muted" style="font-size:12px;margin-bottom:4px">图片样式</div>
  <select bind:value={settings.image_style} style="max-width:240px">
    <option value={0}>原始（不处理）</option>
    <option value={1}>宽度自适应</option>
    <option value={2}>居中显示</option>
    <option value={3}>隐藏图片</option>
  </select>

  <div class="row" style="margin-top:18px">
    <button on:click={save}>保存设置</button>
    {#if saved}<span class="muted">已保存</span>{/if}
  </div>
</div>

<p class="muted" style="margin-top:16px;font-size:12px">
  提示：本服务端版本不移植本地 TXT/EPUB 导入、WebDav 备份、主题与「关于/开源协议」等仅适用于原 Android 客户端的模块。
</p>
