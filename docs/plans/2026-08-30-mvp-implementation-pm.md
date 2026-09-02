# GEN-TO MVP実装 PM実行計画

- 作成日: 2026-08-30
- PM: opencode PM agent
- 対象: [MVP実装計画書](../mvp-implementation-plan.md) WP-0〜WP-18 の実行管理
- 前提: 技術スパイク PASS確定済み（2026-08-30 ユーザー手動確認、コミット `6a15daa`）

## 1. 目的と体制

スパイクで成立した基盤（コミット `def009a`..`cdb9478`、cargo 159 / vitest 218テスト）を出発点に、MVP実装計画書のWP-0〜18を依存順に実装する。PMは委任・検証・コミット・ゲート管理のみを担い、実装は implementer へ委任する（スパイクと同一運用）。

## 2. リスク分類

**プログラム全体: high-risk** — DBマイグレーション（データ損失面）、ファイル操作（ユーザーデータ破壊面）、AI/秘密情報（外部送信面）、クロスプラットフォーム。ただしゲートは各WPの実リスクに比例配分する（下記）。

| WP | 内容 | WP別リスク | 主なリスク面 |
|---|---|---|---|
| WP-0 | スパイク製品化 | normal | リファクタ・feature隔離 |
| WP-1 | SQLite基盤 | **high** | migration・schema正本 |
| WP-2 | 登録ルート・差分スキャン | high | ファイル状態管理の正しさ |
| WP-3 | 画像形式拡張 | normal | デコード安全性・メモリ |
| WP-4 | UIシェル | normal | UI設計 |
| WP-5/6 | メタデータ編集・コレクション | normal | DB整合性 |
| WP-7 | ファイル操作・Undo | **high** | ユーザーデータ破壊 |
| WP-8/9 | FTS・画像メタデータ | normal | 性能 |
| WP-10 | AI基盤 | **high** | 秘密情報・外部送信 |
| WP-11/12 | ruri・USearch | normal | モデル配布・index整合 |
| WP-13 | バックアップ | **high** | 復元データの完全性 |
| WP-14/15 | エクスポート・設定 | normal | 上書き事故 |
| WP-16/17/18 | 性能・OS・RC | high | リリース阻止条件 |

## 3. ゲート戦略（比例配分）

各WP共通:
- テストファースト委任（受入条件→テスト項目変換を契約に含める）
- PM検証プロトコル（git diff・テスト再実行・スコープ確認）→ WP単位コミット（ユーザー指定方針 (b)）
- 全WPで `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test --workspace` / `pnpm check/test/lint/build`

マイルストーンCodexレビュー（high-risk面のWP後、該当WP+直前WP群のdiff）: **WP-1後（WP-0+1基盤レビュー）**、**WP-2後（正確性レビュー: reconciliation・identity・watcher）**、WP-7後、WP-10後、WP-13後、WP-16後、WP-18最終。
QAレビュー（ユーザー可視面）: WP-4後、WP-8後、WP-18最終。
最終ゲート（WP-18後）: PM全diff検査 → Codex最終レビュー → QA最終レビュー → 結果文書。

テスト実行コスト方針: 現行は全テスト数秒で完了するため毎WP全実行する。スイート増大でサイクル時間が問題になった時点で「WP対象テスト＋マイルストーン全実行」へ移行し、本書に記録する。

スキップ・統合は最終報告で開示する。

## 4. 実行順序

計画書§30のDAGに従う: WP-0 → WP-1 → WP-2 → WP-3 → WP-4 → WP-5 → WP-6 → WP-7 → WP-8 → WP-9 → WP-10 → WP-11 → WP-12 → WP-13 → WP-14 → WP-15 → WP-16 → WP-17 → WP-18。

並行は原則なし（high-risk）。ただし§30記載の並行候補（parser fixture整備、ruri ONNX変換検証、Windows環境準備）はMVP後半でCodexレビューを経て判断する。

## 5. セッション戦略

- 19WPは複数セッションにまたがる。各セッションで依存順にWPを進め、WP単位でコミット＋進捗報告する。
- 進捗は計画書§32チェックリストと本書§8で追跡する。
- 中断からの再開は本書と `git log` で状態復元できることを前提とする（各WP完了時にハンドオフ更新は必須にしない——WP単位コミットメッセージとチェックリストで足りるため。ただし high-risk WP (1,7,10,13) 完了時はハンドオフ更新を必須とする）。

## 6. 主要技術判断（PM指示、実装時にimplementerがlicense確認）

1. **dev機能の隔離 (WP-0)**: `GENTO_DEV_BRIDGE`環境変数ゲート（現行）から、cargo feature `devtools`（既定無効）へ移行。`debug_assertions`または`--features devtools`時のみコンパイルに含める。通常release buildでは環境変数によらず呼び出し不能（WP-0受入条件）。**artifact-level証拠必須**（release binaryでhandler当該command不在を確認; Codexレビュー指摘）。計測用release buildは`--features devtools`を明示的に指定（README記載）→「スパイク性能シナリオを継続実行可能」との両立。
2. **SQLite (WP-1)**: `rusqlite` bundled build。WAL・foreign_keys・busy_timeout設定。schema_version + 順方向のみmigration。**migration前backupはWAL安全な方法（SQLite Backup API または `VACUUM INTO`）+ 検証 + 世代保持**（Codexレビュー指摘: メインDBファイルのコピーピングはWAL活性時不整合になり得る）。**FTS5はfeature/実行時双方で可用性を確認するテストを必須とする**。migrationは**1件1トランザクション・失敗時rollback・スタートアップ冪等・中断fixture**を必須とする。
3. **非UTF-8パス**: relative pathは生バイト列BLOB（`path_encoding`列: `'unix-bytes'`/`'utf8'` で符号化方式を明示）+ 表示名TEXT（lossy）分離。WindowsのWTF-8/UTF-16完全保持はWP-17検証時に再検討し、backup形式はパーセントエスケープUTF-8で正準化（Codexレビュー指摘: プラットフォームタグ必須）。
4. **UUID**: v7（時系列整列、TEXT 36文字小文字）。**クロック巻き戻り時の単調性は保証しない**（一意性のみ保証、制約として文書化）。日時はUTC RFC3339 TEXT。enumはTEXT（CHECK制約）。
5. **DB並行性方針 (WP-1で文書化)**: 書き込みは単一writer接続に集約し、読み取りは分離した接続（data-model §13）。scan/UI編集/AIジョブ/backupはこの方針に従う。DB配置・権限・disk full・破損時挙動もWP-1で診断commandと併せて文書化。
6. **BLAKE3/notify等**: 追加依存は各WP契約で確認記録させる。記録フィールドは **入手元・バージョン・license・依存するnative library・再配布義務・対応プラットフォーム**（license名のみ不十分; Codexレビュー指摘）。
7. **ベースライン明確化**: `6a15daa` はdocsのみのコミット（実装ベースラインは `def009a..cdb9478`）。実装ベースラインとの関係は本項で確定記録とする。

## 7. 仮定台帳

| # | 仮定 | 根拠 | 反証条件 | 影響範囲 |
|---|---|---|---|---|
| A1 | スパイクPASS確定は有効 | ユーザーによるdocs更新・手動確認実施 (2026-08-30) | なし（記録済み） | なし |
| A2 | MVP計画書が要件正本。spec-draft/data-modelとの衝突は計画書優先（新規） | 計画書が"Ready for implementation"で最新 | 計画書の記載がspecと矛盾し実装不能な場合→PMが判断記録の上ユーザーへ確認 | 当該WPの設計 |
| A3 | 複数セッション継続。ユーザーの継続指示で順次WPを実施 | 19WPの規模 | ユーザーの停止指示 | スケジュールのみ |
| A4 | `sample/dataset` は引き続き読み取り専用・テスト非依存 | スパイクの確立した規約 | なし | なし |
| A5 | 依存追加のlicense確認はimplementer契約に含め、`docs/dev/dependency-notes.md`へ追記 | 計画書§4・§31 | license問題発覚→代替候補をPMが判断 | 当該WP |
| A6 | WP-0完了まではスパイク計測ドライバが資産。devtools feature化後も`--features devtools`で同等機能 | 計画書§8 | 移行で計測不能→WP-0受入違反として修正 | WP-0 |

## 8. 進捗記録

- 2026-08-30: 本書作成。Codex計画レビュー approve-with-notes。
- 2026-08-30: **WP-0完了** (`e14707f`)。devtools feature隔離+artifact証拠（スモーク8/8）、AppDirs、API version、panic診断、license生成。cargo 165 / vitest 219。
- 2026-08-30: **WP-1完了** (`9cf81c2` + FG-WP1 `a63ae07`)。Codex基盤レビュー: 1回目reject（未来schema受入・writer未強制・backup世代ソート）→修正→2回目**approve-with-notes**。migration #1凍結（ai_run_id列はWP-10 ADD COLUMN戦略、ルート設定列追加）。cargo 243 / vitest 221。
- 2026-08-30: **WP-2完了** (`94a9892` + FG-WP2 `ef920c6` + FG-WP2-2 `bb15f68`)。正確性レビュー: 1回目reject（6ブロッカー）→2回目reject（根因=世代トークン弱さ→**トークン再設計**で対応）→3回目**approve-with-notes**。`(os_file_id,size,mtime_ns)`トークン、fd束ねprobe、積極的不在証明、決定論リトライ。migration #2〜#4。cargo 328 / vitest 236。E2E: 1615/974/0/0。
- 後続硬化メモ（非ブロッキング記録）: write_retの明示的SQLiteトランザクション化（crash-atomicなasset作成+link）/ os_file_id不在プラットフォームのフォールバック強化 / 敵対的in-place変更（size+mtime復元）の検出は範囲外と明記 / WP-1レビューの非ブロッカー未対応分はWP-3以降の健全性確認時に拾う。
- 2026-08-30: **WP-3完了** (`c67cda5`、done-with-notes)。6形式対応（probe/decode/thumb/orientation/animated/背景切替）。**AVIFはblocked**: `libdav1d-dev`不足（検出・mime・型は前方実装済み、feature追加のみで有効化）。cargo 347 / vitest 246。Codex個別レビューなし（normal、基盤レビューで担保）。
- 2026-08-30: **WP-4完了** (`60f5529` + QA-WP4 `c1fd151`)。3ペインUIシェル・ツリー・InfoPanel・クイックプレビュー・コンテキストメニュー・選別セッション。性能退行なし（S3相当long frame +0、S4相当p50同等）。QA **pass-with-concerns** → Major-2件（走査キャンセル不能→scan_token世代切替+cancel_scan+深さ/件数ガードで解決、adhocセッションID不安定→ソート永続化+再開検証で解決）+Minor-3件修正。cargo 372 / vitest 331。
- ユーザー依頼事項: `sudo apt install libdav1d-dev`（AVIF有効化。feature `avif-native`追加は別委任）。→ **完了**: ユーザーがインストール (dav1d 1.4.1)、AVIF-1 `47d9034` で有効化。WebKitGTK自体はAVIF `<img>`非対応（original表示制限→WP-17記録）。
- 2026-08-31: **WP-5完了** (`2246de0`)。評価・選別・タグ・キャプション（一括・Undo・高速選別・比較選別）。cargo 402 / vitest 357。
- 2026-08-31: **WP-6完了** (`785d353`)。手動/スマートコレクション（query_json・明示refresh）。cargo 434 / vitest 372。
- 2026-08-31: **WP-7完了** (`87f2294` + FG-WP7 `07dd963` + FG-WP7-2 `44a88d7`)。Codexレビュー: 1回目reject（7ブロッカー: 恒久削除確認・上書きTOCTOU・旧ファイル保護・ゴミ箱同一性・undo原子性・tmp所有権・watcher和解）→修正→2回目reject（DeleteCopy再検査・トークンバインド）→修正→3回目**approve-with-notes**。no-clobber配置(renameat2 NOREPLACE)・意図バインド確認トークン・補償ロールバック。cargo 530 / vitest 387。**M0〜M3マイルストーン完成**。
- GaOTTT: 2026-08-31に接続復旧。スパイク〜WP-6の保存候補14件を書き戻し済み（規約・世代トークン・会計・notify・image/Svelte実務・進捗スナップショット）。以降はWP毎に書き戻し。
- 残余制限の追跡 (WP-7レビュー): stat→trash同一action内窓 / overwrite宛先内容はトークン非バインド / undo補償はFSレベル（DBはスキャン収束）/ 掃除のstrict正規表現foreign一致（低確率）/ crash recoveryの段階的再構築未実装 / 旧result JSON undoはfails closed。
- 2026-08-31: **WP-8完了** (`6832349` + FG-WP8込み + QA-WP8 `9576bb5`)。FTS5 trigram・検索AST (gento-search新設)・GUIフィルタ合成・保存済み検索=v2 ASTスマートコレクション・一致根拠・fts_integrity/repair/rebuild。5万件合成でp95 42-78ms。**QA pass-with-concerns**: Major（同期コマンドのUIブロック〜1秒）→ async化+spawn_blockingで解消。FG-WP8: PMがE2E失敗を決定的再現（2回）し「ハーネスタイミング」主張を反証——ドライバキー誤り+**検索遷移でcatalogSource未維持の実プロダクト欠陥**を両方修正。グリッド空白化 (WP-7既知1) も根本修正。cargo 575 / vitest 408。**M4完了**。
- QA手動確認リスト（ユーザー）: 50k級実データでの広域検索体感 / 未登録→検索→Meta編集 / 検索ビューからファイル操作 / スコープ=ルート全体の受け入れ / v2保存検索の左ペイン表示。50k実データE2E・検索ビューFileOps E2EはWP-16へ申し送り。
- 2026-09-02: **WP-9完了** (`86f2b65`)。EXIF/IPTC/SD-Forge/ComfyUI/NovelAI parser（fd束び・世代トークン規律・元バイト不変）、正規化+Raw保持、embedded_prompt検索接続、FUTURE_KEYS有効化（model/steps/has/sampler/vae/lora）、InfoPanel 3節。cargo 622 / vitest 419。E2E 9/9×3。
- 2026-09-02: **WP-10完了** (`f2eea9c` + FG-WP10 `f50852e`)。**Codexレビュー**: 1回目reject（5ブロッカー: 縮小上限未強制・非リトライ性失敗の即再送・同時実行レース・応答無制限・鍵欠落未認証送信）→修正→2回目**approve-with-notes**。gento-ai crate（reqwest rustls・mockサーバー）、永続ジョブキュー（backoff・再開・冪等）、keyring+0600ファイルフォールバック、手動編集保護、送信前縮小64-2048強制・GPS独立opt-in・フェイルクローズ。migration #8。cargo 690 / vitest 438。**M5完了**。
- 2026-09-02: ユーザー実機確認サポート: DEVUX-1 (`d76aba4` dev読み出し既定非表示+トグル)、FIX-AI-DTO (`c8d7056` TestReport camelCase_SERIALIZE—the 4種テスト結果が表示されない不具合、WP-5系統の再犯を契約テストで封じた)。**ユーザーが実llama.cppサーバ（外部、Gemma系26B Q4）で4種テスト合格を報告**——WP-10 acceptance 1実機確定（接続2ms/テキスト25s/Vision 44s「青一色」正答/構造化OK、タイムアウト60s既定内）。cargo 691 / vitest 444。

## 9. オープンクエスチョン（ユーザー判断不要・PM判断で進行）

- JPEG XL / APNG (WP-3): 計画書どおり別ゲート。依存基盤確認結果をWP-3完了時に記録し、含めない前提で進める。
- アニメーション再生の詳細仕様 (WP-3): ビューアでの再生/一時停止・一覧は静止画、をMVP範囲とする。
- その他の未決事項 (spec §20) は該当WP到達時にMVP計画書の範囲で解釈し、判断をハンドオフに記録する。
