import { defineConfig } from "vite";
import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Tauri 开发约定：固定 1420 端口、不清屏
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // 不监听 Rust 构建产物（.exe 被 cargo 占用会导致 chokidar EBUSY 崩溃）。
    // build-cache 是 .cargo/config.toml 里 target-dir 的固定位置，同样必须排除；
    // artifacts 下是 agent-host / 打包中间产物，也不必参与热更新监听。
    watch: {
      ignored: ["**/src-tauri/**", "**/build-cache/**", "**/artifacts/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    // WF-1 P0-2：把体积大、且只在「渲染消息/代码块/公式/图表」时才用到的
    // 第三方库拆成独立 vendor chunk。它们是动态 import 触发的，主包不背这些体积；
    // 拆开后首屏、切页与二次进入都能命中浏览器缓存。
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          // WP3-B：preload-helper 必须独立成块。否则它会被并入某个大 vendor chunk
          // （曾把 vendor-mermaid 一起拖成入口 static import，导致首屏 modulepreload
          // 预取 5.26MB 的 mermaid）。单独成块后，入口只静态依赖这个极小 helper。
          if (id.includes("vite/preload-helper")) return "vendor-preload";
          if (!id.includes("node_modules")) return undefined;
          if (id.includes("mermaid") || id.includes("cytoscape") || id.includes("dagre") || id.includes("khroma")) {
            return "vendor-mermaid";
          }
          if (id.includes("shiki") || id.includes("@shikijs")) return "vendor-shiki";
          if (id.includes("katex")) return "vendor-katex";
          if (
            id.includes("react-markdown") ||
            id.includes("remark-") ||
            id.includes("rehype-") ||
            id.includes("mdast") ||
            id.includes("hast") ||
            id.includes("unified") ||
            id.includes("micromark") ||
            id.includes("vfile")
          ) {
            return "vendor-markdown";
          }
          // WP3-B：React 运行时独立成块，走长缓存（升级应用代码时不失效）。
          if (
            id.includes("node_modules/react/") ||
            id.includes("node_modules/react-dom/") ||
            id.includes("node_modules/scheduler/") ||
            id.includes("node_modules/use-sync-external-store/")
          ) {
            return "vendor-react";
          }
          return undefined;
        },
      },
    },
  },
});
