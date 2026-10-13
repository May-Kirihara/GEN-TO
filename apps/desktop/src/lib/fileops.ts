/**
 * ファイル操作 (WP-7) のフロントエンド純粋関数・IPCラップ。
 *
 * - command呼び出しは注入 (FileOpsIpc) してテスト容易にする
 *   (lib/library.tsと同じ規約)。
 * - 衝突ダイアログの状態遷移 (選択の蓄積・以降全部適用) はここでの
 *   純粋関数 (createConflictSession / applyConflictChoice) が正本 —
 *   vitestで全解決組み合わせを検証する。
 * - 型契約は docs/dev/api-contract.md §39 (WP-7追加)。
 */

/** Tauri IPC境界の注入用アダプター。 */
export type FileOpsIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** 操作種別 (バックエンドのsnake_case文字列)。
 * delete_forever は通常削除 (trash) と意図的に分離 (spec §5.1)。 */
export type FileOpKind =
  | "copy"
  | "move"
  | "rename"
  | "mkdir"
  | "trash"
  | "delete_forever";

/** 衝突の解決方針 (spec §5.3)。 */
export type ConflictResolution = "overwrite" | "skip" | "keep_both";

/** dry-runの1ステップ。 */
export type PlanStepDto = {
  file_id: string;
  src: string;
  dst: string;
  display_name: string;
  conflict: ConflictDto | null;
  plan_error: string | null;
};

/** 衝突情報 (容量・更新日時・ハッシュ一致)。 */
export type ConflictDto = {
  dst: string;
  dst_byte_size: number;
  dst_mtime_ms: number;
  hash_matches: boolean | null;
  keep_both_name: string;
};

/** dry-run応答。 */
export type FileOpPlanDto = {
  kind: FileOpKind;
  steps: PlanStepDto[];
  plan_errors: string[];
  dest_root_id: string | null;
  disk_shortfall_bytes: number | null;
  /**
   * 破壊的操作 (delete_forever / 上書き解決の可能性) を含む場合に発行
   * される単発確認トークン (5分失効・1回限り)。execute時に必須。
   * UIは 計画→確認ダイアログ→トークン付き実行 のフローを組む。
   */
  confirm_token: string | null;
};

/** 実行結果の1ステップ。 */
export type StepResultDto = {
  file_id: string;
  from: string;
  to: string | null;
  status: "done" | "skipped" | "failed" | "cancelled";
  resolution: ConflictResolution | null;
  error: string | null;
  /** 完了したが注意喚起がいる事象 (跨界移動のsrc削除失敗等)。 */
  warning: string | null;
};

/** 実行結果 (部分成功レポートを含む)。 */
export type FileOpOutcomeDto = {
  op_id: string;
  kind: FileOpKind;
  status: "done" | "partial" | "failed";
  undoable: boolean;
  /** NoUndo退避 (Windows等) のステップの手動復元案内 (全ステップ分。
   *  旧ファイルはゴミ箱に残る)。通知では件数要約で取りこぼさない。 */
  manual_restore_notes: string[];
  steps: StepResultDto[];
};

/** Undo前提検査の応答。 */
export type UndoCheckDto = {
  op_id: string;
  ok: boolean;
  kind: FileOpKind;
  refusals: string[];
};

/** 操作履歴項目。 */
export type FileOpHistoryDto = {
  op_id: string;
  kind: FileOpKind;
  status: string;
  undoable: boolean;
  created_at: string;
  undone_at: string | null;
  request_json: string;
  result_json: string | null;
};

/** 進捗イベント (`file_ops:progress`)。 */
export type FileOpsProgressEvent = {
  op_id: string;
  index: number;
  total: number;
  from: string;
  to: string | null;
  done: boolean;
};

/** 実行要求の引数 (計画と同じ入力 + 解決方針 + 確認トークン)。 */
export type ExecuteArgs = {
  kind: FileOpKind;
  paths: string[];
  dest_dir?: string | null;
  new_name?: string | null;
  resolutions?: {
    global?: ConflictResolution | null;
    per_file?: Record<string, ConflictResolution>;
  } | null;
  /** 破壊的操作 (delete_forever / overwrite) に必須の単発確認トークン。 */
  confirm_token?: string | null;
};

/** 操作種別のUI表示名。 */
export function describeKind(kind: FileOpKind): string {
  switch (kind) {
    case "copy":
      return "コピー";
    case "move":
      return "移動";
    case "rename":
      return "リネーム";
    case "mkdir":
      return "新規フォルダ";
    case "trash":
      return "ゴミ箱への移動";
    case "delete_forever":
      return "完全削除";
  }
}

// ===== 衝突セッション (ダイアログの状態遷移。純粋関数) =====

/** 衝突ダイアログのセッション状態。 */
export type ConflictSession = {
  /** 未解決の衝突 (先頭が現在表示対象)。 */
  pending: PlanStepDto[];
  /** 解決済みの写像 (file_id → 方針)。 */
  resolved: Record<string, ConflictResolution>;
  /** 現在表示中の衝突。 */
  current: PlanStepDto | null;
  /** 残り件数 (現在表示中を除く)。 */
  remaining: number;
};

/** 計画から衝突セッションを組み立てる (衝突が無い場合はcurrent=null)。 */
export function createConflictSession(
  steps: PlanStepDto[],
): ConflictSession {
  const pending = steps.filter((step) => step.conflict !== null);
  const current = pending.length > 0 ? pending[0] : null;
  return {
    pending,
    resolved: {},
    current,
    remaining: Math.max(0, pending.length - 1),
  };
}

/**
 * 1件の衝突に方針を適用する。
 * `applyAll` (以降すべてへ適用) の場合は残り全件に同一方針を当てて
 * セッションを完了させる。
 */
export function applyConflictChoice(
  session: ConflictSession,
  choice: ConflictResolution,
  applyAll: boolean,
): ConflictSession {
  const current = session.current;
  if (current === null) {
    return session;
  }
  const resolved: Record<string, ConflictResolution> = {
    ...session.resolved,
    [current.file_id]: choice,
  };
  if (applyAll) {
    for (const step of session.pending) {
      resolved[step.file_id] = choice;
    }
    return { pending: [], resolved, current: null, remaining: 0 };
  }
  const pending = session.pending.slice(1);
  return {
    pending,
    resolved,
    current: pending.length > 0 ? pending[0] : null,
    remaining: Math.max(0, pending.length - 1),
  };
}

/** セッション完了時に実行へ渡す解決指定。全衝突が解決済みか。 */
export function resolutionsOf(
  session: ConflictSession,
): Record<string, ConflictResolution> {
  return session.resolved;
}

/** セッションが完了 (未解決が残っていない) か。 */
export function conflictSessionComplete(session: ConflictSession): boolean {
  // pendingに未解決のものが残っていない = currentがnull
  return session.current === null && Object.keys(session.resolved).length >= 0;
}

// ===== 実行結果の集計 (トースト・再試行) =====

/** 部分成功レポート用の集計。 */
export function summarizeOutcome(outcome: FileOpOutcomeDto): {
  done: number;
  skipped: number;
  failed: number;
  cancelled: number;
} {
  let done = 0;
  let skipped = 0;
  let failed = 0;
  let cancelled = 0;
  for (const step of outcome.steps) {
    switch (step.status) {
      case "done":
        done += 1;
        break;
      case "skipped":
        skipped += 1;
        break;
      case "failed":
        failed += 1;
        break;
      case "cancelled":
        cancelled += 1;
        break;
    }
  }
  return { done, skipped, failed, cancelled };
}

/**
 * step警告から手動復元案内 (manual_restore_notesの全文) を除去する。
 * coreは併存時 «既存警告。案内» の順で連結するため、除去で余る「。」を
 * 切り詰め、残りが空 (案内のみの警告) ならnullを返す。
 */
function stripManualRestoreNotes(
  warning: string,
  notes: string[],
): string | null {
  let text = warning;
  for (const note of notes) {
    text = text.split(note).join("");
  }
  while (text.startsWith("。")) {
    text = text.slice(1);
  }
  while (text.endsWith("。")) {
    text = text.slice(0, -1);
  }
  return text.length > 0 ? text : null;
}

/** 実行結果のトースト文。
 *  手動復元案内 (NoUndo退避) は全件を件数要約で反映し、それ以外の
 *  step警告は先頭代表で付記する (案内と連結された警告は案内部分を除く)。 */
export function outcomeNotice(outcome: FileOpOutcomeDto): string {
  const label = describeKind(outcome.kind);
  const { done, skipped, failed, cancelled } = summarizeOutcome(outcome);
  const parts: string[] = [];
  if (done > 0) {
    parts.push(`${done.toLocaleString()}件成功`);
  }
  if (skipped > 0) {
    parts.push(`${skipped.toLocaleString()}件スキップ`);
  }
  if (failed > 0) {
    parts.push(`${failed.toLocaleString()}件失敗`);
  }
  if (cancelled > 0) {
    parts.push(`${cancelled.toLocaleString()}件中止`);
  }
  let notice = `${label}: ${parts.join("・")}`;
  for (const step of outcome.steps) {
    if (step.warning === null) {
      continue;
    }
    const warning = stripManualRestoreNotes(step.warning, outcome.manual_restore_notes);
    if (warning !== null) {
      notice += ` ⚠ ${warning}`;
      break;
    }
  }
  if (outcome.manual_restore_notes.length > 0) {
    notice += ` ⚠ 退避した旧ファイル${outcome.manual_restore_notes.length.toLocaleString()}件はゴミ箱に残ります (この環境ではアプリ内の元に戻しで復帰できません。必要ならゴミ箱から手動で復元してください)`;
  }
  const suffix = !outcome.undoable
    ? ""
    : outcome.manual_restore_notes.length > 0
      ? " (Ctrl+Zで戻せるのは新ファイルのみです)"
      : " (Ctrl+Zで元に戻せます)";
  return notice + suffix;
}

/** 失敗ステップの再試行引数 (同じkind・宛先で失敗分のみ再実行)。 */
export function retryArgsOf(
  outcome: FileOpOutcomeDto,
  original: ExecuteArgs,
): ExecuteArgs | null {
  const failedPaths = outcome.steps
    .filter((step) => step.status === "failed")
    .map((step) => step.from);
  if (failedPaths.length === 0) {
    return null;
  }
  return {
    kind: outcome.kind === "trash" ? "trash" : original.kind,
    paths: failedPaths,
    dest_dir: original.dest_dir,
    new_name: original.new_name,
    // 再試行時は前回の解決を引き継がない (衝突状態が変わっている可能性)
    resolutions: original.resolutions?.global
      ? { global: original.resolutions.global }
      : null,
  };
}

/** 空き容量警告文 (計画が容量不足を検出したとき)。 */
export function diskWarningOf(plan: FileOpPlanDto): string | null {
  if (plan.disk_shortfall_bytes === null) {
    return null;
  }
  const mib = plan.disk_shortfall_bytes / (1024 * 1024);
  const human = mib >= 1 ? `${mib.toFixed(1)} MiB` : `${plan.disk_shortfall_bytes} バイト`;
  return `宛先ドライブの空き容量が約 ${human} 足りません`;
}

// ===== IPCラップ =====

/** dry-run (計画)。 */
export async function planFileOp(
  ipc: FileOpsIpc,
  args: ExecuteArgs,
): Promise<FileOpPlanDto> {
  return ipc.invoke<FileOpPlanDto>("file_ops_plan", {
    kind: args.kind,
    paths: args.paths,
    destDir: args.dest_dir ?? null,
    newName: args.new_name ?? null,
  });
}

/** 実行 (進捗はfile_ops:progressイベント)。 */
export async function executeFileOp(
  ipc: FileOpsIpc,
  args: ExecuteArgs,
): Promise<FileOpOutcomeDto> {
  return ipc.invoke<FileOpOutcomeDto>("file_ops_execute", { args });
}

/** Undo前提検査 (拒否時は理由)。 */
export async function undoCheck(
  ipc: FileOpsIpc,
  opId: string,
): Promise<UndoCheckDto> {
  return ipc.invoke<UndoCheckDto>("file_ops_undo_check", { opId });
}

/** Undo実行結果 (補償ロールバックの状態を含む)。 */
export type UndoResultDto = {
  op_id: string;
  fully_undone: boolean;
  /** "undone" | "partial_undone" | "refused"。 */
  status: string;
  errors: string[];
  refusals: string[];
  /** 補償ロールバックにも失敗した項目 (手動確認が必要)。 */
  rollback_errors: string[];
  /** 全action成功でも注意がいる事象 (NoUndo退避の旧ファイルはゴミ箱に
   *  残るための手動復元案内)。 */
  warnings: string[];
};

/** Undo実行結果の通知文 (手動復元案内などの警告を⚠付きで連結)。 */
export function undoNotice(result: UndoResultDto): string {
  let notice = "元に戻しました";
  for (const warning of result.warnings) {
    notice += ` ⚠ ${warning}`;
  }
  return notice;
}

/** Undo実行。 */
export async function undoFileOp(ipc: FileOpsIpc, opId: string): Promise<UndoResultDto> {
  return ipc.invoke("file_ops_undo", { opId });
}

/** 操作履歴。 */
export async function fileOpsHistory(
  ipc: FileOpsIpc,
  limit = 50,
): Promise<FileOpHistoryDto[]> {
  return ipc.invoke<FileOpHistoryDto[]>("file_ops_history", { limit });
}

/** 実行中操作のキャンセル。 */
export async function cancelFileOps(ipc: FileOpsIpc): Promise<void> {
  await ipc.invoke("file_ops_cancel");
}

/** 新規フォルダ作成。 */
export async function folderCreate(
  ipc: FileOpsIpc,
  parent: string,
  name: string,
): Promise<FileOpOutcomeDto> {
  return ipc.invoke<FileOpOutcomeDto>("folder_create", { parent, name });
}
