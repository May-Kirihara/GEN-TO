/**
 * LibraryStore (WP-2 登録ルートUIストア) のテスト。
 * IPCをモックして状態遷移とcommand引数を検証する。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LibraryStore, type LibraryIpc, type LibraryRoot } from "./library";

function root(overrides: Partial<LibraryRoot> = {}): LibraryRoot {
  return {
    id: "1",
    uuid: "00000000-0000-7000-8000-000000000001",
    path: "/data/photos",
    enabled: true,
    watch_enabled: true,
    available: true,
    ...overrides,
  };
}

type IpcState = {
  roots: LibraryRoot[];
  invokeError?: (command: string) => string | null;
  calls: Array<{ command: string; args?: Record<string, unknown> }>;
};

function makeIpc(state: IpcState): LibraryIpc {
  return {
    invoke: async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      state.calls.push({ command, args });
      const injected = state.invokeError?.(command);
      if (injected !== null && injected !== undefined) {
        throw new Error(injected);
      }
      switch (command) {
        case "library_roots_list":
          return state.roots as T;
        case "library_roots_add":
          return { root: root({ path: (args?.path as string) ?? "" }), overlapping: [] } as T;
        case "scan_status":
          return {
            scanning: false,
            phase: null,
            found: 3,
            updated: 1,
            relocated: 0,
            missing: 0,
            hashed: 3,
            pending_hash: 0,
            errors: 0,
            watch_degraded: false,
          } as T;
        default:
          return undefined as T;
      }
    },
    listen: async () => () => {},
  };
}

describe("LibraryStore", () => {
  let state: IpcState;
  let store: LibraryStore;

  beforeEach(() => {
    state = { roots: [root()], calls: [] };
    store = new LibraryStore(makeIpc(state));
  });

  it("init fetches roots and status and listens roots:changed", async () => {
    const dispose = await store.init();
    expect(store.roots).toEqual([root()]);
    expect(store.status.found).toBe(3);
    const listCalls = state.calls.filter((c) => c.command === "library_roots_list");
    expect(listCalls.length).toBeGreaterThanOrEqual(1);
    expect(state.calls.some((c) => c.command === "scan_status")).toBe(true);
    dispose();
  });

  it("addRoot sends path and shows overlap warning when present", async () => {
    const ipc = makeIpc(state);
    ipc.invoke = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
      state.calls.push({ command, args });
      if (command === "library_roots_add") {
        return {
          root: root({ id: "2", path: "/data/photos/sub" }),
          overlapping: [root()],
        } as T;
      }
      if (command === "library_roots_list") {
        return [
          root(),
          root({ id: "2", path: "/data/photos/sub" }),
        ] as T;
      }
      return undefined as T;
    };
    const storeWithOverlap = new LibraryStore(ipc);
    const ok = await storeWithOverlap.addRoot("/data/photos/sub");
    expect(ok).toBe(true);
    expect(storeWithOverlap.overlapWarning).toContain("/data/photos");
    expect(storeWithOverlap.overlapWarning).toContain("親子関係");
    expect(storeWithOverlap.lastNotice).toContain("登録しました");
    expect(storeWithOverlap.roots.length).toBe(2);
    const addCall = state.calls.find((c) => c.command === "library_roots_add");
    expect(addCall?.args).toEqual({ path: "/data/photos/sub" });
  });

  it("addRoot without overlap clears warning", async () => {
    store.overlapWarning = "stale";
    const ok = await store.addRoot("/fresh");
    expect(ok).toBe(true);
    expect(store.overlapWarning).toBeNull();
  });

  it("addRoot failure records error and returns false", async () => {
    state.invokeError = (command) =>
      command === "library_roots_add" ? "path does not exist" : null;
    const ok = await store.addRoot("/nonexistent");
    expect(ok).toBe(false);
    expect(store.lastError).toContain("path does not exist");
  });

  it("removeRoot passes rootId and refreshes list", async () => {
    const ok = await store.removeRoot("7");
    expect(ok).toBe(true);
    const call = state.calls.find((c) => c.command === "library_roots_remove");
    expect(call?.args).toEqual({ rootId: "7" });
    expect(state.calls.filter((c) => c.command === "library_roots_list").length).toBe(1);
  });

  it("setEnabled sends rootId and enabled", async () => {
    const ok = await store.setEnabled("3", false);
    expect(ok).toBe(true);
    const call = state.calls.find((c) => c.command === "library_roots_set_enabled");
    expect(call?.args).toEqual({ rootId: "3", enabled: false });
  });

  it("startScan and stopScan invoke commands", async () => {
    expect(await store.startScan()).toBe(true);
    expect(state.calls.some((c) => c.command === "scan_start")).toBe(true);
    expect(await store.stopScan()).toBe(true);
    expect(state.calls.some((c) => c.command === "scan_stop")).toBe(true);
  });

  it("startScan rejection surfaces busy error", async () => {
    state.invokeError = (command) => (command === "scan_start" ? "scan already in progress" : null);
    const ok = await store.startScan();
    expect(ok).toBe(false);
    expect(store.lastError).toContain("already in progress");
  });

  it("useSource invokes set_catalog_source and updates source on success", async () => {
    expect(await store.useSource("library")).toBe(true);
    expect(store.source).toBe("library");
    const call = state.calls.find((c) => c.command === "set_catalog_source");
    expect(call?.args).toEqual({ source: "library" });
  });

  it("useSource failure keeps previous source and records error", async () => {
    expect(await store.useSource("temp")).toBe(true);
    state.invokeError = (command) =>
      command === "set_catalog_source" ? "cannot switch catalog source while scanning" : null;
    expect(await store.useSource("library")).toBe(false);
    expect(store.source).toBe("temp");
    expect(store.lastError).toContain("while scanning");
  });

  it("roots:changed event triggers refresh", async () => {
    const handler: {
      current: ((event: { payload: unknown }) => void) | null;
    } = { current: null };
    const ipc: LibraryIpc = {
      invoke: makeIpc(state).invoke,
      listen: async <T,>(_event: string, listener: (event: { payload: T }) => void) => {
        handler.current = listener as (event: { payload: unknown }) => void;
        return () => {};
      },
    };
    const eventStore = new LibraryStore(ipc);
    await eventStore.init();
    state.roots = [root(), root({ id: "2", path: "/data/other" })];
    handler.current?.({ payload: null });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(eventStore.roots.length).toBe(2);
    eventStore.dispose();
  });

  it("refreshStatus failure keeps empty status quietly", async () => {
    state.invokeError = (command) => (command === "scan_status" ? "unavailable" : null);
    await store.refreshStatus();
    expect(store.status.scanning).toBe(false);
    expect(store.status.phase).toBeNull();
    expect(store.lastError).toBeNull();
  });

  it("refreshStatus surfaces watch_degraded degradation flag", async () => {
    // FG-WP2: watcher未確立 (inotify上限等) はstatusのwatch_degradedで露出。
    const ipc = makeIpc(state);
    ipc.invoke = async <T>(command: string): Promise<T> => {
      if (command === "scan_status") {
        return {
          scanning: false,
          phase: null,
          found: 0,
          updated: 0,
          relocated: 0,
          missing: 0,
          hashed: 0,
          pending_hash: 0,
          errors: 0,
          watch_degraded: true,
        } as T;
      }
      return makeIpc(state).invoke<T>(command);
    };
    const degradedStore = new LibraryStore(ipc);
    await degradedStore.refreshStatus();
    expect(degradedStore.status.watch_degraded).toBe(true);
  });

  it("dispose stops polling and unlistens", async () => {
    vi.useFakeTimers();
    const unlisten = vi.fn();
    const ipc: LibraryIpc = {
      invoke: makeIpc(state).invoke,
      listen: async () => unlisten,
    };
    const pollingStore = new LibraryStore(ipc);
    await pollingStore.init();
    vi.advanceTimersByTime(2100);
    const statusCalls1 = state.calls.filter((c) => c.command === "scan_status").length;
    pollingStore.dispose();
    vi.advanceTimersByTime(2100);
    const statusCalls2 = state.calls.filter((c) => c.command === "scan_status").length;
    expect(statusCalls2).toBe(statusCalls1);
    expect(unlisten).toHaveBeenCalled();
    vi.useRealTimers();
  });
});
