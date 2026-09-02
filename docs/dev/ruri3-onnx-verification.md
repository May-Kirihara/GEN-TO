# ruri-v3 ONNX 変換 成果物の誤差検証結果 (CONV-1)

- 検証日: 2026-09-02
- 検証スクリプト: `scripts/verify-ruri3-onnx.py` (JSON報告: `/tmp/opencode/ruri-conv/reports/verify-{70m,30m}.json`)
- 環境: [conversion-env.md](./conversion-env.md) のとおり (CPU, onnxruntime 1.29.0)
- 対象成果物: `/tmp/opencode/ruri-conv/out/ruri-v3-{70m,30m}/`
  (checksumは[結合manifest](./ruri3-onnx-manifest.json)に記録)

## 結論

**両モデルとも PASS。cos最大絶対誤差は閾値 1e-3 に対し約4桁の余裕 (max ≈ 9.5e-8)。**

| 指標 | ruri-v3-70m | ruri-v3-30m | 判定閾値 |
|---|---|---|---|
| cos絶対誤差 (max) | **9.466e-08** | **9.073e-08** | < 1e-3 |
| cos絶対誤差 (mean) | 3.213e-08 | 3.383e-08 | — |
| 正規化後ベクトル要素 |\|diff\| (max, 参考) | 1.693e-07 | 2.556e-07 | — |

## 検証方法

- 正 (参照): `SentenceTransformer(snapshot)` (revision固定snapshotからロード) の
  `encode(normalize_embeddings=True)` 出力。ST内部のmax_seq_length=8192と
  次元 (70m=384 / 30m=256) をmanifest値と突合。
- 被 (候補): `tokenizers.Tokenizer.from_file(tokenizer.json)` (Rust tokenizers
  crateと同一の正本ファイルを直読み) + `onnxruntime` 推論
  (CPUExecutionProvider) + attention mask考慮のmean pooling + L2正規化。
  **pooling/正規化はグラフ外で実施** (設計§5/§6どおり。グラフ出力は
  `last_hidden_state` のみであることをONNXモデルから機械検証)。
- 入力コーパス: 32文 = 日本語15・英語15 (query接頭辞「検索クエリ: 」とdoc接頭辞
  「検索文書: 」を網羅。極端に短い文・絵文字/記号混入を含む) + 8192トークンを
  大幅に超える長文2件 (日本語doc / 英語query)。
- 附加検証 (すべてPASS):
  - パッケージ整合: manifest記載の全ファイルsha256・サイズが実ファイルと一致
  - グラフ構造: 入力 `input_ids`/`attention_mask` (動的軸 batch/sequence)、
    出力 `last_hidden_state` のみ、opset 17
  - 切り詰め: 長文2件とも候補側トークン数がちょうど8192 (切り詰め動作)
  - 次元: 参照・候補ともmanifestの次元と一致
  - tokenizer一致: transformers AutoTokenizer (ST内部経路) と
    tokenizer.json直読みのid列が全32文で完全一致
    (WP-11のRust tokenizers crate採用の根拠)

## 変換方式の記録

- 成立: **第1候補 optimum export** (`optimum-onnx 0.1.0` の
  `main_export(task="feature-extraction", library_name="transformers", opset=17)`)。
  `library_name="transformers"` 指定が必須 (省略するとSentenceTransformers
  としてpooling込みをexportする——設計違反。詳細はconversion-env.md §4)。
- 第2候補 torch.onnx.export は実走不要 (第1候補で成立) だがスクリプトに
  フォールバックとして実装済み (`--method torch` で単独実行も可)。
- optimum export時の内部検証 (torch vs onnxruntime, last_hidden_state,
  atol 1e-5) は実行毎に max diff 2.3e-5〜1.8e-3 のばらつき警告が出るが、
  これはトークン単位の生出力に対する一時的差分であり、本検証 (mean pooling +
  L2正規化後のcos) では影響なく全ケース 1e-7 台に収まる。

## 備考

- 変換は fp32 の数値再エクスポート (量子化・再学習なし)。
- exportの決定性: 同一環境でスクリプトを再実行した場合、model.onnx は
  バイト同一で再生成されることを確認済み (70m: `f9207450…`,
  30m: `a7965d36…`。結合manifestのsha256と突合)。検証数値も再実行で一致。
- 70m/30m の tokenizer.json・tokenizer_config.json は同一ファイル
  (sha256一致——ruri-v3ファミリ共通トークナイザー)。
- HFソースの safetensors sha256 は tree API `lfs.oid` で確認した値を
  manifest `source.model_safetensors_sha256` に記録
  (70m: `c959b76a…`, 30m: `e94155e3…`)。handoff 2026-09-02-mvp-wp11.md
  記載の etag 値は Xet CAS ハッシュであり sha256 ではない (訂正済み)。
- 検証所要: 70m 約91秒 / 30m 約53秒 (32文・長文2件含む、CPU 20スレッド)。
