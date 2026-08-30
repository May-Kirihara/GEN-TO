/**
 * パスコピー用の最小クリップボード実装 (MVP WP-4)。
 *
 * - navigator.clipboard.writeText を優先 (WebViewのsecure context)。
 * - 失敗・不在時は隠しtextarea + document.execCommand("copy") へフォールバック
 *   (ユーザー操作起点なら WebKitGTK でも動作)。
 * - 依存はテストで注入可能 (tauri-plugin-clipboard-managerは入れない)。
 */

export type ClipboardDeps = {
  writeText?: (text: string) => Promise<void>;
  execCopy?: (text: string) => boolean;
};

function execCopyViaTextarea(text: string): boolean {
  if (typeof document === "undefined") {
    return false;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  // 画面外へ置く (レイアウト影響なし・フォーカス移動でスクロールさせない)
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const ok = (() => {
    try {
      return document.execCommand("copy");
    } catch {
      return false;
    }
  })();
  textarea.remove();
  return ok;
}

/** クリップボードへ書き込む。両経路失敗時にfalse。 */
export async function copyText(text: string, deps: ClipboardDeps = {}): Promise<boolean> {
  const write =
    deps.writeText ??
    (typeof navigator !== "undefined" && navigator.clipboard
      ? (t: string) => navigator.clipboard.writeText(t)
      : undefined);
  if (write !== undefined) {
    try {
      // WebKitGTKでは権限まわりでpromiseが決定しない場合があるため
      // 短いタイムアウトで切り上げてフォールバックへ流す
      await Promise.race([
        write(text),
        new Promise((_, reject) => setTimeout(() => reject(new Error("timeout")), 500)),
      ]);
      return true;
    } catch {
      // フォールバックへ
    }
  }
  const exec = deps.execCopy ?? execCopyViaTextarea;
  return exec(text);
}
