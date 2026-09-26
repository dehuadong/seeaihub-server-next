import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

/// 开发期把 `/api` 与 `/v1` 代理到本机 API 进程：浏览器因此不需要跨域，也不需要给 API 开 CORS。
/// 生产是同一来源下的静态产物（见 README 的部署一节）。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': { target: 'http://127.0.0.1:8081', changeOrigin: false },
      '/health': { target: 'http://127.0.0.1:8081', changeOrigin: false },
    },
  },
  build: { outDir: 'dist', sourcemap: true },
});
