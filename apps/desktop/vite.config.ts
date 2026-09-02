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
  // vitest実行時のみ: svelteをブラウザ版で解決し、コンポーネントを
  // クライアント (mount可能) としてコンパイルする (RestoreDialog.test.ts
  // の描画テストで必要。dev/buildの解決には影響しない)。
  ...(process.env.VITEST ? { resolve: { conditions: ["browser"] } } : {}),
  test: {
    include: ["src/**/*.test.ts"],
  },
});
