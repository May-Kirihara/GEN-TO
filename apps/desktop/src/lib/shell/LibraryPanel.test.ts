// @vitest-environment happy-dom
/**
 * LibraryPanel.svelte コンポーネントテスト (UIUX-3 N2)。
 *
 * 確認dialog経由の登録 (= onAddRootのPromiseがdialog確定まで保留される
 * App実装) でも、登録成功時に入力がクリアされることを実際の描画で検証する。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import LibraryPanel from "./LibraryPanel.svelte";
import type { LibraryRoot, LibraryStore } from "../library";
import { EMPTY_SCAN_STATUS } from "../library";

function makeLibrary(): LibraryStore {
  return {
    roots: [],
    status: EMPTY_SCAN_STATUS,
    onChange: () => () => {},
    setEnabled: async () => true,
    removeRoot: async () => true,
    startScan: async () => true,
    stopScan: async () => true,
  } as unknown as LibraryStore;
}

function mountPanel(onAddRoot: (path: string) => Promise<boolean>): void {
  target = document.createElement("div");
  document.body.appendChild(target);
  components.push(
    mount(LibraryPanel, {
      target,
      props: {
        library: makeLibrary(),
        roots: [] as LibraryRoot[],
        notice: "",
        error: null,
        scanBusy: false,
        onRefresh: () => {},
        onShowLibrary: async () => true,
        onBackupRoot: () => {},
        onAddRoot,
      },
    }),
  );
  flushSync();
}

let target: HTMLElement | null = null;
const components: Array<ReturnType<typeof mount>> = [];

afterEach(() => {
  while (components.length > 0) {
    unmount(components.pop()!);
  }
  target?.remove();
  target = null;
});

describe("LibraryPanel 登録入力のクリア契約 (UIUX-3 N2)", () => {
  it("dialog経由の登録 (onAddRootが確定まで保留) でも成功時に入力がクリアされる", async () => {
    let settle!: (ok: boolean) => void;
    const pending = new Promise<boolean>((resolve) => {
      settle = resolve;
    });
    const onAddRoot = vi.fn(() => pending);
    mountPanel(onAddRoot);

    const input = document.querySelector<HTMLInputElement>(".roots-add input");
    expect(input).not.toBeNull();
    input!.value = "/home";
    input!.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();

    (
      document.querySelector<HTMLButtonElement>(".roots-add button")
    )?.click();
    await Promise.resolve();
    expect(onAddRoot).toHaveBeenCalledTimes(1);
    expect(onAddRoot).toHaveBeenCalledWith("/home");
    // dialog確定前 (= Promise未解決) は入力が残る
    expect(input!.value).toBe("/home");

    // dialogで「登録する」→ 登録成功が確定 → 入力クリア
    settle(true);
    await Promise.resolve();
    await Promise.resolve();
    flushSync();
    expect(input!.value).toBe("");
  });

  it("登録が失敗 (false解決) すれば入力はクリアされない", async () => {
    let settle!: (ok: boolean) => void;
    const pending = new Promise<boolean>((resolve) => {
      settle = resolve;
    });
    mountPanel(vi.fn(() => pending));

    const input = document.querySelector<HTMLInputElement>(".roots-add input");
    input!.value = "/home";
    input!.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    (
      document.querySelector<HTMLButtonElement>(".roots-add button")
    )?.click();
    await Promise.resolve();

    settle(false);
    await Promise.resolve();
    await Promise.resolve();
    flushSync();
    expect(input!.value).toBe("/home");
  });
});
