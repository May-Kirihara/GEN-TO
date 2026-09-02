# ruri-v3 ONNX 変換環境の構成と再現手順 (CONV-1)

- 作成日: 2026-09-02
- 目的: `cl-nagoya/ruri-v3-{70m,30m}` を ONNX へ変換する環境の正確な記録。
  Python/PyTorch/SentenceTransformers は**ビルド時ツールとしてのみ**使用し、
  アプリへは同梱しない (docs/embedding-runtime.md §1)。
- 関連: [検証結果](./ruri3-onnx-verification.md)・[結合manifest](./ruri3-onnx-manifest.json)・
  スクリプト `scripts/convert-ruri3-onnx.py` / `scripts/verify-ruri3-onnx.py`

## 1. 環境 (実測の解決版)

venv: `/tmp/opencode/ruri-conv/venv` (コミット外。uv で構築)

| パッケージ | 版 | 備考 |
|---|---|---|
| Python | 3.12.11 | uv venv (システム python3.12) |
| torch | 2.14.0+cpu | CPU版 (CUDA不要。DLは pytorch CPU index) |
| transformers | 4.57.6 | optimum-onnx の制約で 5.x からダウングレード解決 |
| sentence-transformers | 6.0.1 | 正解側参照実装 |
| optimum | 2.1.0 | 本体 (ONNX export API は含まれない) |
| optimum-onnx | 0.1.0 | ONNX export 本体 (`optimum.exporters.onnx`) |
| sentencepiece | 0.2.2 | AutoTokenizer 遅延読み込みに必要 (下記注意) |
| onnx | 1.22.0 | |
| onnxruntime | 1.29.0 | CPUExecutionProvider |
| tokenizers | 0.22.2 | tokenizer.json 直読み (Rust tokenizers crate と同一正本) |
| huggingface_hub | 0.36.2 | snapshot_download (revision固定) |

## 2. 構築コマンド (再現手順)

```bash
uv venv /tmp/opencode/ruri-conv/venv --python 3.12

# torch は CPU index から (CUDA版DLを避ける)
uv pip install --python /tmp/opencode/ruri-conv/venv/bin/python \
    torch --index-url https://download.pytorch.org/whl/cpu

# 残り一括。optimum 2.x では "optimum[exporters]" extra は廃止されており
# (警告のみ出て実害なし)、export API は optimum-onnx が提供する。
uv pip install --python /tmp/opencode/ruri-conv/venv/bin/python \
    sentence-transformers "optimum[exporters]" onnx onnxruntime

# export API 本体 + tokenizer 遅延読み込み対応
uv pip install --python /tmp/opencode/ruri-conv/venv/bin/python \
    optimum-onnx sentencepiece
```

注意: `optimum-onnx` 導入時に transformers 5.16.1 → 4.57.6、tokenizers 0.23.1 → 0.22.2
へ解決される (optimum-onnx が transformers<5 を要求)。上記表が最終状態。

## 3. 変換・検証の実行

```bash
# Apache-2.0 正文を一度だけ取得 (配布物のLICENSEとして同梱)
curl -sS -o /tmp/opencode/ruri-conv/Apache-2.0.txt \
    https://www.apache.org/licenses/LICENSE-2.0.txt
# sha256: cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30

# 変換 (両モデル。HF snapshot は revision 固定で各ファイル1回のみ取得)
/tmp/opencode/ruri-conv/venv/bin/python scripts/convert-ruri3-onnx.py \
    --models 70m 30m \
    --license-path /tmp/opencode/ruri-conv/Apache-2.0.txt \
    --combined-manifest docs/dev/ruri3-onnx-manifest.json

# 誤差検証 (しきい値: cos最大絶対誤差 < 1e-3。超過時は exit 1)
/tmp/opencode/ruri-conv/venv/bin/python scripts/verify-ruri3-onnx.py \
    --package /tmp/opencode/ruri-conv/out/ruri-v3-70m \
    --json-report /tmp/opencode/ruri-conv/reports/verify-70m.json
/tmp/opencode/ruri-conv/venv/bin/python scripts/verify-ruri3-onnx.py \
    --package /tmp/opencode/ruri-conv/out/ruri-v3-30m \
    --json-report /tmp/opencode/ruri-conv/reports/verify-30m.json
```

成果物: `/tmp/opencode/ruri-conv/out/ruri-v3-{70m,30m}/`
(§3 構成: model.onnx / tokenizer.json / tokenizer_config.json / config.json /
manifest.json / LICENSE / NOTICE)。**リポジトリにはコミットしない。**

## 4. 判明した環境依存の落とし穴

1. **optimum は library 推定で pooling を混入する**: snapshot に `modules.json`
   (SentenceTransformers 形式) があると optimum は library を
   sentence_transformers と推定し、pooling 込みグラフ (`sentence_embedding` 出力)
   を export する。`library_name="transformers"` を明示することで
   transformer 部のみ (`last_hidden_state` 出力のみ) になる。設計§5/§6要件。
2. **tokenizer_config.json の `add_prefix_space: false` 明示**: transformers 4.57 の
   LlamaTokenizerFast は add_prefix_space が明示されていると `from_slow=True`
   を強制し、sentencepiece 未導入だとロードが落ちる。sentencepiece を入れるか、
   `tokenizers.Tokenizer.from_file` (tokenizer.json 直読み) を使う。
   両経路のid出力は本コーパスで完全一致を確認 (検証スクリプトで常時確認)。
3. **opset 17 警告**: optimum は modernbert に opset 18以上を推奨する警告を出すが、
   opset 17 (WP要件の下限) で export・検証とも成立。manifest に 17 を記録。
4. **HF etag の罠**: resolve 応答の CDN URL に含まれる Xet CAS ハッシュは
   ファイルの sha256 ではない。sha256 は tree API の `lfs.oid` または
   `X-Linked-ETag` ヘッダで確認すること
   (handoff 2026-09-02-mvp-wp11.md の記載は Xet ハッシュだった——本WPで訂正)。

## 5. リソース実測 (2026-09-02)

| 項目 | 値 |
|---|---|
| venv 容量 | 1.3 GB |
| HF キャッシュ (両モデル snapshot) | 424 MB |
| 成果物 (out/ 両パッケージ) | 421 MB |
| モデルDL (初回) | 70m 280MB 約5分 / 30m 147MB 数十秒 |
| 変換処理 (キャッシュ後, 両モデル) | 約23秒 |
| 検証処理 | 70m 約91秒 / 30m 約53秒 (32文, 長文2件含む, CPU 20スレッド) |
