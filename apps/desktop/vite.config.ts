import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  // tauri dev が期待するdevUrl (tauri.conf.json) と一致させる。
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // Rust側のビルドでフロントエンドを再読み込みさせない。
      ignored: ["**/src-tauri/**"],
    },
  },
  test: {
    include: ["src/**/*.test.ts"],
  },
});
