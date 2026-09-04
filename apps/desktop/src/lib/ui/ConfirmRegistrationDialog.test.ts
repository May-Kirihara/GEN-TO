// @vitest-environment happy-dom
/**
 * ConfirmRegistrationDialog.svelte コンポーネントテスト (UIUX-2)。
 *
 * 危険パス登録の確認ダイアログ: パス表示・警告文言・Cancel/登録の
 * 各操作とEscape取消を実際の描画で検証する。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import ConfirmRegistrationDialog from "./ConfirmRegistrationDialog.svelte";

let target: HTMLElement | null = null;

function mountDialog(
  props: { path: string; onConfirm: () => void; onCancel: () => void },
): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  const component = mount(ConfirmRegistrationDialog, { target, props });
  components.push(component);
  flushSync();
}

const components: Array<ReturnType<typeof mount>> = [];

afterEach(() => {
  while (components.length > 0) {
    unmount(components.pop()!);
  }
  target?.remove();
  target = null;
});

describe("ConfirmRegistrationDialog (UIUX-2)", () => {
  it("対象パスと警告文言を表示する", () => {
    mountDialog({ path: "/", onConfirm: vi.fn(), onCancel: vi.fn() });
    const dialog = document.querySelector('[data-testid="register-confirm"]');
    expect(dialog).not.toBeNull();
    expect(
      document.querySelector('[data-testid="register-confirm-path"]')?.textContent,
    ).toBe("/");
    expect(dialog?.textContent).toContain("ファイルシステム全体");
    expect(dialog?.textContent).toContain("登録しますか");
  });

  it("キャンセルボタンはonCancelのみ呼ぶ (登録不発火)", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    mountDialog({ path: "/home", onConfirm, onCancel });
    (
      document.querySelector<HTMLButtonElement>(
        '[data-testid="register-confirm-cancel"]',
      ) as HTMLButtonElement
    ).click();
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("登録するボタンはonConfirmのみ呼ぶ", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    mountDialog({ path: "C:\\", onConfirm, onCancel });
    (
      document.querySelector<HTMLButtonElement>(
        '[data-testid="register-confirm-ok"]',
      ) as HTMLButtonElement
    ).click();
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("Escapeは取消 (onCancel)", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    mountDialog({ path: "/", onConfirm, onCancel });
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    flushSync();
    expect(onCancel).toHaveBeenCalledTimes(1);
    expect(onConfirm).not.toHaveBeenCalled();
  });
});
