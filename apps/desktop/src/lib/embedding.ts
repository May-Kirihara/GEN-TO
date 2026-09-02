/**
 * 自然言語検索 (ローカルruri-v3埋め込み) のフロントエンド側 (WP-11)。
 *
 * - embedding_status / enable / disable / model_delete のinvokeラッパー
 * - 純粋ヘルパー (表示ラベル・有効化可否・同意表示文言の組み立て) は
 *   vitestで単体テスト対象 (embedding.test.ts)
 * - 検索バー統合 (意味検索結果のマージ) はWP-12 — このmoduleは
 *   AIパネルの「自然言語検索」タブのみを担う
 */

export type EmbeddingIpc = {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void>;
};

/** パッケージ状態DTO (backend ModelPackageStateDto)。 */
export type ModelPackageState = {
  modelId: string;
  /** "verified" | "missing" | "missing_files" | "mismatch" */
  state: string;
  detail: string;
  packageDir: string;
  embeddingDim: number;
  license: string;
};

/** ワーカー状態 (backend WorkerPhaseDto)。 */
export type WorkerPhaseDto = {
  /** "not_started" | "running" | "restarting" | "crashed" */
  phase: string;
  restartCount: number;
  lastError: string | null;
};

/** `embedding_status` 応答DTO (backend EmbeddingStatusDto)。 */
export type EmbeddingStatus = {
  enabled: boolean;
  model: string;
  modelsDir: string;
  packages: ModelPackageState[];
  worker: WorkerPhaseDto;
  loadedModel: string | null;
  embeddedCount: number;
  totalCount: number;
  pendingJobs: number;
  selfcheckMargin: number | null;
  warning: string | null;
};

/** モデル選択肢 (UI表示用。backend MODEL_IDSと同一順)。 */
export const MODEL_CHOICES: ReadonlyArray<{ id: string; label: string; hint: string }> = [
  { id: "ruri-v3-70m", label: "標準 (70m)", hint: "384次元 — 既定" },
  { id: "ruri-v3-30m", label: "軽量 (30m)", hint: "256次元 — 省メモリ" },
];

/** モデル状態の表示ラベル。 */
export function packageStateLabel(state: string): string {
  switch (state) {
    case "verified":
      return "検証済み";
    case "missing":
      return "未インストール";
    case "missing_files":
      return "ファイル欠落";
    case "mismatch":
      return "検証失敗 (破損)";
    default:
      return state;
  }
}

/** ワーカー状態の表示ラベル。 */
export function workerPhaseLabel(phase: string): string {
  switch (phase) {
    case "not_started":
      return "未起動";
    case "running":
      return "実行中";
    case "restarting":
      return "再起動中";
    case "crashed":
      return "自動再起停止 (異常終了連続)";
    default:
      return phase;
  }
}

/** 有効化操作が可能か (選択モデルの検証済みパッケージがある場合のみ)。 */
export function canEnable(status: EmbeddingStatus): boolean {
  return (
    status.packages.some((p) => p.modelId === status.model && p.state === "verified") &&
    status.worker.phase !== "crashed"
  );
}

/** 有効化確認ダイアログの文言 (設計§2: 同意表示 — 処理件数を提示)。 */
export function enableConfirmText(status: EmbeddingStatus): string {
  const target = status.totalCount.toLocaleString();
  return [
    `自然言語検索を有効にします (${modelLabel(status.model)})。`,
    "埋め込みはローカルモデルで実行され、外部へ送信されません。",
    `既存画像 ${target}件を対象にバックグラウンドで埋め込みを生成します。`,
    "この操作はいつでも無効化できます (ベクトルは残ります)。",
  ].join("\n");
}

/** モデルIDの表示ラベル。 */
export function modelLabel(modelId: string): string {
  const choice = MODEL_CHOICES.find((c) => c.id === modelId);
  return choice ? choice.label : modelId;
}

/** 進捗表示 (0-1。対象0件は1)。 */
export function progressRatio(status: EmbeddingStatus): number {
  if (status.totalCount === 0) {
    return 1;
  }
  return Math.min(1, status.embeddedCount / status.totalCount);
}

/** 進捗の一行テキスト。 */
export function progressText(status: EmbeddingStatus): string {
  if (!status.enabled) {
    return "無効";
  }
  const done = status.embeddedCount.toLocaleString();
  const total = status.totalCount.toLocaleString();
  const pending = status.pendingJobs.toLocaleString();
  return `埋め込み済み ${done} / ${total} 件 (キュー ${pending})`;
}

/** 埋め込みストア (状態なしの薄いinvokeラッパ + 通知購読)。 */
export class EmbeddingStore {
  private ipc: EmbeddingIpc;

  constructor(ipc: EmbeddingIpc) {
    this.ipc = ipc;
  }

  async status(): Promise<EmbeddingStatus> {
    return this.ipc.invoke<EmbeddingStatus>("embedding_status");
  }

  async enable(model?: string): Promise<EmbeddingStatus> {
    return this.ipc.invoke<EmbeddingStatus>("embedding_enable", { model: model ?? null });
  }

  async disable(): Promise<EmbeddingStatus> {
    return this.ipc.invoke<EmbeddingStatus>("embedding_disable");
  }

  async deleteModel(model: string): Promise<EmbeddingStatus> {
    return this.ipc.invoke<EmbeddingStatus>("embedding_model_delete", { model });
  }

  /** クラッシュ通知などのイベント購読。 */
  async subscribe(handler: (payload: string) => void): Promise<() => void> {
    return this.ipc.listen<string>("embedding:notice", (event) => handler(event.payload));
  }
}
