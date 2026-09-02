#!/usr/bin/env python3
"""WP-9 E2Eデータセット生成器。

出所記録 ( fixtures生成手段 ):
- **PIL (Pillow 10.2.0) で実PNG/JPEGを生成** (デコード可能な実ファイル —
  プローブ・サムネイル生成も通る)。テキストチャンクは PngInfo.add_text /
  add_itext で挿入 (tEXt/iTXt/zTXt)。
- EXIF (TIFF本体) とIPTC (APP13 8BIM+IIM) のバイト列は本スクリプト内で
  手組み (標準ライブラリのみ)。EXIFはPILの exif= 引数でAPP1へ格納、
  IPTCはSOI直後に手動挿入。
- すべて決定的 (外部実ファイル非依存)。全ファイルユニークバイト
  (BLAKE3統合回避 — WP-8教訓)。

ファイル構成:
  1. sd.png     — tEXt parameters (SD WebUI形式)
  2. comfy.png  — iTXt prompt (ComfyUI API JSON) + iTXt workflow (UI JSON)
  3. nai.png    — tEXt Title/Description/Software/Source/Comment (NovelAI)
  4. photo.jpg  — APP1 EXIF (Make/Model/DateTimeOriginal/GPS/Orientation)
                  + APP13 IPTC (keywords/caption/byline/copyright)
  5. plain.png  — tEXt Title/Comment (一般PNGテキストチャンク)
  6. huge.png   — zTXt 5MiB (1チャンク上限4MiB超過 → スキップ警告の検証)
"""
import io
import json
import struct
import sys
import zlib
from pathlib import Path

from PIL import Image
from PIL.PngImagePlugin import PngInfo


# ===== TIFF EXIFビルダ (LE。IFD0 + Exif IFD + GPS IFD) =====

def tiff_ascii(text: str):
    body = text.encode("ascii") + b"\x00"
    return (2, len(body), body)  # type ASCII


def tiff_short(value: int):
    return (3, 1, struct.pack("<H", value))


def tiff_rationals(values):
    body = b"".join(struct.pack("<II", n, d) for n, d in values)
    return (5, len(values), body)


def build_tiff_exif() -> bytes:
    ifd0 = [
        (0x010F, tiff_ascii("E2ECam")),               # Make
        (0x0110, tiff_ascii("Unit XY")),              # Model
        (0x0112, tiff_short(6)),                      # Orientation
    ]
    exif_ifd = [
        (0x9003, tiff_ascii("2026:09:01 09:15:00")),  # DateTimeOriginal
        (0xA434, tiff_ascii("50mm f/1.8")),           # LensModel
    ]
    gps_ifd = [
        (0x0001, tiff_ascii("N")),
        (0x0002, tiff_rationals([(35, 1), (39, 1), (3100, 100)])),  # 35.6586N
        (0x0003, tiff_ascii("E")),
        (0x0004, tiff_rationals([(139, 1), (44, 1), (4400, 100)])),  # 139.7455E
    ]

    def ifd_size(entries):
        return 2 + len(entries) * 12 + 4

    ifd0_with_ptr = ifd0 + [
        (0x8769, (4, 1, None)),  # ExifIFDPointer (offset後で埋める)
        (0x8825, (4, 1, None)),  # GPSInfoPointer
    ]
    ifd0_off = 8
    exif_off = ifd0_off + ifd_size(ifd0_with_ptr)
    gps_off = exif_off + ifd_size(exif_ifd)
    data_base = gps_off + ifd_size(gps_ifd)

    data = bytearray()
    tiff = bytearray(data_base)
    tiff[0:8] = b"II" + struct.pack("<HI", 0x2A, ifd0_off)

    def emit(base: int, entries):
        """entries: [(tag, (type, count, value|None))]。value=Noneは
        この時点で埋め済みのオフセットLONG前提 (呼び出し側で差し替え)。"""
        entries = sorted(entries, key=lambda e: e[0])
        tiff[base : base + 2] = struct.pack("<H", len(entries))
        for i, (tag, (typ, count, value)) in enumerate(entries):
            at = base + 2 + i * 12
            tiff[at : at + 2] = struct.pack("<H", tag)
            tiff[at + 2 : at + 4] = struct.pack("<H", typ)
            tiff[at + 4 : at + 8] = struct.pack("<I", count)
            if value is None:
                continue
            if len(value) <= 4:
                tiff[at + 8 : at + 8 + len(value)] = value
            else:
                offset = data_base + len(data)
                tiff[at + 8 : at + 12] = struct.pack("<I", offset)
                data.extend(value)

    ifd0_final = ifd0 + [
        (0x8769, (4, 1, struct.pack("<I", exif_off))),
        (0x8825, (4, 1, struct.pack("<I", gps_off))),
    ]
    emit(ifd0_off, ifd0_final)
    emit(exif_off, exif_ifd)
    emit(gps_off, gps_ifd)
    tiff.extend(data)
    return bytes(tiff)


def iim_dataset(record: int, ds: int, value: bytes) -> bytes:
    return bytes([0x1C, record, ds]) + struct.pack(">H", len(value)) + value


def build_iptc_app13() -> bytes:
    iim = b"".join(
        [
            iim_dataset(2, 25, b"e2e-keyword"),
            iim_dataset(2, 80, b"E2E Photographer"),
            iim_dataset(2, 116, b"(c) 2026 e2e"),
            iim_dataset(2, 120, b"E2E IPTC caption"),
        ]
    )
    resource = b"8BIM" + struct.pack(">H", 0x0404) + b"\x00\x00"
    resource += struct.pack(">I", len(iim)) + iim
    if len(iim) % 2 == 1:
        resource += b"\x00"
    return b"Photoshop 3.0\x00" + resource


def jpeg_segment(marker: int, payload: bytes) -> bytes:
    return bytes([0xFF, marker]) + struct.pack(">H", len(payload) + 2) + payload


def build_jpeg() -> bytes:
    """実JPEG (PILエンコード + EXIF APP1 + IPTC APP13手動挿入)。"""
    img = Image.new("RGB", (32, 24))
    for x in range(32):
        for y in range(24):
            img.putpixel((x, y), (x * 8 % 256, y * 10 % 256, 128))
    buf = io.BytesIO()
    img.save(buf, format="JPEG", quality=85, exif=build_tiff_exif())
    jpeg = buf.getvalue()
    # SOI直後にAPP13 (IPTC) を挿入
    return jpeg[:2] + jpeg_segment(0xED, build_iptc_app13()) + jpeg[2:]


def save_png(path: Path, texts: list[tuple[str, str, str]]) -> None:
    """texts: (keyword, text, kind) — kindは "t" (tEXt) / "i" (iTXt) /
    "z" (zTXt)。"""
    img = Image.new("RGBA", (32, 24))
    for x in range(32):
        for y in range(24):
            img.putpixel((x, y), (64 + x * 4, y * 6, 160, 255))
    info = PngInfo()
    for keyword, text, kind in texts:
        if kind == "t":
            info.add_text(keyword, text, zip=False)
        elif kind == "z":
            info.add_text(keyword, text, zip=True)
        else:
            info.add_itxt(keyword, text, zip=False)
    img.save(path, format="PNG", pnginfo=info)


def main() -> None:
    root = Path(sys.argv[1])
    root.mkdir(parents=True, exist_ok=True)

    # 1. sd.png — SD WebUI parameters
    params = (
        "masterpiece, crimson sunset over the sea, 1girl\n"
        "Negative prompt: watermark, lowres\n"
        "Steps: 28, Sampler: Euler a, Schedule type: Karras, CFG scale: 5.5, "
        "Seed: 3141592653589793238, Size: 832x1216, Model hash: aabbccdd11, "
        "Model: animagine xl 3.1, VAE: sdxl_vae.safetensors, Clip skip: 2, "
        "Version: v1.10.0, "
        'Lora hashes: "add-detail-xl:1234abcd, char-loom:5678efab"'
    )
    save_png(root / "sd.png", [("parameters", params, "t")])

    # 2. comfy.png — ComfyUI prompt + workflow (iTXt)
    graph = {
        "1": {"class_type": "CheckpointLoaderSimple",
              "inputs": {"ckpt_name": "e2e-checkpoint.safetensors"}},
        "2": {"class_type": "CLIPTextEncode",
              "inputs": {"text": "golden pagoda at dusk, detailed"}},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry, noisy"}},
        "4": {"class_type": "KSampler",
              "inputs": {"seed": 123456789012, "steps": 30, "cfg": 3.5,
                         "sampler_name": "euler", "scheduler": "simple",
                         "positive": ["2", 0], "negative": ["3", 0], "denoise": 1.0}},
        "5": {"class_type": "LoraLoader",
              "inputs": {"lora_name": "e2e-lora.safetensors", "strength_model": 0.7}},
    }
    workflow = {
        "nodes": [{"id": i, "type": f"node{i}", "pos": [i * 10, i * 20],
                   "mode": 0, "props": {}}
                  for i in range(12)],
        "version": 0.4, "extra": {"ds": 1}, "config": {}, "groups": [],
        "widgets": ["e2e workflow marker" for _ in range(40)],
    }
    save_png(
        root / "comfy.png",
        [
            ("prompt", json.dumps(graph, separators=(",", ":")), "i"),
            ("workflow", json.dumps(workflow, separators=(",", ":")), "i"),
        ],
    )

    # 3. nai.png — NovelAI
    comment = {
        "prompt": "azure ocean, white sails, masterpiece",
        "steps": 23,
        "height": 1216, "width": 832,
        "scale": 6.0,
        "uc": "lowres, bad anatomy",
        "seed": 987654321098765432,
        "sampler": "k_euler",
        "n_samples": 1,
    }
    save_png(
        root / "nai.png",
        [
            ("Title", "AI generated image", "t"),
            ("Description", "azure ocean, white sails, masterpiece", "t"),
            ("Software", "NovelAI", "t"),
            ("Source", "Stable Diffusion XL C1E1DE52", "t"),
            ("Comment", json.dumps(comment, separators=(",", ":")), "t"),
        ],
    )

    # 4. photo.jpg — EXIF + IPTC
    (root / "photo.jpg").write_bytes(build_jpeg())

    # 5. plain.png — 一般テキストチャンク
    save_png(
        root / "plain.png",
        [("Title", "e2e plain text chunk", "t"), ("Comment", "generic tool", "t")],
    )

    # 6. huge.png — 巨大zTXt (上限超過でスキップ警告)
    huge_text = "H" * (5 * 1024 * 1024)
    save_png(root / "huge.png", [("workflow", huge_text, "z")])

    print(f"fixture: 6 files in {root}")


if __name__ == "__main__":
    main()
