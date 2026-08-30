<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { normalizeHealth, type HealthInfo, type HealthUiState } from "./lib/health";
  import {
    CatalogStore,
    type CatalogIpc,
    type CatalogSort,
    type CatalogSummary,
    type ImageListItem,
  } from "./lib/catalog";
  import {
    ThumbnailStore,
    type ThumbnailIpc,
  } from "./lib/thumbnails";
  import { LibraryStore, type LibraryIpc, type LibraryRoot } from "./lib/library";
  import { GridStore, type GridIpc } from "./lib/grid/grid-store";
  import { ScrollPositionStore } from "./lib/grid/scroll-position";
  import type { ThumbCellClass } from "./lib/grid/layout";
  import ThumbnailGrid from "./lib/grid/ThumbnailGrid.svelte";
  import { ViewerStore, type ViewerIpc } from "./lib/viewer/viewer-store";
  import ImageViewer from "./lib/viewer/ImageViewer.svelte";
  import { CompareStore, type CompareIpc } from "./lib/compare/compare-store";
  import CompareView from "./lib/compare/CompareView.svelte";
  import { perf } from "./lib/perf";
  import { breadcrumbOf, deriveEmptyState } from "./lib/browse";
  import { FolderTreeStore } from "./lib/fsview";
  import { registeredStateFor } from "./lib/fsview";
  import { QuickPreviewStore } from "./lib/preview/quick-preview";
  import QuickPreview from "./lib/preview/QuickPreview.svelte";
  import {
    SelectionSessionStore,
    type SessionCounts,
  } from "./lib/session/selection-session";
  import {
    LayoutPrefsStore,
    clampPaneWidth,
  } from "./lib/ui/layout-prefs";
  import { FolderPrefsStore } from "./lib/ui/folder-prefs";
  import { copyText } from "./lib/ui/clipboard";
  import { KeyDispatcher } from "./lib/keys";
  import ContextMenu, { type MenuAction } from "./lib/ui/ContextMenu.svelte";
  import FolderTree from "./lib/shell/FolderTree.svelte";
  import LibraryPanel from "./lib/shell/LibraryPanel.svelte";
  import InfoPanel from "./lib/shell/InfoPanel.svelte";
  import DevDrawer from "./lib/shell/DevDrawer.svelte";

  // ===== IPC・ストア (既存資産) =====

  const ipc: CatalogIpc = { invoke, listen };
  const catalog = new CatalogStore(ipc);
  const thumbIpc: ThumbnailIpc = { invoke, listen };
  const thumbnails = new ThumbnailStore(thumbIpc);
  const libraryIpc: LibraryIpc = { invoke, listen };
  const library = new LibraryStore(libraryIpc);
  const grid = new GridStore({ invoke, listen } satisfies GridIpc);
  const scrollPositions = new ScrollPositionStore(localStorage);

  let summary: CatalogSummary | null = $state(null);
  let progress = $state(catalog.progress);
  let catalogError: string | null = $state(null);
  let devBridgeActive = $state(false);
  let devPanelOpen = $state(false);

  let libraryRoots: LibraryRoot[] = $state([]);
  let libraryNotice = $state("");
  let libraryError = $state<string | null>(null);
  let libraryScanBusy = $state(false);

  // ===== レイアウト (WP-4: 3ペイン) =====

  const layoutStore = new LayoutPrefsStore(localStorage);
  const folderStore = new FolderPrefsStore(localStorage);
  let leftVisible = $state(true);
  let rightVisible = $state(true);
  let leftWidth = $state(240);
  let rightWidth = $state(280);
  let cellClass = $state<ThumbCellClass>("s256");
  let overscanChoice = $state(2);

  // ===== 閲覧状態 =====

  /** 現在閲覧中のフォルダ (Real/adhocモード)。 */
  let currentFolder = $state<string | null>(null);
  let catalogSource = $state<"temp" | "library">("temp");
  let folderNotice = $state("");
  let healthStatus: HealthUiState | null = $state(null);
  let rawHealth: HealthInfo | null = $state(null);
  let fetchedAt = $state("");

  const tree = new FolderTreeStore({ invoke });

  // ===== グリッド・ビューア (スパイク資産) =====

  let gridComponent: ThumbnailGrid | null = $state(null);
  let viewerChoice: "single" | "compare2" | "compare4" = $state("single");
  let viewerMode: "single" | "compare2" | "compare4" | null = $state(null);
  let viewerStore: ViewerStore | null = $state(null);
  let compareStore: CompareStore | null = $state(null);
  let viewerNotice = $state("");
  let gridSelection = $state<{ ids: string[]; focusIndex: number | null }>({
    ids: [],
    focusIndex: null,
  });
  /** グリッド内容の版 (ページ到着・readyで進む。InfoPanel再描画用)。 */
  let gridTick = $state(0);

  const viewerIpc: ViewerIpc = { invoke, listen };
  const compareIpc: CompareIpc = { invoke, listen };
  const viewerItems = {
    total: () => grid.summary?.total ?? 0,
    get: (index: number) => grid.getItem(index),
    ensureRange: (start: number, end: number) => grid.pager.ensureRange(start, end),
  };

  // ===== クイックプレビュー (WP-4) =====

  const quickPreview = new QuickPreviewStore({ invoke, listen }, viewerItems);

  // ===== 選別セッション (WP-4) =====

  const session = new SelectionSessionStore(localStorage);
  let sessionCounts: SessionCounts | null = $state(null);
  let sessionNotice = $state("");
  /** InfoPanelへ渡すセッション表示スナップショット (活性・origin)。 */
  let sessionSnapshot = $state<{ active: boolean; origin: string | null }>({
    active: false,
    origin: null,
  });
  /** 非アクティブ時の再開候補。 */
  let sessionResumable = $state<{ origin: string; count: number } | null>(null);
  /** セッションID → カタログindex (移動・集計の高速化。UI非反映の内部キャッシュ)。 */
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- 永続化不要な補助写像
  const sessionIndex = new Map<string, number>();

  // ===== コンテキストメニュー (WP-4) =====

  let contextMenu: { x: number; y: number; actions: MenuAction[] } | null = $state(null);
  let osNotice = $state("");

  // ===== キー一元化 (WP-4) =====

  const keyContext = { devBridge: false };
  const keys = new KeyDispatcher();
  const unregisterKeys: Array<() => void> = [];

  // ===== 通知の同期 =====

  const stopChangeNotification = catalog.onChange(() => {
    summary = catalog.summary;
    progress = catalog.progress;
    catalogError = catalog.lastError;
    if (catalog.folderError !== null) {
      folderNotice = catalog.folderError;
    } else {
      folderNotice = "";
    }
    if (catalog.summary !== null) {
      grid.syncSummary(catalog.summary);
    }
    refreshSessionCounts();
  });

  let markedFirstPage = false;
  const stopGridNotifyForPerf = grid.onChange(() => {
    gridTick++;
    if (!markedFirstPage && grid.pager.stats.fetched > 0) {
      markedFirstPage = true;
      perf.markFirstPage();
    }
  });

  const stopLibraryNotification = library.onChange(() => {
    libraryRoots = library.roots;
    libraryNotice = library.overlapWarning ?? library.lastNotice ?? "";
    libraryError = library.lastError;
    libraryScanBusy = library.status.scanning;
  });

  const stopSessionNotify = session.onChange(() => {
    refreshSessionCounts();
  });

  let disposeCatalog: (() => void) | null = null;
  let disposeThumbnails: (() => void) | null = null;
  let disposeGrid: (() => void) | null = null;
  let disposeLibrary: (() => void) | null = null;
  let disposeQuickPreview: (() => void) | null = null;

  function toErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  // ===== ヘルス・起動 =====

  async function refreshHealth(): Promise<void> {
    try {
      const response = await invoke<HealthInfo>("health");
      rawHealth = response;
      healthStatus = normalizeHealth(response);
      perf.markHealthOk();
      // 起動直後の現在フォルダ表示 (--dataset の絶対パス)
      if (currentFolder === null && response.dataset_path !== null) {
        currentFolder = response.dataset_path;
      }
    } catch (error) {
      rawHealth = null;
      healthStatus = { kind: "error", message: toErrorMessage(error) };
    }
    fetchedAt = new Date().toLocaleTimeString();
  }

  // ===== フォルダ閲覧 (ツリー・パンくず) =====

  /** フォルダ選択: 登録済みならライブラリソース、未登録なら一時走査へ切替。 */
  async function selectFolder(path: string): Promise<void> {
    osNotice = "";
    const registered = registeredStateFor(path, libraryRoots) !== null;
    tree.select(path);
    if (registered) {
      const ok = await library.useSource("library");
      if (ok) {
        catalogSource = "library";
        currentFolder = path;
        await catalog.refreshSummary();
        await catalog.loadPage(0);
        saveFolderPrefs();
      } else {
        libraryError = library.lastError ?? "ライブラリ表示へ切替できませんでした";
      }
      return;
    }
    const ok = await catalog.openFolder(path);
    if (ok) {
      catalogSource = "temp";
      currentFolder = path;
      gridSelection = { ids: [], focusIndex: null };
      saveFolderPrefs();
    } else {
      folderNotice = catalog.folderError ?? "フォルダを開けませんでした";
    }
  }

  function saveFolderPrefs(): void {
    folderStore.save({
      lastFolder: currentFolder,
      expanded: tree.expandedPaths(),
    });
  }

  async function startScan(): Promise<void> {
    folderNotice = "";
    const ok = await catalog.startScan();
    if (!ok) {
      folderNotice = catalog.lastError ?? "スキャン開始に失敗しました";
    }
  }

  /** $derived内でのflow narrowingを避けるためのアクセサ (常に最新値)。 */
  function summaryNow(): CatalogSummary | null {
    return summary;
  }

  const scanBusy = $derived(
    (summaryNow()?.scanning ?? false) || libraryScanBusy,
  );

  // ===== ビューア (スパイク資産・Enter/ダブルクリック/ボタン) =====

  function closeViewer(): void {
    viewerMode = null;
    viewerStore?.dispose();
    compareStore?.dispose();
    viewerStore = null;
    compareStore = null;
  }

  async function openViewerForChoice(index: number): Promise<void> {
    viewerNotice = "";
    const focus = gridSelection.focusIndex ?? index;
    try {
      if (viewerChoice === "single") {
        const store = new ViewerStore(viewerIpc, viewerItems);
        viewerStore = store;
        viewerMode = "single";
        await store.init();
        await store.open(index);
      } else {
        const count = viewerChoice === "compare2" ? 2 : 4;
        const store = new CompareStore(compareIpc, {
          ...viewerItems,
          findIndexById: (id: string) =>
            grid.pager.findIndicesByIds(new Set([id])).get(id) ?? null,
        });
        compareStore = store;
        viewerMode = viewerChoice;
        await store.init();
        await store.open(count, gridSelection.ids, focus);
      }
    } catch (error) {
      viewerNotice = toErrorMessage(error);
    }
  }

  // ===== 選択・情報パネル・セッション同期 =====

  function onGridSelectionChange(snapshot: { ids: string[]; focusIndex: number | null }): void {
    gridSelection = snapshot;
    if (session.active) {
      const focusId = snapshot.focusIndex !== null ? grid.getItem(snapshot.focusIndex)?.id : null;
      if (focusId !== undefined && focusId !== null) {
        session.noteFocus(focusId);
      }
      rebuildSessionIndex();
      refreshSessionCounts();
    }
  }

  function focusItem(): ImageListItem | null {
    void gridTick;
    const focus = gridSelection.focusIndex;
    return focus === null ? null : grid.getItem(focus);
  }

  function triageOf(id: string): "keep" | "reject" | "undecided" | undefined {
    const index = sessionIndex.get(id);
    if (index !== undefined) {
      return grid.getItem(index)?.triage;
    }
    // index未解決 (フォルダセッションなど) はpagerキャッシュから直接探す
    const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
    return found !== undefined ? grid.getItem(found)?.triage : undefined;
  }

  function refreshSessionCounts(): void {
    sessionCounts = session.counts(triageOf);
    sessionSnapshot = {
      active: session.active,
      origin: session.session?.origin ?? null,
    };
    sessionResumable = session.active ? null : session.peekPersisted();
  }

  /** セッションID→カタログindexの写像を更新 (ソート・再スキャン後の追従)。 */
  function rebuildSessionIndex(): void {
    if (!session.active) {
      sessionIndex.clear();
      return;
    }
    const ids = session.session?.ids ?? [];
    if (ids.length === 0) {
      return;
    }
    const wanted = new Set(ids);
    for (const [id, index] of grid.pager.findIndicesByIds(wanted)) {
      sessionIndex.set(id, index);
    }
  }

  async function collectAllIds(): Promise<string[]> {
    const total = grid.summary?.total ?? 0;
    if (total === 0) {
      return [];
    }
    await grid.pager.ensureRange(0, total - 1);
    const ids: string[] = [];
    for (let index = 0; index < total; index++) {
      const item = grid.getItem(index);
      if (item !== null) {
        ids.push(item.id);
      }
    }
    return ids;
  }

  function sessionOriginLabel(count: number): string {
    const place = catalogSource === "library"
      ? "ライブラリ"
      : (currentFolder ?? "(未選択)");
    return `${place} (${count.toLocaleString()}件)`;
  }

  async function startSessionFolder(): Promise<void> {
    sessionNotice = "";
    const ids = await collectAllIds();
    if (ids.length === 0) {
      sessionNotice = "対象画像がありません";
      return;
    }
    sessionIndex.clear();
    ids.forEach((id, index) => sessionIndex.set(id, index));
    session.start(ids, sessionOriginLabel(ids.length));
    await focusSessionPosition();
  }

  function startSessionSelection(): void {
    sessionNotice = "";
    if (gridSelection.ids.length === 0) {
      sessionNotice = "画像を選択してください";
      return;
    }
    sessionIndex.clear();
    const found = grid.pager.findIndicesByIds(new Set(gridSelection.ids));
    for (const [id, index] of found) {
      sessionIndex.set(id, index);
    }
    session.start(
      [...gridSelection.ids],
      `選択画像 (${gridSelection.ids.length.toLocaleString()}件)`,
      gridSelection.focusIndex ?? 0,
    );
    void focusSessionPosition();
  }

  async function resumeSession(): Promise<void> {
    sessionNotice = "";
    if (!session.resume()) {
      sessionNotice = "再開できるセッションがありません";
      return;
    }
    // 全IDのindexを解決 (前回の閲覧状態と現在カタログの対応付け)
    await collectAllIds();
    rebuildSessionIndex();
    await focusSessionPosition();
  }

  async function focusSessionPosition(): Promise<void> {
    const id = session.currentId();
    if (id === null) {
      return;
    }
    let index = sessionIndex.get(id);
    if (index === undefined) {
      await collectAllIds();
      rebuildSessionIndex();
      index = sessionIndex.get(id);
    }
    if (index === undefined) {
      sessionNotice = "セッション位置の画像が現在の一覧に見つかりません";
      return;
    }
    gridComponent?.selectAndFocus(index);
  }

  function sessionMove(delta: number): void {
    if (!session.active) {
      return;
    }
    if (delta > 0) {
      session.next();
    } else {
      session.prev();
    }
    void focusSessionPosition();
  }

  function endSession(): void {
    session.end();
    sessionIndex.clear();
    sessionCounts = null;
  }

  // ===== コンテキストメニュー・OS連携 (WP-4) =====

  function openGridContextMenu(event: MouseEvent, index: number): void {
    const item = grid.getItem(index);
    if (item === null) {
      return;
    }
    // 未選択項目でのメニューは単一選択に切り替える
    if (!gridSelection.ids.includes(item.id)) {
      gridComponent?.selectAndFocus(index);
    }
    const ids = gridSelection.ids.includes(item.id) ? gridSelection.ids : [item.id];
    const items: ImageListItem[] = [];
    for (const id of ids) {
      const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
      const resolved = found !== undefined ? grid.getItem(found) : null;
      if (resolved !== null) {
        items.push(resolved);
      }
    }
    if (items.length === 0) {
      items.push(item);
    }
    const focus = items[0];
    const suffix = items.length > 1 ? ` (${items.length.toLocaleString()}件中先頭)` : "";
    contextMenu = {
      x: event.clientX,
      y: event.clientY,
      actions: [
        {
          id: "reveal",
          label: `OSのファイルマネージャーで表示${suffix}`,
          run: async () => {
            osNotice = "";
            try {
              await invoke("reveal_in_file_manager", { path: focus.path });
            } catch (error) {
              osNotice = toErrorMessage(error);
            }
          },
        },
        {
          id: "open",
          label: "外部アプリで開く",
          run: async () => {
            osNotice = "";
            try {
              await invoke("open_with_default_app", { path: focus.path });
            } catch (error) {
              osNotice = toErrorMessage(error);
            }
          },
        },
        {
          id: "copy-path",
          label:
            items.length > 1
              ? `パスをコピー (${items.length.toLocaleString()}件)`
              : "パスをコピー",
          run: async () => {
            const ok = await copyText(items.map((entry) => entry.path).join("\n"));
            osNotice = ok
              ? `パスをコピーしました (${items.length.toLocaleString()}件)`
              : "クリップボードへのコピーに失敗しました";
          },
        },
      ],
    };
  }

  // ===== キー割り当て (WP-4一元化。割り当て表はlib/keys.ts) =====

  function focusGrid(): void {
    gridComponent?.focus();
  }

  function registerKeys(): void {
    unregisterKeys.push(
      keys.register("quickPreview", () => {
        if (viewerMode !== null) {
          return true; // ビューア中は不使用 (スクロール等の既定動作を止める)
        }
        if (quickPreview.active) {
          quickPreview.close();
          focusGrid();
          return true;
        }
        const focus = gridSelection.focusIndex;
        if (focus === null || grid.getItem(focus) === null) {
          return false;
        }
        void quickPreview.open(focus);
        return true;
      }),
    );
    unregisterKeys.push(
      keys.register("openViewer", () => {
        if (quickPreview.active) {
          const index = quickPreview.index;
          quickPreview.close();
          void openViewerForChoice(index);
          return true;
        }
        if (viewerMode !== null) {
          return false;
        }
        const focus = gridSelection.focusIndex;
        if (focus === null) {
          return false;
        }
        void openViewerForChoice(focus);
        return true;
      }),
    );
    unregisterKeys.push(
      keys.register("back", () => {
        if (contextMenu !== null) {
          contextMenu = null;
          return true;
        }
        if (quickPreview.active) {
          quickPreview.close();
          focusGrid();
          return true;
        }
        if (viewerMode !== null) {
          closeViewer();
          return true;
        }
        return false;
      }),
    );
    unregisterKeys.push(
      keys.register("prevImage", () => {
        if (!quickPreview.active) {
          return false; // 通常時はグリッドの選択移動へ流す
        }
        void quickPreview.move(-1).then(() => {
          gridComponent?.selectAndFocus(quickPreview.index);
        });
        return true;
      }),
    );
    unregisterKeys.push(
      keys.register("nextImage", () => {
        if (!quickPreview.active) {
          return false;
        }
        void quickPreview.move(1).then(() => {
          gridComponent?.selectAndFocus(quickPreview.index);
        });
        return true;
      }),
    );
    // F2=リネーム / Delete=ゴミ箱 はWP-7で実装 (割り当てのみ登録)。
    // dev bridge有効時はF2が選択操作に使われるため干渉を避ける。
    unregisterKeys.push(
      keys.register("rename", () => {
        if (keyContext.devBridge) {
          return false;
        }
        osNotice = "リネームはWP-7で実装予定です (F2)";
        return true;
      }),
    );
    unregisterKeys.push(
      keys.register("trash", () => {
        if (keyContext.devBridge) {
          return false;
        }
        osNotice = "ゴミ箱への移動はWP-7で実装予定です (Delete)";
        return true;
      }),
    );
  }

  // ===== ペイン操作 =====

  function toggleLeft(): void {
    leftVisible = !leftVisible;
    saveLayout();
  }

  function toggleRight(): void {
    rightVisible = !rightVisible;
    saveLayout();
  }

  function saveLayout(): void {
    layoutStore.save({
      leftVisible,
      rightVisible,
      leftWidth,
      rightWidth,
      thumbSize: cellClass,
    });
  }

  function setCellClass(next: ThumbCellClass): void {
    if (cellClass === next) {
      return;
    }
    cellClass = next;
    grid.setSizeClass(next);
    saveLayout();
  }

  /** サムネイルサイズスライダー (0=小 s256 / 1=大 s512)。 */
  function sizeSliderValue(): number {
    return cellClass === "s512" ? 1 : 0;
  }

  function onSizeSlider(value: number): void {
    setCellClass(value >= 1 ? "s512" : "s256");
  }

  function onSortChange(sort: CatalogSort): void {
    void catalog.setSort(sort);
  }

  // ペインリサイズ (ドラッグ)。pointer captureで要素外移動にも追従。
  function startResize(side: "left" | "right", event: PointerEvent): void {
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const startWidth = side === "left" ? leftWidth : rightWidth;
    const onMove = (move: PointerEvent): void => {
      const delta = move.clientX - startX;
      const next = clampPaneWidth(
        side === "left" ? startWidth + delta : startWidth - delta,
      );
      if (side === "left") {
        leftWidth = next;
      } else {
        rightWidth = next;
      }
    };
    const onUp = (): void => {
      target.removeEventListener("pointermove", onMove);
      target.removeEventListener("pointerup", onUp);
      saveLayout();
    };
    target.addEventListener("pointermove", onMove);
    target.addEventListener("pointerup", onUp);
  }

  // ===== ライフサイクル =====

  onMount(() => {
    perf.markMounted();
    perf.startLongTaskObserver();
    perf.startFrameMonitor();

    // レイアウト・閲覧状態の復元 (WP-4)
    const layout = layoutStore.load();
    leftVisible = layout.leftVisible;
    rightVisible = layout.rightVisible;
    leftWidth = layout.leftWidth;
    rightWidth = layout.rightWidth;
    cellClass = layout.thumbSize;
    grid.setSizeClass(cellClass);

    registerKeys();
    unregisterKeys.push(keys.install());

    void refreshHealth();
    void catalog.init().then(async (dispose) => {
      disposeCatalog = dispose;
      // 起動時は --dataset (決定論的) を一時走査する。前回フォルダは
      // ツリー選択のハイライトとして復元し、クリックで戻れるようにする
      await catalog.startScan();
      const healthPath = rawHealth?.dataset_path ?? null;
      if (healthPath !== null) {
        currentFolder = healthPath;
      }
    });
    void thumbnails.init().then((dispose) => {
      disposeThumbnails = dispose;
    });
    void grid.init().then((dispose) => {
      disposeGrid = dispose;
    });
    void library.init().then((dispose) => {
      disposeLibrary = dispose;
    });
    void quickPreview.init().then((dispose) => {
      disposeQuickPreview = dispose;
    });
    void (async () => {
      const prefs = folderStore.load();
      await tree.init();
      for (const path of prefs.expanded.slice(-64)) {
        await tree.expandTo(path);
      }
      if (prefs.lastFolder !== null) {
        tree.select(prefs.lastFolder);
      }
    })();

    // 実機検証ブリッジ (WP-6/WP-7)。dev build、またはdevtoolsビルドのreleaseで
    // GENTO_DEV_BRIDGE=1 のときのみ有効 (WP-0)。
    void (async () => {
      let enabled = import.meta.env.DEV;
      if (!enabled) {
        try {
          enabled = await invoke<boolean>("dev_bridge_enabled");
        } catch {
          enabled = false;
        }
      }
      devBridgeActive = enabled;
      keyContext.devBridge = enabled;
      devPanelOpen = enabled;
      if (enabled) {
        const { installDevBridge } = await import("./lib/devtest");
        installDevBridge({
          getViewer: () => viewerStore,
          getCompare: () => compareStore,
          getMode: () => viewerMode,
          getGrid: () => grid,
          getThumb: () => thumbnails,
          getPerf: () => perf,
          getGridComponent: () => gridComponent,
          getSelection: () => gridSelection,
          invoke,
        });
      }
    })();
  });

  onDestroy(() => {
    stopChangeNotification();
    stopGridNotifyForPerf();
    stopLibraryNotification();
    stopSessionNotify();
    for (const unregister of unregisterKeys.splice(0)) {
      unregister();
    }
    scrollPositions.flush();
    session.flush();
    saveFolderPrefs();
    disposeCatalog?.();
    disposeThumbnails?.();
    disposeGrid?.();
    disposeLibrary?.();
    disposeQuickPreview?.();
    quickPreview.dispose();
    perf.stop();
  });

  // ===== 表示導出 =====

  const gridModeKey = $derived.by(() => {
    const mode = summaryNow()?.mode;
    if (catalogSource === "library" || mode === "library") {
      return "library";
    }
    if (mode === "virtual") {
      return "virtual";
    }
    return `real:${currentFolder ?? "-"}`;
  });

  const emptyState = $derived(
    deriveEmptyState({
      scanning: scanBusy,
      total: summaryNow()?.total ?? 0,
      folderError: folderNotice !== "" ? folderNotice : null,
    }),
  );

  const crumbs = $derived(
    catalogSource === "library" ? [] : breadcrumbOf(currentFolder ?? ""),
  );

  const sortValue = $derived(summaryNow()?.sort ?? "name");

  const focusInfo = $derived(focusItem());
</script>

<div class="app-shell">
  <header class="app-header">
    <h1 class="app-title">GEN-TO</h1>
    <span
      class="health-dot"
      class:ok={healthStatus?.kind === "ok"}
      class:ng={healthStatus?.kind === "error"}
      title={healthStatus?.kind === "ok"
        ? `DB: ${healthStatus.dbOpen ? "接続" : "未接続"} / ${rawHealth?.dataset_path ?? ""}`
        : healthStatus?.kind === "error"
          ? healthStatus.message
          : "接続確認中"}
    ></span>
    <span class="source-badge" class:library={catalogSource === "library"}>
      {catalogSource === "library" ? "ライブラリ" : "一時走査"}
    </span>
    {#if scanBusy}
      <span class="scan-indicator" title="スキャン進行中">
        スキャン中
        {progress?.found.toLocaleString() ?? summary?.scanned_count.toLocaleString() ?? 0}件
      </span>
    {/if}
    {#if library.status.pending_hash > 0}
      <span class="scan-indicator" title="BLAKE3ハッシュ計算の残件数">
        ハッシュ待ち {library.status.pending_hash.toLocaleString()}
      </span>
    {/if}
    {#if library.status.watch_degraded}
      <span class="warn-indicator" title="フォルダ監視が未確立の登録ルートがあります (ポーリング再スキャンで運用中)">
        ⚠ 監視劣化
      </span>
    {/if}
    <div class="header-spacer"></div>
    {#if osNotice}
      <span class="header-notice">{osNotice}</span>
    {/if}
    <button onclick={() => void startScan()} disabled={scanBusy}>
      スキャン開始
    </button>
    <button class:active={leftVisible} onclick={toggleLeft} title="フォルダツリーの表示切替">
      左ペイン
    </button>
    <button class:active={rightVisible} onclick={toggleRight} title="情報パネルの表示切替">
      右ペイン
    </button>
    {#if devBridgeActive}
      <button class:active={devPanelOpen} onclick={() => (devPanelOpen = !devPanelOpen)}>
        開発パネル
      </button>
    {/if}
  </header>

  <div class="app-panes">
    {#if leftVisible}
      <aside class="side-pane left" style="width:{leftWidth}px">
        <FolderTree
          {tree}
          roots={libraryRoots}
          {currentFolder}
          libraryMode={catalogSource === "library"}
          onSelect={(path) => void selectFolder(path)}
        />
        <LibraryPanel
          {library}
          roots={libraryRoots}
          notice={libraryNotice}
          error={libraryError}
          scanBusy={libraryScanBusy}
          onRefresh={() => void library.refreshRoots()}
        />
      </aside>
      <div
        class="pane-resizer"
        role="separator"
        aria-orientation="vertical"
        onpointerdown={(event) => startResize("left", event)}
      ></div>
    {/if}

    <section class="center-pane">
      <div class="toolbar">
        <nav class="breadcrumb" aria-label="現在場所">
          {#if catalogSource === "library"}
            <span class="crumb-current">ライブラリ (登録ルート全体)</span>
          {:else}
            {#each crumbs as crumb, index (crumb.path)}
              {#if index > 0}<span class="crumb-sep">›</span>{/if}
              <button class="crumb" onclick={() => void selectFolder(crumb.path)}>
                {crumb.name}
              </button>
            {/each}
          {/if}
        </nav>
        <label class="sort-label">
          ソート
          <select
            value={sortValue}
            onchange={(event) => onSortChange(event.currentTarget.value as CatalogSort)}
          >
            <option value="name">名前</option>
            <option value="date_desc">更新日時</option>
            <option value="size_desc">容量</option>
          </select>
        </label>
        <div class="toolbar-spacer"></div>
        <label class="size-label" title="サムネイルサイズ">
          サイズ
          <input
            type="range"
            min="0"
            max="1"
            step="1"
            value={sizeSliderValue()}
            oninput={(event) => onSizeSlider(Number(event.currentTarget.value))}
          />
        </label>
        <span class="viewer-choice">
          <button class:active={viewerChoice === "single"} onclick={() => (viewerChoice = "single")}>
            単一
          </button>
          <button
            class:active={viewerChoice === "compare2"}
            onclick={() => (viewerChoice = "compare2")}
          >
            2枚比較
          </button>
          <button
            class:active={viewerChoice === "compare4"}
            onclick={() => (viewerChoice = "compare4")}
          >
            4枚比較
          </button>
          <button
            onclick={() => {
              const focus = gridSelection.focusIndex;
              if (focus !== null) {
                void openViewerForChoice(focus);
              } else {
                viewerNotice = "グリッドで項目を選択してください";
              }
            }}
          >
            開く
          </button>
        </span>
      </div>
      {#if viewerNotice}
        <p class="toolbar-notice">{viewerNotice}</p>
      {/if}
      {#if folderNotice}
        <p class="toolbar-notice error">{folderNotice}</p>
      {/if}

      <div class="grid-wrap">
        {#key `${gridModeKey}-${cellClass}`}
          <ThumbnailGrid
            bind:this={gridComponent}
            {grid}
            scrollStore={scrollPositions}
            modeKey={gridModeKey}
            {cellClass}
            overscanRows={overscanChoice}
            showStats={devBridgeActive}
            onOpen={(index) => void openViewerForChoice(index)}
            onSelectionChange={onGridSelectionChange}
            onContextMenu={openGridContextMenu}
          />
        {/key}
        {#if emptyState !== null}
          <div class="empty-state" role="status">
            <p class="empty-message">{emptyState.message}</p>
            {#if emptyState.detail !== null}
              <p class="empty-detail">{emptyState.detail}</p>
            {/if}
            {#if emptyState.kind === "empty" && currentFolder !== null && catalogSource === "temp"}
              <p class="empty-detail">場所: {currentFolder}</p>
            {/if}
          </div>
        {/if}
      </div>

      <footer class="status-bar">
        <span>
          選択 {gridSelection.ids.length.toLocaleString()} /
          全 {summary?.total.toLocaleString() ?? 0} 件
        </span>
        {#if catalogError}
          <span class="status-error">{catalogError}</span>
        {/if}
        <div class="status-spacer"></div>
        <span class="status-hint">
          Space: クイックプレビュー · Enter: ビューア · 右クリック: OS連携
        </span>
      </footer>
    </section>

    {#if rightVisible}
      <div
        class="pane-resizer"
        role="separator"
        aria-orientation="vertical"
        onpointerdown={(event) => startResize("right", event)}
      ></div>
      <aside class="side-pane right" style="width:{rightWidth}px">
        <InfoPanel
          item={focusInfo}
          selectionCount={gridSelection.ids.length}
          libraryMode={catalogSource === "library"}
          sessionCounts={sessionCounts}
          sessionOrigin={sessionSnapshot.origin}
          resumable={sessionResumable}
          sessionNotice={sessionNotice}
          onStartFolder={() => void startSessionFolder()}
          onStartSelection={startSessionSelection}
          onResume={() => void resumeSession()}
          onEnd={endSession}
          onSessionMove={sessionMove}
        />
      </aside>
    {/if}
  </div>

  {#if devBridgeActive && devPanelOpen}
    <DevDrawer
      {catalog}
      {thumbnails}
      {perf}
      {rawHealth}
      {fetchedAt}
      onRefreshHealth={() => void refreshHealth()}
      onCacheCleared={(cacheGeneration) => grid.noteCacheCleared(cacheGeneration)}
    />
  {/if}
</div>

{#if contextMenu !== null}
  <ContextMenu
    x={contextMenu.x}
    y={contextMenu.y}
    actions={contextMenu.actions}
    onClose={() => (contextMenu = null)}
  />
{/if}

<QuickPreview store={quickPreview} onClose={() => quickPreview.close()} />

{#if viewerMode === "single" && viewerStore !== null}
  <ImageViewer store={viewerStore} onClose={closeViewer} />
{:else if (viewerMode === "compare2" || viewerMode === "compare4") && compareStore !== null}
  <CompareView store={compareStore} onClose={closeViewer} />
{/if}

<style>
  .app-shell {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .app-header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem 0.75rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
    font-size: 0.85rem;
  }

  .app-title {
    font-size: 1rem;
    margin: 0;
  }

  .health-dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: light-dark(#8b949e, #6e7681);
  }

  .health-dot.ok {
    background: light-dark(#1a7f37, #3fb950);
  }

  .health-dot.ng {
    background: light-dark(#cf222e, #f85149);
  }

  .source-badge {
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    font-size: 0.72rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    color: light-dark(#57606a, #8b949e);
  }

  .source-badge.library {
    border-color: light-dark(#1a7f37, #238636);
    color: light-dark(#1a7f37, #3fb950);
  }

  .scan-indicator {
    font-size: 0.75rem;
    color: light-dark(#0969da, #58a6ff);
  }

  .warn-indicator {
    font-size: 0.75rem;
    color: light-dark(#9a6700, #d29922);
    font-weight: bold;
  }

  .header-spacer {
    flex: 1;
  }

  .header-notice {
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    max-width: 24rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .app-panes {
    flex: 1;
    min-height: 0;
    display: flex;
  }

  .side-pane {
    flex: none;
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .side-pane.left {
    border-right: 1px solid light-dark(#d0d7de, #30363d);
    font-size: 0.875rem;
  }

  .side-pane.right {
    border-left: 1px solid light-dark(#d0d7de, #30363d);
  }

  .pane-resizer {
    flex: none;
    width: 5px;
    cursor: col-resize;
    background: transparent;
  }

  .pane-resizer:hover {
    background: light-dark(#0969da33, #58a6ff33);
  }

  .center-pane {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .toolbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.3rem 0.6rem;
    font-size: 0.8rem;
    flex-wrap: wrap;
  }

  .breadcrumb {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    min-width: 0;
    overflow: hidden;
    flex-wrap: wrap;
  }

  .crumb {
    border: none;
    background: transparent;
    color: light-dark(#0969da, #58a6ff);
    font: inherit;
    padding: 0.1rem 0.25rem;
    border-radius: 0.25rem;
    cursor: pointer;
  }

  .crumb:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .crumb-current {
    font-weight: bold;
  }

  .crumb-sep {
    color: light-dark(#8b949e, #6e7681);
  }

  .sort-label,
  .size-label {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    color: light-dark(#57606a, #8b949e);
    font-size: 0.78rem;
  }

  .toolbar-spacer {
    flex: 1;
  }

  .viewer-choice {
    display: inline-flex;
    gap: 0.3rem;
  }

  button {
    font-size: 0.8rem;
  }

  button.active {
    border-color: light-dark(#0969da, #58a6ff);
    font-weight: bold;
  }

  .toolbar-notice {
    flex: none;
    margin: 0;
    padding: 0 0.6rem;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .toolbar-notice.error {
    color: light-dark(#cf222e, #f85149);
  }

  .grid-wrap {
    flex: 1;
    min-height: 0;
    position: relative;
    padding: 0 0.6rem;
  }

  .empty-state {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    pointer-events: none;
    text-align: center;
    padding: 2rem;
  }

  .empty-message {
    margin: 0;
    font-size: 1rem;
    color: light-dark(#57606a, #8b949e);
  }

  .empty-detail {
    margin: 0;
    font-size: 0.8rem;
    color: light-dark(#8b949e, #6e7681);
    font-family: monospace;
    overflow-wrap: anywhere;
  }

  .status-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.25rem 0.75rem;
    border-top: 1px solid light-dark(#d0d7de, #30363d);
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .status-error {
    color: light-dark(#cf222e, #f85149);
  }

  .status-spacer {
    flex: 1;
  }

  .status-hint {
    color: light-dark(#8b949e, #6e7681);
  }
</style>
