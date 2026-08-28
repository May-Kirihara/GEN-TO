import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  // .svelte内のTypeScript (lang="ts") を処理するために必須。
  preprocess: vitePreprocess(),
};
