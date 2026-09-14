import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vite 配置
// 极致精简：仅 React 插件，无多余依赖
export default defineConfig({
  plugins: [react()],
  // Tauri 推荐配置
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/target/**", "**/node_modules/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
    // 产出到 dist，Tauri 从这里加载前端资源
    outDir: "dist",
    rollupOptions: {
      output: {
        // 手动分包，优化缓存命中率
        manualChunks: {
          react: ["react", "react-dom"],
          tauri: ["@tauri-apps/api", "@tauri-apps/plugin-dialog"],
          router: ["react-router-dom"],
          state: ["zustand"],
        },
      },
    },
  },
});
