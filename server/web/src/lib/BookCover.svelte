<script>
  import { onDestroy } from 'svelte';
  import { api } from './api.js';

  // 馆藏 EPUB / CBZ 的封面是 oread-img:// 本地地址，浏览器不能直接当 <img src>。
  // 这里统一走带 Bearer 的图片代理，取回 Blob 后再用 objectURL 渲染；
  // 在线书源的 http(s) 封面保持直连，避免多一次中转。
  export let url = '';
  export let alt = '';
  export let imgClass = 'cover';
  export let lazy = true;

  let resolved = '';
  let failed = false;
  let objectUrl = '';
  let seq = 0;
  let alive = true;

  $: load(url);

  async function load(next) {
    const my = ++seq;
    failed = false;
    if (objectUrl) {
      URL.revokeObjectURL(objectUrl);
      objectUrl = '';
    }
    resolved = '';
    if (!next) return;
    if (!String(next).startsWith('oread-img://')) {
      resolved = next;
      return;
    }
    try {
      const res = await api.comicImage('', next);
      if (!alive || my !== seq) {
        URL.revokeObjectURL(res.objectUrl);
        return;
      }
      objectUrl = res.objectUrl;
      resolved = res.objectUrl;
    } catch {
      if (alive && my === seq) failed = true;
    }
  }

  onDestroy(() => {
    alive = false;
    if (objectUrl) URL.revokeObjectURL(objectUrl);
  });
</script>

{#if resolved && !failed}
  <img
    class={imgClass}
    src={resolved}
    {alt}
    loading={lazy ? 'lazy' : 'eager'}
    referrerpolicy="no-referrer"
    on:error={() => (failed = true)}
  />
{/if}
