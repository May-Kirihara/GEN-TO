# WP-12 USearch索引 5万件パフォーマンス実測 (2026-09-02)

- 計測環境: 作業マシン (Linux 24.04・g++ 13.3・cargo 1.96)。
- 計測コマンド:
  `cargo test --release -p gento-embedding --test index_benchmark -- --nocapture`
- データ: 合成50,000ベクトル × 384次元 (ruri-v3-70mと同一次元。
  **クラスタ構造付き** — 100クラスタ×500ベクトル + 摂動。
  実テキスト埋め込みは多様体構造を持つため、無作為一様ベクトルよりも
  実データを代表する)。
- USearchパラメータ (vector_index.rs既定): connectivity=16 /
  expansion_add=96 / expansion_search=64・Cos metric・F32スカラー。

## 結果 (release)

| 項目 | 値 |
|---|---|
| 検索 (k=200) p50 | **0.35 ms** |
| 検索 (k=200) p95 / max | 0.45 / **0.45 ms** |
| 構造フィルタ付き検索 (5万の半分を許可) | 0.55 ms |
| recall@10 (vs 全件照合) avg / min | **0.995 / 0.90** |
| 上位10件クラスタ純度 | 1.000 |
| 構築 (50,000追加・単一スレッド) | 16.4 s (3,049 adds/s) |
| メモリ (memory_usage) | 144.4 MiB (3,028 B/vector) |
| 索引ファイルサイズ | 80.3 MiB |
| save / load | 0.05 s / 0.06 s |

**acceptance「5万ベクトルで検索0.5秒未満」**: max 0.45ms — 約3桁余裕で
クリア。ハイブリッド検索全体 (embed_query ~10ms + 索引検索 + FTS +
RRF + 表示構築) でも E2E elapsed 1秒未満。

## 構築中のUI非ブロック実証 (常時実行テスト `status_stays_responsive_during_build`)

5,000ベクトル (debug) のバックグラウンド構築中に `vector_index_status`
を96回呼び出し: **最大応答 4.2ms**・building=true を観測 — 構築スレッドは
索引ロックをページ単位で手放すため、UI操作 (status/検索) はブロック
されない。

## パラメータ感度 (パラメータスイープ・作業メモ)

- expansion_search=24: recall@10 avg=0.920 (min 0.70) → 64へ引き上げ
  0.995 (min 0.90)。検索レイテンシはほぼ不変 (0.35ms)。
- **無作為一様ベクトル (構造ゼロ・HNSWの最悪ケース)**: 同パラメータで
  recall@10 = 0.12。expansionを256へ増やしても0.35、connectivity=32で
  0.46 — 高次元一様乱数はANNの本質的な困難例。実埋め込み (多様体構造)
  では起こらないため、ベンチマークはクラスタ構造データを正本とした。
  念のため実ruri-v3ベクトルでの索引評価 (hybrid_eval) で top-3 完全一致
  (100%) を確認済み。
- numkong SIMD (dynamic dispatch): haswell/alder レベルで稼働
  (`hardware_acceleration_available` = "serial, haswell, alder")。

## 実モデル品質 (GENTO_EMBEDDING_MODELS_DIR=.local-models・release)

- `hybrid_eval` (WP-11評価セット30文書+10クエリ):
  - 索引top-3 == 総当たりcosine top-3: **1.00** (順序完全一致。
    類似度数値も |Δ| < 1e-4)
  - Recall@3 via index: **0.8333** (= WP-11総当たり評価と同一 —
    索引化による品質低下なし)
  - RRF重み挙動: w=1で意味順・w=0でキーワード順・中間で両側一致が上位
  - 類似画像検索 (docベクトル近傍): top-1同トピック 0.67 / top-3内に
    同トピック含む 0.90 (トピックラベルは完全な正解ではないため
    top-1は0.60閾値。無作為なら ~7%)
- `vector_index_service::hybrid_search_pipeline_real_model` (E2E):
  - 自然言語「くつろいでいる猫」(w=1) → cat/kitten が上位2件
  - 「ラーメン」: w=0 はFTS一致1件のみ、w=1 は意味近傍5件 (ramen最上位)
  - 類似画像: cat の最近傍 = kitten (cos 0.912)

## 再現コマンド

```sh
cargo test --release -p gento-embedding --test index_benchmark -- --nocapture
GENTO_EMBEDDING_MODELS_DIR=.local-models \
  cargo test --release -p gento-embedding --test hybrid_eval -- --nocapture
GENTO_EMBEDDING_MODELS_DIR=.local-models \
  cargo test --release -p gento-tauri --test vector_index_service -- --nocapture
```
