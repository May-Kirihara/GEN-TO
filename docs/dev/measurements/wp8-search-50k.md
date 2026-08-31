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

## UI非ブロッキング

`search_run` はTauri command (非async) だが検索自体はreader接続の
数十ms完了 — UIスレッドの長期占有なし。結果はカタログ検索ソースとして
generation更新 + `catalog:changed` で通知され、フロントはページング既存
機構 (200件/ページ) で追従する。検索中はツールバーのボタンが
「検索中…」へ遷移する。

## debugビルドについて

`cargo test` (debug) ではFTS/LIKE解釈が遅いため、性能アサーションは
debug: p95 < 500ms / release: p95 < 100ms の二段閾値にしている
(健全性は両方で担保)。
