# WP-8 検索性能計測 — 5万件合成行 (2026-09-01)

計測テスト: `cargo test -p gento-db --lib search::tests::search_performance_50k_synthetic --release -- --nocapture`

## データセット (合成・実ファイル不要)

- assets 50,000行 + present files 50,000行 (rel_path BLOB・日本語ファイル名
  `dir{0..99}/画像{000000..049999}.png`)
- MIME: png/jpeg/webp を 1/3 ずつ。rating = id % 6。created_at = 2026年1〜12月
- タグ: 10% のassetに `tag{id%50}` (5,000タグ付け)
- FTS文書: `fts_rebuild` で全件構築 (rowid=asset_id管理)

## 結果 (release、作業マシン)

| 項目 | 値 |
|---|---|
| 全文書再構築 (fts_rebuild) | **2.3秒** |
| クエリ p50 | 27.4 ms |
| **クエリ p95** | **78.5 ms** (目安: < 100ms ✓) |
| クエリ max | 78.9 ms |

シナリオ別 (各10回の初回値):

| シナリオ | 一致数 | 所要 |
|---|---|---|
| `画像` (日本語2文字 → LIKEフォールバック・高頻度) | 50,000 | 83.5 ms |
| `画像012345` (日本語 trigram MATCH・狭束縛) | 1 | 0.6 ms |
| `tag:tag0 rating:>=4` (構造化) | 333 | 41.1 ms |
| `tag:tag0 画像` (複合) | 1,000 | 114→78 ms台に改善* |
| `added:2026-01-01..2026-03-31` (日付範囲) | 12,501 | 18.5 ms |

\* 複合シナリの初期計測114msは count 文と page 文で重いサブクエリを
二重実行していたため。`count(*) OVER ()` による1文化で全シナリオが
78ms台へ短縮した。

## 実経路 (search_run相当の全収集 — QA-WP8追加計測)

`search_run` は一致全件のasset idを5000件/ページで全収集 (上限10万件)
してから表示レコードを構築する。この実経路を同等のページング計測として
`search_performance_50k_synthetic` に組み込んだ (QA-WP8):

| 項目 | 値 (2026-09-01 QA-WP8測定) |
|---|---|
| 広域クエリ `画像` (5万一致) の**全収集** | **465 ms** (10ページ) |
| 同じテストの1ページ取得 (200件) | p50=27ms / p95=42ms※ |

QAレビュー計測 (等価SQLの全収集644ms/50k) と同水準 = 広域検索の
実経路は数百ms級。同期Tauri command (メインスレッド実行) では
この間UIを占有するため、QA-WP8 Major-1として `search_run` /
`search_refresh` をasync化 + `spawn_blocking` へ移行した。

※ 同日再測定ではシナリオp95=42ms (初回記録の78.5msと同じバイナリ・
同一合成データ。負荷状態によるばらつき)。いずれも目安 < 100ms内。

## UI非ブロッキング (QA-WP8後の実態)

`search_run` / `search_refresh` / `fts_integrity` / `fts_repair` は
async commandで、一致全件の収集・表示レコード構築・索引保守を
`tauri::async_runtime::spawn_blocking` (ブロッキングプール) で実行
する — 5万件級の広域クエリでもUIスレッドはブロックされない
(応答は収集完了後。検索中はツールバーのボタンが「検索中…」へ遷移)。
カタログ検索ソースへの載せ替え (generation更新 + `catalog:changed`)
は収集完了後の短時間ロックのみ。結果はフロントがページング既存機構
(200件/ページ) で追従する。

### 収集上限の判断記録

初回収集を先頭N=2万件に制限しtotalをcount別途取得する案は**見送り**:
`SearchDisplay.records` は全件表示の正本であり、収集打ち切りは
total (ステータスバー表示) とshown (グリッド実件数) の乖離という
新たなUI課題を生む。async化でUIブロッキングは解消済みのため、
フル収集 (上限10万件・465ms/5万件) を維持する。

## debugビルドについて

`cargo test` (debug) ではFTS/LIKE解釈が遅いため、性能アサーションは
debug: p95 < 500ms / release: p95 < 100ms の二段閾値にしている
(健全性は両方で担保)。
