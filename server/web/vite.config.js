import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// 构建产物输出到 ../ 下的 dist（即 server/web/dist），由 Rust 的 ServeDir 托管。
// base 用相对路径，便于部署到任意子路径。
export default defineConfig({
  plugins: [svelte()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // 开发时若前端与 Rust 不同端口，API 走相对路径即可（同源由 Rust 托管 dist）
    port: 5173,
  },
});
