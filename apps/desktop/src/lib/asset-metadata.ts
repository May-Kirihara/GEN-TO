/**
 * 埋め込み画像メタデータの表示モデル (MVP WP-9)。
 *
 * - backend `asset_metadata_get` のDTO型と、InfoPanel表示用の整形ヘルパー。
 *   永続化の正本はDB (backend)。このモジュールは表示・コピー・折りたたみの
 *   ロジックのみ (vitestで検証可能な純関数として分離)。
 * - 巨大workflowでパネルを止めないため、本文レンダリングは折りたたみ展開時
 *   限定・プレビューは `PREVIEW_LIMIT` 文字で打ち切り (コピーは全文)。
 */

/** parser行DTO (backend `AssetMetadataParserDto`。camelCase)。 */
export type AssetMetadataParserDto = {
  parserName: string;
  parserVersion: string;
  rawExif?: string | null;
  rawIptc?: string | null;
  rawPng?: string | null;
  rawWorkflow?: string | null;
  updatedAt: string;
};

/** 正規化列の統合DTO (複数parser行のマージ結果)。 */
export type AssetMetadataNormalizedDto = {
  takenAt?: string | null;
  cameraMake?: string | null;
  cameraModel?: string | null;
  lens?: string | null;
  gpsLatitude?: number | null;
  gpsLongitude?: number | null;
  orientation?: number | null;
  generator?: string | null;
  generationModel?: string | null;
  vae?: string | null;
  sampler?: string | null;
  scheduler?: string | null;
  steps?: number | null;
  cfgScale?: number | null;
  seed?: string | null;
  positivePrompt?: string | null;
  negativePrompt?: string | null;
  generationExtra?: string | null;
};

/** `asset_metadata_get` 応答 (backend `AssetMetadataDto`)。 */
export type AssetMetadataDto = {
  id: string;
  parsers: AssetMetadataParserDto[];
  normalized: AssetMetadataNormalizedDto;
};

/** parser名の表示ラベル。 */
export const PARSER_LABELS: Record<string, string> = {
  exif: "EXIF",
  iptc: "IPTC",
  "png-text": "PNGテキストチャンク",
  "sd-params": "SD WebUI parameters",
  comfy: "ComfyUI",
  nai: "NovelAI",
};

export function parserLabel(name: string): string {
  return PARSER_LABELS[name] ?? name;
}

/** 折りたたみ展開時にレンダリングするプレビュー上限 (文字数)。 */
export const PREVIEW_LIMIT = 4096;

/** GPS座標の表示形式 (十進度・方位付き)。null = 座標なし。 */
export function formatGps(lat?: number | null, lon?: number | null): string | null {
  if (lat === null || lat === undefined || lon === null || lon === undefined) {
    return null;
  }
  if (!Number.isFinite(lat) || !Number.isFinite(lon)) {
    return null;
  }
  const ns = lat >= 0 ? "N" : "S";
  const ew = lon >= 0 ? "E" : "W";
  return `${Math.abs(lat).toFixed(6)}°${ns}, ${Math.abs(lon).toFixed(6)}°${ew}`;
}

/** EXIF Orientationの表示名 (1..=8。不正値はnull)。 */
export function orientationLabel(value?: number | null): string | null {
  const labels: Record<number, string> = {
    1: "通常 (1)",
    2: "左右反転 (2)",
    3: "180°回転 (3)",
    4: "上下反転 (4)",
    5: "転置 (5)",
    6: "時計回り90° (6)",
    7: "逆転置 (7)",
    8: "反時計回り90° (8)",
  };
  if (value === null || value === undefined) {
    return null;
  }
  return labels[value] ?? null;
}

/** 写真メタデータ節の表示行 (値のあるもののみ)。 */
export type MetaRow = { label: string; value: string; title?: string };

/** 写真メタデータ節 (撮影日時・カメラ・レンズ・GPS・Orientation)。 */
export function photoRows(n: AssetMetadataNormalizedDto): MetaRow[] {
  const rows: MetaRow[] = [];
  if (n.takenAt) {
    rows.push({ label: "撮影日時", value: n.takenAt, title: "EXIF DateTimeOriginal" });
  }
  const camera = [n.cameraMake, n.cameraModel].filter((v) => v).join(" ");
  if (camera) {
    rows.push({ label: "カメラ", value: camera });
  }
  if (n.lens) {
    rows.push({ label: "レンズ", value: n.lens });
  }
  const gps = formatGps(n.gpsLatitude, n.gpsLongitude);
  if (gps !== null) {
    rows.push({ label: "GPS", value: gps });
  }
  const orientation = orientationLabel(n.orientation);
  if (orientation !== null) {
    rows.push({ label: "向き", value: orientation });
  }
  return rows;
}

/** 生成メタデータ節 (ツール・モデル・sampler/steps/cfg/seed等)。 */
export function generationRows(n: AssetMetadataNormalizedDto): MetaRow[] {
  const rows: MetaRow[] = [];
  if (n.generator) {
    rows.push({ label: "ツール", value: n.generator });
  }
  if (n.generationModel) {
    rows.push({ label: "モデル", value: n.generationModel });
  }
  if (n.vae) {
    rows.push({ label: "VAE", value: n.vae });
  }
  if (n.sampler) {
    const scheduler = n.scheduler ? ` (${n.scheduler})` : "";
    rows.push({ label: "Sampler", value: `${n.sampler}${scheduler}` });
  } else if (n.scheduler) {
    rows.push({ label: "Scheduler", value: n.scheduler });
  }
  if (n.steps !== null && n.steps !== undefined) {
    rows.push({ label: "Steps", value: String(n.steps) });
  }
  if (n.cfgScale !== null && n.cfgScale !== undefined) {
    rows.push({ label: "CFG scale", value: String(n.cfgScale) });
  }
  if (n.seed) {
    rows.push({ label: "Seed", value: n.seed, title: "数値範囲を保持するため文字列" });
  }
  return rows;
}

/** プレビュー打ち切り結果。コピーは常に全文。 */
export type Preview = {
  text: string;
  truncated: boolean;
  /** 全文のUTF-16コード単位長 (表示用「全体 N 文字」)。 */
  totalLength: number;
};

/** 長文のプレビュー (PREVIEW_LIMIT文字で打ち切り)。 */
export function preview(text: string, limit: number = PREVIEW_LIMIT): Preview {
  const totalLength = text.length;
  if (totalLength <= limit) {
    return { text, truncated: false, totalLength };
  }
  return {
    text: text.slice(0, limit),
    truncated: true,
    totalLength,
  };
}

/** parser行のrawスロット一覧 (ラベル付き。値のあるもののみ)。 */
export function rawSlots(p: AssetMetadataParserDto): MetaRow[] {
  const rows: MetaRow[] = [];
  if (p.rawExif) {
    rows.push({ label: "EXIF", value: p.rawExif });
  }
  if (p.rawIptc) {
    rows.push({ label: "IPTC", value: p.rawIptc });
  }
  if (p.rawPng) {
    rows.push({ label: "PNGテキスト", value: p.rawPng });
  }
  if (p.rawWorkflow) {
    rows.push({ label: "workflow", value: p.rawWorkflow });
  }
  return rows;
}

/** workflow JSONのコピー対象テキスト (ComfyUI: raw_workflowをそのまま)。 */
export function workflowCopyText(dto: AssetMetadataDto | null): string | null {
  if (dto === null) {
    return null;
  }
  for (const p of dto.parsers) {
    if (p.rawWorkflow) {
      return p.rawWorkflow;
    }
  }
  return null;
}

/** 表示すべき節があるか (InfoPanelの節単位表示判定)。 */
export function hasPhotoSection(dto: AssetMetadataDto | null): boolean {
  return dto !== null && photoRows(dto.normalized).length > 0;
}

export function hasGenerationSection(dto: AssetMetadataDto | null): boolean {
  if (dto === null) {
    return false;
  }
  return (
    generationRows(dto.normalized).length > 0 ||
    Boolean(dto.normalized.positivePrompt || dto.normalized.negativePrompt) ||
    dto.parsers.some((p) => p.rawWorkflow)
  );
}

export function hasRawSection(dto: AssetMetadataDto | null): boolean {
  return dto !== null && dto.parsers.some((p) => rawSlots(p).length > 0);
}
