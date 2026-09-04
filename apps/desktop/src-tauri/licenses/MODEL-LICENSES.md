# モデルライセンス表明 (ruri-v3 ONNXパッケージ)

GEN-TOの自然言語検索 (意味検索) が使用するローカル埋め込みモデルパッケージの
ライセンス表明。モデルパッケージはアプリに同梱せず、ユーザーが別途配置する
(配置手順は README「初回起動ガイド」および docs/embedding-runtime.md)。

## ruri-v3-30m-onnx / ruri-v3-70m-onnx

- 配布元 (ソースモデル): cl-nagoya/ruri-v3-30m、cl-nagoya/ruri-v3-70m
  (Hugging Face)
- ライセンス: **Apache License 2.0** (ソースモデルのLICENSEを継承。
  各モデルパッケージ内 `LICENSE` に全文同梱)
- 変換: 本リポジトリの `scripts/convert-ruri3-onnx.py` によるONNX変換版
  (optimum-export・opset 17・fp32。再学習・量子化なし。
  変換手順と出力誤差検証は docs/dev/ruri3-onnx-verification.md)
- パッケージ構成とsha256 (`manifest.json` で検証):
  `model.onnx` / `tokenizer.json` / `tokenizer_config.json` /
  `config.json` / `LICENSE` / `NOTICE` / `manifest.json`
- 実行: ONNX Runtime (CPU)。推論は独立ワーカープロセスで実行し、
  画像内容・埋め込みベクトルを外部へ送信しない

Apache License 2.0 の全文は各モデルパッケージ内 `LICENSE`、または
http://www.apache.org/licenses/LICENSE-2.0 を参照。
