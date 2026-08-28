# 依存ライブラリメモ (Phase 1時点)

- 記録日: 2026-08-28
- 方針: バージョンはpin (npmは正確なバージョン指定、Cargoは `=` 付き + Cargo.lock)。追加・変更時に本メモへ記録する。

## Rust (workspace直依存)

| crate | pinバージョン | 用途 | ライセンス | 使用crate |
|---|---|---|---|---|
| serde | =1.0.229 (features: derive) | DTOのSerialize/Deserialize | MIT OR Apache-2.0 | gento-core, (tauri間接) |
| serde_json | =1.0.151 | gento-core単体テストのJSON形状検証 (通常依存には入れない) | MIT OR Apache-2.0 | gento-core (dev) |
| tauri | =2.11.5 | デスクトップシェル・IPC・WebView | Apache-2.0 OR MIT | gento-tauri |
| tauri-build | =2.6.3 | build.rsでのアセット/スキーマ生成 | Apache-2.0 OR MIT | gento-tauri (build) |
| tracing | =0.1.44 | 構造化ログ | MIT | gento-tauri |
| tracing-subscriber | =0.3.23 (features: env-filter) | ローグ出力実装 (`GENTO_LOG`環境変数対応) | MIT | gento-tauri |

## TypeScript / Svelte (apps/desktop)

| パッケージ | pinバージョン | 用途 | ライセンス |
|---|---|---|---|
| @tauri-apps/api | 2.11.1 | invoke等のIPCクライアント | Apache-2.0 OR MIT |
| svelte | 5.56.10 | UIフレームワーク (runes使用) | MIT |
| @tauri-apps/cli | 2.11.4 | `pnpm tauri` (dev/build) | Apache-2.0 OR MIT |
| vite | 8.2.2 | devサーバー・bundler | MIT |
| @sveltejs/vite-plugin-svelte | 7.3.0 | Vite向けSvelteコンパイル | MIT |
| typescript | 6.0.3 | 型検査。7系はsvelte-check (peer `^5||^6`) とtypescript-eslint (peer `<6.1.0`) が未対応のため6系最新をpin | Apache-2.0 |
| svelte-check | 4.7.6 | .svelte/.ts型検査 (`pnpm check`) | MIT |
| vitest | 4.1.11 | フロント単体テスト (`pnpm test`) | MIT |
| eslint | 10.9.1 | Lint (`pnpm lint`) | MIT |
| @eslint/js | 10.0.1 | eslint推奨ルールセット | MIT |
| eslint-plugin-svelte | 3.23.0 | .svelteファイルのLint | MIT |
| typescript-eslint | 8.68.0 | TS用eslintルール (非type-checkedのrecommendedのみ) | MIT |
| globals | 17.11.0 | ブラウザglobals定義 | MIT |
| esbuild (間接) | (vite依存) | postinstallビルド許可のためpnpm `onlyBuiltDependencies` に明記 | MIT |

## 選定理由

- **`pnpm tauri` + `@tauri-apps/cli` devDependency**: cargo-tauriサブコマンドをグローバル導入しない。プロジェクトローカルにバージョンをpinでき、`pnpm dlx`/`npx`のような解決時のバージョン漂動がない。
- **eslint最小構成**: js.recommended + typescript-eslint recommended (非type-checked) + eslint-plugin-svelte flat/recommended。type-checked構成はCI時間と設定複雑度に対しスパイクの利益が小さいため入れない。追加は必要になった時点で検討。
- **prettier不導入**: Svelte/TSは `svelte-check` + `eslint` で品質を担保し、フォーマットはRust側 `cargo fmt` と異なりチーム規約がまだないため。フォーマッタが必要になった時点で導入を検討する。
- **TypeScript 6.0.3**: 最新安定7系 (native port) はsvelte-check・typescript-eslintの両方が未対応。6系最新をpinし、対応状況が変わった時点で更新する。
- **Node 25 (非LTS) 注意**: 開発機のNodeはv25.8.2。vite 8・vitest 4は動作確認済みだが、問題発生時はLTS (22系) でも検証する (PM計画 §7-11)。
- **serde手動implでAssetIdを文字列化**: serde_with等の追加依存なしでJS安全整数問題 (u64 > 2^53-1) を回避する。
- **gento-coreのserde_jsonはdev-dependency**: コアはJSON実行時依存を持たず、シリアライズ形状のテストのみに使う。
- **gento-tauriのlib crate-typeはrlibのみ**: 公式テンプレートのstaticlib/cdylibはmobile対象。desktop専用スパイクではビルド時間増大だけが残るため除外した。

## 将来WPでの追加予定 (Phase 1時点では未追加)

- WP-2 (Phase 2): `walkdir` (走査)
- WP-3 (Phase 4): `image` (PNGのみ有効化), `png` (IHDRプローブ), `webp` (lossyエンコード), `twox-hash` (キャッシュキー), `dirs` (キャッシュディレクトリ解決)
- WP-5 (Phase 3): `@tanstack/svelte-virtual` (仮想スクロール。Svelte 5互換プローブの結果により `@tanstack/virtual-core` 直使用に切り替える可能性)
