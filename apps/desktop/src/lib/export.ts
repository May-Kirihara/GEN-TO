/**
 * SD学習用エクスポート (WP-14) のフロントエンド純粋関数・IPCラップ。
 *
 * - 命名・割当の正本はbackend (Rust export.rs)。ここにある表示用の薄い
 *   ミラー (txtNameFor / collisionSuffixName) はbackend契約と同じ期待値を
 *   固定するもので、独立した第二実装ではない。
 * - command呼び出しは注入 (ExportIpc) してテスト容易にする
 *   (lib/backup.tsと同じ規約)。
 * - 型契約は docs/dev/api-contract.md §48 (DTOはcamelCase)。
 */

/** Tauri IPC境界の注入用アダプター。 */
export type ExportIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** フォルダ構造 ("preserve" | "flat")。 */
export type ExportStructure = "preserve" | "flat";

/** 既存出力との関係 ("none" | "identical" | "conflict"。"n/a"はerror項目)。 */
export type ExportExisting = "none" | "identical" | "conflict" | "n/a";

/** `.txt`内容の由来。 */
export type ExportCaptionSource =
  | "caption_sd_tags"
  | "caption_description"
  | "tags"
  | "none";

/** dry-run (export_preview) のエントリ。 */
export type ExportPreviewEntry = {
  assetId: string;
  assetUuid: string;
  displayName: string;
  sourcePath: string;
  outputRelPath: string;
  txtRelPath: string;
  imageExisting: ExportExisting;
  txtExisting: ExportExisting;
  captionSource: ExportCaptionSource;
  captionPreview: string;
  error: string | null;
};

/** dry-run (export_preview) の応答。 */
export type ExportPreviewDto = {
  destDir: string;
  structure: ExportStructure;
  entries: ExportPreviewEntry[];
  summary: {
    total: number;
    toWrite: number;
    alreadyExported: number;
    conflict: number;
    error: number;
  };
};

/** 実行 (export_execute) の応答。 */
export type ExportResultDto = {
  manifestPath: string;
  cancelled: boolean;
  summary: {
    total: number;
    exported: number;
    identical: number;
    conflict: number;
    failed: number;
    planningErrors: number;
    cancelled: number;
  };
  failures: ExportIssue[];
  conflicts: ExportIssue[];
  issuesTruncated: boolean;
};

/** 失敗/衝突の報告項目 (上限あり。全量はmanifest)。 */
export type ExportIssue = {
  assetUuid: string;
  displayName: string;
  outputRelPath: string;
  reason: string;
};

/** `export:progress` イベントペイロード。 */
export type ExportProgress = {
  processed: number;
  total: number;
  assetUuid: string;
  status: "exported" | "identical" | "conflict" | "failed";
};

/**
 * 画像ファイル名から `.txt` ファイル名を導出 (Rust `txt_file_name` と同一規則:
 * stem + ".txt"。拡張子なしには ".txt" を付ける)。
 */
export function txtNameFor(imageName: string): string {
  const dot = imageName.lastIndexOf(".");
  const stem = dot > 0 ? imageName.slice(0, dot) : imageName;
  return `${stem}.txt`;
}

/**
 * 衝突suffix付き名前 `{stem}-{n}.{ext}` (Rust `suffixed_name` と同一規則)。
 */
export function collisionSuffixName(imageName: string, n: number): string {
  const dot = imageName.lastIndexOf(".");
  if (dot > 0) {
    return `${imageName.slice(0, dot)}-${n}${imageName.slice(dot)}`;
  }
  return `${imageName}-${n}`;
}

/** 構造オプションの日本語ラベル。 */
export function structureLabel(structure: ExportStructure): string {
  return structure === "preserve" ? "フォルダ構造を維持" : "平坦化 (直下へ)";
}

/** 既存出力関係の日本語ラベル。 */
export function existingLabel(existing: ExportExisting): string {
  switch (existing) {
    case "none":
      return "新規";
    case "identical":
      return "書き出し済み";
    case "conflict":
      return "衝突 (スキップ)";
    case "n/a":
      return "—";
  }
}

/** caption由来の日本語ラベル。 */
export function captionSourceLabel(source: ExportCaptionSource): string {
  switch (source) {
    case "caption_sd_tags":
      return "SDタグcaption";
    case "caption_description":
      return "説明caption";
    case "tags":
      return "タグ列";
    case "none":
      return "(空)";
  }
}

/** preview集計の日本語サマリ行 (件数0の行は返さない)。 */
export function previewSummaryLines(preview: ExportPreviewDto): string[] {
  const s = preview.summary;
  const lines: string[] = [];
  const push = (label: string, count: number): void => {
    if (count > 0) {
      lines.push(`${label}: ${count.toLocaleString()}件`);
    }
  };
  push("書き出し予定", s.toWrite);
  push("書き出し済み (スキップ)", s.alreadyExported);
  push("衝突 (スキップ)", s.conflict);
  push("エラー", s.error);
  return lines;
}

/** 実行結果の日本語サマリ行 (件数0の行は返さない)。 */
export function resultSummaryLines(result: ExportResultDto): string[] {
  const s = result.summary;
  const lines: string[] = [];
  const push = (label: string, count: number): void => {
    if (count > 0) {
      lines.push(`${label}: ${count.toLocaleString()}件`);
    }
  };
  push("書き出し", s.exported);
  push("書き出し済み (スキップ)", s.identical);
  push("衝突 (スキップ)", s.conflict);
  push("失敗", s.failed);
  push("計画エラー", s.planningErrors);
  push("キャンセル未処理", s.cancelled);
  return lines;
}

/**
 * 実行可能か: dest入力済みかつ dry-run preview を1回以上取得済みかつ
 * 書き込み/確認対象が1件以上あること (全件エラーなら実行しても何も起きない)。
 */
export function canExecute(
  destInput: string,
  preview: ExportPreviewDto | null,
): boolean {
  const dest = destInput.trim();
  if (dest === "" || preview === null) {
    return false;
  }
  return preview.summary.toWrite + preview.summary.alreadyExported > 0;
}

/** IPCラップ (アプリコード用。テストではExportIpcを差し込む)。 */
export function createExportClient(ipc: ExportIpc): {
  preview(
    assetIds: string[],
    destDir: string,
    structure: ExportStructure,
  ): Promise<ExportPreviewDto>;
  execute(
    assetIds: string[],
    destDir: string,
    structure: ExportStructure,
  ): Promise<ExportResultDto>;
  cancel(): Promise<void>;
} {
  return {
    preview: (assetIds, destDir, structure) =>
      ipc.invoke<ExportPreviewDto>("export_preview", {
        assetIds,
        destDir,
        structure,
      }),
    execute: (assetIds, destDir, structure) =>
      ipc.invoke<ExportResultDto>("export_execute", {
        assetIds,
        destDir,
        structure,
      }),
    cancel: () => ipc.invoke<void>("export_cancel"),
  };
}
