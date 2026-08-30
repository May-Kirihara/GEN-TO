# テストfixtureの生成手段 (MVP WP-3)

このcrateのテストは**外部から画像ファイルを取得しない**。すべてテストコード内で
自己生成する (規約: `sample/dataset`非依存・tempdirでの完結)。本READMEは
各形式の生成手段と、将来実ファイルfixtureを追加する場合の出所・license記録規則を
明記する。

## 自己生成fixture (現状)

| 形式 | 生成手段 | 使用テスト |
|---|---|---|
| PNG | `probe::minimal_png_bytes` (IHDR+IENDのみの自前バイナリ。CRC32計算込み) / `image` crateエンコーダ (`DynamicImage::save`) | probe / hash / fs / thumbnail / orientation |
| JPEG | `image` crate (zuneエンコーダ) の `save_with_format` / `write_to` | format / thumbnail / orientation |
| GIF | `image::codecs::gif::GifEncoder` (`encode_frame` / `encode_frames` + `Repeat`) | format / thumbnail |
| BMP | `image` crateの `save` | format / thumbnail |
| TIFF | `image` crateの `save` | format / thumbnail |
| WebP (静止画) | `webp` crate (`Encoder::from_rgba().encode_lossless()`) | format / thumbnail |
| WebP (アニメ・構造のみ) | 手組みRIFF (VP8X+ANIM+ANMF。フレーム本体はダミー) | format (チャンク走査検証のみ) |
| AVIF | magicのみ手組み (`ftyp`ブランド)。実デコードはavif-native不在のため対象外 | format (magic判定) |
| EXIF付きTIFF | 手組みTIFF (IFD0にOrientation tag 0x0112のみ) | orientation |
| EXIF付きJPEG | 手組みAPP1セグメント (上記TIFFを `Exif\0\0` ヘッダで包む) | orientation |
| EXIF付きPNG | 手組み `eXIf` チャンク (CRC込み) | orientation |
| EXIF付きWebP | 手組み `EXIF` チャンク (RIFF) | orientation |

## 変異・切り詰め (fuzz相当)

固定シードの簡易LCG (乗法合同法、`format.rs` と `thumbnail.rs` に同一実装)
でバイト反転位置・個数を決定する。外部依存・ランダム性なし (再現可能)。

- `format::tests::random_mutations_never_panic` — プローブのpanicなし
- `thumbnail::tests::mutated_inputs_never_panic_decode` — デコード→リサイズ→
  エンコード全体のpanicなし
- 巨大寸法詐称 (100000x100000 PNG/BMP、65500x65500 JPEG) → Limits拒否

## 実ファイルfixtureを追加する場合の規則

1. `fixtures/` ディレクトリに配置し、本READMEへ**出所URL・取得日・license**を記録する
2. 再配布可能なlicense (CC0 / パブリックドメイン相当) のみ配置する
3. バイナリサイズは最小限に保つ (数百バイト〜数KiB)
4. 生成コードで代替可能な場合は実ファイルを追加しない (現状この方針で全形式を賄っている)
