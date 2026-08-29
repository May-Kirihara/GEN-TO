/**
 * ローカル画像プロトコルURLビルダ (WP-4 / docs/dev/api-contract.md §12)。
 *
 * - Rustハンドラは authority として `localhost` (または空) のみを受ける。
 *   ビルダは常に `localhost` 形式で生成する。
 * - ID は10進数字のみ・最大20桁、generation (v) は10進・最大18桁。
 *   不正な値は例外にする (静かにエンコードして拡張構文を生まない)。
 */

/** gento-thumb で指定できるサイズクラス (preview2048は gento-preview 側)。 */
export type ThumbSizeClass = "s256" | "s512";

const HOST = "localhost";

function requireDecimalDigits(value: string, what: string, maxLen: number): string {
  if (value.length === 0 || value.length > maxLen || !/^[0-9]+$/.test(value)) {
    throw new Error(`invalid ${what}: expected 1..${maxLen} decimal digits`);
  }
  return value;
}

function requireGeneration(gen: number): string {
  if (!Number.isInteger(gen) || gen < 0) {
    throw new Error("invalid generation: expected a non-negative integer");
  }
  return requireDecimalDigits(String(gen), "generation", 18);
}

/** `gento-thumb://localhost/asset/{id}/{sizeClass}?v={gen}` */
export function thumbUrl(assetId: string, sizeClass: ThumbSizeClass, gen: number): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  if (sizeClass !== "s256" && sizeClass !== "s512") {
    throw new Error(`invalid thumb size class: ${String(sizeClass)}`);
  }
  return `gento-thumb://${HOST}/asset/${id}/${sizeClass}?v=${requireGeneration(gen)}`;
}

/** `gento-preview://localhost/asset/{id}?v={gen}` */
export function previewUrl(assetId: string, gen: number): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  return `gento-preview://${HOST}/asset/${id}?v=${requireGeneration(gen)}`;
}

/** `gento-original://localhost/asset/{id}?v={gen}` */
export function originalUrl(assetId: string, gen: number): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  return `gento-original://${HOST}/asset/${id}?v=${requireGeneration(gen)}`;
}
