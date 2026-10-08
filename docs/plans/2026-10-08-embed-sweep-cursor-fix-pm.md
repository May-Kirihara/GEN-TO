# 埋め込みスイープのページネーション欠陥修正 (EMBED-SWEEP-1)

日付: 2026-10-08 / PM計画・検証 / 実装: implementer
報告経路: ユーザー実機報告「2228件中2048まで埋め込み後『準備完了』で停止・自然言語検索が機能しない」

## 目標

全 present asset の埋め込み差分スイープがライブラリ全体を網羅するようにする
(id順末尾のassetが永久にスイープ対象から漏れる状態の解消)。

## 原因 (確定・コード実読)

`EmbeddingService::sweep_enqueues_pending` (apps/desktop/src-tauri/src/embedding.rs:901):

- 毎回 `after_id=0` から `present_asset_ids(after_id, 256)` (ORDER BY asset_id ASC,
  gento-db/src/embeddings.rs:255) をページングし `examined < limit (SWEEP_PAGE=2000)`
  で打ち切り → 256×8=**2048件**のみ検査 (各スイープで同じ先頭2048件)。
- 先頭2048件の埋め込み完了後は定期スイープ (job_loop, SWEEP_INTERVAL=30s) が
  同じ先頭2048件をunchanged skipして終了 → **末尾 (2228-2048=180件) は永久未検査**。
- 症状2 (意味検索無反応) は派生: 未埋め込みassetは索引に存在せず黙って意味候補から
  除外される。※スライダー既定0 (純キーワード) は別個の仕様 (本WP対象外・ユーザーへ説明済み)。

## スコープ

- 対象: `sweep_enqueues_pending` の走査方式を**サービス内カーソル方式**へ変更
  (AtomicI64 `sweep_cursor` 等): 前回終了位置から継続し、ページが空になったら
  (=\テーブル終端到達) カーソルを0に巻き戻す。1スイープの作業量上限
  (SWEEP_PAGE+繰上げ) とunchanged skip・priority規則 (初回=INITIAL/以降=NEW_ASSET) は維持。
- 非対象: enable()の一括網羅化 (定期スイープ30sで追いつくため不要)・スライダー既定値・
  UI・索引側 (増分on_embedding_upsertedで自動追従)・DBスキーマ・ジョブ優先度設計。

## テスト戦略 (test-first)

1. **回帰テスト (red→green)**: present asset をSWEEP_PAGE超 (例: 2100件・全員に空でない
   検索文書) 用意し、`sweep_enqueues_pending(SWEEP_PAGE)` を繰り返し呼んで
   ジョブ消化を模擬 → **全assetがいずれかのスイープでエンキューされる**ことをassert。
   現行実装では末尾52件が永久に拾われずred。
2. 既存テスト全green維持 (DTO/resolve等)。カーソル導入で単発スイープの件数挙動が
  変わる既存テストがあれば、**契約を弱めず**期待値を更新 (変更理由をコメント)。

## 検証

- `cargo test -p gento-tauri` (src-tauri全体) + `cargo test -p gento-db -p gento-embedding`
  ※パッケージ名は `gento-tauri` (初版計画の「gento-desktop」は誤り・修正済み)
- `cargo clippy` 既存warning水準維持
- ユーザー実機での事後確認手順をhandoffに記載 (AIパネル進捗2228/2228・索引準備完了
  (2,228件)・スライダー右でInfoPanel「意味 xx%」)

## 完了条件

1. 回帰テストが現行コードで失敗し、修正後green (test-first証拠)。
2. 全cargo test green・逸脱パスなし。
3. Codex final review pass。
4. ユーザー向け事後確認手順の提示。

## リスク評価

normal (backend serviceロジック・非破壊・スキーマ不変・UI不変)。fast mode。

## 完了記録 (2026-10-08)

**完了 (done-with-notes)**。実装: `embedding.rs` のみ (+323/-3)・`AtomicI64 sweep_cursor` 追加
(継続走査+終端巻き戻し)。`SWEEP_READ_PAGE=256` を定数化。

- **test-first**: 回帰テスト `sweep_enqueues_all_present_assets_across_repeated_sweeps`
  (2100件) が旧コードで「拾えなかったasset [2049..2100]」でred → 修正後green。
  巻き戻し (2周目の前方差分再検査)・削除混在カーソル継続もテスト固定。
- **PM独立実行**: `cargo test -p gento-tauri` = lib **272** + integration **3+4** 全green。
  clippy/fmt clean (implementer実行)。
- **Codex final**: **approve-with-notes** (`/tmp/codex-final-review-embed-sweep.md`) —
  blockerなし。notes: ①並行スイープのカーソル上書き (巡回で最終網羅・無害) ②DB読取
  エラー時の空ページ扱い=カーソルリセット (遅延のみ・無害)。missing tests提示あり
  (overlap決定性・実job_loop結合 — 残件として記録)。
- **ユーザー実機事後確認**: アプリ再起動 (dev再ビルド) 後、AIパネル進捗が2228/2228へ
  (残り180件は30秒毎スイープで1〜2周期) → 索引「準備完了 (2,228件)」→
  スライダー右で検索しInfoPanel「意味 xx%」表示・類似画像検索で動作確認。

## 既知の残issue (本WP範囲外・次バッチ候補)

1. **索引ベクトルの再埋め込み非反映** (Codex指摘): `on_embedding_upserted` が
   `index.contains(asset_id)` で既存をskipするため、メタデータ編集等で検索文書が
   変わりベクトルが更新されてもUSearch索引は**古いベクトルのまま** (件数不整合が
   起きない限り全再構築されない)。新規埋め込みには影響しない。
2. 検索スライダー既定値0 (=純キーワード) の発見可能性 — 仕様だがユーザーが
   「意味検索が動かない」と誤解しやすい (今回の誤報原因の一部)。
3. sweepの実job_loop結合テスト (Codex missing test)。
