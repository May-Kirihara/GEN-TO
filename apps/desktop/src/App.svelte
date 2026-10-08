<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
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
  import {
    LibraryStore,
    startupCatalogView,
    type LibraryIpc,
    type LibraryRoot,
  } from "./lib/library";
  import { RegistrationConfirmGate } from "./lib/ui/registration-confirm";
  import { GridStore, type GridIpc } from "./lib/grid/grid-store";
  import { ScrollPositionStore } from "./lib/grid/scroll-position";
  import type { ThumbCellClass } from "./lib/grid/layout";
  import ThumbnailGrid from "./lib/grid/ThumbnailGrid.svelte";
  import { ViewerStore, type ViewerIpc } from "./lib/viewer/viewer-store";
  import ImageViewer from "./lib/viewer/ImageViewer.svelte";
  import { CompareStore, type CompareIpc } from "./lib/compare/compare-store";
  import CompareView from "./lib/compare/CompareView.svelte";
  import { perf } from "./lib/perf";
  import { breadcrumbOf, deriveEmptyState, normalizePastedPath } from "./lib/browse";
  import { FolderTreeStore, registeredStateFor } from "./lib/fsview";
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
    DEFAULT_LAYOUT_PREFS,
    LayoutPrefsStore,
    clampPaneWidth,
    toggleSection,
    type LeftPaneTab,
  } from "./lib/ui/layout-prefs";
  import { FolderPrefsStore, LIBRARY_SORT_KEY } from "./lib/ui/folder-prefs";
  import { copyText } from "./lib/ui/clipboard";
  import { collapsedSearchSummary } from "./lib/ui/search-summary";
  import {
    MODE_WEIGHT_PERCENT,
    SemanticModeStore,
    searchBadgeLabel,
    type SemanticMode,
  } from "./lib/ui/search-mode";
  import { suggestsSemantic } from "./lib/ui/semantic-suggest";
  import { filterChips, withoutFilter, type FilterChipKey } from "./lib/ui/filter-chips";
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
    VectorIndexStore,
    semanticSearchAvailable,
    semanticUnavailableReason,
    type VectorIndexProgress,
    type VectorIndexStatus,
  } from "./lib/vector-index";
  import { EmbeddingStore } from "./lib/embedding";
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
  // UIUX-2: 危険パス (`/` 等) 登録の確認ダイアログ
  import ConfirmRegistrationDialog from "./lib/ui/ConfirmRegistrationDialog.svelte";
  // WP-13: バックアップと復元
  import BackupDialog from "./lib/ui/BackupDialog.svelte";
  import ExportDialog from "./lib/ui/ExportDialog.svelte";
  import RestoreDialog from "./lib/ui/RestoreDialog.svelte";
  import SettingsDialog from "./lib/ui/SettingsDialog.svelte";
  // WP-18 E3: 初回起動ガイド (登録ルート0件・空状態の充実)
  import FirstRunGuide from "./lib/ui/FirstRunGuide.svelte";
  import { deriveFirstRunGuide } from "./lib/ui/first-run";
  // WP-18 E5: インライン確認ダイアログのフォーカストラップ
  import { focusTrap } from "./lib/ui/focus-trap";
  type TriageKeyAction = import("./lib/triage-keys").TriageKeyAction;
  import ContextMenu, { type MenuAction } from "./lib/ui/ContextMenu.svelte";
  import MenuBar, { type MenuDef } from "./lib/ui/MenuBar.svelte";
  import LeftPaneTabs from "./lib/ui/LeftPaneTabs.svelte";
  import FolderTree from "./lib/shell/FolderTree.svelte";
  import CollectionsPane from "./lib/shell/CollectionsPane.svelte";
  import CollectionPicker from "./lib/ui/CollectionPicker.svelte";
  import LibraryPanel from "./lib/shell/LibraryPanel.svelte";
  import InfoPanel from "./lib/shell/InfoPanel.svelte";
  import DevDrawer from "./lib/shell/DevDrawer.svelte";
  // WP-10: AI統合 (プロファイル・ジョブキュー・自動処理)
  import AiPanel from "./lib/shell/AiPanel.svelte";
  // UIUX-5/WP7a: App.svelte分割 — shell小成分 (状态・storeはApp残置)
  import ToastStack from "./lib/shell/ToastStack.svelte";
  import {
    EMPTY_TOAST_QUEUE,
    enqueueToast,
    withoutToast,
    type ToastKind,
  } from "./lib/ui/toast-queue";
  import SessionModeBar from "./lib/shell/SessionModeBar.svelte";
  import NoticeStrip from "./lib/shell/NoticeStrip.svelte";
  import AppHeader from "./lib/shell/AppHeader.svelte";
  // UIUX-5/WP7b: App.svelte分割 — toolbar/search-bar抽出 (状態・storeはApp残置)
  import Toolbar from "./lib/shell/Toolbar.svelte";
  import SearchBar from "./lib/shell/SearchBar.svelte";
  import { AiStore, type AiProfile, type AiRun } from "./lib/ai";

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

  // ===== レイアウト (WP-4: 3ペイン / UIUX-1: タブ・スクロール速度) =====

  const layoutStore = new LayoutPrefsStore(localStorage);
  const folderStore = new FolderPrefsStore(localStorage);
  // UIUX-7: 検索モード (キーワード/ハイブリッド/意味) の永続化
  const semanticModeStore = new SemanticModeStore(localStorage);
  let leftVisible = $state(true);
  let rightVisible = $state(true);
  let leftWidth = $state(240);
  let rightWidth = $state(280);
  let cellClass = $state<ThumbCellClass>("s256");
  let overscanChoice = $state(2);
  /** 左ペインのアクティブタブ (全パネルはマウント維持・隠蔽切替)。 */
  let leftTab = $state<LeftPaneTab>("folders");
  /** ホイールスクロール速度係数 (1.0=ネイティブ=ハンドラ実質無効)。 */
  let scrollSpeed = $state(1);
  /** UIUX-3 V-3: 検索行の折りたたみ (true=展開)。layout-prefsへ永続化。 */
  let searchExpanded = $state(true);
  /** UIUX-5 WP5: InfoPanel折りたたみ中のセクションID列 (layout-prefsへ永続化)。 */
  let infoPanelCollapsed = $state<string[]>([...DEFAULT_LAYOUT_PREFS.infoPanelCollapsed]);

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
  /** ビューアオープン失敗の常駐エラー (UIUX-6/WP1, WP3: エラー=strip常駐+
   *  toast併用 / ガイド=toastのみ。オーバーレイ下でもtoastが見える)。 */
  let viewerError: string | null = $state(null);
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

  // ===== AI (WP-10) =====

  const aiStore = new AiStore(ipc);
  let aiPanelOpen = $state(false);
  /** フォーカス項目のAI実行履歴 (InfoPanelのAIバッジ照会)。 */
  let aiRuns: AiRun[] = $state([]);

  /** 安定AssetIdのみの選択 (AIジョブ投入対象)。 */
  function stableSelectionIds(): string[] {
    if (catalogSource !== "library") {
      return [];
    }
    return gridSelection.ids.filter((id) => gridPagerHasId(id));
  }

  /** 選択項目へAI生成ジョブを投入 (成功済みはスキップ。再生成はAIパネル/force)。 */
  async function enqueueAiForSelection(): Promise<void> {
    const ids = stableSelectionIds();
    if (ids.length === 0) {
      showMetaNotice("AI生成はライブラリ表示中の確定済み画像に対して実行します");
      return;
    }
    try {
      const n = await aiStore.enqueue(ids, false);
      showMetaNotice(
        n > 0
          ? `AI生成を${n.toLocaleString()}件キューへ投入しました`
          : "対象はすべて処理済みです (再生成はAI設定から)",
      );
    } catch (error) {
      showMetaNotice(error instanceof Error ? error.message : String(error));
    }
  }

  /** フォーカス項目のAI実行履歴を取得 (detail更新・ジョブ完了時に呼ぶ)。 */
  async function refreshAiRuns(assetId: string | null): Promise<void> {
    if (assetId === null || catalogSource !== "library" || !gridPagerHasId(assetId)) {
      aiRuns = [];
      return;
    }
    try {
      aiRuns = await aiStore.runHistory(assetId);
    } catch {
      aiRuns = [];
    }
  }

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

  // ===== トースト通知 (UIUX-5/WP4) =====

  /**
   * 揮発性noticeのトースト表示経路 (4秒で自動消滅。キュー遷移自体は
   * lib/ui/toast-queue.tsの純粋関数)。fileops系トーストは元noticeと同じ
   * testid・roleを持ち、可視中のみDOMに存在する。
   * 全kind (viewer/fileOps/meta/os) とも設定箇所のshowXxxNoticeヘルパーが
   * 明示pushする (WP10)。viewerのみ2経路 (UIUX-6/WP1, WP3): オープン失敗
   * エラーはviewerError (notice-strip常駐) +toast併用、非エラーガイドは
   * toastのみ。
   * $effectミラーは (1) effect内でtoastQueueを
   * read+writeするため自身の依存となり、非空noticeがあると更新深度超過
   * まで無限pushする潜在バグ、(2) 同一文言の連続設定はstate不変で再発火
   * しない、の両方を抱えるため廃止 (Codex review 2026-10-07 3回目
   * blocker)。クリア (空文字設定) はトースト不要のため直接代入のまま。
   * header popoverが閉じている間やshowMetaNoticeの2.5秒自動消滅後も
   * フィードバックが保証される。エラー性は文面から判定できないため
   * 色分けなし (info相当の既定スタイルで統一)。
   */
  let toastQueue = $state(EMPTY_TOAST_QUEUE);
  // eslint-disable-next-line svelte/prefer-svelte-reactivity -- 期限管理のみで描画に使わない
  const toastTimers = new Map<number, ReturnType<typeof setTimeout>>();

  function pushToast(kind: ToastKind, text: string): void {
    toastQueue = enqueueToast(toastQueue, kind, text);
    const id = toastQueue.seq; // enqueueToastが割り当てた直近id
    toastTimers.set(id, setTimeout(() => dismissToast(id), 4000));
  }

  function dismissToast(id: number): void {
    const timer = toastTimers.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      toastTimers.delete(id);
    }
    toastQueue = withoutToast(toastQueue, id);
  }

  function showViewerNotice(text: string): void {
    // 非エラーガイド (openViewerFromToolbarの «グリッドで項目を選択
    // してください») 専用のtoast経路。オープン失敗エラーは
    // openViewerForChoiceのcatchがstrip+toast併用で出す (UIUX-6/WP1, WP3)。
    pushToast("viewer", text);
  }

  function showFileOpsNotice(text: string): void {
    fileOpsNotice = text;
    pushToast("fileops", text);
  }

  // ===== バックアップと復元 (WP-13) =====

  /** .gento作成ダイアログ中のルート。 */
  let backupFor = $state<LibraryRoot | null>(null);
  /** 復元ダイアログ。 */
  let restoreOpen = $state(false);
  // WP-15: 設定ダイアログ (メニューバー ツール>設定… から)
  let settingsOpen = $state(false);
  let settingsInitialTab = $state<"general" | "about">("general");
  let settingsProfiles = $state<AiProfile[]>([]);
  const settingsDisplayInfo = $derived.by(() => {
    const prefs = folderStore.load();
    const sortKey = currentFolder ?? LIBRARY_SORT_KEY;
    const sort = prefs.sorts[sortKey] ?? "name";
    const sortLabel = sort === "date_desc" ? "撮影日降順" : sort === "size_desc" ? "サイズ降順" : "名前順";
    return {
      lastFolder: currentFolder,
      expandedCount: tree.expandedPaths().length,
      sortLabel,
      scrollSpeed,
    };
  });

  /** メニューの「設定…」/「このアプリについて」導線 (初期タブ指定)。 */
  async function openSettings(initialTab: "general" | "about" = "general"): Promise<void> {
    settingsInitialTab = initialTab;
    settingsOpen = true;
    // AI タブ表示用にプロファイルを一度だけ取得 (短命モーダル規約)
    try {
      settingsProfiles = await invoke<AiProfile[]>("ai_profile_list");
    } catch {
      settingsProfiles = [];
    }
  }
  /** フルDBバックアップ実行中。 */
  let fullDbBusy = $state(false);

  // ===== SD学習用エクスポート (WP-14) =====

  /** エクスポートダイアログ中の入力 (解決済みasset id列 + 表示ラベル)。 */
  let exportFor = $state<{ assetIds: string[]; sourceLabel: string } | null>(null);

  /** 選択項目をエクスポート導線へ (stable AssetIdを持つ項目のみ)。 */
  function startExportForSelection(items: ImageListItem[]): void {
    const ids = items
      .filter((entry) => entry.dbState === "asset")
      .map((entry) => entry.id);
    if (ids.length === 0) {
      showMetaNotice("エクスポート対象がありません (確定済みの画像を選択してください)");
      return;
    }
    exportFor = {
      assetIds: ids,
      sourceLabel: `選択 (${ids.length.toLocaleString()}件)`,
    };
  }

  /** 表示中コレクションの全項目を解決してエクスポート導線へ。 */
  async function startExportForCollection(collection: CollectionDto): Promise<void> {
    try {
      const ids: string[] = [];
      if (collection.kind === "manual") {
        let offset = 0;
        for (;;) {
          const page = await collectionsStore.items(collection.id, offset, 500);
          ids.push(...page.items.map((item) => item.asset_id));
          offset += page.items.length;
          if (offset >= page.total || page.items.length === 0) {
            break;
          }
        }
      } else {
        let offset = 0;
        for (;;) {
          const page = await collectionsStore.smartItems(collection.id, offset, 500);
          ids.push(...page.asset_ids);
          offset += page.asset_ids.length;
          if (offset >= page.total || page.asset_ids.length === 0) {
            break;
          }
        }
      }
      if (ids.length === 0) {
        collectionNotice = "コレクションが空のためエクスポートできません";
        return;
      }
      exportFor = {
        assetIds: ids,
        sourceLabel: `コレクション「${collection.name}」`,
      };
    } catch (error) {
      collectionNotice = error instanceof Error ? error.message : String(error);
    }
  }

  async function runFullDbBackup(): Promise<void> {
    if (fullDbBusy) {
      return;
    }
    fullDbBusy = true;
    try {
      const out = await invoke<{ path: string; keptGenerations: number }>("backup_full_db", {
        destDir: null,
      });
      libraryNotice = `DBバックアップを作成しました: ${out.path} (保持${out.keptGenerations}世代)`;
    } catch (error) {
      libraryError = error instanceof Error ? error.message : String(error);
    } finally {
      fullDbBusy = false;
    }
  }

  // ===== UIUX-1: メニューバー・ツリー右クリック・パンくず入力 =====

  /** アプリ終了 (Tauri window close経路。capabilitiesがcore:window:allow-closeを要求)。 */
  async function quitApp(): Promise<void> {
    try {
      await getCurrentWindow().close();
    } catch (error) {
      folderNotice = `終了できませんでした: ${toErrorMessage(error)}`;
    }
  }

  /** ツリー右クリック: 未登録フォルダを登録 (登録→見える導線はUXLIB-1規約)。
   * UIUX-2: 危険パス (`/` 等の広域ルート) は確認ダイアログを挟む。
   * UIUX-3 N2: dialog確定まで requestAddRoot のPromiseを保留し、
   * 確認経由の登録でも呼び出し側 (LibraryPanel) の入力クリアが効く。 */
  let registerConfirmPath = $state<string | null>(null);
  const registrationGate = new RegistrationConfirmGate(
    () => (registerConfirmPath = registrationGate.pendingPath),
  );

  function requestAddRoot(path: string): Promise<boolean> {
    return registrationGate.request(path, addRootConfirmed);
  }

  async function addRootConfirmed(path: string): Promise<boolean> {
    const ok = await library.addRoot(path);
    if (!ok) {
      libraryError = library.lastError ?? "登録に失敗しました";
      return false;
    }
    await showLibrary();
    return true;
  }

  function registerRootFromTree(path: string): void {
    void requestAddRoot(path);
  }

  /** ツリー右クリック: 登録解除 (状態が変わるだけ。表示はshowLibrary/ツリーで追従)。 */
  async function unregisterRootFromTree(rootId: string): Promise<void> {
    const ok = await library.removeRoot(rootId);
    if (!ok) {
      libraryError = library.lastError ?? "解除に失敗しました";
    }
  }

  /** ツリー右クリック: ライブラリ全体の再スキャン (per-root APIは存在しない)。 */
  async function rescanLibraryFromTree(): Promise<void> {
    const ok = await library.startScan();
    if (!ok) {
      libraryError = library.lastError ?? "再スキャン開始に失敗しました";
    }
  }

  /** ツール>「ルートのバックアップ…」: 1ルートなら直開、複数なら選択picker。 */
  let backupPickOpen = $state(false);

  function backupRootFromMenu(): void {
    if (libraryRoots.length === 1) {
      backupFor = libraryRoots[0]!;
      return;
    }
    if (libraryRoots.length > 1) {
      backupPickOpen = true;
    }
  }

  /** メニューのエクスポート対象があるか (表示中コレクション優先・次に選択資産)。 */
  function menuExportAvailable(): boolean {
    if (activeCollection !== null) {
      return true;
    }
    return stableSelectionIds().length > 0;
  }

  function exportFromMenu(): void {
    const collection = activeCollection;
    if (collection !== null) {
      void startExportForCollection(collection);
      return;
    }
    const items: ImageListItem[] = [];
    const ids = new Set(gridSelection.ids);
    for (const [, index] of grid.pager.findIndicesByIds(ids)) {
      const item = grid.getItem(index);
      if (item !== null) {
        items.push(item);
      }
    }
    startExportForSelection(items);
  }

  /** パンくずのパス入力モード (UIUX-1 / R-E)。 */
  let pathInputOpen = $state(false);
  let pathInputValue = $state("");

  function openPathInput(): void {
    pathInputValue = currentFolder ?? "/";
    pathInputOpen = true;
  }

  function cancelPathInput(): void {
    pathInputOpen = false;
  }

  function commitPathInput(): void {
    const raw = pathInputValue;
    pathInputOpen = false;
    const result = normalizePastedPath(raw);
    if (!result.ok) {
      folderNotice = result.error;
      return;
    }
    // 通常のツリー選択と同一遷移 (library/collection表示モードからの脱出を含む)
    void selectFolder(result.path);
  }

  /** 左ペインタブ切替 (永続化のみ)。フォーカスモデルはAPG標準に統一:
   * クリック・キーボード (自動活性化+rAFフォーカス追従) ともタブに留まり、
   * パネルへは移さない (r1 N2 — タブ自体が操作面のまま操作性が一貫)。 */
  function setLeftTab(tab: LeftPaneTab): void {
    leftTab = tab;
    saveLayout();
  }

  /** スクロール速度 (UIUX-3 V-1: 設定dialog表示タブから編集。即時反映・
   * changeで永続化・「既定に戻す」=明示2.0書込。UIUX-1のtoolbarスライダー
   * から移設 — dialogはstore所有権を持たせずcallbackで受ける)。 */
  function onScrollSpeedInput(value: number): void {
    scrollSpeed = value;
  }

  function onScrollSpeedCommit(value: number): void {
    void value;
    saveLayout();
  }

  function onScrollSpeedReset(): void {
    scrollSpeed = DEFAULT_LAYOUT_PREFS.scrollSpeed;
    saveLayout();
  }

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
      showFileOpsNotice("ファイル操作はライブラリ表示中のみ利用できます");
      return false;
    }
    if (fileOpsBusy) {
      showFileOpsNotice("別のファイル操作が実行中です");
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
      showFileOpsNotice("対象ファイルを選択してください");
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
      showFileOpsNotice(outcomeNotice(outcome));
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
      showFileOpsNotice("対象ファイルを選択してください");
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
      showFileOpsNotice("対象ファイルを選択してください");
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
      showFileOpsNotice("リネームは1ファイルずつ行います (1件を選択してください)");
      return;
    }
    const focus = focusLibraryPath();
    if (focus === null) {
      showFileOpsNotice("対象ファイルを選択してください");
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
      showFileOpsNotice("登録ルートがありません");
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
      showFileOpsNotice(`「${name.trim()}」を作成しました`);
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
  /** WP-12: 索引イベント購読の解除関数。 */
  let disposeVectorIndexReady: (() => void) | null = null;
  let disposeVectorIndexProgress: (() => void) | null = null;

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
      collectionToolsOpen = false;
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
      collectionToolsOpen = false;
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
  /** フィルタpopoverの開閉 (正本はApp — レイアウト系。SearchBarへprops+callback)。 */
  let searchFiltersOpen = $state(false);
  /** UIUX-5/WP3: manual collection操作のオーバーフローpopover開閉。
   * state正本はここ (App) — Toolbarへprops+callbackで授受し、
   * showCollection/exitCollectionでのresetはApp側関数で実施。 */
  let collectionToolsOpen = $state(false);
  let searchScopeMode = $state<"library" | "folder" | "collection">("library");
  /** 検索結果表示中 (カタログはsearchソース)。 */
  let searchActive = $state(false);
  let searchNotice = $state("");
  /**
    * UIUX-3 V-3: 最終実行の検索状態スナップショット (折りたたみ要約の原料)。
    * 確定済み状態のみ — searchInput (入力途中) は使わない。scopeは実行時の
    * 実態 (解決済みroot path / コレクション名) を記録する。
    */
  let lastSearchMeta = $state<{
    kind: "query" | "similar";
    query: string;
    scopeLabel: string | null;
    weightPercent: number;
    filtersActive: boolean;
  } | null>(null);
  /** evidence写像の変化通知 (InfoPanel・ツールチップ再評価用)。 */
  let searchEvidenceTick = $state(0);
  // ===== 意味検索 (WP-12 / UIUX-7: 3モードセグメント化) =====
  const vectorIndex = new VectorIndexStore({ invoke, listen });
  const embeddingProbe = new EmbeddingStore({ invoke, listen });
  let vectorIndexStatus = $state<VectorIndexStatus | null>(null);
  let embeddingEnabled = $state(false);
  let indexProgress = $state<VectorIndexProgress | null>(null);
  /** 索引状態の変化通知 (モードセグメントdisabled・サジェスト判定の再評価用)。 */
  let semanticStateTick = $state(0);

  /** 意味検索が現在使えるか (モードセグメント・類似検索の可用性)。 */
  function semanticAvailable(): boolean {
    void semanticStateTick;
    return semanticSearchAvailable(embeddingEnabled, vectorIndexStatus);
  }

  function semanticReason(): string {
    void semanticStateTick;
    return semanticUnavailableReason(embeddingEnabled, vectorIndexStatus);
  }

  /** UIUX-7: 検索モード (keyword/hybrid/semantic)。実行重みはMODE_WEIGHT_PERCENTで導出。 */
  let semanticMode = $state<SemanticMode>(semanticModeStore.load());

  /** UIUX-7: 検索モード切替 (永続化のみ — 実行は検索ボタン/Enterで明示的に)。 */
  function setSemanticMode(mode: SemanticMode): void {
    semanticMode = mode;
    semanticModeStore.save(mode);
  }

  /**
    * UIUX-7/C: 意味検索サジェストチップの表示判定。検索実行後の確定値
    * (lastSearchMeta.query) のみで判定する — 入力中の逐次表示はしない。
    * WP3: チップclickで却下された同一クエリでは出さない (再実行が失敗/
    * 遅延しても旧メタのまま出し続けない)。検索実行中も出さない。
    * WP4: 検索結果表示中 (searchActive) のみ — exitSearchは折りたたみ
    * 要約のためにlastSearchMetaを残すため、ここで検索終了後の表示を断つ。
    */
  let semanticSuggestDismissedQuery = $state<string | null>(null);
  const semanticSuggestVisible = $derived.by(() => {
    void searchEvidenceTick; // searchStore.running変化 (onChange) で再評価
    const meta = lastSearchMeta;
    return (
      meta !== null &&
      meta.kind === "query" &&
      suggestsSemantic(meta.query, meta.weightPercent, semanticAvailable(), {
        dismissedQuery: semanticSuggestDismissedQuery,
        running: searchStore.running,
        searchActive,
      })
    );
  });

  /**
    * UIUX-7/C: チップclick — hybridへ切替え、確定済みクエリで再実行する。
    * WP3: 対象クエリを却下済みに記録してから再実行する。成功なら
    * weightPercent>0で、失敗・遅延なら却下済みquery一致で、それぞれ
    * チップは消滅する (新しいクエリの検索では再表示される)。
    */
  async function trySemanticSearch(): Promise<void> {
    const meta = lastSearchMeta;
    if (meta === null) {
      return;
    }
    semanticSuggestDismissedQuery = meta.query;
    setSemanticMode("hybrid");
    searchInput = meta.query;
    await runSearch();
  }

  /** UIUX-7/D: 索引ready通知の表示済みフラグ (1回限り表示の担保)。 */
  const SEMANTIC_READY_NOTICE_KEY = "gento.semanticReadyNotice.v1";

  /**
    * UIUX-7/D: 索引準備完了トースト。vector-index:readyイベント到着時と
    * (イベントが購読前に発火済みの場合の) 現在状態判定 (WP3) の両経路から
    * 呼ばれる — localStorageフラグでアプリ生涯1回限りとする。$effect内で
    * なく明示的操作 (Svelte 5: effect内read+writeは無限ループ —
    * toast-queue.tsのWP10契約と同一理由)。
    */
  function notifySemanticReadyOnce(): void {
    if (localStorage.getItem(SEMANTIC_READY_NOTICE_KEY) !== null) {
      return;
    }
    localStorage.setItem(SEMANTIC_READY_NOTICE_KEY, "1");
    pushToast(
      "os",
      "自然言語検索の準備ができました — 検索バーのモードを「ハイブリッド/意味」に切り替えると使えます",
    );
  }

  /** 埋め込み有効化・索引状態の再取得 (起動時・索引readyイベント後)。 */
  async function refreshSemanticState(): Promise<void> {
    try {
      const [status, embedding] = await Promise.all([
        vectorIndex.status(),
        embeddingProbe.status(),
      ]);
      vectorIndexStatus = status;
      embeddingEnabled = embedding.enabled;
      guideModel = {
        modelsDir: embedding.modelsDir,
        verifiedCount: embedding.packages.filter((pkg) => pkg.state === "verified").length,
      };
    } catch {
      // 状態取得失敗はモードセグメントdisabled側へ吸収される
    }
    semanticStateTick++;
  }

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

  /** 検索scopeの実態名 (UIUX-3 V-3: 折りたたみbadge用。「現在のフォルダ」は
   * 実際の範囲 (登録ルート全体) と不一致のため使わない)。 */
  function searchScopeLabelOf(scope: { rootId?: string; collectionId?: string }): string {
    if (scope.rootId !== undefined) {
      const root = libraryRoots.find((candidate) => candidate.id === scope.rootId);
      return `登録ルート: ${root?.path ?? scope.rootId}`;
    }
    if (scope.collectionId !== undefined) {
      return `コレクション: ${activeCollection?.name ?? scope.collectionId}`;
    }
    return "ライブラリ全体";
  }

  /** 検索実行 → 結果をカタログのsearchソースへ表示。 */
  async function runSearch(): Promise<void> {
    const scope = resolveSearchScope();
    if (typeof scope === "string") {
      searchNotice = scope;
      return;
    }
    searchNotice = "";
    // UIUX-7: 実行重みはモードから導出 (keyword=0 / hybrid=0.5 / semantic=1)。
    const weight = semanticAvailable() ? MODE_WEIGHT_PERCENT[semanticMode] / 100 : 0;
    const ok = await searchStore.run(searchInput, searchFilters, scope, weight);
    if (!ok) {
      searchNotice = searchStore.lastError ?? "検索に失敗しました";
      return;
    }
    searchActive = true;
    lastSearchMeta = {
      kind: "query",
      query: searchInput,
      scopeLabel: searchScopeLabelOf(scope),
      weightPercent: Math.round(weight * 100),
      filtersActive: filtersActive(searchFilters),
    };
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

  /** 類似画像検索 (WP-12): 対象assetの意味近傍を検索表示する。
   * backendはlibrary全体から近傍を取る (scope引数なし)。 */
  async function runSimilarSearch(assetId: string): Promise<void> {
    searchNotice = "";
    const ok = await searchStore.runSimilar(assetId);
    if (!ok) {
      searchNotice = searchStore.lastError ?? "類似画像検索に失敗しました";
      return;
    }
    searchActive = true;
    lastSearchMeta = {
      kind: "similar",
      query: "",
      scopeLabel: "ライブラリ全体",
      weightPercent: 100,
      filtersActive: false,
    };
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

  // ===== 検索行の折りたたみ (UIUX-3 V-3) =====

  /** 折りたたみ行の要約 (確定済み検索状態から構築 — search-summary参照)。 */
  const collapsedSearchLine = $derived.by(() => {
    void searchEvidenceTick; // SearchStore変化 (running/lastResult) で再評価
    const committed = searchStore.lastResult !== null;
    // UIUX-7: 意味重みのbadgeは「意味 x%」(search-summary) から
    // モードラベル (キーワード/ハイブリッド/意味/類似) へ変更するため、
    // search-summaryへは0を渡して旧badgeを出させずApp側で付ける。
    const summary = collapsedSearchSummary({
      kind: !committed ? "none" : searchStore.lastKind,
      query: searchStore.lastQuery,
      scopeLabel: lastSearchMeta?.scopeLabel ?? null,
      semanticWeightPercent: 0,
      filtersActive: lastSearchMeta?.filtersActive ?? false,
      running: searchStore.running,
      active: searchActive,
    });
    if (!committed || lastSearchMeta === null) {
      return summary;
    }
    return {
      ...summary,
      badges: [...summary.badges, searchBadgeLabel(lastSearchMeta.kind, lastSearchMeta.weightPercent)],
    };
  });

  /** 折りたたみトグル。折りたたみ時はfilters popoverも閉じる (開いたまま畳むと再展開で復活する)。
    * 展開時はDOM挿入後に検索inputへfocus (tick後)。 */
  function toggleSearchExpanded(): void {
    if (searchExpanded) {
      searchFiltersOpen = false;
    }
    searchExpanded = !searchExpanded;
    saveLayout();
    if (searchExpanded) {
      void tick().then(() => {
        document.querySelector<HTMLInputElement>('[data-testid="search-input"]')?.focus();
      });
    }
  }

  /** 折りたたみ行のクリア (✕): 入力を空にし、検索結果表示中なら解除。 */
  function clearSearchFromCollapsed(): void {
    searchInput = "";
    if (searchActive) {
      void exitSearch();
    }
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

  /** 意味スコア (ハイブリッド/類似表示中のみ。WP-12)。 */
  function semanticScoreOf(id: string): number | null {
    if (!searchActive || id === "") {
      return null;
    }
    void searchEvidenceTick;
    const score = searchStore.semantic[id];
    return score === undefined ? null : score;
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

  /**
   * OS連携 (reveal/open/copy-path) の通知: header表示 + 設定時の明示トースト
   * (WP9 — 同一文言の連続設定は$effectミラーで検出できないため)。
   */
  function showOsNotice(text: string): void {
    osNotice = text;
    pushToast("os", text);
  }

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
   * ライブラリ表示 (登録ルート全体) へ切替える共通遷移 (UXLIB-1)。
   * selectFolderの登録済み分岐・LibraryPanel (行クリック/表示ボタン/
   * addRoot後の自動切替)・起動時既定表示が共有する。`folder` は
   * currentFolderへ反映するパス (省略=現状維持。selectFolderの
   * registered分岐と同等の状態遷移)。失敗時はlibraryErrorへ理由を出す。
   */
  async function showLibrary(
    opts: { folder?: string | null; sortOverride?: CatalogSort } = {},
  ): Promise<boolean> {
    osNotice = "";
    collectionNotice = "";
    const ok = await library.useSource("library");
    if (!ok) {
      libraryError = library.lastError ?? "ライブラリ表示へ切替できませんでした";
      return false;
    }
    catalogSource = "library";
    if (opts.folder !== undefined) {
      currentFolder = opts.folder;
    }
    activeCollection = null;
    searchActive = false;
    collectionNotes = {};
    gridSelection = { ids: [], focusIndex: null };
    await applySavedSort(LIBRARY_SORT_KEY, opts.sortOverride);
    await catalog.refreshSummary();
    await catalog.loadPage(0);
    return true;
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
      const ok = await showLibrary({ folder: path, sortOverride });
      if (ok) {
        saveFolderPrefs();
        return true;
      }
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

  /**
   * notice-stripの表示条件。エラー系 (search/folder/collection/fileOpsError/
   * viewerError) は常に生DOMへ (devtest xerr は .toolbar-notice.error の
   * 存在を読む)。viewer/fileOpsの平文noticeはトースト経由のためstripには
   * 含めない。
   */
  const noticeStripActive = $derived(
    searchNotice !== "" ||
      folderNotice !== "" ||
      collectionNotice !== "" ||
      fileOpsError !== null ||
      viewerError !== null ||
      (fileOpsBusy && fileOpsProgress !== null),
  );

  // ===== ヘッダーステータス (UIUX-5/WP4) =====
  // 走査/警告インジケータとヘッダーnoticeを1個のステータスボタン+popoverへ集約。

  let headerStatusOpen = $state(false);

  /** 忙碌色の条件 (スキャン/ハッシュ待ち/索引構築のいずれかが進行中)。 */
  const headerStatusBusy = $derived(
    scanBusy || library.status.pending_hash > 0 || indexProgress !== null,
  );
  /** 警告色の条件 (忙碌色より優先)。 */
  const headerStatusWarn = $derived(library.status.watch_degraded);
  /** popover内に表示すべき行があるか (無い場合は空き状態案内)。 */
  const headerStatusHasRows = $derived(
    headerStatusBusy ||
      headerStatusWarn ||
      metaNotice !== "" ||
      osNotice !== "",
  );

  /** GUIフィルタに未適用の設定があるか (フィルターボタンの強調用)。 */
  const searchFiltersActive = $derived(filtersActive(searchFilters));

  /** 設定済み (ドラフト) フィルタのチップ列挙 (UIUX-5/WP2)。 */
  const searchFilterChipList = $derived(filterChips(searchFilters));

  /** チップ×: 当該フィルタのみ既定値へ戻す (ドラフト更新)。 */
  function removeSearchFilterChip(key: FilterChipKey): void {
    searchFilters = withoutFilter(searchFilters, key);
  }

  // ===== ビューア (スパイク資産・Enter/ダブルクリック/ボタン) =====

  function closeViewer(): void {
    viewerMode = null;
    viewerStore?.dispose();
    compareStore?.dispose();
    viewerStore = null;
    compareStore = null;
  }

  async function openViewerForChoice(index: number): Promise<void> {
    viewerError = null;
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
      // エラー=strip常駐 (永続) + toast (即時)。toastも出す理由: 失敗時も全画面
      // ビューアオーバーレイ (z-100) が残りstripが背後に隠れるため、オーバーレイ上
      // (z-600) に出るtoastで即時フィードバックを保証する (UIUX-6/WP3)。
      // fileOpsErrorと違いtoast併用なのはこのオーバーレイ構造の違いによる。
      viewerError = toErrorMessage(error);
      pushToast("viewer", toErrorMessage(error));
    }
  }

  /** toolbar「開く」ボタン (WP7b抽出): 選択フォーカスがあればビューアへ、
   * 無ければガイドnotice (gridSelectionの正本はApp。noticeはトースト表示)。 */
  function openViewerFromToolbar(): void {
    const focus = gridSelection.focusIndex;
    if (focus !== null) {
      void openViewerForChoice(focus);
    } else {
      showViewerNotice("グリッドで項目を選択してください");
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

  /**
   * モードバーの採用/除外 (UIUX-5/WP6)。対象はセッション現在位置の画像。
   * 編集可否はhandleTriageActionと同一基準 (Library表示 + stable AssetId)。
   * 適用成功時のみ次へ進む — 失敗時は位置を動かさない。
   */
  async function sessionTriage(value: "picked" | "rejected"): Promise<void> {
    const id = session.currentId();
    if (id === null) {
      return;
    }
    if (catalogSource !== "library" || !gridPagerHasId(id)) {
      sessionNotice = "セッション選別はライブラリ表示中の確定済み画像に対して実行します";
      return;
    }
    try {
      await applyCull([id], value);
    } catch (error) {
      sessionNotice = error instanceof Error ? error.message : String(error);
      return;
    }
    sessionMove(1);
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
    // WP9: ミラーだと同一文言の再設定でトーストが出ないため設定時に明示push
    pushToast("meta", text);
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
    void refreshAiRuns(assetId);
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
    // AI生成 (WP-10。stable AssetIdが必要なため Library系表示のみ)
    if (canCollection) {
      actions.push({
        id: "ai-caption-tags",
        label:
          items.length > 1
            ? `AI: タグ・キャプション生成 (${items.length.toLocaleString()}件)`
            : "AI: タグ・キャプション生成",
        run: async () => {
          await enqueueAiForSelection();
        },
      });
    }
    // 類似画像検索 (WP-12。単一assetのdocベクトル近傍 — 常にfocus対象)
    if (canCollection) {
      const similarOk = semanticAvailable();
      actions.push({
        id: "similar-search",
        label: "類似画像を検索",
        disabled: !similarOk,
        title: similarOk ? "この画像に意味的に近い画像を検索します" : semanticReason(),
        run: async () => {
          await runSimilarSearch(focus.id);
        },
      });
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
      // SD学習用エクスポート (WP-14。stable AssetIdが必要なためLibrary系表示のみ)
      actions.push({
        id: "export-sd",
        label:
          items.length > 1
            ? `SD学習用にエクスポート… (${items.length.toLocaleString()}件)`
            : "SD学習用にエクスポート…",
        run: async () => {
          startExportForSelection(items);
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
            showOsNotice(toErrorMessage(error));
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
            showOsNotice(toErrorMessage(error));
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
          showOsNotice(
            ok
              ? `パスをコピーしました (${items.length.toLocaleString()}件)`
              : "クリップボードへのコピーに失敗しました",
          );
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

  /** InfoPanelセクションの折りたたみ切替 (UIUX-5 WP5 — layout-prefsへ永続化)。 */
  function toggleInfoPanelSection(id: string): void {
    infoPanelCollapsed = toggleSection(infoPanelCollapsed, id);
    saveLayout();
  }

  function saveLayout(): void {
    layoutStore.save({
      leftVisible,
      rightVisible,
      leftWidth,
      rightWidth,
      thumbSize: cellClass,
      scrollSpeed,
      leftTab,
      searchExpanded,
      infoPanelCollapsed,
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

    // レイアウト・閲覧状態の復元 (WP-4 / UIUX-1: タブ・速度)
    const layout = layoutStore.load();
    leftVisible = layout.leftVisible;
    rightVisible = layout.rightVisible;
    leftWidth = layout.leftWidth;
    rightWidth = layout.rightWidth;
    cellClass = layout.thumbSize;
    leftTab = layout.leftTab;
    scrollSpeed = layout.scrollSpeed;
    searchExpanded = layout.searchExpanded;
    infoPanelCollapsed = layout.infoPanelCollapsed;
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
    // WP-18 E3: 初回起動ガイド用にAIプロファイル数を一度だけ取得
    // (登録ルート0件の導線表示にのみ使う。失敗時は「状態不明」表示)。
    void aiStore
      .listProfiles()
      .then((profiles) => {
        guideAiProfiles = profiles.length;
      })
      .catch(() => {
        guideAiProfiles = null;
      });
    // WP-12: 意味検索の状態 (埋め込み有効化・索引構築) 追跡
    void refreshSemanticState().then(async () => {
      disposeVectorIndexReady = await vectorIndex.subscribeReady(() => {
        indexProgress = null;
        void refreshSemanticState();
        notifySemanticReadyOnce();
      });
      disposeVectorIndexProgress = await vectorIndex.subscribeProgress((progress) => {
        indexProgress = progress;
        semanticStateTick++;
      });
      // UIUX-7/WP3: 起動時の既存索引ロードでreadyイベントがこの購読より前に
      // 発火済みの場合がある (vectorindex.rs ensure_ready → emit_ready)。
      // 取得済みの現在状態でも通知を判定する (イベント経路と同じ1回限り
      // フラグを通すため二重表示しない)。
      if (
        vectorIndexStatus !== null &&
        vectorIndexStatus.ready &&
        vectorIndexStatus.indexedCount > 0
      ) {
        notifySemanticReadyOnce();
      }
    });
    void catalog.init().then(async (dispose) => {
      disposeCatalog = dispose;
      // 起動既定表示の決定には --dataset (health) と登録ルートの両方が要る
      // (UXLIB-1。onMountで先行発行したrefreshHealthとの二重発行は冪等)。
      await Promise.all([refreshHealth(), library.refreshRoots()]);
      const view = startupCatalogView(
        rawHealth?.dataset_path ?? null,
        rawHealth?.dataset_exists ?? false,
        library.roots.map((root) => root.path),
      );
      if (view.kind === "library") {
        // 登録済みデータの見える化: 空adhocや重複走査ではなくライブラリ
        // 表示で起動する (失敗時はshowLibraryがlibraryErrorへ出す)
        await showLibrary();
        return;
      }
      // 起動時は --dataset (決定論的) を一時走査する。前回フォルダは
      // ツリー選択のハイライトとして復元し、クリックで戻れるようにする
      await catalog.startScan();
      if (view.path !== null) {
        currentFolder = view.path;
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
    disposeVectorIndexReady?.();
    disposeVectorIndexProgress?.();
    disposeFileOpsProgress?.();
    for (const timer of toastTimers.values()) {
      clearTimeout(timer);
    }
    toastTimers.clear();
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

  // ===== 初回起動ガイド (WP-18 E3) =====
  // 登録ルート0件かつグリッド空のとき、空状態に代えて3ステップ導線を表示。
  // AI・モデルの状態は表示用に保持する (導線の「現在の状態」行)。
  let guideAiProfiles = $state<number | null>(null);
  let guideModel = $state<{ modelsDir: string | null; verifiedCount: number | null }>({
    modelsDir: null,
    verifiedCount: null,
  });

  const firstRunSteps = $derived(
    deriveFirstRunGuide({
      rootsCount: libraryRoots.length,
      aiProfilesCount: guideAiProfiles,
      model: guideModel,
    }),
  );

  const firstRunVisible = $derived(
    firstRunSteps !== null &&
      emptyState?.kind === "empty" &&
      !searchActive &&
      activeCollection === null,
  );

  const crumbs = $derived(
    catalogSource === "library" ? [] : breadcrumbOf(currentFolder ?? ""),
  );

  /** UIUX-1 (R-A): メニューバー項目 (状態依存の有効無効はここで毎評価)。 */
  const menus = $derived.by(() => {
    const scanning = summaryNow()?.scanning ?? false;
    const file: MenuDef = {
      id: "file",
      label: "ファイル",
      items: [
        scanning
          ? {
              id: "temp-scan-cancel",
              label: "一時走査を中止",
              title: "実行中の一時走査を中止します (確定済みの結果は残ります)",
              run: () => void cancelScan(),
            }
          : {
              id: "temp-scan",
              label: "一時走査を実行",
              title: "現在の一時走査対象を再走査します",
              run: () => void startScan(),
              disabled: scanBusy,
            },
        {
          id: "export",
          label: "SD学習用エクスポート…",
          title: "表示中コレクション (優先) または選択中の確定済み画像をSD学習用データセットとして書き出します",
          run: () => exportFromMenu(),
          disabled: !menuExportAvailable(),
        },
        { id: "quit", label: "終了", run: () => void quitApp() },
      ],
    };
    const view: MenuDef = {
      id: "view",
      label: "表示",
      items: [
        {
          id: "toggle-left",
          label: "左ペイン",
          title: "フォルダツリー・コレクション・登録ルートの表示切替",
          run: toggleLeft,
          checked: leftVisible,
        },
        {
          id: "toggle-right",
          label: "右ペイン",
          title: "情報パネルの表示切替",
          run: toggleRight,
          checked: rightVisible,
        },
        {
          id: "size-256",
          label: "サムネイルサイズ: 標準",
          run: () => setCellClass("s256"),
          checked: cellClass === "s256",
        },
        {
          id: "size-512",
          label: "サムネイルサイズ: 大",
          run: () => setCellClass("s512"),
          checked: cellClass === "s512",
        },
      ],
    };
    const tools: MenuDef = {
      id: "tools",
      label: "ツール",
      items: [
        {
          id: "ai",
          label: "AIパネル…",
          title: "AIプロファイル・ジョブキュー・自動処理設定",
          run: () => {
            aiPanelOpen = true;
          },
        },
        {
          id: "settings",
          label: "設定…",
          title: "設定・診断",
          run: () => void openSettings(),
          testId: "settings-button",
        },
        {
          id: "backup-root",
          label: "ルートのバックアップ…",
          title: "登録ルート単位の.gentoバックアップを作成します",
          run: backupRootFromMenu,
          disabled: libraryRoots.length === 0,
        },
        {
          id: "backup-db",
          label: "全データベースをバックアップ",
          title: "DB全体を単一SQLiteファイルへバックアップ (世代保持)",
          run: () => void runFullDbBackup(),
          disabled: fullDbBusy,
        },
        {
          id: "restore",
          label: "バックアップから復元…",
          title: ".gentoバックアップからメタデータを復元",
          run: () => {
            restoreOpen = true;
          },
        },
        ...(devBridgeActive
          ? [
              {
                id: "dev",
                label: "開発パネル",
                run: () => {
                  devPanelOpen = !devPanelOpen;
                },
              } satisfies MenuAction,
            ]
          : []),
      ],
    };
    const help: MenuDef = {
      id: "help",
      label: "ヘルプ",
      items: [
        {
          id: "about",
          label: "このアプリについて",
          title: "設定ダイアログの「このアプリについて」を開きます",
          run: () => void openSettings("about"),
        },
      ],
    };
    return [file, view, tools, help];
  });

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
  <AppHeader
    {healthStatus}
    {rawHealth}
    {catalogSource}
    {searchActive}
    {headerStatusOpen}
    {headerStatusBusy}
    {headerStatusWarn}
    {headerStatusHasRows}
    {scanBusy}
    {progress}
    {summary}
    pendingHash={library.status.pending_hash}
    watchDegraded={library.status.watch_degraded}
    {indexProgress}
    {metaNotice}
    {osNotice}
    onToggleStatus={() => (headerStatusOpen = !headerStatusOpen)}
    onCloseStatus={() => (headerStatusOpen = false)}
  />

  <MenuBar {menus} />

  <div class="app-panes">
    {#if leftVisible}
      <aside class="side-pane left" style="width:{leftWidth}px">
        <!-- UIUX-1 (R-B): 3パネルは全マウント維持。非アクティブはhidden属性で
             アクセシブルに隠蔽 (ドラフト入力がタブ切替で失われない)。 -->
        <LeftPaneTabs active={leftTab} onChange={setLeftTab} />
        <div
          class="left-tabpanel"
          id="left-panel-folders"
          role="tabpanel"
          aria-labelledby="left-tab-folders"
          tabindex="-1"
          hidden={leftTab !== "folders"}
        >
          <FolderTree
            {tree}
            roots={libraryRoots}
            {currentFolder}
            libraryMode={catalogSource === "library"}
            onSelect={(path) => void selectFolder(path)}
            onDrop={handleTreeDrop}
            onRegisterRoot={(path) => void registerRootFromTree(path)}
            onUnregisterRoot={(rootId) => void unregisterRootFromTree(rootId)}
            onBackupRoot={(root) => (backupFor = root)}
            onRescan={() => void rescanLibraryFromTree()}
          />
        </div>
        <div
          class="left-tabpanel"
          id="left-panel-collections"
          role="tabpanel"
          aria-labelledby="left-tab-collections"
          tabindex="-1"
          hidden={leftTab !== "collections"}
        >
          <CollectionsPane
            store={collectionsStore}
            activeCollectionId={activeCollection?.id ?? null}
            onSelect={(collection) => void showCollection(collection)}
          />
        </div>
        <div
          class="left-tabpanel"
          id="left-panel-library"
          role="tabpanel"
          aria-labelledby="left-tab-library"
          tabindex="-1"
          hidden={leftTab !== "library"}
        >
          <LibraryPanel
            {library}
            roots={libraryRoots}
            notice={libraryNotice}
            error={libraryError}
            scanBusy={libraryScanBusy}
            confirmBusy={registerConfirmPath !== null}
            onRefresh={() => void library.refreshRoots()}
            onShowLibrary={() => showLibrary()}
            onBackupRoot={(root) => (backupFor = root)}
            onAddRoot={requestAddRoot}
          />
        </div>
      </aside>
      <div
        class="pane-resizer"
        role="separator"
        aria-orientation="vertical"
        onpointerdown={(event) => startResize("left", event)}
      ></div>
    {/if}

    <section class="center-pane">
      <!-- UIUX-5/WP7b: toolbar抽出。DOM・class・testidはApp.svelte時代と
           不変 (状態正本はApp — props/callbackでのみ授受)。 -->
      <Toolbar
        {searchActive}
        searchRunning={searchStore.running}
        searchKind={searchStore.lastKind}
        searchResultTotal={searchStore.lastResult?.total ?? 0}
        searchResultElapsedMs={searchStore.lastResult?.elapsedMs ?? 0}
        onRefreshSearch={refreshSearch}
        onExitSearch={exitSearch}
        {activeCollection}
        onExitCollection={exitCollection}
        {catalogSource}
        {crumbs}
        onSelectFolder={selectFolder}
        {pathInputOpen}
        {pathInputValue}
        onOpenPathInput={openPathInput}
        onPathInput={(value) => (pathInputValue = value)}
        onCommitPathInput={commitPathInput}
        onCancelPathInput={cancelPathInput}
        {sortValue}
        {onSortChange}
        {collectionToolsOpen}
        onToggleCollectionTools={() => (collectionToolsOpen = !collectionToolsOpen)}
        onCloseCollectionTools={() => (collectionToolsOpen = false)}
        {collectionBusy}
        selectionCount={gridSelection.ids.length}
        {collectionPositionInput}
        onCollectionPositionInput={(value) => (collectionPositionInput = value)}
        onMoveSelectedInCollection={moveSelectedInCollection}
        onRemoveSelectedFromCollection={removeSelectedFromCollection}
        onRefreshActiveSmartCollection={refreshActiveSmartCollection}
        {fileOpsBusy}
        onStartNewFolder={startNewFolder}
        sizeValue={sizeSliderValue()}
        {onSizeSlider}
        {viewerChoice}
        onViewerChoice={(choice) => (viewerChoice = choice)}
        onOpenViewer={openViewerFromToolbar}
      />
      <!-- UIUX-5/WP7b: search-bar抽出。DOM・class・testidはApp.svelte時代と
           不変 (data-testid="search-input"はsearch-bar内 — devtest srFocusInput契約)。 -->
      <SearchBar
        {searchExpanded}
        onToggleSearchExpanded={toggleSearchExpanded}
        {collapsedSearchLine}
        onClearSearchFromCollapsed={clearSearchFromCollapsed}
        searchRunning={searchStore.running}
        {searchInput}
        onSearchInput={(value) => (searchInput = value)}
        onRunSearch={() => void runSearch()}
        {searchScopeMode}
        onSearchScopeChange={(mode) => (searchScopeMode = mode)}
        semanticAvailable={semanticAvailable()}
        semanticReason={semanticReason()}
        {semanticMode}
        onSemanticModeChange={setSemanticMode}
        showSemanticSuggest={semanticSuggestVisible}
        onSemanticSuggestClick={() => void trySemanticSearch()}
        {searchFiltersOpen}
        onToggleSearchFilters={() => (searchFiltersOpen = !searchFiltersOpen)}
        onCloseSearchFilters={() => (searchFiltersOpen = false)}
        {searchFiltersActive}
        searchWarnings={searchStore.lastResult?.warnings ?? []}
        canSaveSearch={searchStore.lastResult !== null}
        onSaveCurrentSearch={() => void saveCurrentSearch()}
        {searchFilters}
        onSearchFiltersChange={(filters) => (searchFilters = filters)}
        {searchFilterChipList}
        onRemoveSearchFilterChip={removeSearchFilterChip}
      />
      {#if noticeStripActive}
        <!-- UIUX-5/WP4: 6種のnotice行を1行stripへ集約 (エラー最優先)。各要素の
             class・testid・roleは集約前そのまま — 活性エラーは常に生DOMに存在
             する契約 (devtest xerr)。viewer/fileOpsの平文noticeはトースト経由。 -->
        <NoticeStrip
          {searchNotice}
          {folderNotice}
          {collectionNotice}
          {viewerError}
          {fileOpsError}
          {fileOpsBusy}
          {fileOpsProgress}
          {conflictOf}
          onCancelFileOps={() => cancelFileOps(fileOpsIpc)}
        />
      {/if}

      {#if sessionSnapshot.active}
        <!-- UIUX-5/WP6: 選別セッションのモードバー。InfoPanelを開かなくても選別操作
             できるようにする。次へボタンを「次へ →」で始めるとdevtest sessionNext
             (clickButtonは文書順最初のprefix一致) がInfoPanel側を押せなくなるため
             「次へ ▶」としている。 -->
        <SessionModeBar
          {sessionCounts}
          {sessionNotice}
          onSessionMove={sessionMove}
          onSessionTriage={sessionTriage}
          onEndSession={endSession}
        />
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
            scrollSpeed={scrollSpeed}
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
        {#if firstRunVisible}
          <FirstRunGuide steps={firstRunSteps!} />
        {:else if emptyState !== null}
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
          semanticScore={semanticScoreOf(focusInfo?.id ?? "")}
          semanticLabel={searchStore.lastKind === "similar" ? "類似度" : "意味"}
          canSimilarSearch={focusInfo !== null && focusInfo.dbState === "asset" && semanticAvailable()}
          onSimilarSearch={() => {
            const item = focusInfo;
            if (item !== null) {
              void runSimilarSearch(item.id);
            }
          }}
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
          aiRuns={focusInfo !== null && metaDetail?.id === focusInfo.id ? aiRuns : []}
          onEnqueueAi={() => void enqueueAiForSelection()}
          collapsedSections={infoPanelCollapsed}
          onToggleSection={toggleInfoPanelSection}
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
      getGridStats={() => gridComponent?.getStatsSnapshot() ?? null}
    />
  {/if}
</div>

{#if aiPanelOpen}
  <AiPanel
    store={aiStore}
    roots={libraryRoots}
    {ipc}
    onClose={() => (aiPanelOpen = false)}
    onJobsChanged={() => {
      // ジョブ完了 → フォーカス項目の表示を更新 (AIバッジ・キャプション)
      const item = focusInfo;
      if (item !== null) {
        void refreshMetaDetail(item.id);
        void refreshAiRuns(item.id);
      }
    }}
  />
{/if}

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
      showFileOpsNotice("操作を中止しました");
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

{#if backupFor !== null}
  <BackupDialog
    root={backupFor}
    roots={libraryRoots}
    onClose={() => (backupFor = null)}
  />
{/if}

{#if registerConfirmPath !== null}
  <ConfirmRegistrationDialog
    path={registerConfirmPath}
    onConfirm={() => registrationGate.dialogConfirmed(addRootConfirmed)}
    onCancel={() => registrationGate.dialogCancelled()}
  />
{/if}

{#if restoreOpen}
  <RestoreDialog roots={libraryRoots} onClose={() => (restoreOpen = false)} />
{/if}

{#if settingsOpen}
  <SettingsDialog
    roots={libraryRoots}
    profiles={settingsProfiles}
    displayInfo={settingsDisplayInfo}
    initialTab={settingsInitialTab}
    onClose={() => (settingsOpen = false)}
    onScrollSpeedInput={onScrollSpeedInput}
    onScrollSpeedChange={onScrollSpeedCommit}
    onScrollSpeedReset={onScrollSpeedReset}
  />
{/if}

{#if backupPickOpen}
  <!-- UIUX-1: ツール>ルートのバックアップ…の複数ルート選択 (短命モーダル規約) -->
  <div class="picker-backdrop" role="presentation">
    <div
      class="picker backup-root-picker"
      role="dialog"
      aria-modal="true"
      aria-label="バックアップするルートの選択"
      tabindex="-1"
      use:focusTrap
      onkeydown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          backupPickOpen = false;
        }
      }}
    >
      <h2>バックアップするルートを選択</h2>
      <ul class="backup-root-list">
        {#each libraryRoots as root (root.id)}
          <li>
            <button
              class:unavailable={!root.available}
              onclick={() => {
                backupPickOpen = false;
                backupFor = root;
              }}
            >
              <span class="backup-root-path">{root.path}</span>
              <span class="backup-root-state">
                {root.enabled ? (root.available ? "有効" : "不在") : "無効"}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      <div class="picker-actions">
        <button onclick={() => (backupPickOpen = false)}>キャンセル</button>
      </div>
    </div>
  </div>
{/if}

{#if exportFor !== null}
  <ExportDialog
    assetIds={exportFor.assetIds}
    sourceLabel={exportFor.sourceLabel}
    roots={libraryRoots}
    onClose={() => (exportFor = null)}
  />
{/if}

{#if retryReport !== null}
  <div class="retry-backdrop" role="presentation">
    <div
      class="retry"
      role="dialog"
      aria-modal="true"
      aria-label="一部のファイルで失敗"
      tabindex="-1"
      use:focusTrap
      onkeydown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          retryReport = null;
        }
      }}
    >
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

<!-- UIUX-5/WP4: 揮発性noticeのトースト (画面右下・4秒自動消滅)。fileops系は
     元noticeのtestid・roleを引き継ぐ (可視中のみDOM)。ビューア表示中も出す
     ためルート階層に置く。 -->
<ToastStack toasts={toastQueue.toasts} onDismissToast={dismissToast} />

<style>
  .app-shell {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
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
    border-right: 1px solid var(--color-border-strong);
    font-size: 0.875rem;
  }

  /* UIUX-1 (R-B): タブパネル。hidden属性でアクセシブルに隠蔽しつつ
   * マウント維持 (ドラフト入力の生存)。パネル毎に独立スクロール。 */
  .left-tabpanel {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    outline: none;
  }

  /* フォルダタブはツリー自身が内部スクロールを持つため
   * 二重スクロールを避ける (collapsible treeの既存動作維持)。 */
  #left-panel-folders {
    overflow: hidden;
  }

  .left-tabpanel[hidden] {
    display: none;
  }

  .side-pane.right {
    border-left: 1px solid var(--color-border-strong);
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

  /* UIUX-1: ツール>ルートのバックアップ… の選択ダイアログ
   * (他ダイアログと同じpicker様式 — SettingsDialog等と共通の見た目) */
  .picker-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 60;
  }

  .backup-root-picker {
    display: flex;
    flex-direction: column;
    gap: var(--space-50);
    width: min(40rem, 92vw);
    max-height: 70vh;
    overflow-y: auto;
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-xl);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: var(--space-90) var(--space-100);
    font-size: var(--font-size-85);
  }

  .backup-root-picker h2 {
    font-size: var(--font-size-100);
    margin: 0;
  }

  .backup-root-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-25);
  }

  .backup-root-list button {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-15);
    padding: 0.3rem 0.4rem;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .backup-root-list button:hover {
    background: var(--color-bg-subtle);
  }

  .backup-root-list button.unavailable {
    border-color: var(--color-attention-border);
  }

  .backup-root-path {
    font-family: monospace;
    font-size: var(--font-size-75);
    overflow-wrap: anywhere;
  }

  .backup-root-state {
    font-size: var(--font-size-70);
    color: var(--color-text-muted);
  }

  .backup-root-picker .picker-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-40);
  }

  button {
    font-size: var(--font-size-80);
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
    gap: var(--space-40);
    pointer-events: none;
    text-align: center;
    padding: 2rem;
  }

  .empty-message {
    margin: 0;
    font-size: var(--font-size-100);
    color: var(--color-text-muted);
  }

  .empty-detail {
    margin: 0;
    font-size: var(--font-size-80);
    color: var(--color-text-subtle);
    font-family: monospace;
    overflow-wrap: anywhere;
  }

  .status-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--space-75);
    padding: 0.25rem 0.75rem;
    border-top: 1px solid var(--color-border-strong);
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
  }

  .status-error {
    color: var(--color-danger);
  }

  .status-spacer {
    flex: 1;
  }

  .status-hint {
    color: var(--color-text-subtle);
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
    gap: var(--space-50);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-xl);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: var(--space-90) var(--space-100);
    font-size: var(--font-size-85);
  }

  .retry h2 {
    font-size: var(--font-size-100);
    margin: 0;
  }

  .retry-list {
    margin: 0;
    padding: 0 0 0 1.2rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-25);
    font-size: var(--font-size-80);
  }

  .retry-list li {
    display: flex;
    flex-direction: column;
  }

  .retry-path {
    overflow-wrap: anywhere;
  }

  .retry-error {
    color: var(--color-danger);
    overflow-wrap: anywhere;
  }

  .retry-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-40);
  }

  .retry-actions .primary {
    background: var(--color-accent-emphasis);
    color: #ffffff;
    border: none;
    border-radius: var(--radius-sm);
    padding: var(--space-30) var(--space-80);
    cursor: pointer;
  }
</style>
