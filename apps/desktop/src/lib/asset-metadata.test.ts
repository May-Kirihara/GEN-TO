import { describe, expect, test } from "vitest";
import {
  formatGps,
  generationRows,
  hasGenerationSection,
  hasPhotoSection,
  hasRawSection,
  orientationLabel,
  photoRows,
  preview,
  rawSlots,
  workflowCopyText,
  type AssetMetadataDto,
} from "./asset-metadata";

function dto(partial: {
  normalized?: Record<string, unknown>;
  parsers?: AssetMetadataDto["parsers"];
}): AssetMetadataDto {
  return {
    id: "1",
    normalized: {
      ...(partial.normalized ?? {}),
    } as AssetMetadataDto["normalized"],
    parsers: partial.parsers ?? [],
  };
}

describe("photoRows / formatGps", () => {
  test("値のある行だけを表示順に並べる", () => {
    const rows = photoRows(
      dto({
        normalized: {
          takenAt: "2026-08-31T12:34:56",
          cameraMake: "TestCam",
          cameraModel: "Model X100",
          gpsLatitude: 35.658611,
          gpsLongitude: -139.745556,
          orientation: 6,
        },
      }).normalized,
    );
    expect(rows.map((r) => r.label)).toEqual(["撮影日時", "カメラ", "GPS", "向き"]);
    expect(rows[1].value).toBe("TestCam Model X100");
    expect(rows[2].value).toBe("35.658611°N, 139.745556°W");
    expect(rows[3].value).toBe("時計回り90° (6)");
  });

  test("レンズのみ・緯度のみの場合", () => {
    expect(
      photoRows(
        dto({ normalized: { lens: "23mm f/2" } }).normalized,
      ).map((r) => r.label),
    ).toEqual(["レンズ"]);
    // 緯度だけでは座標表示しない (片側のみは破棄済みだが防御)
    expect(formatGps(35.6, null)).toBeNull();
    expect(formatGps(undefined, 139.7)).toBeNull();
  });

  test("南緯・西経は負の座標", () => {
    expect(formatGps(-33.865, 151.2094)).toBe("33.865000°S, 151.209400°E");
  });

  test("orientationの不正値", () => {
    expect(orientationLabel(0)).toBeNull();
    expect(orientationLabel(9)).toBeNull();
    expect(orientationLabel(null)).toBeNull();
  });
});

describe("generationRows", () => {
  test("生成系の主要項目", () => {
    const rows = generationRows(
      dto({
        normalized: {
          generator: "Stable Diffusion WebUI",
          generationModel: "animagine xl",
          vae: "sdxl_vae.safetensors",
          sampler: "Euler a",
          scheduler: "Karras",
          steps: 28,
          cfgScale: 5.5,
          seed: "3141592653589793238",
        },
      }).normalized,
    );
    expect(rows.map((r) => r.label)).toEqual([
      "ツール",
      "モデル",
      "VAE",
      "Sampler",
      "Steps",
      "CFG scale",
      "Seed",
    ]);
    expect(rows[3].value).toBe("Euler a (Karras)");
    expect(rows[6].value).toBe("3141592653589793238");
  });

  test("schedulerのみでも表示", () => {
    const rows = generationRows(dto({ normalized: { scheduler: "simple" } }).normalized);
    expect(rows.map((r) => r.label)).toEqual(["Scheduler"]);
  });
});

describe("preview (折りたたみ展開時の巨大workflow保護)", () => {
  test("上限内はそのまま", () => {
    const p = preview("short text");
    expect(p).toEqual({ text: "short text", truncated: false, totalLength: 10 });
  });

  test("上限超は打ち切り + 全文長", () => {
    const long = "x".repeat(10_000);
    const p = preview(long, 100);
    expect(p.truncated).toBe(true);
    expect(p.text.length).toBe(100);
    expect(p.totalLength).toBe(10_000);
  });
});

describe("rawSlots / workflowCopyText / 節の有無", () => {
  const parserRow = {
    parserName: "comfy",
    parserVersion: "1",
    rawExif: null,
    rawIptc: null,
    rawPng: '{"prompt":"..."}',
    rawWorkflow: '{"nodes":[]}',
    updatedAt: "2026-09-02T00:00:00.000Z",
  };

  test("値のあるrawスロットのみ", () => {
    expect(rawSlots(parserRow).map((r) => r.label)).toEqual(["PNGテキスト", "workflow"]);
    expect(
      rawSlots({
        parserName: "exif",
        parserVersion: "1",
        rawExif: "{}",
        rawIptc: null,
        rawPng: null,
        rawWorkflow: null,
        updatedAt: "",
      }).map((r) => r.label),
    ).toEqual(["EXIF"]);
  });

  test("workflowコピーはraw_workflowをそのまま", () => {
    expect(workflowCopyText(dto({ parsers: [parserRow] }))).toBe('{"nodes":[]}');
    expect(workflowCopyText(null)).toBeNull();
    expect(workflowCopyText(dto({}))).toBeNull();
  });

  test("節の有無判定", () => {
    expect(hasPhotoSection(null)).toBe(false);
    expect(hasPhotoSection(dto({}))).toBe(false);
    expect(hasPhotoSection(dto({ normalized: { takenAt: "x" } }))).toBe(true);
    expect(hasGenerationSection(dto({}))).toBe(false);
    expect(hasGenerationSection(dto({ normalized: { positivePrompt: "p" } }))).toBe(true);
    expect(hasGenerationSection(dto({ parsers: [parserRow] }))).toBe(true);
    expect(hasRawSection(dto({}))).toBe(false);
    expect(hasRawSection(dto({ parsers: [parserRow] }))).toBe(true);
  });
});
