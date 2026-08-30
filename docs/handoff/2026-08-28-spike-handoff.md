# WP-7 スパイク実装 引き継ぎ (2026-08-28着手 / 2026-08-29完了)

> ファイル名の日付 (2026-08-28) はWP着手日。本文の記録は完了日 (2026-08-29)
> 時点のもの。FG-1 (最終ゲート修正) の引き継ぎは
> [2026-08-30-fg1-final-gate-fixes.md](./2026-08-30-fg1-final-gate-fixes.md) を参照。

## 引き継ぎメモ

WP-7 (フェーズ8「優先度、キャンセル、耐障害性」+ フェーズ9「計測と技術採否」) を完了した。
計画にあった全10シナリオをrelease buildで自動実施し、結果を `docs/technical-spike-results.md` にまとめた。
**暫定判断は CONDITIONAL PASS** (自動計測のみに基づく。最終判断はユーザー+PMの手動確認待ち)。
実装・コミットはPM検収待ち (実装者はコミットを作成していない — `git status` のWP-7差分が本体)。
計測の生ログ・スクリプトは検証機 `/tmp/opencode/wp7/` にある (一時領域のため再起動等で消える可能性あり。必要に応じてPMが保全すること)。

## ステータス

- **完了**: Part A (A1〜A5) の実装と実証、Part B (S1〜S10) の自動計測、Part C (結果文書・ハンドオフ・README)。
- GaOTTT: 本セッションでMCPツール未接続のため recall/remember とも不可だった (PM計画 §9と同一状態)。
- 計画書 §21 チェックリスト: PMが実施するため本WPでは触れていない。

## 変更内容 (意図ベース)

### Rust (最小変更)
1. `crates/gento-image/src/pool.rs`: `queue_depths_by_priority()` — 優先度別未開始ジョブ数。開発パネルの「キュー長・優先度別件数」表示 (WP-7 A1) に必要なため追加。
2. `crates/gento-image/src/thumbnail.rs`: `FailureInjection` / `InjectionMode` — 決定的エラー注入。`ThumbnailJob` に `injection` フィールドを追加し、decode注入はデコード前に、cache_write注入は一時ファイル作成後に失敗させる (Dropガードの後始末を実パスで通す)。
3. `apps/desktop/src-tauri/src/thumbnails.rs`: サービスが `FailureInjection` を保持して全ジョブへ共有。`set_failure_injection` command。`ThumbnailStatsDto` に `queue_depths` / `injection_mode` / `injection_rate` を追加。
4. `apps/desktop/src-tauri/src/lib.rs`: SIGTERM/SIGINT の graceful終了 (libc self-pipe + 監視スレッド → `app.exit(0)` → RunEvent::Exit → 既存のshutdown)。`dev_bridge_enabled` (環境変数 `GENTO_DEV_BRIDGE=1` でreleaseでも計測ブリッジ有効化) と `devtest_report` (機械可読ログ行) の2 command。
5. `apps/desktop/src-tauri/Cargo.toml`: `libc = "0.2"` 追加 (依存グラフ既存。依存メモに記載)。

### TypeScript / Svelte
6. `lib/grid/demand.ts`: `keepZoneRows()` (可視∪先読み∪overscanの行範囲) と `FailureSet.ids()/retract()`。
7. `lib/grid/grid-store.ts`: `reapOutsideKeepZone()` — デバウンス毎に画面外のpending要求を `cancel_thumbnail_requests` でキャンセル (進行方向先読みは維持)、画面外の失敗記録をリトラクト (再登場時に再試行)。`isFailed()`。統計に `cancelled`/`cancelBatches`。
8. `lib/grid/ThumbnailGrid.svelte`: 失敗セルの赤枠+✕表示 (A2)。`focus()`/`jumpTo(fraction)` のexport (計測用)。
9. `lib/perf.ts` (新規): 起動タイムマーク (mount→health→初回ページ) と longtask/ロングフレーム計測。`lib/perf.test.ts` 付き。
10. `lib/thumbnails.ts` / `thumbnail-types.ts`: `setInjection()`、DTO型拡張。
11. `App.svelte`: 起動マークの配線、パフォーマンスパネル、注入ボタン、優先度別キュー表示、ブリッジ引数の拡張。
12. `lib/devtest.ts`: G/T/P行 (グリッド・統計・パフォーマンス) とV/C行のログ送出 (`reportCompact`)、document.titleチャネル、新アクション (generateAll/clearCache/focusGrid/jump{Start,Middle,End}/generateAllPreviews/cycleInjection) とF8〜F12の修飾キー割り当て。`clickCell` をネイティブ `click()` ベースへ変更 (後述の教訓)。

## 変更理由 (設計判断の記録)

- **注入の閾値をassetIdでなくキャッシュキーハッシュにした**: ジョブはキャッシュキー (fingerprint+size_class) でdedupされ、1ジョブが複数の仮想assetIdに対応する。キャッシュキーは実ファイル+size+mtimeから決まるため決定性は同等で、実カタログではassetIdハッシュと同値になる。api-contract.md に明記。
- **SIGTERMハンドラを自作した**: Tauri 2 (GTK) は既定でSIGTERMを受けると即死しRunEvent::Exitが走らない。self-pipe + 監視スレッドで `AppHandle::exit(0)` へ繋ぐのが最小実装。signal-hook等の新依存は避けた。
- **キャンセルは「未解決IDは保守的に残す」**: ページ未取得でインデックスを解決できないIDをキャンセルすると誤爆の恐れがあるため、解決できたものだけキャンセルする。
- **失敗記録のリトラクト**: 「要求再試行は次のレンジ変化に任せる」のため、画面外へ出た失敗を記録から外し、再登場時の需要計画で自然に再試行させる。同一レンジ内の自動リトライは行わない。

## WP表 (本WPの成果物)

| 項目 | 成果物 | 状態 |
|---|---|---|
| A1 画面外キャンセル+優先度別統計 | grid-store.ts / pool.rs / パネル表示 | 実装+実証 (S3, a1-priority.log) |
| A2 失敗セルUI | ThumbnailGrid.svelte | 実装+実証 (a2_failed_cells.png) |
| A3 エラー注入 | FailureInjection + set_failure_injection | 実装+実証 (a3-inject.log) + 単体テスト |
| A4 終了安全 | SIGTERM graceful + 実証スクリプト | 実装+実証3回 (sigterm-r{1..3}.log) |
| A5 再起動キャッシュ再利用 | (WP-3実装の実証) | 実証3回 (S8: hr=1.000) |
| B フロント計測 | perf.ts + devtest拡張 | 実装 |
| B 10シナリオ計測 | /tmp/opencode/wp7/ (results.json, logs, shots) | 自動実施 (手動確認項目は結果文書§6) |
| C 結果文書 | docs/technical-spike-results.md | 作成 (暫定判断: CONDITIONAL PASS) |
| C ハンドオフ | 本書 | 作成 |
| C README | 測定コマンド追記 | 更新 |

## 触ったファイル

- `crates/gento-image/src/pool.rs` / `src/thumbnail.rs`
- `apps/desktop/src-tauri/Cargo.toml` / `src/lib.rs` / `src/thumbnails.rs`
- `apps/desktop/src/App.svelte`
- `apps/desktop/src/lib/perf.ts` (新規) / `perf.test.ts` (新規)
- `apps/desktop/src/lib/devtest.ts` / `thumbnails.ts` / `thumbnail-types.ts`
- `apps/desktop/src/lib/grid/demand.ts` / `grid-store.ts` / `ThumbnailGrid.svelte` (+ 各test.ts)
- `docs/technical-spike-results.md` (新規) / `docs/handoff/2026-08-28-spike-handoff.md` (本書) / `README.md` / `docs/dev/api-contract.md` / `docs/dev/dependency-notes.md`
- `Cargo.lock` (libc追加による)

## テスト

実行コマンドと結果:

- `cargo fmt --all -- --check` — **PASS**
- `cargo clippy --workspace --all-targets -- -D warnings` — **PASS** (警告0)
- `cargo test --workspace` — **PASS** 145件 (gento-core 26 / gento-image 69 / gento-tauri 50。新規: pool優先度別件数1, 注入6, サービス注入2 他)
- `cd apps/desktop && pnpm check` — **PASS** (エラー0。既存警告7件はWP-5由来で不変)
- `cd apps/desktop && pnpm test` — **PASS** 216件 (18ファイル。新規: perf 4, demand +6, grid-store +5)
- `cd apps/desktop && pnpm lint` — **PASS**
- `cd apps/desktop && pnpm build` — **PASS**
- release build `pnpm tauri build --no-bundle` — **PASS** (`target/release/gento-tauri`)

未実行 (と理由):

- プロトコルのネガティブテスト再実施 — WP-4で実施済みで本WPの変更が触れていないため (アプリ内スモークパネルは残置)。
- `sample/dataset` 以外でのE2E — 計画外。

実機実証 (コマンドと結果の概要):

- A4: `bash /tmp/opencode/wp7/a4-sigterm.sh` → 3/3 PASS (graceful, `.tmp` 0)
- A3: `bash /tmp/opencode/wp7/a3-inject.sh` → decode 28.4% / cache_write 27.8% / 復帰0%失敗、tmp 0、アプリ継続
- S1〜S10: `python3 /tmp/opencode/wp7/run_b.py` (+ `run-b2.py`) → `results.json` (結果文書参照)

## ドキュメント

- 更新: `docs/technical-spike-results.md` (新規), `README.md` (測定コマンド節), `docs/dev/api-contract.md` (§14 WP-7追加API), `docs/dev/dependency-notes.md` (libc), 本書。
- 未更新: `docs/implementation-plan.md` §21チェックリスト (PM担当のため), 既存docs本文。

## 手動確認 (ユーザー確認依頼)

結果文書 §6 の7項目 (スクロール体感 / 連続送りと空白 / ズーム・パン体感 / リサイズ / ウィンドウ閉じ終了 / 高DPI品質 / 低スペック再計測)。**これらは未実施のため、結果文書の判断は暫定のまま。**

## 既知の問題

1. S2/S10のスループットに2倍のばらつき (9s↔16s)。原因未特定 (システム状態の疑い)。
2. アイドルRSS約504MB (本体+WebView合計)。WebKitGTKベースラインが大半。要経過観察。
3. S9の末尾ジャンプが中央の約3倍 (1.03s)。実用上問題なし。
4. 開発パネルの統計表示は1秒ポーリングのため、interactive要求がキューに見えることはほぼない (優先度が高すぎて即実行される — 仕様どおりだが計測上の留意点)。
5. `GENTO_DEV_BRIDGE=1` を付けるとreleaseでも計測ブリッジが有効になる (既定無効。計測用の意図的な拡張)。

## 残TODO

- 結果文書 §8 の10項目 (次段階課題)。
- §21チェックリストのPMによる実施。
- WP-7差分のコミット (PM)。
- `/tmp/opencode/wp7/` の成果物保全判断 (PM)。

## リスク

- SIGTERMハンドラはlibc直接呼び出し (async-signal-safeなwriteのみ使用)。GTK側の後片付けはExitイベント経由のためTauri通常終了と同一パス。
- 注入はキャッシュキー閾値のため、同一実ファイルを参照する仮想ID群は全て同じ成否になる (仕様書き込み済み)。
- 画面外キャンセルは現在サイズクラスのみ対象。サイズクラス切替直後の旧クラスpendingは従来どおりready/failedイベントで解放される。

## ロールバックメモ

WP-7差分は `git checkout -- <files>` + 新規ファイル削除で戻せる (コミット前のため)。libc依存の除去は `apps/desktop/src-tauri/Cargo.toml` の該当行と `Cargo.lock` の更新で完了。機能単位のロールバックは不要見込み (全て追加型の変更で既存動作の互換性を維持)。

## 次の担当者メモ (PM向け)

1. 検収は `git diff` + 本書のテスト結果 + `/tmp/opencode/wp7/results.json` の突合で可能。
2. 計測再現手順は README「計測 (WP-7)」節と `/tmp/opencode/wp7/run_b.py` を参照。
3. **教訓 (計測自動化)**: WebKitGTK (Svelte 5委譲イベント) では合成 `MouseEvent` の `dispatchEvent` がハンドラへ届かない場合があり、ボタン類はネイティブ `element.click()` を使うこと。xdotoolのキー名はX keysym (`Right` = DOM `ArrowRight`)。tracingのANSI色は `NO_COLOR=1` で落とさないとgrepが崩れる。
4. devtestブリッジのログ行 (`devtest:`) は1秒周期の累積カウンタ。差分抽出で使うこと (絶対値は起動からの累積)。
