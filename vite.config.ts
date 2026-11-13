import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// https://vite.dev/config/
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    strictPort: true,
    watch: {
      // 不监视 Rust 构建产物，避免 EBUSY 崩溃
      ignored: ["**/src-tauri/target/**", "**/src-tauri/binaries/**"],
    },
  },
  build: {
    target: "chrome110",
    outDir: "dist",
  },
});
