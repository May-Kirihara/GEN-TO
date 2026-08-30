import { describe, expect, it, vi } from "vitest";
import { FolderTreeStore, registeredStateFor, type FsDirList, type FsIpc } from "./fsview";

type InvokeLog = Array<{ command: string; args?: Record<string, unknown> }>;
type MockIpc = FsIpc & { invokes: InvokeLog };

function makeIpc(responses: {
  roots?: FsDirList;
  dirs?: Record<string, FsDirList>;
}): MockIpc {
  const invokes: InvokeLog = [];
  const ipc: MockIpc = {
    invokes,
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      invokes.push({ command, args });
      if (command === "fs_list_dirs") {
        const path = args?.path;
        if (path === undefined) {
          if (responses.roots === undefined) {
            throw new Error("no roots fixture");
          }
          return responses.roots as T;
        }
        const list = responses.dirs?.[String(path)];
        if (list === undefined) {
          throw new Error(`no fixture for ${String(path)}`);
        }
        return list as T;
      }
      throw new Error(`unexpected command: ${command}`);
    },
  };
  return ipc;
}

function dirList(path: string, names: string[], error: string | null = null): FsDirList {
  return {
    path,
    parent: path === "/" ? null : path.split("/").slice(0, -1).join("/") || "/",
    entries: names.map((name) => ({ name, path: path === "/" ? `/${name}` : `${path}/${name}` })),
    error,
  };
}

describe("registeredStateFor", () => {
  const roots = [{ path: "/data/lib" }, { path: "/" }];

  it("完全一致はroot", () => {
    expect(registeredStateFor("/data/lib", roots)).toBe("root");
  });

  it("配下はinside", () => {
    expect(registeredStateFor("/data/lib/2026", roots)).toBe("inside");
  });

  it("ルート/自身は全パスがinside扱い (プレフィックス事故を避ける)", () => {
    expect(registeredStateFor("/etc", roots)).toBe("inside");
  });

  it("無関係・空はnull (ただし/登録ルート配下はinside)", () => {
    // "/" を登録ルートにした場合は全パスがinside (仕様どおり)
    expect(registeredStateFor("/data/other", roots)).toBe("inside");
    const libOnly = [{ path: "/data/lib" }];
    expect(registeredStateFor("/data/other", libOnly)).toBeNull();
    expect(registeredStateFor("/data/library2", libOnly)).toBeNull();
    expect(registeredStateFor("/data/lib2", libOnly)).toBeNull();
    expect(registeredStateFor("/x", [])).toBeNull();
  });
});

describe("FolderTreeStore", () => {
  it("initで起点ノードを読み込みvisibleRowsに反映する", async () => {
    const ipc = makeIpc({
      roots: {
        path: "",
        parent: null,
        entries: [
          { name: "ホーム", path: "/home/u" },
          { name: "ファイルシステム (/)", path: "/" },
        ],
        error: null,
      },
    });
    const store = new FolderTreeStore(ipc);
    await store.init();
    const rows = store.visibleRows();
    expect(rows.map((r) => r.name)).toEqual(["ホーム", "ファイルシステム (/)"]);
    expect(rows.every((r) => r.depth === 0 && !r.expanded)).toBe(true);
    expect(ipc.invokes).toEqual([{ command: "fs_list_dirs", args: {} }]);
  });

  it("toggleで子を遅延取得し展開、再toggleで折りたたむ", async () => {
    const ipc = makeIpc({
      roots: dirList("", ["home"]),
      dirs: { "/home": dirList("/home", ["u"]) },
    });
    const store = new FolderTreeStore(ipc);
    await store.init();
    await store.toggle("/home");
    let rows = store.visibleRows();
    expect(rows.map((r) => r.path)).toEqual(["/home", "/home/u"]);
    expect(rows[0].expanded).toBe(true);
    expect(rows[1].depth).toBe(1);

    await store.toggle("/home");
    rows = store.visibleRows();
    expect(rows.map((r) => r.path)).toEqual(["/home"]);
    // 子はキャッシュ済み (再展開で再取得しない)
    const invokesBefore = ipc.invokes.length;
    await store.toggle("/home");
    expect(ipc.invokes.length).toBe(invokesBefore);
  });

  it("エラー応答はノードに読み取り不能状態として保持する", async () => {
    const ipc = makeIpc({
      roots: dirList("", ["home"]),
      dirs: {
        "/home": dirList("/home", ["locked"]),
        "/home/locked": dirList("/home/locked", [], "読み取りできません (権限がありません)"),
      },
    });
    const store = new FolderTreeStore(ipc);
    await store.init();
    await store.toggle("/home");
    await store.expandTo("/home/locked");
    expect(store.errorOf("/home/locked")).toContain("読み取りできません");
    const row = store.visibleRows().find((r) => r.path === "/home/locked");
    expect(row?.error).toContain("読み取りできません");
    expect(row?.hasChildren).toBe(false);
  });

  it("選択状態の設定と展開パス列挙 (永続化用)", async () => {
    const ipc = makeIpc({ roots: dirList("", ["home"]), dirs: { "/home": dirList("/home", ["u"]) } });
    const store = new FolderTreeStore(ipc);
    await store.init();
    await store.toggle("/home");
    store.select("/home/u");
    expect(store.selected).toBe("/home/u");
    expect(store.expandedPaths()).toEqual(["/home"]);
  });

  it("invoke失敗はlastErrorへ (ツリー全体は壊れない)", async () => {
    const failing: FsIpc = {
      invoke: vi.fn().mockRejectedValue(new Error("ipc gone")),
    };
    const store = new FolderTreeStore(failing);
    await store.init();
    expect(store.lastError).toContain("ipc gone");
    expect(store.visibleRows()).toEqual([]);
  });
});
