/**
 * ローカル画像プロトコルURLビルダ (WP-4 / docs/dev/api-contract.md §12)。
 *
 * - Rustハンドラは authority として `localhost` (または空) のみを受ける。
 * - Windows/Android (WebView2 / Android WebView) は非標準スキームURLを扱えない
 *   ため、wry の回避形式 `{http|https}://{scheme}.localhost/{path}` で要求する
 *   (wry custom_protocol_workaround。ハンドラへ渡す際にwryが元の
 *   `{scheme}://localhost/...` へ復元するため、Rustパーサは全プラットフォームで
 *   同一形式を受け取り、protocol.rs にプラットフォーム分岐は不要)。
 * - プラットフォーム形式は tauri が注入する `convertFileSrc`
 *   (window.__TAURI_INTERNALS__) から得る。同関数はパス全体を
 *   `encodeURIComponent` で符号化するが、Rustパーサは生セグメントのASCII厳密
 *   一致で `%2F` 等の符号化区切りを拒否する契約のため、`/` をprobeして応答から
 *   基点のみを取り出し、パスは生のまま結合する。
 * - internalsが無い環境 (単体テスト・素ブラウザ) や非互換の応答形式では従来形式
 *   `{scheme}://localhost/...` へフォールバックする。
 * - ID は10進数字のみ・最大20桁、version (v) は10進・最大18桁。
 *   不正な値は例外にする (静かにエンコードして拡張構文を生まない)。
 * - `?v=` は内容感応バージョン (FG-1): 元ファイルの path+size+mtime と
 *   サムネイルアルゴリズムバージョンから決まる値 (thumb:ready イベントまたは
 *   ページDTOの version)。元ファイルが変わればURLが変わり、WebViewの
 *   immutableキャッシュが更新される。
 */

import { convertFileSrc } from "@tauri-apps/api/core";

/** gento-thumb で指定できるサイズクラス (preview2048は gento-preview 側)。 */
export type ThumbSizeClass = "s256" | "s512";

const HOST = "localhost";

/** `"/"` を `convertFileSrc` へ渡した際の応答末尾 (テンプレートの `/` + `encodeURIComponent("/")`)。 */
const PROBE_SUFFIX = "/%2F";

/** tauri (crates/tauri/scripts/core.js) が注入する internals の必要部分。 */
type TauriInternals = {
  convertFileSrc?: (filePath: string, protocol?: string) => string;
};

/**
 * スキームのURL基点を返す。internals注入済みなら `convertFileSrc("/", scheme)`
 * の応答から末尾の `/%2F` を除いた基点 (Windows/Androidは
 * `{http|https}://{scheme}.localhost`、他は `{scheme}://localhost`)。
 * 未注入・非互換応答は従来形式。
 * パスそのものを `convertFileSrc` へ渡してはならない: 内部の
 * `encodeURIComponent` が `/` も `%2F` へ符号化し、protocol.rs の
 * 生セグメント厳密一致契約で拒否されるため。
 */
function schemeBase(scheme: string): string {
  const internals =
    typeof window === "undefined"
      ? undefined
      : (window as { __TAURI_INTERNALS__?: TauriInternals }).__TAURI_INTERNALS__;
  if (typeof internals?.convertFileSrc !== "function") {
    return `${scheme}://${HOST}`;
  }
  const converted = convertFileSrc("/", scheme);
  return converted.endsWith(PROBE_SUFFIX)
    ? converted.slice(0, -PROBE_SUFFIX.length)
    : `${scheme}://${HOST}`;
}

function requireDecimalDigits(value: string, what: string, maxLen: number): string {
  if (value.length === 0 || value.length > maxLen || !/^[0-9]+$/.test(value)) {
    throw new Error(`invalid ${what}: expected 1..${maxLen} decimal digits`);
  }
  return value;
}

function requireVersion(version: number): string {
  if (!Number.isInteger(version) || version < 0) {
    throw new Error("invalid version: expected a non-negative integer");
  }
  return requireDecimalDigits(String(version), "version", 18);
}

/** `{基点}/asset/{id}/{sizeClass}?v={version}` (基点はプラットフォーム形式) */
export function thumbUrl(
  assetId: string,
  sizeClass: ThumbSizeClass,
  version: number,
): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  if (sizeClass !== "s256" && sizeClass !== "s512") {
    throw new Error(`invalid thumb size class: ${String(sizeClass)}`);
  }
  return `${schemeBase("gento-thumb")}/asset/${id}/${sizeClass}?v=${requireVersion(version)}`;
}

/** `{基点}/asset/{id}?v={version}` (基点はプラットフォーム形式) */
export function previewUrl(assetId: string, version: number): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  return `${schemeBase("gento-preview")}/asset/${id}?v=${requireVersion(version)}`;
}

/** `{基点}/asset/{id}?v={version}` (基点はプラットフォーム形式) */
export function originalUrl(assetId: string, version: number): string {
  const id = requireDecimalDigits(assetId, "asset id", 20);
  return `${schemeBase("gento-original")}/asset/${id}?v=${requireVersion(version)}`;
}
