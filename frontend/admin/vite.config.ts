import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 开发期把 /api 与 /healthz 代理到本地导出服务器，前端无需处理 CORS
// 生产部署在 https://weimo.ink/douban-admin/ 子路径下（详见 ops/txy部署手册.md §4）
export default defineConfig({
  base: process.env.NODE_ENV === 'production' ? '/douban-admin/' : '/',
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': { target: 'http://127.0.0.1:8080', changeOrigin: true },
      '/healthz': { target: 'http://127.0.0.1:8080', changeOrigin: true },
    },
  },
});
