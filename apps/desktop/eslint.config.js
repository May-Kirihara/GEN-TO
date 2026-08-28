import { defineConfig, globalIgnores } from "eslint/config";
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import svelte from "eslint-plugin-svelte";
import globals from "globals";

export default defineConfig(
  globalIgnores(["dist/", "src-tauri/", ".tauri/"]),
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...svelte.configs["flat/recommended"],
  {
    languageOptions: {
      globals: { ...globals.browser },
    },
  },
  {
    files: ["**/*.svelte"],
    languageOptions: {
      parserOptions: {
        // .svelte内の lang="ts" をTypeScriptパーサーで解釈する。
        parser: tseslint.parser,
      },
    },
  },
);
