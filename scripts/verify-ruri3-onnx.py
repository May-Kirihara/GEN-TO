#!/usr/bin/env python3
# ruri-v3 ONNX変換成果物の誤差検証 (CONV-1)。
#
# 使い方:
#   scripts/verify-ruri3-onnx.py --package /tmp/opencode/ruri-conv/out/ruri-v3-70m \
#       [--cache-dir DIR] [--threshold 1e-3] [--json-report FILE]
#
# 検証内容 (docs/embedding-runtime.md §12 技術検証項目「公式SentenceTransformers実装との出力誤差」):
#   - 正: SentenceTransformers (revision固定snapshot) のencode出力 (L2正規化済み)
#   - 被: tokenizers (tokenizer.json直読み = Rust tokenizers crateと同一正本)
#         + onnxruntime推論 + attention mask考慮のmean pooling + L2正規化
#   - 同一入力: 日本語/英語mix・query/doc両接頭辞・8192超過長文 (truncation) 計32文
#   - 指標: cos一致の最大絶対誤差 / 平均誤差 (目安: max < 1e-3。超過なら要調査)
#   - 附加検証: パッケージファイルsha256整合・グラフ入出力・次元・
#     transformers AutoTokenizer と tokenizers (tokenizer.json) のid一致
#
# 終了コード: 0=PASS / 1=しきい値超過または整合性エラー
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import sys
from pathlib import Path

import numpy as np


def log(msg: str) -> None:
    print(f"[verify] {msg}", flush=True)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# 32文コーパス。日本語/英語 × query/doc接頭辞を網羅し、境界入力
# (極端に短い文・絵文字/記号・改行/空白混入) と8192トークン超過の長文2件を含む。
QUERY_PREFIX = "検索クエリ: "
DOC_PREFIX = "検索文書: "

JA_SENTENCES = [
    "猫が窓辺で日向ぼっこをしている写真",
    "白いワンピースの少女と夏の向日葵の畑",
    "夜景を背景にした街のネオンサイン",
    "山頂から見た雲海と朝日",
    "古書店の棚に並ぶ背表紙の質感",
    "雨の日の傘と濡れたアスファルトの反射",
    "和室でのお茶と和菓子の静かな構図",
    "雪原地帯を歩く登山者の遠景",
    "厨房で働く料理人の手元の動き",
    "廃墟となった工場の錆びた機械",
    "祭りの夜に打ち上がる花火と群衆",
    "図書館で本を読む学生の横顔",
    "海岸でサーフィンをする人々の朝",
    "市場に並ぶ色鮮やかな果物と野菜",
    "猫",
]

EN_SENTENCES = [
    "a red sports car parked at sunset near the coast",
    "an oil painting style portrait with dramatic lighting",
    "children playing soccer on a green field in spring",
    "a cozy reading nook with warm lamp light and blankets",
    "snow-capped mountains reflected in a still alpine lake",
    "a bustling night market street with food stalls",
    "vintage film photography of a Parisian cafe terrace",
    "astronaut floating outside a space station above Earth",
    "a minimalist workspace with a single monitor and plant",
    "autumn forest path covered with golden fallen leaves",
    "storm clouds gathering over a vast prairie",
    "a detailed macro shot of a dew-covered spider web",
    "wooden sailing ship anchored in a foggy harbor",
    "street art mural on a brick wall in an alley",
    "cafe latte art with a heart shape and croissant 🌸☕ ±½",
]

# 8192トークンを超える長文 (truncation 8192 が効くことの確認用)。
_LONG_JA = "これは切り詰め検証用の長文です。埋め込みモデルは最大入力長を超える文書をトークン列の先頭から切り詰める必要があります。花样年華の記憶、数字の羅列1234567890、そして約物——「」・（！）？も含まれます。" * 220
_LONG_EN = ("This is a deliberately long document used to verify that the tokenizer and the "
            "runtime truncate inputs longer than the maximum sequence length of 8192 tokens. "
            "It mixes terminology like tokenizer, attention mask, mean pooling, and L2 "
            "normalization with plain narrative sentences and numbers 0123456789. ") * 260


def build_corpus() -> list[str]:
    texts: list[str] = []
    for i, s in enumerate(JA_SENTENCES):
        texts.append((QUERY_PREFIX if i % 2 == 0 else DOC_PREFIX) + s)
    for i, s in enumerate(EN_SENTENCES):
        texts.append((DOC_PREFIX if i % 2 == 0 else QUERY_PREFIX) + s)
    texts.append(DOC_PREFIX + _LONG_JA)
    texts.append(QUERY_PREFIX + _LONG_EN)
    assert len(texts) == 32
    return texts


def check_package_integrity(pkg: Path, manifest: dict) -> None:
    for name, meta in manifest["files"].items():
        p = pkg / name
        if not p.is_file():
            raise SystemExit(f"整合性エラー: {name} が存在しません")
        if p.stat().st_size != meta["size_bytes"]:
            raise SystemExit(f"整合性エラー: {name} のサイズがmanifestと不一致")
        actual = sha256_file(p)
        if actual != meta["sha256"]:
            raise SystemExit(f"整合性エラー: {name} のsha256がmanifestと不一致")
    log(f"package integrity: {len(manifest['files'])} files OK")


def check_graph(pkg: Path, expect_dim: int) -> None:
    import onnx

    m = onnx.load(str(pkg / "model.onnx"))
    ins = {i.name for i in m.graph.input}
    outs = {o.name for o in m.graph.output}
    if ins != {"input_ids", "attention_mask"}:
        raise SystemExit(f"グラフ入力が想定外: {sorted(ins)}")
    if outs != {"last_hidden_state"}:
        raise SystemExit(f"グラフ出力が想定外 (pooling混入の疑い): {sorted(outs)}")
    out = m.graph.output[0].type.tensor_type.shape.dim
    if len(out) == 3 and out[2].dim_value not in (0, expect_dim):
        raise SystemExit(f"グラフ出力次元がmanifestと不一致: {out[2].dim_value} != {expect_dim}")
    opset = {op.domain or "ai.onnx": op.version for op in m.opset_import}
    log(f"graph: inputs={sorted(ins)}, outputs={sorted(outs)}, opset={opset}")


def encode_reference(snapshot_dir: Path, texts: list[str], batch_size: int,
                     max_len: int) -> np.ndarray:
    from sentence_transformers import SentenceTransformer

    st = SentenceTransformer(str(snapshot_dir), device="cpu")
    get_dim = getattr(st, "get_embedding_dimension", None) or \
        st.get_sentence_embedding_dimension
    dim = get_dim()
    if dim is None:
        raise SystemExit("SentenceTransformerの次元取得に失敗")
    log(f"reference: ST max_seq_length={st.max_seq_length}, dim={dim}")
    if st.max_seq_length != max_len:
        raise SystemExit(
            f"STのmax_seq_length ({st.max_seq_length}) がmanifestのmax_input_length ({max_len}) と不一致")
    ref = st.encode(
        texts,
        batch_size=batch_size,
        normalize_embeddings=True,
        convert_to_numpy=True,
        show_progress_bar=False,
    )
    return ref.astype(np.float64)


def encode_candidate(pkg: Path, texts: list[str], batch_size: int, max_len: int,
                     long_from: int) -> tuple[np.ndarray, list[int]]:
    import onnxruntime as ort
    from tokenizers import Tokenizer

    tk = Tokenizer.from_file(str(pkg / "tokenizer.json"))
    tk.enable_truncation(max_length=max_len)
    pad_id = tk.token_to_id("<pad>")
    if pad_id is None:
        raise SystemExit("tokenizer.jsonに<pad>が見つかりません")
    tk.enable_padding(pad_id=pad_id, pad_token="<pad>")

    sess = ort.InferenceSession(
        str(pkg / "model.onnx"), providers=["CPUExecutionProvider"])
    log(f"candidate: onnxruntime {ort.__version__}, providers={sess.get_providers()}")

    def run_batch(batch: list[str]) -> np.ndarray:
        seqs = tk.encode_batch(batch)
        ids = np.array([s.ids for s in seqs], dtype=np.int64)
        mask = np.array([s.attention_mask for s in seqs], dtype=np.int64)
        ntoks.extend(int(mask[j].sum()) for j in range(len(seqs)))
        hs = sess.run(["last_hidden_state"],
                      {"input_ids": ids, "attention_mask": mask})[0]
        m = mask[:, :, None].astype(np.float64)
        pooled = (hs.astype(np.float64) * m).sum(axis=1) / np.clip(m.sum(axis=1), 1e-9, None)
        return pooled / np.linalg.norm(pooled, axis=1, keepdims=True)

    embs: list[np.ndarray] = []
    ntoks: list[int] = []
    standard = texts[:long_from]
    for start in range(0, len(standard), batch_size):
        embs.append(run_batch(standard[start:start + batch_size]))
    # 長文は単独実行 (8192トークンのglobal attentionでメモリが跳ねるため)。
    for t in texts[long_from:]:
        embs.append(run_batch([t]))
    return np.concatenate(embs), ntoks


def check_tokenizer_parity(snapshot_dir: Path, pkg: Path, texts: list[str],
                           max_len: int) -> bool:
    """transformers AutoTokenizer (ST内部と同じ読み込み経路) と tokenizers
    (tokenizer.json直読み = Rust crateと同一正本) のid一致確認。

    切り詰めは enable_truncation で設定する (post-processorの特殊トークン付与と
    の整合をtokenizers backendに任せる)。手動スライスは特殊トークンの位置関係
    が変わり不正な比較になる。
    """
    from transformers import AutoTokenizer
    from tokenizers import Tokenizer

    auto = AutoTokenizer.from_pretrained(str(snapshot_dir))
    raw = Tokenizer.from_file(str(pkg / "tokenizer.json"))
    raw.no_padding()
    raw.enable_truncation(max_length=max_len)
    for t in texts:
        a = auto(t, truncation=True, max_length=max_len)["input_ids"]
        r = raw.encode(t).ids
        if a != r:
            log(f"tokenizer parity MISMATCH at: {t[:40]!r}")
            log(f"  auto: {a[:8]}…({len(a)})")
            log(f"  raw : {r[:8]}…({len(r)})")
            return False
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--package", type=Path, required=True,
                    help="変換成果物ディレクトリ (manifest.json含む)")
    ap.add_argument("--cache-dir", type=Path,
                    default=Path("/tmp/opencode/ruri-conv/hf-cache"))
    ap.add_argument("--batch-size", type=int, default=8)
    ap.add_argument("--threshold", type=float, default=1e-3,
                    help="cos誤差の許容最大値 (既定 1e-3。勝手に緩めないこと)")
    ap.add_argument("--json-report", type=Path, default=None)
    args = ap.parse_args()

    manifest = json.loads((args.package / "manifest.json").read_text(encoding="utf-8"))
    check_package_integrity(args.package, manifest)
    check_graph(args.package, manifest["model"]["embedding_dim"])

    from huggingface_hub import snapshot_download
    snap = Path(snapshot_download(
        repo_id=manifest["source"]["repository_id"],
        revision=manifest["source"]["revision"],
        cache_dir=str(args.cache_dir),
    ))
    log(f"reference snapshot: {snap}")

    texts = build_corpus()
    max_len = manifest["model"]["max_input_length"]
    parity = check_tokenizer_parity(snap, args.package, texts, max_len)
    log(f"tokenizer parity (transformers vs tokenizers.json): "
        f"{'OK' if parity else 'MISMATCH'}")

    ref = encode_reference(snap, texts, args.batch_size, max_len)
    cand, ntoks = encode_candidate(args.package, texts, args.batch_size, max_len, long_from=30)
    if ref.shape != cand.shape:
        raise SystemExit(f"出力形状不一致: ref={ref.shape} cand={cand.shape}")

    cos = np.einsum("ij,ij->i", ref, cand)
    abs_err = np.abs(1.0 - cos)
    elem_diff = np.abs(ref - cand)
    max_cos_err = float(abs_err.max())
    mean_cos_err = float(abs_err.mean())
    max_elem = float(elem_diff.max())
    long_tokens = ntoks[30:]
    trunc_ok = all(n == max_len for n in long_tokens)
    dim_ok = ref.shape[1] == manifest["model"]["embedding_dim"]

    log(f"texts: {len(texts)} (long: 2), candidate tokens (long): {long_tokens}")
    log(f"cos abs error: max={max_cos_err:.3e}, mean={mean_cos_err:.3e}")
    log(f"element-wise |diff| on normalized vecs: max={max_elem:.3e}")
    log(f"truncation@{max_len}: {'OK' if trunc_ok else 'FAIL'} {long_tokens}")
    log(f"dim check: {ref.shape[1]} vs manifest "
        f"{manifest['model']['embedding_dim']} -> {'OK' if dim_ok else 'FAIL'}")

    passed = (
        max_cos_err < args.threshold
        and parity and trunc_ok and dim_ok
        and bool(np.isfinite(ref).all() and np.isfinite(cand).all())
    )
    log(f"RESULT: {'PASS' if passed else 'FAIL'} (threshold={args.threshold:g})")

    if args.json_report:
        report = {
            "package": str(args.package),
            "package_name": manifest["package_name"],
            "source_revision": manifest["source"]["revision"],
            "verified_utc": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "n_texts": len(texts),
            "long_text_tokens": long_tokens,
            "tokenizer_parity_ok": parity,
            "dim_ok": dim_ok,
            "metrics": {
                "cos_abs_error_max": max_cos_err,
                "cos_abs_error_mean": mean_cos_err,
                "element_diff_max_on_normalized": max_elem,
                "threshold": args.threshold,
            },
            "pass": passed,
        }
        args.json_report.parent.mkdir(parents=True, exist_ok=True)
        args.json_report.write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        log(f"json report written to {args.json_report}")

    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
