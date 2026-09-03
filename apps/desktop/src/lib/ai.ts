/**
 * AI統合のフロントエンド側 (WP-10)。
 *
 * - プロファイル編集・4種テスト・ジョブキュー操作のinvokeラッパー
 * - `ai:jobs` イベント購読 (500msバッチ)
 * - 純粋ヘルパー (表示ラベル・ジョブ操作可否・APIキー入力の状態機械) は
 *   vitestで単体テスト対象 (ai.test.ts)
 */

import { classifyEndpoint } from "./settings";

export type AiIpc = {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void>;
};

/** プロファイルDTO (backend AiProfileDto。camelCase)。 */
export type AiProfile = {
  id: string;
  uuid: string;
  displayName: string;
  baseUrl: string;
  model: string;
  visionCapable: boolean;
  enabled: boolean;
  timeoutMs: number;
  concurrency: number;
  extra: AiProfileExtra;
  hasApiKey: boolean;
  apiKeyStore: string | null;
};

/** プロファイル追加設定 (backend ProfileExtra)。 */
export type AiProfileExtra = {
  send_exif: boolean;
  send_gps: boolean;
  /** 送信画像の最大辺 (px)。backendが64〜2048へ強制 (IPCは範囲外を拒否)。 */
  max_image_edge: number;
  send_response_format: boolean;
};

export const DEFAULT_PROFILE_EXTRA: AiProfileExtra = {
  send_exif: false,
  send_gps: false,
  max_image_edge: 1024,
  send_response_format: false,
};

/** 保存入力 (新規はid無し。APIキー: undefined=変更なし / ""=削除 / 値=保存)。 */
export type AiProfileInput = {
  id?: string;
  displayName: string;
  baseUrl: string;
  model: string;
  visionCapable: boolean;
  enabled: boolean;
  timeoutMs: number;
  concurrency: number;
  extra: AiProfileExtra;
};

/** テスト機能の結果 (backend TestReport)。 */
export type AiTestReport = {
  ok: boolean;
  detail: string;
  elapsedMs: number;
  status: number | null;
};

/** ジョブDTO (backend AiJobDto)。 */
export type AiJob = {
  id: string;
  jobType: string;
  targetId: string;
  status: AiJobStatus;
  priority: number;
  attempts: number;
  maxAttempts: number;
  error: string | null;
  scheduledAt: string | null;
  createdAt: string;
  updatedAt: string;
};

export type AiJobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "retry_waiting";

/** `ai:jobs` イベントペイロード。 */
export type AiJobsEvent = {
  changes: AiJob[];
  counts: Record<string, number>;
  paused: boolean;
};

/** AI実行履歴DTO (backend AiRunDto)。 */
export type AiRun = {
  id: string;
  profileId: string | null;
  operation: string;
  status: string;
  promptVersion: number;
  requestSummary: Record<string, unknown> | null;
  error: string | null;
  startedAt: string;
  finishedAt: string | null;
};

export type AiJobsSnapshot = {
  jobs: AiJob[];
  counts: Record<string, number>;
  paused: boolean;
};

/** ジョブ状態の表示ラベル。 */
export function jobStatusLabel(status: AiJobStatus): string {
  const labels: Record<AiJobStatus, string> = {
    queued: "待機中",
    running: "処理中",
    succeeded: "成功",
    failed: "失敗",
    cancelled: "キャンセル",
    retry_waiting: "再試行待ち",
  };
  return labels[status] ?? status;
}

/** ジョブ種別の表示ラベル。 */
export function jobTypeLabel(jobType: string): string {
  const labels: Record<string, string> = {
    ai_caption_tags: "タグ・キャプション生成",
    ai_reparse: "メタデータ再解析",
  };
  return labels[jobType] ?? jobType;
}

/** ジョブがキャンセル操作を受け付ける状態か (queued/retry_waitingのみ)。 */
export function jobCancellable(status: AiJobStatus): boolean {
  return status === "queued" || status === "retry_waiting";
}

/** 状態ごとの件数と0埋め (サマリ行表示用)。 */
export function statusSummary(counts: Record<string, number>): {
  label: string;
  count: number;
}[] {
  const order: AiJobStatus[] = [
    "queued",
    "running",
    "retry_waiting",
    "succeeded",
    "failed",
    "cancelled",
  ];
  return order.map((status) => ({
    label: jobStatusLabel(status),
    count: counts[status] ?? 0,
  }));
}

/** APIキー入力欄の状態。 */
export type ApiKeyInputState = {
  /** 入力欄の現在値 (未入力="")。 */
  value: string;
  /** 編集を開始したか (未編集なら保存時に鍵を送らない)。 */
  edited: boolean;
};

/** 編集を始めた直後の状態。 */
export function apiKeyEditing(existing: boolean): ApiKeyInputState {
  return {
    value: "",
    edited: existing === false ? true : false,
  };
}

/**
 * 保存時にbackendへ渡すAPIキー値。
 * - 未編集 → undefined (変更なし)
 * - 空入力 + 既存鍵あり → "" (削除)
 * - 空入力 + 既存なし → undefined (何もしない)
 * - 入力あり → 値
 */
export function apiKeyForSave(
  state: ApiKeyInputState,
  hasExistingKey: boolean,
): string | undefined {
  if (!state.edited) {
    return undefined;
  }
  if (state.value === "") {
    return hasExistingKey ? "" : undefined;
  }
  return state.value;
}

/** 鍵入力欄のプレースホルダ (既存鍵はマスク表示)。 */
export function apiKeyPlaceholder(hasExistingKey: boolean): string {
  if (hasExistingKey) {
    return "•••••••• (保存済み — 入力で更新、空で削除)";
  }
  return "APIキー (ローカルendpointでは空欄で可)";
}

/** 鍵の保存先表示。 */
export function apiKeyStoreLabel(store: string | null): string {
  if (store === "keyring") {
    return "資格情報ストアに保存";
  }
  if (store === "file") {
    return "平文ファイルに保存 (資格情報ストア利用不可)";
  }
  return "APIキーなし";
}

/** 接続先がローカルか (spec §16「接続先がローカルか外部かを分かりやすく」)。 */
export function endpointIsLocal(baseUrl: string): boolean {
  // WP-15: 3値分類 (lib/settings.ts) へ委譲。従来の「ローカル判定」は
  // localhost/127.0.0.1/::1 のまま (RFC1918は「プライベート」で区別)。
  return classifyEndpoint(baseUrl) === "local";
}

/** テスト4種の実行順序と表示名。 */
export const AI_TESTS = [
  { id: "connection", label: "接続" },
  { id: "text", label: "テキスト" },
  { id: "vision", label: "Vision" },
  { id: "structured", label: "構造化JSON" },
] as const;

export type AiTestKind = (typeof AI_TESTS)[number]["id"];

/** invokeラッパー群。 */
export class AiStore {
  private ipc: AiIpc;
  lastError: string | null = null;

  constructor(ipc: AiIpc) {
    this.ipc = ipc;
  }

  async listProfiles(): Promise<AiProfile[]> {
    return this.call("ai_profile_list");
  }

  async saveProfile(profile: AiProfileInput, apiKey: string | undefined): Promise<AiProfile> {
    const input: Record<string, unknown> = { profile };
    if (apiKey !== undefined) {
      input.apiKey = apiKey;
    }
    return this.call("ai_profile_save", input);
  }

  async deleteProfile(profileId: string): Promise<void> {
    return this.call("ai_profile_delete", { profileId });
  }

  async runTest(profileId: string, kind: AiTestKind): Promise<AiTestReport> {
    const cmd =
      kind === "connection"
        ? "ai_profile_test_connection"
        : kind === "text"
          ? "ai_profile_test_text"
          : kind === "vision"
            ? "ai_profile_test_vision"
            : "ai_profile_test_structured";
    return this.call(cmd, { profileId });
  }

  async enqueue(assetIds: string[], force: boolean): Promise<number> {
    return this.call("ai_jobs_enqueue", { assetIds, force });
  }

  async pendingCount(rootId: string): Promise<number> {
    return this.call("ai_root_pending_count", { rootId });
  }

  async enqueueAllForRoot(rootId: string, confirmed: boolean): Promise<number> {
    return this.call("ai_jobs_enqueue_all_for_root", { rootId, confirmed });
  }

  async listJobs(status?: string): Promise<AiJobsSnapshot> {
    return this.call("ai_jobs_list", { status: status ?? null });
  }

  async cancelJob(jobId: string): Promise<void> {
    return this.call("ai_job_cancel", { jobId });
  }

  async pause(): Promise<void> {
    return this.call("ai_jobs_pause");
  }

  async resume(): Promise<void> {
    return this.call("ai_jobs_resume");
  }

  async retryFailed(): Promise<number> {
    return this.call("ai_jobs_retry_failed");
  }

  async runHistory(assetId: string): Promise<AiRun[]> {
    return this.call("ai_run_history", { assetId });
  }

  async setRootSettings(
    rootId: string,
    aiEnabled: boolean,
    sendExif: boolean,
    sendGps: boolean,
  ): Promise<void> {
    return this.call("ai_root_settings_set", {
      rootId,
      aiEnabled,
      sendExif,
      sendGps,
    });
  }

  /** `ai:jobs` イベント購読。 */
  async subscribe(handler: (event: AiJobsEvent) => void): Promise<() => void> {
    return this.ipc.listen<AiJobsEvent>("ai:jobs", (event) => {
      handler(event.payload);
    });
  }

  private async call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    try {
      const out = await this.ipc.invoke<T>(cmd, args);
      this.lastError = null;
      return out;
    } catch (error) {
      this.lastError = error instanceof Error ? error.message : String(error);
      throw error;
    }
  }
}
