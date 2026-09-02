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
    planAdhocResume,
    sessionCountDrifted,
    type SessionCounts,
    type SessionOriginDetail,
  } from "./lib/session/selection-session";
  import {
    LayoutPrefsStore,
    clampPaneWidth,
  } from "./lib/ui/layout-prefs";
  import { FolderPrefsStore, LIBRARY_SORT_KEY } from "./lib/ui/folder-prefs";
  import { copyText } from "./lib/ui/clipboard";
  import { KeyDispatcher } from "./lib/keys";
  import {
    MetadataStore,
    canEditMetadata,
    patchOfEvent,
    type AssetDetailDto,
    type AssetMetadataDto,
    type CullValue,
  } from "./lib/metadata";
  import {
    CollectionsStore,
    planReorder,
    type CollectionDto,
    type CollectionsChangedEvent,
  } from "./lib/collections";
  import { UndoStack } from "./lib/undo";
  import {
    EMPTY_FILTERS,
    SearchStore,
    filtersActive,
    type GuiFilters,
    type SearchFieldMatch,
  } from "./lib/search";
  import {
    applyConflictChoice,
    cancelFileOps,
    createConflictSession,
    describeKind,
    diskWarningOf,
    executeFileOp,
    outcomeNotice,
    planFileOp,
    resolutionsOf,
    retryArgsOf,
    undoCheck,
    undoFileOp,
    type ConflictResolution,
    type ConflictSession,
    type ExecuteArgs,
    type FileOpOutcomeDto,
    type FileOpsProgressEvent,
  } from "./lib/fileops";
  import DestPicker from "./lib/ui/DestPicker.svelte";
  import ConflictDialog from "./lib/ui/ConflictDialog.svelte";
  import DeleteForeverDialog from "./lib/ui/DeleteForeverDialog.svelte";
  type TriageKeyAction = import("./lib/triage-keys").TriageKeyAction;
  import ContextMenu, { type MenuAction } from "./lib/ui/ContextMenu.svelte";
  import FolderTree from "./lib/shell/FolderTree.svelte";
  import CollectionsPane from "./lib/shell/CollectionsPane.svelte";
  import CollectionPicker from "./lib/ui/CollectionPicker.svelte";
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

  // ===== メタデータ編集 (WP-5) =====

  const metadata = new MetadataStore(ipc);
  const undo = new UndoStack();
  let metaDetail: AssetDetailDto | null = $state(null);
  let metaBusy = $state(false);
  let metaError = $state<string | null>(null);
  let undoLabel = $state<string | null>(null);
  /** WP-9: フォーカス項目の埋め込み画像メタデータ。 */
  let metaAssetMetadata: AssetMetadataDto | null = $state(null);
  /** フォーカス項目の詳細取得世代 (古い応答の破棄)。 */
  let detailGeneration = 0;
  /** 高速選別モード (ビューア内。アプリ稼働中は引き継ぐ — 設定トグル)。 */
  let triageModeOn = $state(false);
  /** 編集操作の通知 (InfoPanel外 — ツールバー横の短いフィードバック)。 */
  let metaNotice = $state("");

  undo.onChange(() => {
    undoLabel = undo.peekLabel();
  });

  // ===== ファイル操作 (WP-7) =====

  const fileOpsIpc = { invoke };
  /** 実行中 (進捗表示・操作ガード)。 */
  let fileOpsBusy = $state(false);
  let fileOpsProgress = $state<{ index: number; total: number; from: string } | null>(null);
  let fileOpsNotice = $state("");
  let fileOpsError = $state<string | null>(null);
  /** 宛先選択ダイアログ (コピー/移動)。 */
  let destPickerFor = $state<{ kind: "copy" | "move"; paths: string[] } | null>(null);
  /** 衝突ダイアログ (計画実行の保留中。トークンは計画時に発行されたもの)。 */
  let conflictOf = $state<{
    session: ConflictSession;
    args: ExecuteArgs;
    confirmToken: string | null;
  } | null>(null);
  /** 完全削除の確認 (paths + 計画発行の確認トークン)。 */
  let deleteForeverFor = $state<{ paths: string[]; confirmToken: string | null } | null>(null);
  /** 部分成功レポート (再試行)。 */
  let retryReport = $state<{ outcome: FileOpOutcomeDto; args: ExecuteArgs } | null>(null);
  /** F2インラインリネーム中の項目id。 */
  let renameId = $state<string | null>(null);
  let renameInitial = $state("");
  let disposeFileOpsProgress: (() => void) | null = null;

  /** 選択項目のパス列 (ライブラリ表示中のみ。ファイル操作の対象)。 */
  function selectedLibraryPaths(): string[] {
    if (catalogSource !== "library") {
      return [];
    }
    const paths: string[] = [];
    for (const id of gridSelection.ids) {
      const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
      const item = found !== undefined ? grid.getItem(found) : null;
      if (item !== null) {
        paths.push(item.path);
      }
    }
    return paths;
  }

  /** フォーカス項目 (単一操作: リネーム等)。 */
  function focusLibraryPath(): { path: string; displayName: string } | null {
    if (catalogSource !== "library") {
      return null;
    }
    const focus = gridSelection.focusIndex;
    if (focus === null) {
      return null;
    }
    const item = grid.getItem(focus);
    if (item === null) {
      return null;
    }
    return { path: item.path, displayName: item.displayName };
  }

  /** ライブラリモードで無い旨の通知 (操作ガード共通)。 */
  function requireLibraryMode(): boolean {
    if (catalogSource !== "library") {
      fileOpsNotice = "ファイル操作はライブラリ表示中のみ利用できます";
      return false;
    }
    if (fileOpsBusy) {
      fileOpsNotice = "別のファイル操作が実行中です";
      return false;
    }
    return true;
  }

  /** コピー/移動の開始 (宛先ダイアログ)。 */
  function startCopyMove(kind: "copy" | "move"): void {
    if (!requireLibraryMode()) {
      return;
    }
    const paths = selectedLibraryPaths();
    if (paths.length === 0) {
      fileOpsNotice = "対象ファイルを選択してください";
      return;
    }
    fileOpsNotice = "";
    destPickerFor = { kind, paths };
  }

  /**
   * 宛先確定 → 計画 (dry-run) → 衝突があれば解決ダイアログ → 実行。
   * 破壊的操作を含む計画はトークンが発行されるため、確認ダイアログを
   * 経てトークン付きで実行する (backend確認ゲート)。
   */
  async function runPlannedOp(args: ExecuteArgs): Promise<void> {
    fileOpsError = null;
    fileOpsNotice = "";
    const plan = await planFileOp(fileOpsIpc, args);
    if (plan.plan_errors.length > 0) {
      fileOpsError = plan.plan_errors.join(" / ");
      return;
    }
    const warning = diskWarningOf(plan);
    if (warning !== null && !window.confirm(`${warning}\n続行しますか?`)) {
      return;
    }
    const session = createConflictSession(plan.steps);
    if (session.current !== null) {
      conflictOf = { session, args, confirmToken: plan.confirm_token };
      return;
    }
    await doExecute({ ...args, confirm_token: plan.confirm_token });
  }

  /** 実行 + 結果通知 + Undo登録 (操作ログ確定後の応答で通知する)。 */
  async function doExecute(args: ExecuteArgs): Promise<void> {
    conflictOf = null;
    fileOpsError = null;
    fileOpsBusy = true;
    fileOpsProgress = null;
    try {
      const outcome = await executeFileOp(fileOpsIpc, args);
      fileOpsNotice = outcomeNotice(outcome);
      if (outcome.undoable) {
        registerFileOpsUndo(outcome);
      }
      const failed = outcome.steps.some((step) => step.status === "failed");
      if (failed) {
        retryReport = { outcome, args };
      }
    } catch (error) {
      fileOpsError = toErrorMessage(error);
    } finally {
      fileOpsBusy = false;
      fileOpsProgress = null;
    }
  }

  /** 衝突ダイアログの選択反映。 */
  function onConflictChoose(choice: ConflictResolution, applyAll: boolean): void {
    if (conflictOf === null) {
      return;
    }
    const session = applyConflictChoice(conflictOf.session, choice, applyAll);
    if (session.current !== null) {
      conflictOf = { ...conflictOf, session };
      return;
    }
    // 全衝突が解決 → 実行 (上書きを含む場合は計画時トークンが必要)
    const perFile = resolutionsOf(session);
    void doExecute({
      ...conflictOf.args,
      resolutions: { per_file: perFile },
      confirm_token: conflictOf.confirmToken,
    });
  }

  /** ゴミ箱への移動 (Delete。確認なし — ゴミ箱から復帰可能なため)。 */
  function startTrash(): void {
    if (!requireLibraryMode()) {
      return;
    }
    const paths = selectedLibraryPaths();
    if (paths.length === 0) {
      fileOpsNotice = "対象ファイルを選択してください";
      return;
    }
    void runPlannedOp({ kind: "trash", paths });
  }

  /**
   * 完全削除 (Shift+Delete)。計画 (dry-run + 確認トークン発行) →
   * 確認ダイアログ+チェックボックス → トークン付き実行。
   */
  function startDeleteForever(): void {
    if (!requireLibraryMode()) {
      return;
    }
    const paths = selectedLibraryPaths();
    if (paths.length === 0) {
      fileOpsNotice = "対象ファイルを選択してください";
      return;
    }
    fileOpsError = null;
    fileOpsNotice = "";
    void (async () => {
      try {
        const plan = await planFileOp(fileOpsIpc, { kind: "delete_forever", paths });
        if (plan.plan_errors.length > 0) {
          fileOpsError = plan.plan_errors.join(" / ");
          return;
        }
        deleteForeverFor = { paths, confirmToken: plan.confirm_token };
      } catch (error) {
        fileOpsError = toErrorMessage(error);
      }
    })();
  }

  async function confirmDeleteForever(): Promise<void> {
    const pending = deleteForeverFor;
    deleteForeverFor = null;
    if (pending === null) {
      return;
    }
    await doExecute({
      kind: "delete_forever",
      paths: pending.paths,
      confirm_token: pending.confirmToken,
    });
  }

  /** F2リネーム開始 (グリッド内インライン編集)。 */
  function startRename(): void {
    if (!requireLibraryMode()) {
      return;
    }
    if (gridSelection.ids.length !== 1) {
      fileOpsNotice = "リネームは1ファイルずつ行います (1件を選択してください)";
      return;
    }
    const focus = focusLibraryPath();
    if (focus === null) {
      fileOpsNotice = "対象ファイルを選択してください";
      return;
    }
    renameInitial = focus.displayName;
    renameId = gridSelection.ids[0];
  }

  /** インラインリネーム確定。 */
  async function commitRename(name: string): Promise<void> {
    const focus = focusLibraryPath();
    renameId = null;
    if (focus === null) {
      return;
    }
    if (name === focus.displayName) {
      return; // 変化なし
    }
    await runPlannedOp({ kind: "rename", paths: [focus.path], new_name: name });
  }

  /** フォルダツリーへのドロップ (デフォルト=移動 / Ctrl=コピー)。 */
  function handleTreeDrop(destPath: string, paths: string[], copy: boolean): void {
    if (!requireLibraryMode()) {
      return;
    }
    void runPlannedOp({ kind: copy ? "copy" : "move", paths, dest_dir: destPath });
  }

  /** ツールバー: 新規フォルダ (現在フォルダ配下。ライブラリ時は最初のルート)。 */
  async function startNewFolder(): Promise<void> {
    if (!requireLibraryMode()) {
      return;
    }
    const parent =
      catalogSource === "library"
        ? (libraryRoots.find((root) => root.available)?.path ?? null)
        : currentFolder;
    if (parent === null) {
      fileOpsNotice = "登録ルートがありません";
      return;
    }
    const name = window.prompt("新しいフォルダ名", "");
    if (name === null || name.trim() === "") {
      return;
    }
    try {
      const outcome = await invoke<{ status: string; steps: Array<{ error: string | null }> }>(
        "folder_create",
        { parent, name: name.trim() },
      );
      if (outcome.status !== "done") {
        fileOpsError = outcome.steps[0]?.error ?? "フォルダを作成できませんでした";
        return;
      }
      fileOpsNotice = `「${name.trim()}」を作成しました`;
    } catch (error) {
      fileOpsError = toErrorMessage(error);
    }
  }

  /**
   * ファイル操作のUndo登録 (メタデータUndoと混在する統合スタック)。
   * 前提検証で拒否された場合は理由を通知してスタックから外す。
   */
  function registerFileOpsUndo(outcome: FileOpOutcomeDto): void {
    const label = describeKind(outcome.kind);
    const opId = outcome.op_id;
    undo.push({
      label,
      undo: async () => {
        const check = await undoCheck(fileOpsIpc, opId);
        if (!check.ok) {
          throw new Error(check.refusals.join(" / "));
        }
        const result = await undoFileOp(fileOpsIpc, opId);
        if (!result.fully_undone) {
          const reasons = [
            ...result.refusals,
            ...result.errors,
            ...result.rollback_errors,
          ];
          throw new Error(reasons.length > 0 ? reasons.join(" / ") : "元に戻せませんでした");
        }
      },
    });
  }

  /** パス→サムネイルURL (衝突ダイアログのプレビュー。表示中カタログ内のみ)。 */
  function thumbUrlForPath(path: string | null): string | null {
    if (path === null) {
      return null;
    }
    void gridTick;
    for (let index = 0; index < (grid.summary?.total ?? 0); index++) {
      const item = grid.getItem(index);
      if (item !== null && item.path === path) {
        return grid.thumbUrlFor(item.id);
      }
    }
    return null;
  }

  /** 進捗イベント購読。 */
  function initFileOpsProgress(): void {
    void (async () => {
      disposeFileOpsProgress = await listen<FileOpsProgressEvent>("file_ops:progress", (event) => {
        fileOpsProgress = {
          index: event.payload.index,
          total: event.payload.total,
          from: event.payload.from,
        };
      });
    })();
  }

  // ===== コレクション (WP-6) =====

  const collectionsStore = new CollectionsStore(ipc);
  /** 表示中のコレクション (中央グリッドがその内容を表示中)。 */
  let activeCollection: CollectionDto | null = $state(null);
  /** 表示中manualコレクションの項目メモ (assetId → 本文)。 */
  let collectionNotes = $state<Record<string, string>>({});
  let collectionNotice = $state("");
  let collectionBusy = $state(false);
  /** 「コレクションへ追加」ピッカーへ渡す対象ID列 (null=閉)。 */
  let collectionPickerFor: string[] | null = $state(null);
  /** 位置入力による並び替え (1-based表示 → 0-basedへ変換して適用)。 */
  let collectionPositionInput = $state("");
  let disposeCollections: (() => void) | null = null;
  let stopCollectionsChanged: (() => void) | null = null;

  /**
   * collections:changed → 表示追従。backendは表示中コレクションの内容変化で
   * catalog:changed (collection_refresh) を出すため、ここでは主に
   * activeCollection DTOの最新化とメモ再取得を行う。
   */
  function onCollectionsChangedEvent(event: CollectionsChangedEvent): void {
    const current = activeCollection;
    if (current === null) {
      return;
    }
    if (event.reason === "deleted" && event.collection_id === current.id) {
      // backendがlibrary表示へ戻している (clear_collection_display)
      activeCollection = null;
      collectionNotes = {};
      return;
    }
    if (event.collection_id === current.id) {
      const updated =
        collectionsStore.collections.find((c) => c.id === current.id) ?? null;
      activeCollection = updated;
      if (updated !== null && updated.kind === "manual" && event.reason === "items") {
        void refreshCollectionNotes(updated.id);
      }
    }
  }

  /** コレクションを中央グリッドへ表示 (catalog:changedでCatalogStoreが追従)。 */
  async function showCollection(collection: CollectionDto): Promise<boolean> {
    collectionNotice = "";
    collectionBusy = true;
    try {
      await collectionsStore.show(collection.id);
      activeCollection = collection;
      catalogSource = "library";
      searchActive = false;
      gridSelection = { ids: [], focusIndex: null };
      collectionPositionInput = "";
      if (collection.kind === "manual") {
        await refreshCollectionNotes(collection.id);
      } else {
        collectionNotes = {};
      }
      return true;
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
      return false;
    } finally {
      collectionBusy = false;
    }
  }

  async function refreshCollectionNotes(collectionId: string): Promise<void> {
    try {
      const page = await collectionsStore.items(collectionId, 0, 1000);
      const notes: Record<string, string> = {};
      for (const item of page.items) {
        if (typeof item.note === "string" && item.note !== "") {
          notes[item.asset_id] = item.note;
        }
      }
      collectionNotes = notes;
    } catch {
      collectionNotes = {};
    }
  }

  /** コレクション表示をやめて通常ライブラリ表示へ戻る。 */
  async function exitCollection(): Promise<void> {
    collectionNotice = "";
    const ok = await library.useSource("library");
    if (ok) {
      activeCollection = null;
      searchActive = false;
      collectionNotes = {};
      await catalog.refreshSummary();
      await catalog.loadPage(0);
    } else {
      libraryError = library.lastError ?? "ライブラリ表示へ戻せませんでした";
    }
  }

  /** manual表示中: 選択項目を移動 (先頭/末尾/位置入力)。 */
  async function moveSelectedInCollection(target: "front" | "back" | "position"): Promise<void> {
    const collection = activeCollection;
    if (collection === null || collection.kind !== "manual" || collectionBusy) {
      return;
    }
    const ids = await collectAllIds();
    if (ids.length === 0 || gridSelection.ids.length === 0) {
      collectionNotice = "移動する画像を選択してください";
      return;
    }
    let plan: string[] | null;
    if (target === "position") {
      const index = Number.parseInt(collectionPositionInput, 10);
      if (!Number.isInteger(index) || index < 1 || index > ids.length) {
        collectionNotice = `位置は1〜${ids.length}の整数で指定してください`;
        return;
      }
      // 1-based表示 → 選択を除いた残りの世界の0-based位置へ変換
      const selected = new Set(gridSelection.ids);
      const staying = ids.filter((id) => !selected.has(id));
      const position = Math.min(index - 1, staying.length);
      plan = planReorder(ids, gridSelection.ids, { kind: "position", index: position });
    } else {
      plan = planReorder(ids, gridSelection.ids, { kind: target });
    }
    if (plan === null) {
      collectionNotice = "移動できませんでした";
      return;
    }
    collectionBusy = true;
    collectionNotice = "";
    try {
      await collectionsStore.reorder(collection.id, plan);
      collectionPositionInput = "";
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
    } finally {
      collectionBusy = false;
    }
  }

  /** manual表示中: 選択項目をコレクションから除外 (元画像は影響しない)。 */
  async function removeSelectedFromCollection(): Promise<void> {
    const collection = activeCollection;
    if (collection === null || collection.kind !== "manual" || collectionBusy) {
      return;
    }
    if (gridSelection.ids.length === 0) {
      collectionNotice = "除外する画像を選択してください";
      return;
    }
    collectionBusy = true;
    collectionNotice = "";
    try {
      for (const assetId of gridSelection.ids) {
        await collectionsStore.removeItem(collection.id, assetId);
      }
      gridSelection = { ids: [], focusIndex: null };
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
    } finally {
      collectionBusy = false;
    }
  }

  /** スマートコレクションの明示再評価 (メタデータ変更後の反映)。 */
  async function refreshActiveSmartCollection(): Promise<void> {
    const collection = activeCollection;
    if (collection === null || collection.kind !== "smart" || collectionBusy) {
      return;
    }
    collectionBusy = true;
    collectionNotice = "";
    try {
      const updated = await collectionsStore.refreshSmart(collection.id);
      activeCollection = updated;
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
    } finally {
      collectionBusy = false;
    }
  }

  /** InfoPanelのコレクション内メモ保存 (フォーカス項目)。 */
  async function saveCollectionMemo(assetId: string, note: string): Promise<void> {
    const collection = activeCollection;
    if (collection === null || collection.kind !== "manual") {
      return;
    }
    try {
      await collectionsStore.setItemNote(collection.id, assetId, note === "" ? null : note);
      collectionNotes = { ...collectionNotes, [assetId]: note };
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
    }
  }

  // ===== 検索 (WP-8) =====

  const searchStore = new SearchStore(ipc);
  let searchInput = $state("");
  let searchFilters = $state<GuiFilters>({ ...EMPTY_FILTERS });
  let searchFiltersOpen = $state(false);
  let searchScopeMode = $state<"library" | "folder" | "collection">("library");
  /** 検索結果表示中 (カタログはsearchソース)。 */
  let searchActive = $state(false);
  let searchNotice = $state("");
  /** evidence写像の変化通知 (InfoPanel・ツールチップ再評価用)。 */
  let searchEvidenceTick = $state(0);

  searchStore.onChange(() => {
    searchEvidenceTick++;
  });

  /** 検索スコープの解決 (folder=現在の登録ルート / collection=表示中コレクション)。 */
  function resolveSearchScope(): { rootId?: string; collectionId?: string } | string {
    if (searchScopeMode === "folder") {
      if (currentFolder === null) {
        return "フォルダを先に選択してください";
      }
      if (registeredStateFor(currentFolder, libraryRoots) === null) {
        return "現在のフォルダが登録ルート内にありません (ライブラリ全体で検索してください)";
      }
      const folder = currentFolder;
      const root = libraryRoots.find(
        (candidate) => folder === candidate.path || folder.startsWith(candidate.path + "/"),
      );
      if (root === undefined) {
        return "現在のフォルダに対応する登録ルートが見つかりません";
      }
      return { rootId: root.id };
    }
    if (searchScopeMode === "collection") {
      if (activeCollection === null) {
        return "コレクションを先に表示してください";
      }
      return { collectionId: activeCollection.id };
    }
    return {};
  }

  /** 検索実行 → 結果をカタログのsearchソースへ表示。 */
  async function runSearch(): Promise<void> {
    const scope = resolveSearchScope();
    if (typeof scope === "string") {
      searchNotice = scope;
      return;
    }
    searchNotice = "";
    const ok = await searchStore.run(searchInput, searchFilters, scope);
    if (!ok) {
      searchNotice = searchStore.lastError ?? "検索に失敗しました";
      return;
    }
    searchActive = true;
    // 検索結果は安定AssetIdのLibrary系表示 (api-contract §42。backendも
    // CatalogMode::Library)。コレクション表示 (showCollection) と同じ扱いに
    // して、一時走査ビューから検索しても編集・詳細取得を無効化しない。
    catalogSource = "library";
    activeCollection = null;
    collectionNotes = {};
    gridSelection = { ids: [], focusIndex: null };
    await catalog.refreshSummary();
    await catalog.loadPage(0);
  }

  /** 検索結果表示をやめて通常ライブラリ表示へ戻る。 */
  async function exitSearch(): Promise<void> {
    if (!searchActive) {
      return;
    }
    const ok = await library.useSource("library");
    if (ok) {
      searchActive = false;
      // libraryスナップショット表示へ戻す (一時走査由来の検索でも図る)。
      catalogSource = "library";
      gridSelection = { ids: [], focusIndex: null };
      await catalog.refreshSummary();
      await catalog.loadPage(0);
    } else {
      searchNotice = library.lastError ?? "ライブラリ表示へ戻せませんでした";
    }
  }

  /** 表示中検索を新しいDB状態で再実行 (メタデータ編集後の反映)。 */
  async function refreshSearch(): Promise<void> {
    const ok = await searchStore.refresh();
    if (!ok) {
      searchNotice = searchStore.lastError ?? "再実行に失敗しました";
      return;
    }
    searchActive = true;
    // 再実行も検索結果表示 (Library系)。runSearchと同一の遷移規則。
    catalogSource = "library";
    gridSelection = { ids: [], focusIndex: null };
    await catalog.refreshSummary();
    await catalog.loadPage(0);
  }

  /** 現在の検索を保存済み検索 (スマートコレクション) として保存。 */
  async function saveCurrentSearch(): Promise<void> {
    const queryJson = searchStore.lastResult?.queryJson;
    if (queryJson === undefined) {
      searchNotice = "先に検索を実行してください";
      return;
    }
    const name = window.prompt("保存済み検索の名前", searchInput.trim() || "新しい検索");
    if (name === null || name.trim() === "") {
      return;
    }
    const ok = await searchStore.save(name.trim(), queryJson);
    searchNotice = ok ? "保存しました (左ペインの「保存済み検索」に表示されます)" : searchStore.lastError ?? "保存できませんでした";
  }

  /** 検索表示中の一致根拠 (フォーカス + 先頭ページ分。未取得のみ取得)。 */
  function prefetchSearchEvidence(): void {
    if (!searchActive) {
      return;
    }
    void gridTick;
    void searchEvidenceTick;
    const total = grid.summary?.total ?? 0;
    const count = Math.min(total, 500);
    const ids: string[] = [];
    for (let index = 0; index < count; index++) {
      const item = grid.getItem(index);
      if (item !== null) {
        ids.push(item.id);
      }
    }
    if (ids.length > 0) {
      void searchStore.evidenceFor(ids);
    }
  }

  /** 一致根拠の概略行 (セルツールチップ用)。 */
  function searchEvidenceOf(id: string): SearchFieldMatch[] | null {
    if (!searchActive) {
      return null;
    }
    void searchEvidenceTick;
    return searchStore.evidence[id] ?? null;
  }

  /** セルツールチップ文言 (表示名 + 検索一致根拠)。 */
  function cellTooltip(displayName: string, id: string): string {
    const matches = searchEvidenceOf(id);
    if (matches === null || matches.length === 0) {
      return displayName;
    }
    const fieldLabels: Record<string, string> = {
      filename: "ファイル名",
      path: "パス",
      tags: "タグ",
      captions: "キャプション",
      note: "メモ",
    };
    const lines = matches
      .map((match) => `${fieldLabels[match.field] ?? match.field}: ${match.snippet}`)
      .join("\n");
    return `${displayName}\n— 検索一致 —\n${lines}`;
  }

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
    // 検索表示中は到着ページ分の一致根拠を先読み (WP-8)
    prefetchSearchEvidence();
  });

  const stopLibraryNotification = library.onChange(() => {
    libraryRoots = library.roots;
    libraryNotice = library.overlapWarning ?? library.lastNotice ?? "";
    libraryError = library.lastError;
    libraryScanBusy = library.status.scanning;
  });

  const stopSessionNotify = session.onChange(() => {
    refreshSessionCounts();
    surfaceSessionError();
  });

  // フォーカス項目 or 表示モードが変わったら詳細を引き直す (WP-5)
  $effect(() => {
    const item = focusInfo;
    if (item !== null && canEditMetadata(catalogSource === "library", item)) {
      void refreshMetaDetail(item.id);
    } else {
      detailGeneration++;
      metaDetail = null;
      metaAssetMetadata = null;
      metaBusy = false;
    }
  });

  // Ctrl+Z: メタデータUndo (KeyDispatcherは修飾キー付きを扱わないため個別リスナ)
  function onGlobalKeydown(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "z") {
      event.preventDefault();
      metaActions.undo();
    }
  }

  let disposeCatalog: (() => void) | null = null;
  let disposeThumbnails: (() => void) | null = null;
  let disposeGrid: (() => void) | null = null;
  let disposeLibrary: (() => void) | null = null;
  let disposeQuickPreview: (() => void) | null = null;
  let disposeMetadata: (() => void) | null = null;
  let stopMetadataUpdates: (() => void) | null = null;

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

  /** 現在のソート保存キー (一時走査=フォルダパス / ライブラリ="library")。 */
  function sortPrefsKey(): string | null {
    if (catalogSource === "library") {
      return LIBRARY_SORT_KEY;
    }
    return currentFolder;
  }

  /** 保存値のソートを検証 (破損したlocalStorage値の伝播防止)。 */
  function safeSort(value: string | undefined): CatalogSort | undefined {
    return value === "name" || value === "date_desc" || value === "size_desc"
      ? value
      : undefined;
  }

  /**
   * フォルダ選択: 登録済みならライブラリソース、未登録なら一時走査へ切替。
   * 走査中の切替はbackendが旧走査を世代無効化する (QA-WP4 Major-1)。
   * `sortOverride` はセッション復元等の明示指定 (保存済みソート優先)。
   */
  async function selectFolder(path: string, sortOverride?: CatalogSort): Promise<boolean> {
    osNotice = "";
    collectionNotice = "";
    const registered = registeredStateFor(path, libraryRoots) !== null;
    tree.select(path);
    if (registered) {
      const ok = await library.useSource("library");
      if (ok) {
        catalogSource = "library";
        currentFolder = path;
        activeCollection = null;
        searchActive = false;
        collectionNotes = {};
        gridSelection = { ids: [], focusIndex: null };
        await applySavedSort(LIBRARY_SORT_KEY, sortOverride);
        await catalog.refreshSummary();
        await catalog.loadPage(0);
        saveFolderPrefs();
        return true;
      }
      libraryError = library.lastError ?? "ライブラリ表示へ切替できませんでした";
      return false;
    }
    // フォルダ別保存ソート (QA-WP4 Major-2)。走査開始時点でbackendへ反映
    // する (走査中はset_catalog_sortを使えないためopen_folder経由)。
    const wantedSort = sortOverride ?? safeSort(folderStore.sortFor(path));
    const ok = await catalog.openFolder(path, wantedSort);
    if (ok) {
      catalogSource = "temp";
      currentFolder = path;
      activeCollection = null;
      searchActive = false;
      collectionNotes = {};
      gridSelection = { ids: [], focusIndex: null };
      saveFolderPrefs();
      return true;
    }
    folderNotice = catalog.folderError ?? "フォルダを開けませんでした";
    return false;
  }

  /** 保存済みソートを現在カタログへ適用 (ライブラリ表示の再適用用)。 */
  async function applySavedSort(key: string, override?: CatalogSort): Promise<void> {
    const wanted = override ?? safeSort(folderStore.sortFor(key));
    if (wanted === undefined) {
      return;
    }
    if ((summary?.sort ?? "name") !== wanted) {
      await catalog.setSort(wanted);
    }
  }

  function saveFolderPrefs(): void {
    // sortsはフォルダ別設定なので保存対象外 (saveSortで管理される)
    folderStore.save({
      lastFolder: currentFolder,
      expanded: tree.expandedPaths(),
      sorts: folderStore.load().sorts,
    });
  }

  async function startScan(): Promise<void> {
    folderNotice = "";
    const ok = await catalog.startScan();
    if (!ok) {
      folderNotice = catalog.lastError ?? "スキャン開始に失敗しました";
    }
  }

  /** 実行中の一時走査をキャンセル (部分結果は残る。QA-WP4)。 */
  async function cancelScan(): Promise<void> {
    const ok = await catalog.cancelScan();
    if (!ok) {
      folderNotice = catalog.lastError ?? "走査のキャンセルに失敗しました";
    }
  }

  /** 一時走査の完了を待つ (セッション再開の検証前に全件が必要)。 */
  async function waitForScanSettled(timeoutMs = 30000): Promise<boolean> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      await catalog.refreshSummary();
      if (!(catalog.summary?.scanning ?? false)) {
        return true;
      }
      if (Date.now() >= deadline) {
        return false;
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }

  /** $derived内でのflow narrowingを避けるためのアクセサ (常に最新値)。 */
  function summaryNow(): CatalogSummary | null {
    return summary;
  }

  const scanBusy = $derived(
    (summaryNow()?.scanning ?? false) || libraryScanBusy,
  );

  /** GUIフィルタに未適用の設定があるか (フィルターボタンの強調用)。 */
  const searchFiltersActive = $derived(filtersActive(searchFilters));

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
    const collection = activeCollection;
    if (collection !== null) {
      return `コレクション「${collection.name}」 (${count.toLocaleString()}件)`;
    }
    const place = catalogSource === "library"
      ? "ライブラリ"
      : (currentFolder ?? "(未選択)");
    return `${place} (${count.toLocaleString()}件)`;
  }

  /** セッション開始時のorigin詳細 (再開時のフォルダ/ソート/コレクション復元用)。 */
  function sessionOriginDetail(): SessionOriginDetail {
    const collection = activeCollection;
    if (collection !== null) {
      return { kind: "collection", collectionId: collection.id };
    }
    if (catalogSource === "library") {
      return { kind: "library" };
    }
    return {
      kind: "adhoc-folder",
      folderPath: currentFolder ?? "",
      sort: sortValue,
    };
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
    session.start(ids, sessionOriginLabel(ids.length), 0, sessionOriginDetail());
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
    const detail: SessionOriginDetail =
      catalogSource === "library"
        ? { kind: "library" }
        : { kind: "adhoc-selection", folderPath: currentFolder ?? "", sort: sortValue };
    session.start(
      [...gridSelection.ids],
      `選択画像 (${gridSelection.ids.length.toLocaleString()}件)`,
      gridSelection.focusIndex ?? 0,
      detail,
    );
    void focusSessionPosition();
  }

  /**
   * セッション再開 (QA-WP4 Major-2)。
   * adhocセッションは保存時のフォルダ+ソートを復元してからID解決する。
   * 復元後も件数ずれ (>5%) や現在位置のID解決失敗がある場合は警告表示して
   * 再開を中止する (誤画像へのサイレント再開防止)。
   */
  async function resumeSession(): Promise<void> {
    sessionNotice = "";
    const persisted = session.peekPersistedSession();
    if (persisted === null || persisted.ids.length === 0) {
      sessionNotice = "再開できるセッションがありません";
      return;
    }
    const detail = persisted.detail;
    // コレクション由来セッション: 同一コレクションを再表示してからID解決
    // (WP-6。stable AssetIdなので走査待ち・ドリフト判定は不要)
    if (detail !== null && detail.kind === "collection") {
      const target =
        collectionsStore.collections.find((c) => c.id === detail.collectionId) ?? null;
      if (target === null) {
        sessionNotice = "セッションのコレクションが見つからないため再開を中止しました";
        return;
      }
      if (activeCollection?.id !== target.id) {
        const ok = await showCollection(target);
        if (!ok) {
          sessionNotice = "セッションのコレクションを表示できないため再開を中止しました";
          return;
        }
      }
    }
    if (detail !== null && detail.kind !== "library" && detail.kind !== "collection") {
      const plan = planAdhocResume(persisted, {
        source: catalogSource,
        folderPath: currentFolder,
        sort: sortValue,
      });
      if (plan.action === "restore") {
        sessionNotice = "セッションのフォルダ・並びへ戻します…";
        const ok = await selectFolder(plan.folderPath, plan.sort);
        if (!ok) {
          sessionNotice = "セッションのフォルダを開けなかったため再開を中止しました";
          return;
        }
      }
      // 復元したフォルダの走査完了を待つ (検証に全件が必要)
      if (!(await waitForScanSettled())) {
        sessionNotice = "フォルダの走査が完了しないため再開を中止しました";
        return;
      }
    }
    if (!session.resume()) {
      sessionNotice = "再開できるセッションがありません";
      return;
    }
    // 全IDのindexを解決 (前回の閲覧状態と現在カタログの対応付け)
    await collectAllIds();
    rebuildSessionIndex();
    if (detail?.kind === "adhoc-folder") {
      const total = grid.summary?.total ?? 0;
      if (sessionCountDrifted(persisted.ids.length, total)) {
        session.abort();
        sessionNotice =
          `フォルダの内容が保存時と大きく異なるため再開を中止しました ` +
          `(保存時 ${persisted.ids.length.toLocaleString()}件 / 現在 ${total.toLocaleString()}件)`;
        return;
      }
    }
    const currentId = session.currentId();
    if (currentId !== null && sessionIndex.get(currentId) === undefined) {
      session.abort();
      sessionNotice = "セッション位置の画像が現在の一覧に見つからないため再開を中止しました";
      return;
    }
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

  // ===== メタデータ編集 (WP-5) =====

  /** セッション保存失敗等のlastErrorを通知へ出す (QA-WP4 Minor-7)。 */
  function surfaceSessionError(): void {
    if (session.lastError !== null) {
      sessionNotice = `セッション状態の保存に失敗しました: ${session.lastError}`;
      session.lastError = null;
    }
  }

  /** 項目の現在値読み取り (pagerキャッシュ優先、無ければdetail)。 */
  function itemRatingOf(id: string): number {
    const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
    if (found !== undefined) {
      return grid.getItem(found)?.rating ?? 0;
    }
    return metaDetail?.id === id ? metaDetail.rating : 0;
  }

  function itemCullOf(id: string): CullValue {
    const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
    const triage = found !== undefined
      ? grid.getItem(found)?.triage
      : metaDetail?.id === id
        ? metaDetail.triage === "picked"
          ? "keep"
          : metaDetail.triage === "rejected"
            ? "reject"
            : "undecided"
        : undefined;
    return triage === "keep" ? "picked" : triage === "reject" ? "rejected" : "unselected";
  }

  function itemFavoriteOf(id: string): boolean {
    const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
    if (found !== undefined) {
      return grid.getItem(found)?.favorite ?? false;
    }
    return metaDetail?.id === id ? metaDetail.favorite : false;
  }

  function showMetaNotice(text: string): void {
    metaNotice = text;
    setTimeout(() => {
      metaNotice = "";
    }, 2500);
  }

  /** Undo付きの評価編集 (before値を捕捉して巻き戻し可能にする)。 */
  async function applyRating(ids: string[], rating: number): Promise<void> {
    const before = ids.map((id) => ({ id, rating: itemRatingOf(id) }));
    await metadata.setRating(ids, rating);
    undo.push({
      label: before.length > 1 ? `評価 ★${rating} (${before.length}件)` : `評価 ★${rating}`,
      undo: async () => {
        for (const entry of before) {
          await metadata.setRating([entry.id], entry.rating);
        }
      },
    });
    showMetaNotice(`評価 ★${rating} を適用しました (${ids.length.toLocaleString()}件)`);
  }

  async function applyCull(ids: string[], value: CullValue): Promise<void> {
    const before = ids.map((id) => ({ id, cull: itemCullOf(id) }));
    await metadata.setCull(ids, value);
    const label = value === "picked" ? "採用" : value === "rejected" ? "除外" : "未選別";
    undo.push({
      label: before.length > 1 ? `${label} (${before.length}件)` : label,
      undo: async () => {
        for (const entry of before) {
          await metadata.setCull([entry.id], entry.cull);
        }
      },
    });
    showMetaNotice(`${label}を適用しました (${ids.length.toLocaleString()}件)`);
  }

  async function applyFavorite(ids: string[], favorite: boolean): Promise<void> {
    const before = ids.map((id) => ({ id, favorite: itemFavoriteOf(id) }));
    await metadata.setFavorite(ids, favorite);
    undo.push({
      label: favorite
        ? `お気に入りON (${before.length}件)`
        : `お気に入りOFF (${before.length}件)`,
      undo: async () => {
        for (const entry of before) {
          await metadata.setFavorite([entry.id], entry.favorite);
        }
      },
    });
  }

  async function applyAddTags(ids: string[], names: string[]): Promise<void> {
    await metadata.addTags(ids, names);
    // undo: 付与したタグを各assetから解除する (タグIDは付与後に名前解決)
    undo.push({
      label: `タグ付与 ${names.join(", ")} (${ids.length}件)`,
      undo: async () => {
        for (const id of ids) {
          const tags = await metadata.listTags(id);
          for (const tag of tags) {
            if (names.some((n) => n.trim().toLowerCase() === tag.normalized_name)) {
              await metadata.removeTag(id, tag.id);
            }
          }
        }
      },
    });
    showMetaNotice(`タグ ${names.join(", ")} を付与しました (${ids.length.toLocaleString()}件)`);
  }

  async function applyRemoveTag(assetId: string, tagId: string, tagName = "タグ"): Promise<void> {
    await metadata.removeTag(assetId, tagId);
    undo.push({
      label: `タグ解除 ${tagName}`,
      undo: async () => {
        await metadata.addTags([assetId], [tagName]);
      },
    });
  }

  async function applyCaption(
    assetId: string,
    kind: "description" | "sd_tags" | "user_note",
    body: string,
  ): Promise<void> {
    const before =
      metaDetail?.id === assetId
        ? (metaDetail.captions.find((c) => c.kind === kind)?.body ?? "")
        : "";
    await metadata.setCaption(assetId, kind, body);
    undo.push({
      label: `${kind === "user_note" ? "メモ" : kind === "sd_tags" ? "SDタグ列" : "説明"}編集`,
      undo: async () => {
        await metadata.setCaption(assetId, kind, before);
      },
    });
  }

  const metaActions = {
    setRating: applyRating,
    setCull: applyCull,
    setFavorite: applyFavorite,
    addTags: applyAddTags,
    removeTag: (assetId: string, tagId: string) => applyRemoveTag(assetId, tagId),
    setCaption: applyCaption,
    undo: () => {
      void undo.undo().then((done) => {
        if (!done && undo.lastError !== null) {
          metaError = `元に戻せませんでした: ${undo.lastError}`;
        } else if (done) {
          showMetaNotice("元に戻しました");
        }
      });
    },
  };

  /** asset:updated → セル/InfoPanel/セッション集計の局所更新。 */
  function onAssetUpdated(event: import("./lib/metadata").AssetUpdatedEvent): void {
    const ids = new Set(event.ids);
    const patch = patchOfEvent(event);
    if (Object.keys(patch).length > 0) {
      grid.applyItemPatch(ids, patch);
    }
    // tags/caption変更・一括適用はフォーカス項目のdetail再取得で追従
    const focus = gridSelection.focusIndex !== null ? grid.getItem(gridSelection.focusIndex) : null;
    if (focus !== null && ids.has(focus.id)) {
      void refreshMetaDetail(focus.id);
    }
    refreshSessionCounts();
  }

  /** フォーカス項目の詳細取得 (InfoPanel表示用。世代ガード付き)。 */
  async function refreshMetaDetail(assetId: string): Promise<void> {
    const generation = ++detailGeneration;
    metaBusy = true;
    // WP-9: 埋め込みメタデータはdetailと並行取得 (raw含む全行。
    // 失敗は節を表示しないだけ — 詳細取得の失敗とは分離)。
    void metadata
      .assetMetadata(assetId)
      .then((value) => {
        if (generation === detailGeneration) {
          metaAssetMetadata = value;
        }
      })
      .catch(() => {
        if (generation === detailGeneration) {
          metaAssetMetadata = null;
        }
      });
    try {
      const detail = await metadata.assetDetail(assetId);
      if (generation === detailGeneration) {
        metaDetail = detail;
        metaError = null;
      }
    } catch (error) {
      if (generation === detailGeneration) {
        metaDetail = null;
        metaError = error instanceof Error ? error.message : String(error);
      }
    } finally {
      if (generation === detailGeneration) {
        metaBusy = false;
      }
    }
  }

  /** 高速選別キーの適用 (ビューアから。true=適用済み→自動次送り)。 */
  function handleTriageAction(action: TriageKeyAction): boolean {
    const id = viewerStore?.current?.assetId ?? null;
    if (id === null) {
      return false;
    }
    // Library表示 + Asset確定済み (stable ID) のみ編集可。dbState判定は
    // ページDTO由来 (backendのAssetId符号化が正本)
    if (catalogSource !== "library" || !gridPagerHasId(id)) {
      return false;
    }
    const apply =
      action.kind === "rating"
        ? applyRating([id], action.value)
        : applyCull([id], action.value);
    void apply.catch((error) => {
      showMetaNotice(error instanceof Error ? error.message : String(error));
    });
    return true;
  }

  function gridPagerHasId(id: string): boolean {
    const focus = gridSelection.focusIndex;
    if (focus !== null) {
      const item = grid.getItem(focus);
      if (item?.id === id) {
        return item.dbState === "asset";
      }
    }
    const found = grid.pager.findIndicesByIds(new Set([id])).get(id);
    return found !== undefined ? grid.getItem(found)?.dbState === "asset" : false;
  }

  /** ビューアの表示indexへグリッド選択を同期 (高速選別の自動次送り後)。 */
  function syncSelectionToViewer(): void {
    if (viewerStore === null) {
      return;
    }
    const index = viewerStore.displayIndex;
    if (index >= 0 && grid.getItem(index) !== null) {
      gridComponent?.selectAndFocus(index);
    }
  }

  /** 比較スロットの採用/除外 (WP-5)。 */
  function handleCullSlot(slotIndex: number, value: "picked" | "rejected"): void {
    const id = compareStore?.slots[slotIndex]?.assetId ?? null;
    if (id === null) {
      return;
    }
    void applyCull([id], value).catch((error) => {
      showMetaNotice(error instanceof Error ? error.message : String(error));
    });
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
    // Library表示中のみコレクション操作を載せる (stable AssetIdが必要なため)
    const canCollection = catalogSource === "library" && items.every((entry) => entry.dbState === "asset");
    const inManualCollection =
      activeCollection !== null && activeCollection.kind === "manual" && canCollection;
    const actions: MenuAction[] = [];
    // ファイル操作 (WP-7。ライブラリ表示中のみ)
    const canFileOps = catalogSource === "library";
    if (canFileOps) {
      actions.push(
        {
          id: "file-copy",
          label:
            items.length > 1
              ? `コピー… (${items.length.toLocaleString()}件)`
              : "コピー…",
          run: async () => {
            startCopyMove("copy");
          },
        },
        {
          id: "file-move",
          label:
            items.length > 1
              ? `移動… (${items.length.toLocaleString()}件)`
              : "移動…",
          run: async () => {
            startCopyMove("move");
          },
        },
        {
          id: "file-rename",
          label: "リネーム (F2)",
          run: async () => {
            startRename();
          },
        },
        {
          id: "file-trash",
          label: "ゴミ箱へ移動 (Delete)",
          run: async () => {
            startTrash();
          },
        },
        {
          id: "file-delete-forever",
          label: "完全削除… (Shift+Delete)",
          run: async () => {
            startDeleteForever();
          },
        },
      );
    }
    if (canCollection) {
      actions.push({
        id: "add-to-collection",
        label:
          items.length > 1
            ? `コレクションへ追加… (${items.length.toLocaleString()}件)`
            : "コレクションへ追加…",
        run: async () => {
          collectionPickerFor = items.map((entry) => entry.id);
        },
      });
    }
    if (inManualCollection) {
      actions.push({
        id: "remove-from-collection",
        label: "このコレクションから除外",
        run: async () => {
          for (const entry of items) {
            await collectionsStore.removeItem(activeCollection!.id, entry.id);
          }
        },
      });
    }
    actions.push(
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
    );
    contextMenu = {
      x: event.clientX,
      y: event.clientY,
      actions,
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
    // F2=リネーム / Delete=ゴミ箱 / Shift+Delete=完全削除 (WP-7)。
    // dev bridge有効時はF2が選択操作に使われるため干渉を避ける。
    unregisterKeys.push(
      keys.register("rename", () => {
        if (keyContext.devBridge) {
          return false;
        }
        if (renameId !== null) {
          return true; // 編集中はグリッド側inputが処理
        }
        startRename();
        return true;
      }),
    );
    // Delete/Shift+Deleteはdev bridgeと衝突しない (F-keyのみ使用のため
    // ゲート不要。リネームのF2のみ衝突するため既定のゲートを維持)。
    unregisterKeys.push(
      keys.register("trash", () => {
        startTrash();
        return true;
      }),
    );
    unregisterKeys.push(
      keys.register("deleteForever", () => {
        startDeleteForever();
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
    void catalog.setSort(sort).then((ok) => {
      if (!ok) {
        return;
      }
      // フォルダ別にソートを保存し、再オープン時に再適用する (QA-WP4)
      const key = sortPrefsKey();
      if (key !== null) {
        folderStore.saveSort(key, sort);
      }
    });
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
    window.addEventListener("keydown", onGlobalKeydown, true);
    initFileOpsProgress();
    void (async () => {
      const stop = await metadata.init();
      disposeMetadata = stop;
      stopMetadataUpdates = metadata.onAssetUpdated(onAssetUpdated);
    })();
    // コレクション (WP-6): リスナ登録後に初期化 (イベント取りこぼし防止)
    stopCollectionsChanged = collectionsStore.onCollectionsChanged(onCollectionsChangedEvent);
    void collectionsStore.init().then((dispose) => {
      disposeCollections = dispose;
    });

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
          getSession: () => session,
          getSessionNotice: () => sessionNotice,
          getMeta: () => ({
            detail: metaDetail,
            triageMode: triageModeOn,
            undoDepth: undo.depth,
            canEdit: canEditMetadata(catalogSource === "library", focusInfo),
            focusItem: focusInfo === null
              ? null
              : {
                  id: focusInfo.id,
                  rating: focusInfo.rating,
                  triage: focusInfo.triage,
                  favorite: focusInfo.favorite,
                  dbState: focusInfo.dbState,
                },
          }),
          getCollections: () => ({
            active: activeCollection === null
              ? null
              : {
                  id: activeCollection.id,
                  kind: activeCollection.kind,
                  name: activeCollection.name,
                  itemCount: activeCollection.item_count,
                },
            list: collectionsStore.collections.map((c) => ({
              id: c.id,
              kind: c.kind,
              name: c.name,
              itemCount: c.item_count,
            })),
            focusMemo:
              collectionMemoContext !== null && focusInfo !== null
                ? (collectionNotes[focusInfo.id] ?? null)
                : null,
          }),
          getSearch: () => ({
          lastResult: searchStore.lastResult === null
            ? null
            : {
                total: searchStore.lastResult.total,
                shown: searchStore.lastResult.shown,
                elapsedMs: searchStore.lastResult.elapsedMs,
                warnings: searchStore.lastResult.warnings,
                queryJson: searchStore.lastResult.queryJson,
              },
          lastQuery: searchStore.lastQuery,
          running: searchStore.running,
          evidenceCount: Object.keys(searchStore.evidence).length,
          save: (name: string) => searchStore.save(name, searchStore.lastResult?.queryJson ?? ""),
          refresh: () => searchStore.refresh(),
        }),
        getFileOps: () => ({
            busy: fileOpsBusy,
            progress: fileOpsProgress,
            notice: fileOpsNotice,
            error: fileOpsError,
            destPicker: destPickerFor !== null,
            conflict: conflictOf !== null,
            forever: deleteForeverFor !== null,
            retry: retryReport !== null,
            renaming: renameId !== null,
            undoDepth: undo.depth,
            undoLabel: undo.peekLabel(),
          }),
          invoke,
        });
      }
    })();
  });

  onDestroy(() => {
    disposeFileOpsProgress?.();
    stopChangeNotification();
    stopGridNotifyForPerf();
    stopLibraryNotification();
    stopSessionNotify();
    stopMetadataUpdates?.();
    disposeMetadata?.();
    stopCollectionsChanged?.();
    disposeCollections?.();
    window.removeEventListener("keydown", onGlobalKeydown, true);
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

  /** コレクション内メモの編集可否 (manual表示 + Asset確定 + フォーカス)。 */
  const collectionMemoContext = $derived.by(() => {
    const collection = activeCollection;
    const item = focusInfo;
    if (collection === null || collection.kind !== "manual" || item === null) {
      return null;
    }
    return item.dbState === "asset" ? collection.name : null;
  });

  const focusCollectionNote = $derived.by(() => {
    const item = focusInfo;
    if (collectionMemoContext === null || item === null) {
      return null;
    }
    return collectionNotes[item.id] ?? "";
  });
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
    <span
      class="source-badge"
      class:library={catalogSource === "library"}
      class:search={searchActive}
    >
      {searchActive ? "検索結果" : catalogSource === "library" ? "ライブラリ" : "一時走査"}
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
    {#if metaNotice}
      <span class="header-notice">{metaNotice}</span>
    {/if}
    {#if osNotice}
      <span class="header-notice">{osNotice}</span>
    {/if}
    {#if summary?.scanning}
      <button
        onclick={() => void cancelScan()}
        title="実行中の一時走査を中止します (確定済みの結果は残ります)"
      >
        走査をキャンセル
      </button>
    {:else}
      <button onclick={() => void startScan()} disabled={scanBusy}>
        スキャン開始
      </button>
    {/if}
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
          onDrop={handleTreeDrop}
        />
        <CollectionsPane
          store={collectionsStore}
          activeCollectionId={activeCollection?.id ?? null}
          onSelect={(collection) => void showCollection(collection)}
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
          {#if searchActive}
            <span class="coll-current">
              <span class="coll-current-kind" aria-hidden="true">⌕</span>
              検索結果
              <span class="coll-current-count" data-testid="search-status">
                ({(searchStore.lastResult?.total ?? 0).toLocaleString()}件・
                {(searchStore.lastResult?.elapsedMs ?? 0).toLocaleString()}ms)
              </span>
            </span>
            <button
              class="crumb"
              title="検索条件を新しいDB状態で再実行"
              onclick={() => void refreshSearch()}
              disabled={searchStore.running}
            >
              再実行
            </button>
            <button
              class="crumb"
              title="通常のライブラリ表示へ戻る"
              onclick={() => void exitSearch()}
            >
              閉じる
            </button>
          {:else if activeCollection !== null}
            <span class="coll-current">
              <span class="coll-current-kind" aria-hidden="true">
                {activeCollection.kind === "smart" ? "✦" : "▤"}
              </span>
              コレクション「{activeCollection.name}」
              <span class="coll-current-count">
                ({activeCollection.item_count.toLocaleString()}件
                {activeCollection.kind === "smart" ? "・評価結果" : ""})
              </span>
            </span>
            <button
              class="crumb"
              title="通常のライブラリ表示へ戻る"
              onclick={() => void exitCollection()}
            >
              閉じる
            </button>
          {:else if catalogSource === "library"}
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
        {#if activeCollection === null}
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
        {:else if activeCollection.kind === "manual"}
          <span class="collection-tools" role="group" aria-label="コレクション内の並び替え">
            <button
              disabled={collectionBusy || gridSelection.ids.length === 0}
              title="選択項目をコレクションの先頭へ移動"
              onclick={() => void moveSelectedInCollection("front")}
            >
              選択を先頭へ
            </button>
            <button
              disabled={collectionBusy || gridSelection.ids.length === 0}
              title="選択項目をコレクションの末尾へ移動"
              onclick={() => void moveSelectedInCollection("back")}
            >
              選択を末尾へ
            </button>
            <span class="position-move">
              <input
                type="number"
                min="1"
                placeholder="位置"
                aria-label="移動先の位置 (1-based)"
                value={collectionPositionInput}
                oninput={(event) => (collectionPositionInput = event.currentTarget.value)}
                onkeydown={(event) => {
                  if (event.key === "Enter") {
                    void moveSelectedInCollection("position");
                  }
                }}
              />
              <button
                disabled={collectionBusy || gridSelection.ids.length === 0}
                title="選択項目を指定位置へ移動"
                onclick={() => void moveSelectedInCollection("position")}
              >
                へ移動
              </button>
            </span>
            <button
              disabled={collectionBusy || gridSelection.ids.length === 0}
              title="選択項目をこのコレクションから除外 (元画像は影響しません)"
              onclick={() => void removeSelectedFromCollection()}
            >
              コレクションから除外
            </button>
          </span>
        {:else}
          <span class="collection-tools" role="group" aria-label="スマートコレクション操作">
            <button
              disabled={collectionBusy}
              title="検索条件を再評価して内容を更新 (評価・タグ等の変更後)"
              onclick={() => void refreshActiveSmartCollection()}
            >
              再評価
            </button>
          </span>
        {/if}
        {#if activeCollection !== null}
          <button class="export-entry" disabled title="エクスポートはWP-14で実装予定です">
            エクスポート…
          </button>
        {/if}
        <button
          class="toolbar-folder"
          disabled={catalogSource !== "library" || fileOpsBusy}
          title="現在の登録ルートに新しいフォルダを作成"
          onclick={() => void startNewFolder()}
        >
          + 新規フォルダ
        </button>
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
      <div class="search-bar" role="search" data-testid="search-bar">
        <input
          type="search"
          class="search-input"
          placeholder="検索 (例: 青い髪の少女 tag:night rating:>=3 type:png)"
          aria-label="検索クエリ"
          data-testid="search-input"
          value={searchInput}
          oninput={(event) => (searchInput = event.currentTarget.value)}
          onkeydown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void runSearch();
            }
          }}
        />
        <select
          class="search-scope"
          aria-label="検索範囲"
          title="検索範囲。「現在のフォルダ」は選択フォルダを含む登録ルート全体 (サブフォルダ単位の絞り込みはなし)"
          value={searchScopeMode}
          onchange={(event) =>
            (searchScopeMode = event.currentTarget.value as typeof searchScopeMode)}
        >
          <option value="library">ライブラリ全体</option>
          <option value="folder">現在のフォルダ（登録ルート全体）</option>
          <option value="collection">表示中コレクション</option>
        </select>
        <button
          class:active={searchFiltersOpen || searchFiltersActive}
          onclick={() => (searchFiltersOpen = !searchFiltersOpen)}
          title="評価・選別・お気に入り・形式・タグ・日付で絞り込み (検索語とAND合成)"
          data-testid="search-filter-toggle"
        >
          フィルター{searchFiltersActive ? " ●" : ""}
        </button>
        <button
          onclick={() => void runSearch()}
          disabled={searchStore.running}
          data-testid="search-run"
        >
          {searchStore.running ? "検索中…" : "検索"}
        </button>
        <button
          onclick={() => void saveCurrentSearch()}
          disabled={searchStore.lastResult === null}
          title="現在の検索条件を保存済み検索 (スマートコレクション) として保存"
          data-testid="search-save"
        >
          保存
        </button>
        {#if searchStore.lastResult !== null && searchStore.lastResult.warnings.length > 0}
          <span class="search-warn" title={searchStore.lastResult.warnings.join("\n")}>
            ⚠ {searchStore.lastResult.warnings.length}件の警告
          </span>
        {/if}
        {#if searchFiltersOpen}
          <div class="search-filters" data-testid="search-filters">
            <label>
              評価≥
              <select
                value={searchFilters.ratingMin === null ? "" : String(searchFilters.ratingMin)}
                onchange={(event) => {
                  const value = event.currentTarget.value;
                  searchFilters = {
                    ...searchFilters,
                    ratingMin: value === "" ? null : Number(value),
                  };
                }}
              >
                <option value="">-</option>
                {#each [1, 2, 3, 4, 5] as rating (rating)}
                  <option value={rating}>★{rating}</option>
                {/each}
              </select>
            </label>
            <label>
              選別
              <select
                value={searchFilters.cull ?? ""}
                onchange={(event) => {
                  const value = event.currentTarget.value;
                  searchFilters = {
                    ...searchFilters,
                    cull:
                      value === "picked" || value === "rejected" || value === "unselected"
                        ? value
                        : null,
                  };
                }}
              >
                <option value="">-</option>
                <option value="picked">採用</option>
                <option value="rejected">除外</option>
                <option value="unselected">未選別</option>
              </select>
            </label>
            <label>
              <input
                type="checkbox"
                checked={searchFilters.favorite === true}
                onchange={(event) =>
                  (searchFilters = {
                    ...searchFilters,
                    favorite: event.currentTarget.checked ? true : null,
                  })}
              />
              ♥ お気に入り
            </label>
            <label>
              形式
              <select
                value={searchFilters.type}
                onchange={(event) =>
                  (searchFilters = { ...searchFilters, type: event.currentTarget.value })}
              >
                {#each ["", "png", "jpg", "webp", "gif", "avif", "bmp", "tiff"] as format (format)}
                  <option value={format}>{format === "" ? "-" : format}</option>
                {/each}
              </select>
            </label>
            <label>
              タグ
              <input
                type="text"
                placeholder="tag"
                value={searchFilters.tag}
                oninput={(event) =>
                  (searchFilters = { ...searchFilters, tag: event.currentTarget.value })}
              />
            </label>
            <label>
              追加日
              <input
                type="date"
                value={searchFilters.addedFrom}
                onchange={(event) =>
                  (searchFilters = { ...searchFilters, addedFrom: event.currentTarget.value })}
              />
              〜
              <input
                type="date"
                value={searchFilters.addedTo}
                onchange={(event) =>
                  (searchFilters = { ...searchFilters, addedTo: event.currentTarget.value })}
              />
            </label>
            <button onclick={() => (searchFilters = { ...EMPTY_FILTERS })}>クリア</button>
          </div>
        {/if}
      </div>
      {#if searchNotice}
        <p class="toolbar-notice error" role="alert" data-testid="search-notice">{searchNotice}</p>
      {/if}
      {#if viewerNotice}
        <p class="toolbar-notice">{viewerNotice}</p>
      {/if}
      {#if folderNotice}
        <p class="toolbar-notice error">{folderNotice}</p>
      {/if}
      {#if collectionNotice}
        <p class="toolbar-notice error" role="status">{collectionNotice}</p>
      {/if}
      {#if fileOpsNotice}
        <p class="toolbar-notice" role="status" data-testid="fileops-notice">{fileOpsNotice}</p>
      {/if}
      {#if fileOpsError !== null}
        <p class="toolbar-notice error" role="alert" data-testid="fileops-error">{fileOpsError}</p>
      {/if}
      {#if fileOpsBusy && fileOpsProgress !== null}
        <div class="fileops-progress" role="status" data-testid="fileops-progress">
          <span>
            {describeKind(conflictOf?.args.kind ?? "copy")}中
            {fileOpsProgress.index + 1}/{fileOpsProgress.total}
          </span>
          <progress max={fileOpsProgress.total} value={fileOpsProgress.index + 1}></progress>
          <button onclick={() => void cancelFileOps(fileOpsIpc)}>キャンセル</button>
        </div>
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
            tooltipOf={(item) => cellTooltip(item.displayName, item.id)}
            {renameId}
            {renameInitial}
            onRenameCommit={(name) => void commitRename(name)}
            onRenameCancel={() => (renameId = null)}
            dragPathsOf={(index) => {
              const item = grid.getItem(index);
              if (item === null) {
                return [];
              }
              // ドラッグ起点が選択内なら選択全体を運ぶ (ファイルマネージャ規約)
              if (gridSelection.ids.includes(item.id)) {
                return selectedLibraryPaths();
              }
              return [item.path];
            }}
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
          searchEvidence={searchEvidenceOf(focusInfo?.id ?? "")}
          assetMetadata={focusInfo !== null && metaAssetMetadata?.id === focusInfo.id
            ? metaAssetMetadata
            : null}
          sessionCounts={sessionCounts}
          sessionOrigin={sessionSnapshot.origin}
          resumable={sessionResumable}
          sessionNotice={sessionNotice}
          onStartFolder={() => void startSessionFolder()}
          onStartSelection={startSessionSelection}
          onResume={() => void resumeSession()}
          onEnd={endSession}
          onSessionMove={sessionMove}
          detail={metaDetail}
          metaBusy={metaBusy}
          metaError={metaError}
          actions={metaActions}
          suggestTags={(prefix) => metadata.suggestTags(prefix)}
          undoLabel={undoLabel}
          bulkTargetIds={gridSelection.ids}
          collectionMemoContext={collectionMemoContext}
          collectionMemoNote={focusCollectionNote}
          onSaveCollectionMemo={(note) => {
            const item = focusInfo;
            if (item !== null) {
              void saveCollectionMemo(item.id, note);
            }
          }}
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

{#if collectionPickerFor !== null}
  <CollectionPicker
    store={collectionsStore}
    assetIds={collectionPickerFor}
    onClose={() => (collectionPickerFor = null)}
  />
{/if}

{#if destPickerFor !== null}
  <DestPicker
    roots={libraryRoots}
    title={destPickerFor.kind === "copy" ? "コピー先フォルダ" : "移動先フォルダ"}
    confirmLabel={destPickerFor.kind === "copy" ? "コピー" : "移動"}
    onPick={(destDir) => {
      const target = destPickerFor;
      destPickerFor = null;
      if (target !== null) {
        void runPlannedOp({
          kind: target.kind,
          paths: target.paths,
          dest_dir: destDir,
        });
      }
    }}
    onClose={() => (destPickerFor = null)}
  />
{/if}

{#if conflictOf !== null && conflictOf.session.current !== null}
  {@const conflictStep = conflictOf.session.current}
  <ConflictDialog
    session={conflictOf.session}
    totalCount={conflictOf.args.paths.length}
    srcThumbUrl={thumbUrlForPath(conflictStep.src)}
    dstThumbUrl={thumbUrlForPath(conflictStep.conflict?.dst ?? null)}
    onChoose={onConflictChoose}
    onCancel={() => {
      conflictOf = null;
      fileOpsNotice = "操作を中止しました";
    }}
  />
{/if}

{#if deleteForeverFor !== null}
  <DeleteForeverDialog
    count={deleteForeverFor.paths.length}
    onConfirm={() => void confirmDeleteForever()}
    onCancel={() => (deleteForeverFor = null)}
  />
{/if}

{#if retryReport !== null}
  <div class="retry-backdrop" role="presentation">
    <div class="retry" role="dialog" aria-modal="true" aria-label="一部のファイルで失敗">
      <h2>一部のファイルを処理できませんでした</h2>
      <ul class="retry-list" data-testid="retry-list">
        {#each retryReport.outcome.steps.filter((step) => step.status === "failed") as step (step.file_id)}
          <li>
            <span class="retry-path">{step.from}</span>
            <span class="retry-error">{step.error ?? ""}</span>
          </li>
        {/each}
      </ul>
      <div class="retry-actions">
        <button onclick={() => (retryReport = null)}>閉じる</button>
        <button
          class="primary"
          data-testid="retry-button"
          onclick={() => {
            const report = retryReport;
            retryReport = null;
            if (report !== null) {
              const retry = retryArgsOf(report.outcome, report.args);
              if (retry !== null) {
                // 再計画からやり直す (確認トークンは単発のため再発行が必要。
                // 衝突状態も変わっている可能性がある)。
                void runPlannedOp(retry);
              }
            }
          }}
        >
          失敗した {retryReport.outcome.steps.filter((s) => s.status === "failed").length.toLocaleString()}件を再試行
        </button>
      </div>
    </div>
  </div>
{/if}

<QuickPreview store={quickPreview} onClose={() => quickPreview.close()} />

{#if viewerMode === "single" && viewerStore !== null}
  <ImageViewer
    store={viewerStore}
    onClose={closeViewer}
    triageMode={triageModeOn}
    onToggleTriageMode={() => (triageModeOn = !triageModeOn)}
    onTriageAction={(action) => {
      const handled = handleTriageAction(action);
      if (handled) {
        // 自動次送り (spec §9.2 高速選別モード)
        viewerStore?.nav(1);
        syncSelectionToViewer();
      }
      return handled;
    }}
  />
{:else if (viewerMode === "compare2" || viewerMode === "compare4") && compareStore !== null}
  <CompareView store={compareStore} onClose={closeViewer} onCullSlot={handleCullSlot} />
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

  .source-badge.search {
    border-color: light-dark(#8250df, #a371f7);
    color: light-dark(#8250df, #a371f7);
  }

  /* WP-8: 検索バー */
  .search-bar {
    flex: none;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.4rem;
    padding: 0.25rem 0.6rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
  }

  .search-input {
    flex: 1 1 16rem;
    min-width: 12rem;
  }

  .search-scope {
    flex: none;
  }

  .search-warn {
    font-size: 0.75rem;
    color: light-dark(#9a6700, #d29922);
  }

  .search-filters {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.6rem;
    width: 100%;
    padding: 0.3rem 0.2rem;
    font-size: 0.78rem;
    border-top: 1px dashed light-dark(#d0d7de, #30363d);
  }

  .search-filters label {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
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

  .coll-current {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-weight: bold;
  }

  .coll-current-kind {
    color: light-dark(#0969da, #58a6ff);
  }

  .coll-current-count {
    font-weight: normal;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
  }

  .collection-tools {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    flex-wrap: wrap;
  }

  .position-move {
    display: inline-flex;
    align-items: center;
    gap: 0.2rem;
  }

  .position-move input {
    width: 4.5rem;
    font: inherit;
    font-size: 0.78rem;
  }

  .export-entry:disabled {
    opacity: 0.55;
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

  .fileops-progress {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
  }

  .fileops-progress progress {
    width: 10rem;
  }

  .toolbar-folder {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.15rem 0.5rem;
    border-radius: 0.375rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  .toolbar-folder:disabled {
    opacity: 0.5;
    cursor: default;
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

  .retry-backdrop {
    position: fixed;
    inset: 0;
    z-index: 660;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .retry {
    width: min(36rem, 92vw);
    max-height: 70vh;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.6rem;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: 0.9rem 1rem;
    font-size: 0.85rem;
  }

  .retry h2 {
    font-size: 1rem;
    margin: 0;
  }

  .retry-list {
    margin: 0;
    padding: 0 0 0 1.2rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.78rem;
  }

  .retry-list li {
    display: flex;
    flex-direction: column;
  }

  .retry-path {
    overflow-wrap: anywhere;
  }

  .retry-error {
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .retry-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .retry-actions .primary {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
    border: none;
    border-radius: 0.375rem;
    padding: 0.3rem 0.8rem;
    cursor: pointer;
  }
</style>
