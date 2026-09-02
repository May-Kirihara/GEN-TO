# WP-11 埋め込み評価・レイテンシ実測 (ruri-v3-70m)

- 記録日: 2026-09-02 (WP-11実装時)
- 対象: `cl-nagoya/ruri-v3-70m` ONNX (CONV-1変換物・manifest package_version 1)
- 実行環境: Linux x86_64 (開発機。CPU実行・ort 1.28静的リンク・intra-thread 2)
- 再現コマンド:
  ```sh
  GENTO_EMBEDDING_MODELS_DIR=.local-models \
    cargo test --release -p gento-embedding --test eval -- --nocapture
  GENTO_EMBEDDING_MODELS_DIR=.local-models \
    cargo test --release -p gento-embedding --test vector_agreement -- --nocapture
  ```

## 1. 類似妥当性 (acceptance 5-3): Recall@3

評価セット: 合成検索文書30件 (10トピック×3文書。日本語のキャプション風
短文) + 10クエリ。正解 = 同一トピックの3文書。上位3件に含まれた正解の
割合 (Recall@3)。

| パイプライン | Recall@3 | 備考 |
|---|---:|---|
| Rust実装 (manifest接頭辞「検索クエリ: /検索文書: 」) | **0.8333 (25/30)** | 本番パイプライン |
| 公式ST + 同一接頭辞 (Python参照) | 0.8333 (25/30) | Rustと完全一致 |
| 公式ST + 接頭辞なし | 0.9333 (28/30) | 参考 |

- **閾値 = 0.80** (`tests/eval.rs` RECALL_THRESHOLD)。実測0.8333が閾値を
  上回る。実測と閾値の差は、評価セットの入れ替えに対する簡易マージン。
- 接頭辞なしの方が本評価セットでは高く出るが、**接頭辞付与はruri-v3の
  推奨する非対称検索方式** (モデルカード「1+3 prefix scheme」) であり、
  設計正本 (docs/embedding-runtime.md §6「接頭辞…モデルマニフェストへ
  固定」+ manifest正本) に従い接頭辞を採用する。本評価セットは同質的
  (キャプション風文書×キャプション風クエリ) なため、異質クエリと文書の
  組み合わせでの頑健性を図る接頭辞の利益が appearances 上不利に働いた
  と解釈する (将来の実データ評価で再検証予定)。
- 低下要因の内訳 (per-query): query 4「おいしそうな食べ物の写真」が1/3
  (ラーメン文書12を取りこぼし、ポートレート28が上位に入る) — 70mモデル
  の語彙的意味橋渡し (食べ物→ラーメン) の限界に起因。実装バグではない
  (下記の公式実装とのベクトル一致検証で排除)。

## 2. 実装の数値一致 (参照実装比較)

`tests/fixtures/ruri70m_reference.json`: CONV-1 venv
(sentence-transformers 6.0.1 / torch 2.14.0+cpu・revision固定) が生成した
参照ベクトル (クエリ1件+文書4件。短文・英日混在・長文を含む)。

| 比較 | cos |
|---|---:|
| Rust (ort 1.28静的リンク) vs 参照 (torch CPU) | 0.9999993 以上 (実測min。query=1.0000002) |

許容閾値 0.999999 (カーネル差 ~1e-6 を許容)。特殊トークン (BOS/EOS)
欠落時は 0.966 まで劣化するため、実装ドリフトは確実に検出される。

**実装上の発見 (重要)**:
- `tokenizers` crateの `encode_batch(texts, add_special_tokens)` は
  **`true` が正しい** (tokenizer.jsonのTemplateProcessing post-processorが
  `<s>…</s>` を付与。参照実装と同一)。`false` ではRecall@3が0.6333まで
  劣化した (実測。修正済み)。
- モデル出荷の `sentence_transformers` prompts は `{"query": "", "document": ""}`
  (**空文字列**)。公式推奨の検索接頭辞を使うには呼び出し側で
  `m.prompts` を上書きする必要がある。manifest正本の接頭辞はこの上書き
  と同一の文字列 (「検索クエリ: 」「検索文書: 」半角コロン+スペース)。

## 3. レイテンシ (acceptance 5-5)

`cargo test --release`・intra-thread 2・実モデル70m (280MB) での実測:

| 操作 | 実測 |
|---|---|
| embed_query 1件 | avg 9.6ms / max 12.7ms (10クエリ) |
| embed_documents 30件バッチ | 346ms (**11.5 ms/件**) |
| 初回ロード (model.onnx読み込み込み) | 約1.2〜1.4s (テスト全体実行時間から) |

debugビルドでは query 18.7ms — releaseビルドで約半分。バッチサイズを
大きくしても1件あたり時間はほぼ一定 (メモリ帯域律速の様子)。

## 4. セルフチェック (acceptance 5-4)

`EmbeddingService` はロード完了後に固定対 (近い文書/遠い文書) の
cosineマージンを計算し、`embedding_status.selfcheckMargin` へ公開する。
マージン < 0.05 で `warning` フィールドが設定されUIに表示される。
実測マージン (近い対0.85 / 遠い対0.71) ≈ 0.14 — 閾値に十分余裕。

## 5. E2E (acceptance 5-2/5-6)

`cargo test --release -p gento-tauri --test embedding_service` (release
ビルド・実モデル・実際のワーカーバイナリspawn):

1. `enable` → 3 asset埋め込み完了 (約5秒)
2. `similar_debug("猫が寝ている画像")` → 猫assetが1位
3. `kill_worker_for_test` (SIGKILL相当) → 新規assetをsweep投入 →
   ジョブは再試行待ち → ワーカー自動再起動 → 4件目の埋め込み完了 →
   新規assetが検索にヒット
4. 即死ワーカー注入で3回連続異常終了 → 自動再起停止 + `embedding:notice`
   通知 + status.phase=crashed → 手動リセットで復帰
