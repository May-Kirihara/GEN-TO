// @vitest-environment happy-dom
/**
 * FolderTree.svelte コンポーネントテスト (UIUX-1 r1 B4)。
 *
 * - 右クリック3状態: root=3項目 / unrelated=登録1項目 / descendant=メニューなし
 * - roots差し替え (同path別rootId) で開いているメニューが閉じる (stale検出)
 * - ラベルクリックで選択+展開 (折りたたみは展開矢印のみ) (WIN-1 #2)
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import FolderTree from "./FolderTree.svelte";
import FolderTreeHarness from "./FolderTreeHarness.svelte";
import { FolderTreeStore, type FsDirList, type FsIpc } from "../fsview";
import type { LibraryRoot } from "../library";

function makeIpc(responses: Record<string, FsDirList>): FsIpc {
  return {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      if (command !== "fs_list_dirs") {
        throw new Error(`unexpected command: ${command}`);
      }
      const path = (args?.path as string | undefined) ?? "";
      const found = responses[path];
      if (found === undefined) {
        throw new Error(`no fixture for ${path}`);
      }
      return found as T;
    },
  };
}

function rootOf(id: string, path: string): LibraryRoot {
  return {
    id,
    uuid: `uu-${id}`,
    path,
    enabled: true,
    watch_enabled: true,
    available: true,
    ai_enabled: false,
    send_exif: true,
    send_gps: false,
  };
}

const IPC = makeIpc({
  "": {
    path: "",
    parent: null,
    entries: [
      { name: "lib", path: "/data/lib" },
      { name: "other", path: "/mnt/other" },
    ],
    error: null,
  },
});

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;
let unregister: ReturnType<typeof vi.fn>;
let rescan: ReturnType<typeof vi.fn>;

async function mountTree(
  roots: LibraryRoot[],
  options?: { ipc?: FsIpc; onSelect?: (path: string) => void },
): Promise<void> {
  const tree = new FolderTreeStore(options?.ipc ?? IPC);
  await tree.init();
  target = document.createElement("div");
  document.body.appendChild(target);
  unregister = vi.fn();
  rescan = vi.fn();
  component = mount(FolderTree, {
    target,
    props: {
      tree,
      roots,
      currentFolder: null,
      libraryMode: false,
      onSelect: options?.onSelect ?? (() => {}),
      onDrop: undefined,
      onRegisterRoot: () => {},
      onUnregisterRoot: unregister as unknown as (rootId: string) => void,
      onBackupRoot: () => {},
      onRescan: rescan as unknown as () => void,
    },
  });
  flushSync();
}

function rowLabel(path: string): HTMLElement {
  const found = Array.from(
    target!.querySelectorAll<HTMLElement>(".tree-row .tree-label"),
  ).find((label) => label.title === path);
  if (found === undefined) {
    throw new Error(`tree row not found: ${path}`);
  }
  return found;
}

function contextMenu(): HTMLElement | null {
  return target!.querySelector<HTMLElement>(".context-menu");
}

function menuLabels(): string[] {
  return Array.from(contextMenu()?.querySelectorAll("button") ?? []).map(
    (button) => (button.textContent ?? "").trim(),
  );
}

/** メニュー外のpointerdownを合成 (ContextMenuのwindow委托listener経由で閉じる)。 */
function clickOutside(): void {
  window.dispatchEvent(
    new PointerEvent("pointerdown", { bubbles: true, cancelable: true }),
  );
  flushSync();
}

function rightClick(path: string): void {
  rowLabel(path).dispatchEvent(
    new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
  );
  flushSync();
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  clickOutside();
  target?.remove();
  target = null;
});

describe("FolderTree 右クリック (3状態)", () => {
  it("登録ルート行: 解除・バックアップ・再スキャンの3項目", async () => {
    await mountTree([rootOf("7", "/data/lib")]);
    rightClick("/data/lib");
    expect(menuLabels()).toEqual(["登録を解除", "このルートをバックアップ", "ライブラリを再スキャン"]);
  });

  it("rootメニューの「登録を解除」はスナップショットのrootIdを渡す", async () => {
    await mountTree([rootOf("7", "/data/lib")]);
    rightClick("/data/lib");
    contextMenu()!.querySelectorAll("button")[0]!.click();
    flushSync();
    expect(unregister).toHaveBeenCalledWith("7");
  });

  it("未登録行: 「このフォルダーを登録ルートに追加」のみ", async () => {
    await mountTree([rootOf("7", "/data/lib")]);
    rightClick("/mnt/other");
    expect(menuLabels()).toEqual(["このフォルダーを登録ルートに追加"]);
  });

  it("登録ルート配下 (descendant): メニューを開かない", async () => {
    // /data/lib配下の行が存在する状態を作る (子を1つ持つツリー)
    const ipc = makeIpc({
      "": {
        path: "",
        parent: null,
        entries: [{ name: "lib", path: "/data/lib" }],
        error: null,
      },
      "/data/lib": {
        path: "/data/lib",
        parent: "",
        entries: [{ name: "2026", path: "/data/lib/2026" }],
        error: null,
      },
    });
    const tree = new FolderTreeStore(ipc);
    await tree.init();
    await tree.expandTo("/data/lib");
    target = document.createElement("div");
    document.body.appendChild(target);
    component = mount(FolderTree, {
      target,
      props: {
        tree,
        roots: [rootOf("7", "/data/lib")],
        currentFolder: null,
        libraryMode: false,
        onSelect: () => {},
        onDrop: undefined,
        onRegisterRoot: () => {},
        onUnregisterRoot: () => {},
        onBackupRoot: () => {},
        onRescan: () => {},
      },
    });
    flushSync();
    rightClick("/data/lib/2026");
    expect(contextMenu()).toBeNull();
  });
});

describe("FolderTree stale検出 (r1 B4: rootId込み完全比較)", () => {
  /** harness経由でmount (propsのrootsをリアクティブに差し替えるため)。 */
  async function mountHarness(
    roots: LibraryRoot[],
  ): Promise<{ setRoots(next: LibraryRoot[]): void }> {
    const tree = new FolderTreeStore(IPC);
    await tree.init();
    target = document.createElement("div");
    document.body.appendChild(target);
    unregister = vi.fn();
    rescan = vi.fn();
    const handle = mount(FolderTreeHarness, {
      target,
      props: {
        tree,
        initialRoots: roots,
        currentFolder: null,
        libraryMode: false,
        onUnregisterRoot: unregister as unknown as (rootId: string) => void,
        onRescan: rescan as unknown as () => void,
      },
    });
    component = handle as unknown as Record<string, unknown>;
    flushSync();
    return handle;
  }

  it("同path別rootIdへ差し替わったら開いているメニューを閉じる", async () => {
    const handle = await mountHarness([rootOf("7", "/data/lib")]);
    rightClick("/data/lib");
    expect(contextMenu()).not.toBeNull();
    handle.setRoots([rootOf("42", "/data/lib")]);
    flushSync();
    expect(contextMenu()).toBeNull();
  });

  it("無関係なroots変化 (他ルート追加) ではメニューを閉じない", async () => {
    const handle = await mountHarness([rootOf("7", "/data/lib")]);
    rightClick("/data/lib");
    expect(contextMenu()).not.toBeNull();
    handle.setRoots([rootOf("7", "/data/lib"), rootOf("9", "/mnt/other")]);
    flushSync();
    expect(contextMenu()).not.toBeNull();
    expect(menuLabels()).toHaveLength(3);
  });

  it("登録解除 (同pathのルートが消えた) でもメニューを閉じる", async () => {
    const handle = await mountHarness([rootOf("7", "/data/lib")]);
    rightClick("/data/lib");
    expect(contextMenu()).not.toBeNull();
    handle.setRoots([rootOf("9", "/mnt/other")]);
    flushSync();
    expect(contextMenu()).toBeNull();
  });
});

describe("FolderTree ラベルクリック (WIN-1 #2: 選択+展開)", () => {
  /** ルート直下に /data/lib (子1つ) を持つfixture。ラベルクリック展開で
   * ensureChildrenのinvokeが走るため、子パスの応答も用意する。 */
  const childIpc = makeIpc({
    "": {
      path: "",
      parent: null,
      entries: [{ name: "lib", path: "/data/lib" }],
      error: null,
    },
    "/data/lib": {
      path: "/data/lib",
      parent: "",
      entries: [{ name: "2026", path: "/data/lib/2026" }],
      error: null,
    },
  });

  it("ラベルクリックで選択され、折りたたみ状態なら展開する", async () => {
    const select = vi.fn();
    await mountTree([rootOf("7", "/data/lib")], { ipc: childIpc, onSelect: select });
    rowLabel("/data/lib").click();
    flushSync();
    expect(select).toHaveBeenCalledWith("/data/lib");
    // 展開されて子行が現れる (ensureChildrenのinvoke解決後)
    await vi.waitFor(() => {
      rowLabel("/data/lib/2026");
    });
  });

  it("展開済み行のラベル再クリックでは折りたたまない", async () => {
    await mountTree([rootOf("7", "/data/lib")], { ipc: childIpc });
    rowLabel("/data/lib").click();
    await vi.waitFor(() => {
      rowLabel("/data/lib/2026");
    });
    rowLabel("/data/lib").click();
    flushSync();
    await new Promise((resolve) => setTimeout(resolve, 0));
    flushSync();
    // 子行は表示されたまま (折りたたみは展開矢印のみ)
    expect(() => rowLabel("/data/lib/2026")).not.toThrow();
    expect(
      rowLabel("/data/lib").closest(".tree-row")?.getAttribute("aria-expanded"),
    ).toBe("true");
  });
});
