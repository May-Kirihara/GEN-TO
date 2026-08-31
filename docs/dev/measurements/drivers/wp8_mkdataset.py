#!/usr/bin/env python3
"""WP-8 E2E用の合成PNGデータセット生成 (6ファイル)。

全ファイルをユニークなバイト列 (seedごとにピクセル色を変える) にする
こと — 同一バイトだとBLAKE3内容ハッシュでNファイル=1 Assetに統合され、
検索件数が合わなくなる (Asset統合モデルの正しい挙動)。
ファイル名は検索シナリオの題材 (日本語キーワード・sunset)。
"""
import struct, zlib, sys, os


def png(path, w=4, h=4, seed=0):
    px = bytes([(50 + seed * 17) % 256, (60 + seed * 29) % 256, (70 + seed * 43) % 256])
    raw = b"".join(b"\x00" + px * w for _ in range(h))

    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    data = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )
    open(path, "wb").write(data)


names = [
    "青い髪の少女_01.png",
    "青い髪の少女_02.png",
    "sunset_beach.png",
    "night_city.png",
    "portrait_2026.png",
    "landscape.png",
]

for i, n in enumerate(names):
    png(os.path.join(sys.argv[1], n), seed=i)
print("dataset:", len(names), "files")
