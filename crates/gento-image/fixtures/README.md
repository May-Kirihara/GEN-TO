# テストfixtureの生成手段 (MVP WP-3 / AVIF-1)

このcrateのテストは**外部から画像ファイルを取得しない**。すべてテストコード内で
自己生成する (規約: `sample/dataset`非依存・tempdirでの完結)。本READMEは
各形式の生成手段と、実ファイルfixtureの出所・license記録規則を明記する。

## 自己生成fixture (コード内生成)

| 形式 | 生成手段 | 使用テスト |
|---|---|---|
| PNG | `probe::minimal_png_bytes` (IHDR+IENDのみの自前バイナリ。CRC32計算込み) / `image` crateエンコーダ (`DynamicImage::save`) | probe / hash / fs / thumbnail / orientation |
| JPEG | `image` crate (zuneエンコーダ) の `save_with_format` / `write_to` | format / thumbnail / orientation |
| GIF | `image::codecs::gif::GifEncoder` (`encode_frame` / `encode_frames` + `Repeat`) | format / thumbnail |
| BMP | `image` crateの `save` | format / thumbnail |
| TIFF | `image` crateの `save` | format / thumbnail |
| WebP (静止画) | `webp` crate (`Encoder::from_rgba().encode_lossless()`) | format / thumbnail |
| WebP (アニメ・構造のみ) | 手組みRIFF (VP8X+ANIM+ANMF。フレーム本体はダミー) | format (チャンク走査検証のみ) |
| AVIF (magicのみ) | magic手組み (`ftyp`ブランド) | format (magic判定) |
| EXIF付きTIFF | 手組みTIFF (IFD0にOrientation tag 0x0112のみ) | orientation |
| EXIF付きJPEG | 手組みAPP1セグメント (上記TIFFを `Exif\0\0` ヘッダで包む) | orientation |
| EXIF付きPNG | 手組み `eXIf` チャンク (CRC込み) | orientation |
| EXIF付きWebP | 手組み `EXIF` チャンク (RIFF) | orientation |

## 実ファイルfixture (AVIF-1で追加)

AVIFはコード内生成手段が無い (Rust側にAV1エンコーダがなく、`image` の
`avif` feature=ravifは重量級かつ導入しない方針) ため、外部ツールで**自己生成**した
実ファイルを配置する。**外部からダウンロードした画像は含まない。**

| ファイル | 内容 | サイズ | 生成コマンド (2026-08-31) |
|---|---|---|---|
| `still-yuv420.avif` | 400x260 不透過 yuv420p still。brand `avif` | 2,399 B | `ffmpeg -f lavfi -i testsrc2=size=400x260:duration=1:rate=1 -frames:v 1 -c:v libaom-av1 -still-picture 1 -pix_fmt yuv420p -crf 40 -cpu-used 8 still-yuv420.avif` |
| `still-alpha.avif` | 80x60 左上4分の1透過 (alpha aux item `urn:mpeg:mpegB:cicp:systems:auxiliary:alpha` 入り)。brand `avif` | 575 B | `convert -size 80x60 gradient:blue-red -alpha set -region 20x15+0+0 -channel A -evaluate set 0 +channel PNG32:alpha-src.png` → `convert alpha-src.png -quality 60 still-alpha.avif` |
| `animated-avis.avif` | 96x64 3フレームのアニメーション (シーケンス)。brand `avis` | 1,757 B | `ffmpeg -f lavfi -i testsrc2=size=96x64:duration=0.3:rate=10 -c:v libaom-av1 -pix_fmt yuv420p -crf 45 -cpu-used 8 animated-avis.avif` |

- 生成環境: Ubuntu 24.04 / ffmpeg 6.1.1 (libaom-av1エンコーダ + avif muxer) /
  ImageMagick 6.9.12 (libheif 1.17.6 + aomenc plugin経由のAVIF書き込み)。
  **ffmpeg 6.1のavif muxerはalpha aux itemを書かないため、alpha付きは
  ImageMagick (libheif) を使用**した (正規のalpha aux item構造を持つ)。
- 入力ソース: `testsrc2` (ffmpeg内蔵テストパターン) / `gradient:` (ImageMagick内蔵
  グラデーション)。いずれも外部画像を用いない**完全自己生成**。
- license: **自己生成データであるため著作権上の制約なし** (配布自由)。
- 使用テスト: `format.rs` (probe 7形式化・alpha維持・avisアニメ判定・
  切り詰め安全性・変異fuzz) / `thumbnail.rs` (7形式サムネイル生成・変異fuzz・
  E2Eデータセット)。

## 変異・切り詰め (fuzz相当)

固定シードの簡易LCG (乗法合同法、`format.rs` と `thumbnail.rs` に同一実装)
でバイト反転位置・個数を決定する。外部依存・ランダム性なし (再現可能)。

- `format::tests::random_mutations_never_panic` — プローブのpanicなし
- `thumbnail::tests::mutated_inputs_never_panic_decode` — デコード→リサイズ→
  エンコード全体のpanicなし
- 巨大寸法詐称 (100000x100000 PNG/BMP、65500x65500 JPEG) → Limits拒否
- AVIFは寸法詐称の実bitstreamが作成不能なため、変異fuzz (dav1dの
  エラー処理) と切り詰めで安全性を担保する

## 実ファイルfixtureを追加する場合の規則

1. `fixtures/` ディレクトリに配置し、本READMEへ**出所URL・取得日・license**を記録する
2. 再配布可能なlicense (CC0 / パブリックドメイン相当 / 自己生成) のみ配置する
3. バイナリサイズは最小限に保つ (数百バイト〜数KiB)
4. 生成コードで代替可能な場合は実ファイルを追加しない (AVIFはコード内
   生成手段が無いためこの限りでない)
