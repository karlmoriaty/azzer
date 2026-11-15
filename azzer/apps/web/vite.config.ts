import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import fs from "node:fs";
import path from "node:path";

/**
 * 开发期代理：从运行期文件读端口与 token，由 Vite 代填 Authorization。
 *
 * 这样前端代码里完全不出现 token，也不需要在 .env 里手工同步。
 * 生产（Tauri）走 initialization script 注入 window.__AZZER__，
 * 那时 proxy 不生效，走真实 URL。
 */
function readRuntime(): { port: number; token: string } | null {
  const candidates = [
    process.env.AZZER_RUNTIME_FILE,
    // 工作目录内的运行期文件（与 serve.rs 的推导保持一致）
    path.resolve(import.meta.dirname, "../../../.build/runtime.json"),
  ].filter(Boolean) as string[];

  for (const p of candidates) {
    try {
      const raw = fs.readFileSync(p, "utf8");
      const j = JSON.parse(raw);
      if (typeof j.port === "number" && typeof j.token === "string") {
        return { port: j.port, token: j.token };
      }
    } catch {
      // 服务未启动时静默跳过，页面会显示"未连接"
    }
  }
  return null;
}

const runtime = readRuntime();

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "src") },
  },
  server: {
    port: 5173,
    ...(runtime
      ? {
          proxy: {
            "/v1": {
              target: `http://127.0.0.1:${runtime.port}`,
              changeOrigin: false,
              headers: { Authorization: `Bearer ${runtime.token}` },
            },
            "/health": {
              target: `http://127.0.0.1:${runtime.port}`,
              changeOrigin: false,
            },
          },
        }
      : {}),
  },
  build: {
    target: "chrome110",
    sourcemap: true,
  },
});
