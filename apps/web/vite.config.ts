import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';
import { PORTAL_DEEP_LINKS } from './src/portal/paths';

/// 开发服务把客户端的无扩展名深链回退到 `portal.html`：生产那条由 API 的静态兜底做
/// （`apps/api/src/main.rs` 的 `serve_from`），本机没有那条兜底，直接打开 `/usage` 会落到 Vite 的 404。
/// 两条要落回同一组地址（`docs/design/0014` §5）。
function portalDevDeepLinks(): Plugin {
  return {
    name: 'portal-dev-deep-links',
    configureServer(server) {
      server.middlewares.use((request, _response, next) => {
        const path = (request.url ?? '').split('?')[0];
        if (PORTAL_DEEP_LINKS.has(path)) request.url = '/portal.html';
        next();
      });
    },
  };
}

/// 开发期把 `/api` 与 `/v1` 代理到本机 API 进程：浏览器因此不需要跨域，也不需要给 API 开 CORS。
/// 生产由 API 按主机名分发两份产物（见 README 的部署一节）。
///
/// **两个入口、一次构建**：`console.html` 是运营后台，`portal.html` 是客户控制台。它们各引各的
/// 入口模块，因此客户那一份产物里不会出现管理端的代码——这是 Spec D4 要的性质。共享的取数与展示
/// 骨架在 `src/shared/`，由两个入口各自引用；会话的存放各写各的。
export default defineConfig({
  plugins: [react(), portalDevDeepLinks()],
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
