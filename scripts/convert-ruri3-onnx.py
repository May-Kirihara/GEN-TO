#!/usr/bin/env python3
# ruri-v3 ONNX変換パイプライン (CONV-1, WP-11アンブロック用)。
#
# 使い方:
#   scripts/convert-ruri3-onnx.py [--models 70m 30m] [--out-root DIR]
#       [--cache-dir DIR] [--method auto|optimum|torch] [--opset 17]
#       [--license-path FILE] [--combined-manifest FILE]
#
# 前提:
#   - 変換環境 (torch/sentence-transformers/optimum/onnx/onnxruntime) は
#     docs/dev/conversion-env.md の手順で構築すること (アプリには同梱しない)。
#   - Apache-2.0 本文を --license-path へ渡す (再配布の正本として配置する)。
#     例: curl -o /tmp/opencode/ruri-conv/Apache-2.0.txt \
#             https://www.apache.org/licenses/LICENSE-2.0.txt
#
# 仕様 (docs/embedding-runtime.md §3 配布物構成 / §6 推論方式):
#   - HF safetensors を revision 固定で取得し、transformer部のみ ONNX へ出力する。
#     pooling (attention mask考慮のmean) と L2正規化はグラフに含めない
#     (§5/§6: ワーカー側で実施)。
#   - 出力は last_hidden_state。入力は input_ids / attention_mask (動的軸)。
#   - 変換方式: 第1候補 optimum export (task=feature-extraction)。
#     失敗時は第2候補 torch.onnx.export (legacy, opset 17以上) へフォールバック。
#   - 成果物 (model.onnx 等の実ファイル) はリポジトリへコミットしない。
#     checksum・メタデータは manifest.json / 結合manifest に記録する。
#
# モデル固定情報の出典: docs/handoff/2026-09-02-mvp-wp11.md (2026-09-02実測)。
from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import importlib.metadata as md
import json
import shutil
import sys
import tempfile
from pathlib import Path

SCRIPT_VERSION = "1.0.0"
# 変換スクリプトが依存するリポジトリの基盤コミット (manifest記録用)。
BASE_COMMIT = "477a839"

# 前回調査 (2026-09-02) で固定した revision と実測値。変更はPM承認要件。
@dataclasses.dataclass(frozen=True)
class ModelSpec:
    key: str
    repo_id: str
    revision: str
    embedding_dim: int
    max_input_length: int
    safetensors_sha256: str
    safetensors_size: int


MODELS: dict[str, ModelSpec] = {
    # safetensors_sha256 は HF tree API (lfs.oid) / resolve応答の X-Linked-ETag で
    # 確認した実sha256。handoff 2026-09-02-mvp-wp11.md 記載の etag 値は Xet CAS
    # ハッシュ (sha256ではない) だったため訂正 (2026-09-02 再実測)。
    "70m": ModelSpec(
        key="70m",
        repo_id="cl-nagoya/ruri-v3-70m",
        revision="07a8b0aba47d29d2ca21f89b915c1efe2c23d1cc",
        embedding_dim=384,
        max_input_length=8192,
        safetensors_sha256="c959b76a535eee838b1f1e6b8999ba513ef8d87a6f8c3534c50dcda1eb5f6a62",
        safetensors_size=280_019_040,
    ),
    "30m": ModelSpec(
        key="30m",
        repo_id="cl-nagoya/ruri-v3-30m",
        revision="24899e5de370b56d179604a007c0d727bf144504",
        embedding_dim=256,
        max_input_length=8192,
        safetensors_sha256="e94155e342cbfc33280c345c6e2912905fb5de6e1a3adeb938f395aafd2f8832",
        safetensors_size=146_828_152,
    ),
}

# ruri-v3 README「1+3 prefix scheme」より (設計§6でquery/docに採用)。
QUERY_PREFIX = "検索クエリ: "
DOCUMENT_PREFIX = "検索文書: "

# §3 配布物構成の過剰コピーを避けるため、snapshotから取り込むファイルを固定する。
COPY_FILES = ("tokenizer.json", "tokenizer_config.json", "config.json")


def log(msg: str) -> None:
    print(f"[convert] {msg}", flush=True)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def package_version_of(dist: str) -> str:
    try:
        return md.version(dist)
    except md.PackageNotFoundError:
        return "not-installed"


def fetch_snapshot(spec: ModelSpec, cache_dir: Path) -> Path:
    """revision固定でsnapshot取得。HFへの取得は各ファイル1回 (hubキャッシュ利用)。"""
    from huggingface_hub import snapshot_download

    log(f"{spec.repo_id}: snapshot_download (revision={spec.revision[:12]}…)")
    snap = Path(snapshot_download(
        repo_id=spec.repo_id,
        revision=spec.revision,
        cache_dir=str(cache_dir),
    ))
    # 前回調査実測値との突合 (改竄・誤revision検出)。
    st = snap / "model.safetensors"
    actual_size = st.stat().st_size
    actual_sha = sha256_file(st)
    log(f"  model.safetensors: {actual_size} bytes, sha256={actual_sha[:16]}…")
    if actual_size != spec.safetensors_size:
        raise RuntimeError(
            f"{spec.repo_id}: safetensors size mismatch: expected {spec.safetensors_size}, got {actual_size}"
        )
    if actual_sha != spec.safetensors_sha256:
        raise RuntimeError(
            f"{spec.repo_id}: safetensors sha256 mismatch: expected {spec.safetensors_sha256}, got {actual_sha}"
        )
    return snap


def load_export_model(spec: ModelSpec, snap: Path):
    """export用にtransformer部をeager attentionでロード (pooling層なし)。"""
    from transformers import AutoModel

    model = AutoModel.from_pretrained(str(snap), attn_implementation="eager")
    model.eval()
    return model


def export_with_optimum(spec: ModelSpec, snap: Path, out_onnx: Path, opset: int) -> str:
    """第1候補: optimum export (task=feature-extraction, last_hidden_state出力)。

    library_name="transformers" 必須: 指定しない場合 optimum は modules.json から
    libraryをsentence_transformersと推定し、pooling込みのグラフ
    (sentence_embedding出力) をexportしてしまう (設計§5/§6違反)。
    """
    from optimum.exporters.onnx import main_export

    with tempfile.TemporaryDirectory(prefix="optimum-export-") as td:
        main_export(
            model_name_or_path=str(snap),
            output=td,
            task="feature-extraction",
            opset=opset,
            trust_remote_code=False,
            library_name="transformers",
        )
        produced = Path(td) / "model.onnx"
        if not produced.is_file():
            # optimumが別名 (e.g. model_quantized.onnx) を出力した場合も想定して列挙。
            candidates = sorted(Path(td).glob("*.onnx"))
            raise RuntimeError(
                f"optimum export produced no model.onnx (candidates: {[p.name for p in candidates]})"
            )
        shutil.copy2(produced, out_onnx)
    return "optimum-export"


def export_with_torch(spec: ModelSpec, snap: Path, out_onnx: Path, opset: int) -> str:
    """第2候補: torch.onnx.export (legacy exporter / 動的軸 / opset 17以上)。"""
    import torch
    from tokenizers import Tokenizer

    class Wrapper(torch.nn.Module):
        def __init__(self, model):
            super().__init__()
            self.model = model

        def forward(self, input_ids, attention_mask):
            out = self.model(input_ids=input_ids, attention_mask=attention_mask)
            return out.last_hidden_state

    model = load_export_model(spec, snap)
    wrapper = Wrapper(model)

    # トレース入力のトークナイズは正本tokenizer.jsonを直接使う
    # (AutoTokenizerはadd_prefix_space明示時にfrom_slow経由となる癖がある)。
    tk = Tokenizer.from_file(str(snap / "tokenizer.json"))
    enc = tk.encode_batch(["検索クエリ: 変換のトレース用短文"])
    pad_id = tk.token_to_id("<pad>")
    if pad_id is None:
        pad_id = 0
    maxlen = max(len(e.ids) for e in enc)
    input_ids = torch.tensor(
        [e.ids + [pad_id] * (maxlen - len(e.ids)) for e in enc], dtype=torch.int64)
    attention_mask = torch.tensor(
        [e.attention_mask + [0] * (maxlen - len(e.attention_mask)) for e in enc],
        dtype=torch.int64)

    with torch.no_grad():
        torch.onnx.export(
            wrapper,
            (input_ids, attention_mask),
            str(out_onnx),
            input_names=["input_ids", "attention_mask"],
            output_names=["last_hidden_state"],
            dynamic_axes={
                "input_ids": {0: "batch", 1: "sequence"},
                "attention_mask": {0: "batch", 1: "sequence"},
                "last_hidden_state": {0: "batch", 1: "sequence"},
            },
            opset_version=opset,
            do_constant_folding=True,
            dynamo=False,  # legacy exporter (transformer実績多い)
        )
    return "torch-onnx-export"


def write_notice(spec: ModelSpec, method: str, opset: int) -> str:
    return f"""This package contains an ONNX conversion of the ruri-v3 model.

Source model:     {spec.repo_id} (Hugging Face)
Source revision:  {spec.revision}
Source license:   Apache License 2.0 (full text in LICENSE)
Upstream weights: model.safetensors sha256 {spec.safetensors_sha256}

Conversion:       scripts/convert-ruri3-onnx.py v{SCRIPT_VERSION}
                  (method: {method}, opset {opset}; fp32 re-export of the
                  original weights, no retraining or quantization)

The ONNX graph covers the transformer forward pass only and outputs
last_hidden_state for inputs (input_ids, attention_mask). Attention-mask-aware
mean pooling and L2 normalization are NOT part of the graph and must be applied
by the inference runtime (see docs/embedding-runtime.md sections 5-6 in the
GEN-TO repository).
"""


def build_package(spec: ModelSpec, snap: Path, out_root: Path, method: str, opset: int,
                  license_text: str) -> dict:
    """§3構成の配布物を組み立て、manifest内容 (dict) を返す。"""
    pkg_dir = out_root / f"ruri-v3-{spec.key}"
    if pkg_dir.exists():
        shutil.rmtree(pkg_dir)
    pkg_dir.mkdir(parents=True)

    onnx_path = pkg_dir / "model.onnx"
    chosen = method
    if method == "auto":
        errors: list[str] = []
        for name, fn in (("optimum", export_with_optimum), ("torch", export_with_torch)):
            try:
                log(f"{spec.key}: trying export method '{name}'")
                chosen = fn(spec, snap, onnx_path, opset)
                break
            except Exception as e:  # noqa: BLE001 — フォールバック判定のため広く捕捉
                errors.append(f"{name}: {type(e).__name__}: {e}")
                log(f"{spec.key}: method '{name}' failed: {e}")
                if onnx_path.exists():
                    onnx_path.unlink()
        else:
            raise RuntimeError(f"{spec.key}: all export methods failed: {errors}")
    elif method == "optimum":
        chosen = export_with_optimum(spec, snap, onnx_path, opset)
    elif method == "torch":
        chosen = export_with_torch(spec, snap, onnx_path, opset)
    else:
        raise ValueError(f"unknown method: {method}")
    log(f"{spec.key}: exported via {chosen} ({onnx_path.stat().st_size:,} bytes)")

    for name in COPY_FILES:
        shutil.copy2(snap / name, pkg_dir / name)
    (pkg_dir / "LICENSE").write_text(license_text, encoding="utf-8")
    (pkg_dir / "NOTICE").write_text(
        write_notice(spec, chosen, opset), encoding="utf-8")

    files: dict[str, dict] = {}
    for p in sorted(pkg_dir.iterdir()):
        if p.name == "manifest.json":
            continue
        files[p.name] = {
            "sha256": sha256_file(p),
            "size_bytes": p.stat().st_size,
        }

    manifest = {
        "package_name": f"ruri-v3-{spec.key}-onnx",
        "package_version": 1,
        "created_utc": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "source": {
            "repository_id": spec.repo_id,
            "revision": spec.revision,
            "license": "Apache-2.0",
            "model_safetensors_sha256": spec.safetensors_sha256,
        },
        "conversion": {
            "script": "scripts/convert-ruri3-onnx.py",
            "script_version": SCRIPT_VERSION,
            "base_commit": BASE_COMMIT,
            "method": chosen,
            "onnx_opset": opset,
            "precision": "fp32",
        },
        "model": {
            "model_type": "modernbert",
            "architecture": "ModernBertModel",
            "embedding_dim": spec.embedding_dim,
            "max_input_length": spec.max_input_length,
            "distance_metric": "cosine",
            "pooling": "mean_tokens",
            "normalization": "l2",
            "query_prefix": QUERY_PREFIX,
            "document_prefix": DOCUMENT_PREFIX,
        },
        "runtime": {
            # 検証に用いたonnxruntime版。「必要最低版」の確定はWP-11 (ort crate採用時)。
            "verified_onnxruntime_version": package_version_of("onnxruntime"),
        },
        "files": files,
    }
    (pkg_dir / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    log(f"{spec.key}: package written to {pkg_dir}")
    return manifest


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--models", nargs="+", choices=sorted(MODELS), default=["70m", "30m"])
    ap.add_argument("--out-root", type=Path,
                    default=Path("/tmp/opencode/ruri-conv/out"))
    ap.add_argument("--cache-dir", type=Path,
                    default=Path("/tmp/opencode/ruri-conv/hf-cache"))
    ap.add_argument("--method", choices=("auto", "optimum", "torch"), default="auto")
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--license-path", type=Path, required=True,
                    help="Apache-2.0本文のパス (配布物のLICENSEとして配置)")
    ap.add_argument("--combined-manifest", type=Path, default=None,
                    help="両モデル分を結合したmanifestの書き出し先 (docs/dev/ruri3-onnx-manifest.json)")
    args = ap.parse_args()

    license_text = args.license_path.read_text(encoding="utf-8")
    if "Apache License" not in license_text or "Version 2.0, January 2004" not in license_text:
        raise SystemExit("--license-path は Apache-2.0 本文である必要があります")

    args.out_root.mkdir(parents=True, exist_ok=True)
    args.cache_dir.mkdir(parents=True, exist_ok=True)

    env = {
        "python": sys.version.split()[0],
        "torch": package_version_of("torch"),
        "transformers": package_version_of("transformers"),
        "sentence-transformers": package_version_of("sentence-transformers"),
        "optimum": package_version_of("optimum"),
        "optimum-onnx": package_version_of("optimum-onnx"),
        "onnx": package_version_of("onnx"),
        "onnxruntime": package_version_of("onnxruntime"),
        "tokenizers": package_version_of("tokenizers"),
    }
    log(f"environment: {json.dumps(env)}")

    manifests: dict[str, dict] = {}
    for key in args.models:
        spec = MODELS[key]
        snap = fetch_snapshot(spec, args.cache_dir)
        manifests[f"ruri-v3-{key}"] = build_package(
            spec, snap, args.out_root, args.method, args.opset, license_text)

    if args.combined_manifest:
        combined = {
            "schema": "gento-ruri3-onnx-manifest",
            "schema_version": 1,
            "generated_utc": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "conversion_script": {
                "path": "scripts/convert-ruri3-onnx.py",
                "version": SCRIPT_VERSION,
                "base_commit": BASE_COMMIT,
                "environment": env,
            },
            "packages": manifests,
        }
        args.combined_manifest.parent.mkdir(parents=True, exist_ok=True)
        args.combined_manifest.write_text(
            json.dumps(combined, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        log(f"combined manifest written to {args.combined_manifest}")

    log("done")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
