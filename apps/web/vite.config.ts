import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';

/// 开发期把 `/api` 与 `/v1` 代理到本机 API 进程：浏览器因此不需要跨域，也不需要给 API 开 CORS。
/// 生产由 API 按主机名分发两份产物（见 README 的部署一节）。
///
/// **两个入口、一次构建**：`console.html` 是运营后台，`portal.html` 是客户控制台。它们各引各的
/// 入口模块，因此客户那一份产物里不会出现管理端的代码——这是 Spec D4 要的性质。共享的取数与展示
/// 骨架在 `src/shared/`，由两个入口各自引用；会话的存放各写各的。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': { target: 'http://127.0.0.1:8081', changeOrigin: false },
      '/v1': { target: 'http://127.0.0.1:8081', changeOrigin: false },
      '/health': { target: 'http://127.0.0.1:8081', changeOrigin: false },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: true,
    rollupOptions: {
      input: {
        console: resolve(__dirname, 'console.html'),
        portal: resolve(__dirname, 'portal.html'),
      },
    },
  },
});
