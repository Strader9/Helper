import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// Vite 配置
// tailwindcss 插件只负责编译 src/styles/system.css 里的 @theme/@utility，
// 旧页面的 App.css / cyberpunk.css 仍按普通 CSS 处理，互不影响。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "src"),
    },
  },
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
      input: {
        main: path.resolve(__dirname, "index.html"),
        splashscreen: path.resolve(__dirname, "splashscreen.html"),
      },
      output: {
        // 手动分包，优化缓存命中率
        manualChunks: {
          react: ["react", "react-dom"],
          tauri: ["@tauri-apps/api", "@tauri-apps/plugin-dialog"],
          router: ["react-router-dom"],
          state: ["zustand"],
          ui: ["lucide-react", "motion", "sonner"],
        },
      },
    },
  },
});
