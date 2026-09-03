#!/usr/bin/env python3
"""WP-16 性能ゲート用コーパス生成スクリプト。

実画像バイト (PNG/JPEG/WebP混在・多様な寸法) の大規模コーパスを
/tmp 等の作業領域へ生成する。リポジトリへ大容量成果物を置かないため、
出力先は /tmp/opencode/wp16/ を想定 (WP-16制約)。

構成 (既定 50,000件):
  - 寸法: 8〜64MP級は少数、大部分は 0.3〜8MP (実写真コレクション分布を模倣)
  - 形式: PNG 55% / JPEG 40% / WebP 4% / AVIF 0.5% / 非画像・破損 ~1%
  - ディレクトリ: dir000..dir099 (1ディレクトリあたり~500件)
  - JPEG には確率的に EXIF (Orientation含む) を付与

使い方:
  python3 scripts/wp16_gen_corpus.py <出力root> [件数]
  例: python3 scripts/wp16_gen_corpus.py /tmp/opencode/wp16/corpus1k 1000   # ドライヤラン
      python3 scripts/wp16_gen_corpus.py /tmp/opencode/wp16/corpus50k       # 本番

(seeded random — 同一seedで再現可能。Pillow依存)
"""
from __future__ import annotations

import random
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ProcessPoolExecutor
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw

SEED = 20260903
# 実コレクション風の寸法分布 (w, h, 重量)。合計 ~1.0
SIZE_BUCKETS = [
    ((640, 480), 0.20),        # サムネ相当
    ((1280, 720), 0.20),       # HD
    ((1600, 1200), 0.20),      # 2MP
    ((2448, 3264), 0.18),      # 8MP (携帯写真)
    ((3008, 2000), 0.10),      # 6MP DSLR
    ((4000, 3000), 0.08),      # 12MP
    ((5472, 3648), 0.03),      # 20MP
    ((7952, 5304), 0.01),      # 42MP (最大級。スパイクdataset最大7.9MP相当)
]
FORMAT_MIX = [("png", 0.545), ("jpg", 0.40), ("webp", 0.04), ("avif", 0.005), ("bad", 0.01)]

WORDS = ["sunset", "cat", "beach", "mountain", "street", "portrait", "flower", "city",
         "night", "forest", "river", "snow", "dog", "food", "festival", "青い空", "夕焼け"]


def pick_bucket(rng: random.Random):
    r = rng.random()
    acc = 0.0
    for (w, h), weight in SIZE_BUCKETS:
        acc += weight
        if r <= acc:
            # わずかに揺らして同一寸法の塊を避ける
            dw = rng.randint(-32, 32)
            dh = rng.randint(-32, 32)
            return max(64, w + dw), max(64, h + dh)
    return 1600, 1200


def pick_format(rng: random.Random) -> str:
    r = rng.random()
    acc = 0.0
    for name, weight in FORMAT_MIX:
        acc += weight
        if r <= acc:
            return name
    return "png"


def make_image(rng: random.Random, w: int, h: int) -> Image.Image:
    # 写真らしく見せるためグラデーション+図形ノイズ (完全ランダムノイズより
    # 圧縮率・デコード時間が実ファイルに近い)
    base = rng.randint(0, 255)
    img = Image.new("RGB", (w, h), (base, (base * 2) % 255, (base * 3) % 255))
    draw = ImageDraw.Draw(img)
    for _ in range(rng.randint(2, 12)):
        x0, y0 = rng.randint(0, w), rng.randint(0, h)
        r = rng.randint(min(w, h) // 16 + 1, min(w, h) // 3 + 1)
        color = (rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255))
        shape = rng.randint(0, 2)
        if shape == 0:
            draw.ellipse([x0, y0, x0 + r, y0 + r], fill=color)
        elif shape == 1:
            draw.rectangle([x0, y0, x0 + r, y0 + r], fill=color)
        else:
            draw.line([x0, y0, x0 + r, y0 + r], fill=color, width=max(1, r // 8))
    return img


def gen_chunk(args: tuple[str, int, int, int]) -> dict:
    """1ディレクトリ分 (start<=i<end) を生成。seedは (SEED, dir) で決定論的。"""
    out, d, start, end = args
    rng = random.Random(f"{SEED}:{d}")
    dir_out = Path(out) / f"dir{d:03d}"
    dir_out.mkdir(parents=True, exist_ok=True)
    counts = {"png": 0, "jpg": 0, "webp": 0, "avif": 0, "bad": 0}
    total_bytes = 0
    for i in range(start, end):
        fmt = pick_format(rng)
        counts[fmt] += 1
        name = f"{WORDS[i % len(WORDS)]}_{i:06d}"
        path = dir_out / f"{name}.{fmt if fmt != 'bad' else 'png'}"
        if fmt == "bad":
            # 破損: PNGヘッダのみ/途中切り詰め、または非画像バイト
            kind = i % 3
            if kind == 0:
                data = b"\x89PNG\r\n\x1a\n" + b"\x00" * 64  # ヘッダのみ
            elif kind == 1:
                img = make_image(rng, 640, 480)
                buf = BytesIO()
                img.save(buf, "PNG")
                data = buf.getvalue()[: len(buf.getvalue()) // 3]  # 切り詰め
            else:
                data = b"this is not an image at all " * 32  # 非画像 (拡張子.png)
            path.write_bytes(data)
            total_bytes += len(data)
            continue
        w, h = pick_bucket(rng)
        img = make_image(rng, w, h)
        if fmt == "png":
            img.save(path, "PNG")
        elif fmt == "jpg":
            # 一部にEXIF Orientationを付与 (Orientation=6: 90°CW)
            exif = Image.Exif()
            exif[274] = 6 if i % 5 == 0 else 1
            img.save(path, "JPEG", quality=85, exif=exif)
        elif fmt == "webp":
            img.save(path, "WEBP", quality=80)
        elif fmt == "avif":
            # Pillowはpillow-avif-plugin無しではAVIF符号化不能なためffmpeg
            # (libaom-av1)で実AVIFバイトを生成 (~0.7s/枚。失敗時はJPEGへ
            # フォールバックし形式混在は維持)
            try:
                with tempfile.NamedTemporaryFile(suffix=".png", delete=True) as tmp:
                    img.save(tmp, "PNG")
                    tmp.flush()
                    subprocess.run(
                        ["ffmpeg", "-y", "-loglevel", "error", "-i", tmp.name,
                         "-c:v", "libaom-av1", "-still-picture", "1", str(path)],
                        check=True, capture_output=True, timeout=60)
            except Exception:
                img.save(path, "JPEG", quality=85)
        total_bytes += path.stat().st_size
    return {"counts": counts, "bytes": total_bytes}


def main() -> None:
    out = Path(sys.argv[1])
    total = int(sys.argv[2]) if len(sys.argv) > 2 else 50_000
    per_dir = 500
    n_dirs = (total + per_dir - 1) // per_dir
    chunks = []
    for d in range(n_dirs):
        start = d * per_dir
        end = min(start + per_dir, total)
        if start < end:
            chunks.append((str(out), d, start, end))

    started = time.time()
    agg = {"png": 0, "jpg": 0, "webp": 0, "avif": 0, "bad": 0}
    total_bytes = 0
    done_files = 0
    # 8プロセス並列 (ディレクトリ単位。seedはdir決定論 → 並列でも再現可能)
    with ProcessPoolExecutor(max_workers=8) as ex:
        for i, res in enumerate(ex.map(gen_chunk, chunks)):
            for k, v in res["counts"].items():
                agg[k] += v
            total_bytes += res["bytes"]
            done_files += sum(res["counts"].values())
            print(f"  dir {i+1}/{len(chunks)} done ({done_files} files, "
                  f"{total_bytes/1024/1024:.0f} MiB, {time.time()-started:.0f}s)",
                  flush=True)
    elapsed = time.time() - started
    print(f"done: {total} files in {elapsed:.1f}s ({total/elapsed:.0f}/s)")
    print(f"counts: {agg}")
    print(f"total bytes: {total_bytes/1024/1024:.1f} MiB")


if __name__ == "__main__":
    main()
