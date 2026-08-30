/**
 * Rust `health` command (gento-core HealthInfo) とのDTO契約とUI状態への正規化。
 *
 * ID類 (将来の画像ID等) はRust側u64をJSON数値で渡すとJSの安全な整数範囲
 * (2^53 - 1) を超えるため、IPC DTOでは文字列とする
 * (docs/implementation-plan.md §9 DTO候補参照)。
 */
export type HealthInfo = {
  app_name: string;
  version: string;
  /** IPC DTO契約のAPIバージョン (Rust: gento_core::GENTO_API_VERSION)。 */
  api_version: number;
  dataset_path: string | null;
  dataset_exists: boolean;
  /** Unixエポックからのミリ秒 (JS安全整数範囲内) 。 */
  timestamp_ms: number;
};

export type HealthUiState =
  | {
      kind: "ok";
      /** データセットディレクトリが存在するか。 */
      datasetPresent: boolean;
      /** UI表示用の絶対パス。未設定ならnull。 */
      datasetPath: string | null;
      timestampMs: number;
    }
  | {
      kind: "error";
      message: string;
    };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isHealthInfo(value: Record<string, unknown>): value is HealthInfo {
  return (
    typeof value["app_name"] === "string" &&
    typeof value["version"] === "string" &&
    typeof value["api_version"] === "number" &&
    (typeof value["dataset_path"] === "string" || value["dataset_path"] === null) &&
    typeof value["dataset_exists"] === "boolean" &&
    typeof value["timestamp_ms"] === "number"
  );
}

/** health応答 (unknown) をUI状態へ正規化する。形状不正はerrorへ落とす。 */
export function normalizeHealth(response: unknown): HealthUiState {
  if (!isRecord(response)) {
    return { kind: "error", message: "health response is not an object" };
  }
  if (!isHealthInfo(response)) {
    return { kind: "error", message: "health response has an unexpected shape" };
  }
  return {
    kind: "ok",
    datasetPresent: response["dataset_exists"],
    datasetPath: response["dataset_path"],
    timestampMs: response["timestamp_ms"],
  };
}
