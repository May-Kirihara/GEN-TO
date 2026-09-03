#!/usr/bin/env python3
"""WP-16 S4/S5用fixture生成 (100MP級・破損・不正寸法・巨大メタデータ)。

出力先は引数ディレクトリ (リポジトリ外:/tmp/opencode/wp16/fixtures を想定)。
生成物 (計5ファイル):
  - photo100mp.png  : 12000x8500 PNG (102MP)
  - photo100mp.jpg  : 12000x8500 JPEG (102MP)
  - corrupt_header.png : PNGシグネチャ直後に壊れたIHDR (デコードエラー)
  - fake_dims.png   : IHDR改竄で 30000x30000 を主張する最小PNG (寸法虚偽)
  - huge_text.png   : 50MBのtEXtチャンクを持つ正常PNG

使い方: python3 scripts/wp16_gen_fixtures.py <出力dir>
(Pillow依存。fake_dimsはPNGチャンク手組みのため追加依存なし)
"""
from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

from PIL import Image


def png_chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    w, h = 12000, 8500
    # 102MP。グラデーション+数図形 (完全単色だとJPEG圧縮が非現実的に小さくなる)
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(0, h, 4):  # 4px刻みの帯グラデーション (生成時間抑えつつ非平坦)
        c = (y * 255 // h, (y * 7) % 255, 255 - y * 255 // h)
        for x in range(0, w, 64):
            for yy in range(y, min(y + 4, h)):
                for xx in range(x, min(x + 64, w)):
                    px[xx, yy] = c
    img.save(out / "photo100mp.png", "PNG", compress_level=1)
    img.save(out / "photo100mp.jpg", "JPEG", quality=90)
    print("100MP:", (out / "photo100mp.png").stat().st_size // 1024, "KiB png,",
          (out / "photo100mp.jpg").stat().st_size // 1024, "KiB jpg")

    # 破損ヘッダ: シグネチャ+IHDR後のIDATが途中で切れている
    small = Image.new("RGB", (640, 480), (10, 200, 30))
    raw = small.tobytes()
    idat = zlib.compress(raw, 6)
    ihdr = struct.pack(">IIBBBBB", 640, 480, 8, 2, 0, 0, 0)
    data = (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
            + png_chunk(b"IDAT", idat[:len(idat) // 2])[:8])  # チャンク途中で切断
    (out / "corrupt_header.png").write_bytes(data)

    # 寸法虚偽: IHDRが 30000x30000 を主張する最小構造 (実IDATは64x48相当)
    fake_ihdr = struct.pack(">IIBBBBB", 30000, 30000, 8, 2, 0, 0, 0)
    small2 = Image.new("RGB", (64, 48), (200, 10, 30))
    idat2 = zlib.compress(small2.tobytes(), 6)
    data = (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", fake_ihdr)
            + png_chunk(b"IDAT", idat2) + png_chunk(b"IEND", b""))
    (out / "fake_dims.png").write_bytes(data)

    # 巨大tEXt (50MB): 正常な1x1 PNGに50MBのメタデータチャンクを付与
    # (PNG走査行は「フィルタバイト+ピクセル列」 — tobytesのみでは不足)
    one = Image.new("RGB", (1, 1), (5, 5, 5))
    raw = b"\x00" + one.tobytes()  # filter byte 0 (None)
    normal = (b"\x89PNG\r\n\x1a\n"
              + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
              + png_chunk(b"IDAT", zlib.compress(raw, 6))
              + png_chunk(b"tEXt", b"Comment\x00" + b"A" * (50 * 1024 * 1024))
              + png_chunk(b"IEND", b""))
    (out / "huge_text.png").write_bytes(normal)
    for name in ("corrupt_header.png", "fake_dims.png", "huge_text.png"):
        print(name, (out / name).stat().st_size // 1024, "KiB")


if __name__ == "__main__":
    main()
