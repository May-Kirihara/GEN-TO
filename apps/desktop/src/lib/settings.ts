/**
 * 設定・診断のIPCラッパーとUI正規化 (MVP WP-15)。
 *
 * - 設定の正本は backend `app_settings` (api-contract §49)。
 *   破損値はbackendが既定値へフォールバックした形で返るため、
 *   フロントは常に描画可能な値を受け取る。
 * - endpoint分類 (ローカル/プライベート/外部) はURL hostの純関数。
 *   外部送信の判断材料としてAI設定面・ルートAI panelへ表示する。
 */

export type SettingsDto = {
  schemaVersion: number;
  corruptKeys: string[];
  general: { logLevel: string };
  cache: {
    thumbsMaxBytes: number;
    usedBytes: number;
    maxBytes: number;
  };
};

export type DiagnosticsDto = {
  db: {
    dbOpen: boolean;
    integrityCheck: string;
    schemaVersion: number;
    dbBytes: number;
    walBytes: number;
    settingsCorruptCount: number;
    dbPath: string;
  };
  fts: {
    expectedDocs: number;
    actualDocs: number;
    missingDocs: number;
    orphanDocs: number;
    duplicateDocs: number;
    staleContents: number;
    consistent: boolean;
    lastRebuildAt: string | null;
  };
  usearch: {
    model: string;
    indexedCount: number;
    sqliteCount: number;
    stale: boolean;
    dimensions: number | null;
    ready: boolean;
    building: boolean;
    lastError: string | null;
  };
  models: {
    enabled: boolean;
    packages: Array<{
      modelId: string;
      state: string;
      embeddingDim: number;
      license: string;
    }>;
    workerPhase: string;
    workerRestarts: number;
    workerLastError: string | null;
    loadedModel: string | null;
  };
  jobs: {
    byType: Array<{ jobType: string; status: string; count: number }>;
    oldestPendingAgeSeconds: number | null;
  };
};

/** `diagnostics_bundle_create` の応答。 */
export type BundleResultDto = { path: string };

/** AI endpointの分類 (外部送信リスクの判断材料)。 */
export type EndpointClass = "local" | "private" | "external" | "invalid";

/**
 * URL hostからendpoint分類を導く (WP-15。PM決定6):
 * - `local`: localhost / 127.0.0.1 / ::1 (同一マシン)
 * - `private`: RFC1918 (10/8, 172.16/12, 192.168/16) + link-local
 *   (169.254/10, fe80::/10) + ULA (fc00::/7) (閉じたネットワーク)
 * - `external`: それ以外 (インターネットへ出る)
 * - `invalid`: URLとして解釈できない
 */
export function classifyEndpoint(baseUrl: string): EndpointClass {
  let url: URL;
  try {
    url = new URL(baseUrl);
  } catch {
    return "invalid";
  }
  const host = url.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (host === "localhost" || host === "127.0.0.1" || host === "::1" || host === "0.0.0.0") {
    return "local";
  }
  // IPv4
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (v4) {
    const oct = (i: number) => Number.parseInt(v4[i] ?? "0", 10);
    if ([oct(1), oct(2), oct(3), oct(4)].some((n) => Number.isNaN(n) || n > 255)) {
      return "external";
    }
    const [a, b] = [oct(1), oct(2)];
    if (a === 10) return "private";
    if (a === 172 && b >= 16 && b <= 31) return "private";
    if (a === 192 && b === 168) return "private";
    if (a === 169 && b === 254) return "private";
    if (a === 127) return "local";
    return "external";
  }
  // IPv6
  if (host.includes(":")) {
    if (host === "::1") return "local";
    if (host.startsWith("fe8") || host.startsWith("fe9") || host.startsWith("fea") || host.startsWith("feb")) {
      return "private"; // link-local
    }
    if (host.startsWith("fc") || host.startsWith("fd")) return "private"; // ULA
    return "external";
  }
  return "external";
}

/** endpoint分類の日本語表示。 */
export function endpointClassLabel(kind: EndpointClass): string {
  switch (kind) {
    case "local":
      return "ローカル";
    case "private":
      return "プライベートネットワーク";
    case "external":
      return "外部";
    default:
      return "不明なURL";
  }
}

/** バイト数の日本語表示 (GiB/MiB/KiB)。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return "-";
  }
  if (bytes >= 1024 ** 3) {
    return `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
  }
  if (bytes >= 1024 ** 2) {
    return `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }
  return `${bytes} B`;
}

/** MiB入力 → バイト数 (キャッシュ上限入力用)。 */
export function mibToBytes(mib: number): number {
  return Math.round(mib * 1024 * 1024);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function numOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function strOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

/**
 * settings_get応答 (unknown) をUI状態へ正規化。形状不正・IPC失敗時も
 * 既定値 (info / 2GiB) を返す — 「設定破損時に安全な既定値で起動」の
 * フロント側堅牢化 (backendのフォールバックと二重)。
 */
export function normalizeSettings(response: unknown): SettingsDto {
  if (!isRecord(response)) {
    return defaultSettings();
  }
  const cache = isRecord(response.cache) ? response.cache : {};
  const general = isRecord(response.general) ? response.general : {};
  const corrupt = Array.isArray(response.corruptKeys)
    ? response.corruptKeys.filter((k): k is string => typeof k === "string")
    : [];
  return {
    schemaVersion: numOr(response.schemaVersion, 1),
    corruptKeys: corrupt,
    general: { logLevel: strOr(general.logLevel, "info") },
    cache: {
      thumbsMaxBytes: numOr(cache.thumbsMaxBytes, 2 * 1024 ** 3),
      usedBytes: numOr(cache.usedBytes, 0),
      maxBytes: numOr(cache.maxBytes, numOr(cache.thumbsMaxBytes, 2 * 1024 ** 3)),
    },
  };
}

export function defaultSettings(): SettingsDto {
  return {
    schemaVersion: 1,
    corruptKeys: [],
    general: { logLevel: "info" },
    cache: {
      thumbsMaxBytes: 2 * 1024 ** 3,
      usedBytes: 0,
      maxBytes: 2 * 1024 ** 3,
    },
  };
}

/** IPC境界 (テストで差し替え)。 */
export type SettingsIpc = {
  settingsGet: () => Promise<unknown>;
  setLogLevel: (level: string) => Promise<unknown>;
  setThumbsMaxBytes: (bytes: number) => Promise<unknown>;
  diagnosticsSnapshot: () => Promise<unknown>;
  bundleCreate: (destDir: string) => Promise<BundleResultDto>;
};

export class SettingsStore {
  private ipc: SettingsIpc;
  lastError: string | null = null;

  constructor(ipc: SettingsIpc) {
    this.ipc = ipc;
  }

  async load(): Promise<SettingsDto> {
    this.lastError = null;
    try {
      return normalizeSettings(await this.ipc.settingsGet());
    } catch (err) {
      this.lastError = err instanceof Error ? err.message : String(err);
      return defaultSettings();
    }
  }

  async setLogLevel(level: string): Promise<SettingsDto> {
    return normalizeSettings(await this.ipc.setLogLevel(level));
  }

  async setThumbsMaxBytes(bytes: number): Promise<SettingsDto> {
    return normalizeSettings(await this.ipc.setThumbsMaxBytes(bytes));
  }

  async diagnostics(): Promise<DiagnosticsDto> {
    return (await this.ipc.diagnosticsSnapshot()) as DiagnosticsDto;
  }

  async createBundle(destDir: string): Promise<BundleResultDto> {
    return this.ipc.bundleCreate(destDir);
  }
}

/** Tauri invoke実装。 */
export function tauriSettingsIpc(invoke: (cmd: string, args?: unknown) => Promise<unknown>): SettingsIpc {
  return {
    settingsGet: () => invoke("settings_get"),
    setLogLevel: (level) => invoke("settings_set_log_level", { level }),
    setThumbsMaxBytes: (bytes) => invoke("settings_set_thumbs_max_bytes", { bytes }),
    diagnosticsSnapshot: () => invoke("diagnostics_snapshot"),
    bundleCreate: (destDir) =>
      invoke("diagnostics_bundle_create", { destDir }) as Promise<BundleResultDto>,
  };
}
