/**
 * モーダルダイアログ用の最低限フォーカストラップ (WP-18 E5)。
 *
 * - 適用時にコンテナ (または最初のフォーカス可能要素) へフォーカスを移す
 * - Tab / Shift+Tab をコンテナ内でラップさせる
 * - Escape等の他キーは既存ハンドラへ素通し
 *
 * 循環トラップ以外の包括的なWCAG対応はスコープ外
 * (docs/dev/a11y-scope.md 参照)。
 */

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function focusTrap(node: HTMLElement): { destroy(): void } {
  node.setAttribute("tabindex", "-1");

  function focusables(): HTMLElement[] {
    return Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key !== "Tab") {
      return;
    }
    const items = focusables();
    if (items.length === 0) {
      event.preventDefault();
      node.focus();
      return;
    }
    const first = items[0]!;
    const last = items[items.length - 1]!;
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || !node.contains(active)) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || !node.contains(active)) {
      event.preventDefault();
      first.focus();
    }
  }

  const items = focusables();
  if (items.length > 0) {
    items[0]!.focus();
  } else {
    node.focus();
  }
  node.addEventListener("keydown", onKeydown);

  return {
    destroy() {
      node.removeEventListener("keydown", onKeydown);
    },
  };
}
