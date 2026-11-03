import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vitest/config";

// vitest 独立配置（与 vite.config.ts 分离，避免测试环境读 Tauri 专用配置）
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["./src/test/setup.ts"],
    // 路由改为 React.lazy 后，App 集成测试要动态加载页面 chunk；并行跑全量时 CPU 争用会让
    // 个别重测试（如 App.test「跳过引导后手动入口」）超过默认 5s 整体超时。放大整体/钩子超时，
    // 断言失败仍会失败，只是给懒加载留足时间。
    testTimeout: 20000,
    hookTimeout: 20000,
  },
});