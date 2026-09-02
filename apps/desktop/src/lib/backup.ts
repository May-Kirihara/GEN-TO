/**
 * バックアップと復元 (WP-13) のフロントエンド純粋関数・IPCラップ。
 *
 * - command呼び出しは注入 (BackupIpc) してテスト容易にする
 *   (lib/fileops.tsと同じ規約)。
 * - plan結果の集計・確認フローのゲーティングはここでの純粋関数が正本 —
 *   vitestで全分類を検証する。
 * - 型契約は docs/dev/api-contract.md §47 (WP-13追加。DTOはcamelCase)。
 */

/** Tauri IPC境界の注入用アダプター。 */
export type BackupIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** マッチング分類 (バックエンド RestoreMatch の文字列)。 */
export type RestoreMatchKind =
  | "uuid"
  | "hash"
  | "path_add"
  | "conflict"
  | "missing"
  | "ambiguous";

/** 実行action (バックエンド RestoreAction の文字列)。 */
export type RestoreActionKind = "same" | "update" | "add" | "skip";

/** plan項目 (asset単位)。 */
export type RestoreEntryDto = {
  assetUuid: string;
  contentHash: string;
  displayName: string;
  currentAssetId: string | null;
  matched: RestoreMatchKind;
  action: RestoreActionKind;
  reason: string;
  files: Array<{
    backupRelPath: string;
    currentRelPath: string | null;
    onDisk: boolean;
  }>;
};

/** plan項目 (コレクション単位)。 */
export type RestoreCollectionDto = {
  uuid: string;
  name: string;
  kind: string;
  willCreate: boolean;
  itemsTotal: number;
  itemsResolvable: number;
};

export type RestoreSummaryDto = {
  same: number;
  update: number;
  add: number;
  conflict: number;
  missing: number;
  ambiguous: number;
};

/** 復元plan (dry-run) の応答。confirmTokenはexecuteに必須。 */
export type RestorePlanDto = {
  rootUuid: string;
  rootDisplayPath: string;
  createdAt: string;
  targetRootId: string;
  targetRootPath: string;
  entries: RestoreEntryDto[];
  collections: RestoreCollectionDto[];
  summary: RestoreSummaryDto;
  confirmToken: string;
};

/** 復元実行の結果。 */
export type RestoreResultDto = {
  applied: Array<{ assetUuid: string; action: RestoreActionKind; changed: string[] }>;
  skipped: Array<{ assetUuid: string; matched: RestoreMatchKind; reason: string }>;
  collectionsCreated: number;
  collectionsMerged: number;
  collectionItemsAdded: number;
  collectionItemsUnresolved: number;
  ftsRebuilt: number;
};

/** ポータブルバックアップ作成結果。 */
export type PortableCreatedDto = {
  path: string;
  assetCount: number;
  fileCount: number;
  collectionCount: number;
};

/** フルDBバックアップ作成結果。 */
export type FullDbBackupDto = {
  path: string;
  keptGenerations: number;
};

/** バックアップ設定。 */
export type BackupSettingsDto = {
  /** "off" | "daily" | "weekly" */
  autoFullDb: string;
  retentionGenerations: number;
};

/** 分類ごとの日本語ラベル (plan表示・結果表示共通)。 */
export function matchLabel(matched: RestoreMatchKind): string {
  switch (matched) {
    case "uuid":
      return "UUID一致";
    case "hash":
      return "ハッシュ再関連付け";
    case "path_add":
      return "新規登録";
    case "conflict":
      return "衝突";
    case "missing":
      return "行方不明";
    case "ambiguous":
      return "曖昧 (同一内容複数箇所)";
  }
}

/** actionごとの日本語ラベル。 */
export function actionLabel(action: RestoreActionKind): string {
  switch (action) {
    case "same":
      return "変更なし";
    case "update":
      return "更新";
    case "add":
      return "追加";
    case "skip":
      return "スキップ";
  }
}

/** planの確認表示向けサマリ行 (日本語。件数0の行は返さない)。 */
export function planSummaryLines(plan: RestorePlanDto): string[] {
  const s = plan.summary;
  const lines: string[] = [];
  const push = (label: string, count: number): void => {
    if (count > 0) {
      lines.push(`${label}: ${count.toLocaleString()}件`);
    }
  };
  push("変更なし", s.same);
  push("更新", s.update);
  push("追加", s.add);
  push("衝突 (スキップ)", s.conflict);
  push("行方不明 (スキップ)", s.missing);
  push("曖昧 (スキップ)", s.ambiguous);
  const createCols = plan.collections.filter((c) => c.willCreate).length;
  const mergeCols = plan.collections.length - createCols;
  if (createCols > 0) {
    lines.push(`コレクション新規作成: ${createCols.toLocaleString()}件`);
  }
  if (mergeCols > 0) {
    lines.push(`コレクション項目マージ: ${mergeCols.toLocaleString()}件`);
  }
  return lines;
}

/** 注意喚起が必要な項目 (衝突・行方不明・曖昧・未解決コレクション項目)。 */
export function planWarnings(plan: RestorePlanDto): string[] {
  const warnings: string[] = [];
  for (const entry of plan.entries) {
    if (entry.matched === "conflict" || entry.matched === "missing" || entry.matched === "ambiguous") {
      warnings.push(`${entry.displayName}: ${entry.reason}`);
    }
  }
  const unresolved = plan.collections
    .filter((c) => c.itemsTotal > c.itemsResolvable)
    .map(
      (c) =>
        `コレクション「${c.name}」: ${c.itemsTotal - c.itemsResolvable}件の項目が復元対象に解決できません`,
    );
  warnings.push(...unresolved);
  return warnings;
}

/**
 * 実行すべき作業があるか (更新/追加/コレクション項目追加)。
 * 全部スキップなら実行ボタンを無効化する。
 */
export function planHasWork(plan: RestorePlanDto): boolean {
  const entriesWork = plan.entries.some(
    (e) => e.action === "update" || e.action === "add",
  );
  const collectionsWork = plan.collections.some((c) => {
    if (!c.willCreate) {
      // 既存コレクションへのマージは実行時に判定 (追加が無ければ加算0)
      return true;
    }
    return c.itemsResolvable > 0 || c.kind === "smart";
  });
  return entriesWork || collectionsWork;
}

/** 実行結果の確認表示向けサマリ行。 */
export function resultSummaryLines(result: RestoreResultDto): string[] {
  const byAction = new Map<RestoreActionKind, number>();
  for (const item of result.applied) {
    byAction.set(item.action, (byAction.get(item.action) ?? 0) + 1);
  }
  const lines: string[] = [];
  const push = (label: string, count: number): void => {
    if (count > 0) {
      lines.push(`${label}: ${count.toLocaleString()}件`);
    }
  };
  push("追加", byAction.get("add") ?? 0);
  push("更新", byAction.get("update") ?? 0);
  push("変更なし", byAction.get("same") ?? 0);
  push("スキップ", result.skipped.length);
  push("コレクション作成", result.collectionsCreated);
  push("コレクション項目追加", result.collectionItemsAdded);
  push("コレクション未解決項目", result.collectionItemsUnresolved);
  push("FTS再構築", result.ftsRebuilt);
  return lines;
}

/** `.gento` バックアップディレクトリ名の検証 (選択UI用の即時チェック)。 */
export function looksLikeGentoBackup(dirName: string): boolean {
  // `{root名}.gento` 形式 — 拡張子だけの名前やtmp残骸は受け付けない
  return (
    dirName.length > ".gento".length &&
    dirName.endsWith(".gento") &&
    !dirName.startsWith(".gento-tmp-")
  );
}

/** IPCラップ (アプリコード用。テストではBackupIpcを差し込む)。 */
export function createBackupClient(ipc: BackupIpc): {
  createPortable(rootId: string, destDir?: string | null): Promise<PortableCreatedDto>;
  fullDb(destDir?: string | null): Promise<FullDbBackupDto>;
  defaultDir(): Promise<string>;
  restorePlan(backupDir: string, targetRootId?: string | null): Promise<RestorePlanDto>;
  restoreExecute(
    backupDir: string,
    confirmToken: string,
    targetRootId?: string | null,
  ): Promise<RestoreResultDto>;
  settings(): Promise<BackupSettingsDto>;
  setSettings(
    autoFullDb: string,
    retentionGenerations?: number | null,
  ): Promise<BackupSettingsDto>;
} {
  return {
    createPortable: (rootId, destDir) =>
      ipc.invoke<PortableCreatedDto>("backup_portable_create", {
        rootId,
        destDir: destDir ?? null,
      }),
    fullDb: (destDir) =>
      ipc.invoke<FullDbBackupDto>("backup_full_db", { destDir: destDir ?? null }),
    defaultDir: () => ipc.invoke<string>("backup_default_dir"),
    restorePlan: (backupDir, targetRootId) =>
      ipc.invoke<RestorePlanDto>("backup_restore_plan", {
        backupDir,
        targetRootId: targetRootId ?? null,
      }),
    restoreExecute: (backupDir, confirmToken, targetRootId) =>
      ipc.invoke<RestoreResultDto>("backup_restore_execute", {
        backupDir,
        confirmToken,
        targetRootId: targetRootId ?? null,
      }),
    settings: () => ipc.invoke<BackupSettingsDto>("backup_settings_get"),
    setSettings: (autoFullDb, retentionGenerations) =>
      ipc.invoke<BackupSettingsDto>("backup_settings_set", {
        autoFullDb,
        retentionGenerations: retentionGenerations ?? null,
      }),
  };
}
