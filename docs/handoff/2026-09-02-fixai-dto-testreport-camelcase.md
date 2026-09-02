# FIX-AI-DTO ハンドオフ — AIテスト結果がUIに出ないserde命名不整合の修正 (2026-09-02)

実装者: implementer subagent。基盤コミット: d76aba4。
**`gento_ai::TestReport` に `rename_all = "camelCase"` が無く、IPC応答が
`elapsed_ms` (snake_case) で返る一方、AiPanelが `report.elapsedMs.toLocaleString()`
を読むためレンダリング中にTypeErrorが発生し、4種テストの結果表示が
一切出ない不具合の修正** (WP-5と同一の不具合クラスの再犯)。

## 変更内容

- `crates/gento-ai/src/client.rs` `TestReport` へ `#[serde(rename_all = "camelCase")]`
  を追加。tauri command層 (`ai_profile_test_*` 4種) がこの型をDTO経由せず
  直接返すため、crate型でありながらIPC形状はfrontend型と一对一である必要が
  ある (WP-5「委任仕様系DTOはsnake_case / UI系DTOはcamelCase」棲み分けの
  うちUI系側の形状)。
- 直列化形状テスト `test_report_serializes_camel_case_matching_frontend`
  (client.rs testsモジュール、red→green実施): serde_json出力キーが
  `ok/detail/elapsedMs/status` であることを固定。
- `lib/ai.test.ts`: `runTest`がcamelCase応答を`AiTestReport`としてそのまま
  返す契約テストを追加 (コンポーネントテスト基盤が無いため最小のTS側pin)。

## 横断チェック (全 `#[tauri::command]` 返り値型)

**crate型を直接返すコマンドは2系統のみ**:

| コマンド | 型 | serde形状 | frontend読み | 判定 |
|---|---|---|---|---|
| `ai_profile_test_{connection,text,vision,structured}` | `gento_ai::TestReport` | ~~snake~~ → **camelCase修正** | `AiTestReport` (ai.ts) | **修正** |
| `health` | `gento_core::HealthInfo` | snake_case (意図的) | `HealthInfo` (health.ts, 全snake) | 一致 |

他はすべてtauri層DTOまたはプリミティブ。DTO↔frontend型はすべて一致
(委任仕様系snake: TagDto/CaptionDto/AssetDetailDto, fileops全系, collections全系,
library全系, thumbnails全系, CatalogSummary/CatalogPageDto外殻 — それぞれ
frontend型もsnake。UI系camel: CatalogItemDto, search系, WP-9 metadata系,
AiProfileDto/AiJobDto/AiRunDto — frontend型もcamel。`ai_jobs_list`の
`serde_json::Value`はcamelCase AiJobDtoを包む。`ProfileExtra` (gento-ai) は
snake_case付きで`AiProfileExtra`と一致)。`db_status`はfrontend呼び出しなし。
**不一致はTestReportのみ** (既存の直列化形状テスト: metadata.rs:1418,
collections.rs:1220, search.rs:487, catalog.rs:1525/1729, ai.rs:2081が
各DTOを既に固定済み)。

## テスト・検証

- cargo test --workspace: **691 passed** (基準690 +1) / 0 failed。
  vitest: **444 passed** (基準443 +1)。
- `cargo fmt --all -- --check` クリーン。
  `cargo clippy --workspace --all-targets` /
  `cargo clippy -p gento-tauri --features devtools --all-targets`: 0警告。
- `pnpm check` 0エラー (9警告は既存のSvelte5クロージャ警告で本変更無関係)。
  `pnpm test` / `pnpm lint` / `pnpm build` クリーン。
- `scripts/release-smoke.sh`: **SMOKE RESULT: PASS** (全ステップ)。

## 実機確認 (Xephyr実デスクトップ :1 + モックOpenAIサーバー)

release-smoke生成のreleaseバイナリ (修正込み) + pythonモック
(`/v1/models`, `/v1/chat/completions`, 1.2秒意図的遅延) + DBへ直接シードした
プロファイルで、xdotoolによりヘッダー「AI」→ プロファイル編集 → 4種テストを
実行。**全4結果行が緑で描画** (スクリーンショット `/tmp/opencode/fixai-dto/shot-17-big.png`):

```
接続: OK — 接続OK (models応答: {"data": [{"id": "mock-vision-model"}]}) (0ms)
テキスト: OK — 応答: OK (1,201ms)
Vision: OK — 応答: OK (1,200ms)
構造化JSON: OK — 構造化JSON検証OK: {"caption": "mock caption", "tags": ["sky", "night"]} (1,201ms)
```

`(1,20Xms)` のカンマ区切りは `elapsedMs.toLocaleString()` が実評価された
直接証拠 (修正前は `undefined.toLocaleString()` のTypeErrorで結果行が
1行も出ない)。モックの受信ログ (GET×2 / POST×4) でbackend実行も確認。

## 教訓 (再発防止)

- **crate型をtauri command返り値に直接使う場合、そのcrate型はIPC DTOとして
  の形状契約を持つ** — tauri層DTOの直列化形状テスト群の守備範囲外になる。
  新規コマンドでcrate型を直接返す場合は、そのcrateに形状テストを置くか
  tauri層DTOで包む。
