/**
 * 実機検証用の開発限定ブリッジ (WP-6 + WP-7計測拡張)。
 *
 * 目的: 検証環境のデスクトップがセッション共有でポインタ自動操作
 * (xdotool mousemove/click) の到達が安定しないため、キーボード (F1〜F12)
 * から本物のDOMイベントを生成して生産コードパスを駆動し、
 * 機械可読な状態リードアウト (画面右上) で数値を実測する。
 *
 * - このモジュールは常にバンドルに含まれるが、**動的import + ランタイム
 *   フラグ**で起動時にのみ読み込まれる: dev build (import.meta.env.DEV)、
 *   またはdevtoolsビルドのreleaseで Rust側 `dev_bridge_enabled` がOkを返す場合
 *   (環境変数 `GENTO_DEV_BRIDGE` の値が**厳密に "1"** のときのみ有効。
 *   `0`・`true`・空文字では無効)。通常releaseビルドではbackend側に
 *   `dev_bridge_enabled` / `devtest_report` / `set_failure_injection` 等の
 *   command自体がコンパイルされておらず (WP-0のdevtools feature隔離)、
 *   invokeはrejectされるため本ブリッジは読み込まれない (安全なno-op)。
 *   読み込まれた後も各invokeは `.catch` で黙殺する。
 * - wheel/pointerイベントは実リスナー (store.zoomAt / dragBy) を通る。
 *   setPointerCaptureだけは合成pointerIdでNotFoundになるため
 *   操作中のみ一時的にパッチする (本来の呼び出しは維持)
 * - DEVUX-1: 画面右上の読み出しpreは**既定で非表示** (ヘッダーとの重なりが
 *   製品UIの視認を妨げるため)。ctrl+shift+F12 → m (readoutToggle) でトグル、
 *   選択はlocalStorageへ永続化。ログ送出 (reportCompact) とは独立に常時継続
 * - WP-7: グリッド/統計/パフォーマンス行 (G/T/P) を追加し、
 *   `devtest:` ログ行 (Rust経由) と document.title を機械可読チャネルとして
 *   1秒周期で送出する (bash側はログをgrepして数値を採取)
 */

import {
  DEVTEST_READOUT_KEY,
  parseDevReadoutVisible,
  toggleDevReadout,
} from "./devtest-readout";

export type DevBridgeStores = {
  getViewer: () => unknown;
  getCompare: () => unknown;
  getMode: () => string | null;
  /** WP-7: グリッド状態 (需要・キャンセル統計)。 */
  getGrid: () => GridLike | null;
  /** WP-7: サムネイル統計ストア (1秒ポーリング値)。 */
  getThumb: () => ThumbLike | null;
  /** WP-7: パフォーマンス計測。 */
  getPerf: () => PerfLike;
  /** WP-7: グリッドコンポーネント (focus/jumpTo)。 */
  getGridComponent: () => GridComponentLike | null;
  /** WP-7: App側の選択スナップショット (計測・診断用)。 */
  getSelection: () => { ids: string[]; focusIndex: number | null } | null;
  /** QA-WP4: 選別セッションストア (再開検証の実機E2E用)。 */
  getSession: () => SessionLike | null;
  /** QA-WP4: セッション通知文言 (再開中止警告の検証用)。 */
  getSessionNotice: () => string;
  /** WP-5: メタデータ編集状態 (InfoPanel詳細・undo・高速選別)。 */
  getMeta: () => MetaLike | null;
  /** WP-6: コレクション状態 (E2E検証用)。 */
  getCollections: () => CollectionsLike | null;
  /** WP-7(MVP): ファイル操作状態 (E2E検証用)。 */
  getFileOps: () => FileOpsLike | null;
  /** WP-8: 検索状態 (E2E検証用)。lastResultの件数・警告と一致根拠保持数)。 */
  getSearch: () => {
    lastResult: {
      total: number;
      shown: number;
      elapsedMs: number;
      warnings: string[];
      queryJson: string;
    } | null;
    lastQuery: string;
    running: boolean;
    evidenceCount: number;
    /** 保存 (スマートコレクション作成)。E2Eから固定名で呼ぶ。 */
    save: (name: string) => Promise<boolean>;
    /** 表示中検索の再実行 (search_refresh)。 */
    refresh: () => Promise<boolean>;
  } | null;
  /** WP-7: invoke (エラー注入など)。App.svelteのcore invokeを流用。 */
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** WP-7(MVP): ファイル操作状態の読み出し口 (E2E検証用)。 */
export type FileOpsLike = {
  /** 実行中か。 */
  busy: boolean;
  /** 進捗 (index/total)。null=非実行。 */
  progress: { index: number; total: number } | null;
  /** 直近の通知文。 */
  notice: string;
  /** 直近のエラー。 */
  error: string | null;
  /** 宛先ダイアログ表示中か。 */
  destPicker: boolean;
  /** 衝突ダイアログ表示中か。 */
  conflict: boolean;
  /** 完全削除確認表示中か。 */
  forever: boolean;
  /** 再試行レポート表示中か。 */
  retry: boolean;
  /** リネーム入力表示中か。 */
  renaming: boolean;
  /** Undoスタック (混在履歴の深度・ラベル)。 */
  undoDepth: number;
  undoLabel: string | null;
};

/** WP-6: コレクション状態の読み出し口 (E2E検証用)。 */
export type CollectionsLike = {
  /** 表示中コレクション (null=非表示)。 */
  active: { id: string; kind: string; name: string; itemCount: number } | null;
  /** 一覧 (id昇順 = 作成順)。 */
  list: Array<{ id: string; kind: string; name: string; itemCount: number }>;
  /** 表示中フォーカス項目のコレクション内メモ (未編集・対象外はnull)。 */
  focusMemo: string | null;
};

/** WP-5: メタデータ編集状態の読み出し口 (E2E検証用)。 */
export type MetaLike = {
  /** フォーカス項目のasset_detail応答 (未取得・編集不可はnull)。 */
  detail: {
    id: string;
    rating: number;
    triage: string;
    favorite: boolean;
    tags: Array<{ name: string; manually_edited: boolean }>;
    captions: Array<{ kind: string; body: string; manually_edited: boolean }>;
  } | null;
  /** 高速選別モード (ビューア)。 */
  triageMode: boolean;
  /** Undoスタック残数。 */
  undoDepth: number;
  /** フォーカス項目が編集可能 (Library + Asset確定)。 */
  canEdit: boolean;
  /** フォーカス項目のグリッド表示値 (rating/triage/favorite)。 */
  focusItem: {
    id: string;
    rating: number;
    triage: string;
    favorite: boolean;
    dbState: string;
  } | null;
};

type GridLike = {
  summary: { total: number; mode: string; sort?: string; scanning?: boolean } | null;
  stats: {
    requested: number;
    batches: number;
    rejectedFull: number;
    readyReceived: number;
    failedReceived: number;
    cancelled: number;
    cancelBatches: number;
  };
  pager: {
    stats: { fetched: number; errors: number };
    generation: number | null;
    findIndicesByIds(ids: ReadonlySet<string>): Map<string, number>;
  };
  thumbUrlFor(id: string): string | null;
  getItem(index: number): {
    id: string;
    displayName: string;
    triage?: string;
    rating?: number;
    favorite?: boolean;
  } | null;
};

type SessionLike = {
  active: boolean;
  session: {
    ids: string[];
    position: number;
    origin: string;
  } | null;
  currentId(): string | null;
};

type ThumbLike = {
  stats: {
    generated: number;
    hits: number;
    misses: number;
    hit_rate: number | null;
    failures: number;
    queue_depth: number;
    queue_depths: {
      interactive: number;
      viewport: number;
      prefetch: number;
      overscan: number;
      background: number;
    };
    p50_ms: number | null;
    p95_ms: number | null;
    p99_ms: number | null;
    /** devtoolsビルドのみbackendが返す (WP-0)。 */
    injection_mode?: string;
    injection_rate?: number;
    proto_requests: number;
    proto_p50_ms: number | null;
    proto_p95_ms: number | null;
    proto_p99_ms: number | null;
    cache_bytes: number;
    evictions: number;
    paused: boolean;
  } | null;
};

type PerfLike = {
  longTasks: number;
  worstLongTaskMs: number | null;
  longFrames: number;
  worstLongFrameMs: number | null;
  longTaskObserverSupported: boolean | null;
  marks: { mountedAt: number | null; healthOkAt: number | null; firstPageAt: number | null };
  startupDurations(): { mountToHealthMs: number | null; mountToFirstPageMs: number | null };
};

type GridComponentLike = {
  focus(): void;
  jumpTo(fraction: number): void;
};

type ViewerLike = {
  displayIndex: number;
  current: { assetId: string | null; width: number; height: number; url: string | null; isOriginal: boolean } | null;
  retained: { assetId: string | null } | null;
  transform: { scale: number; centerX: number; centerY: number; mode: string };
  viewport: { width: number; height: number };
  prefetchEnabled: boolean;
  generation: number;
  stats: { applied: number; staleDropped: number; requests: number; prefetchRequests: number; prefetchReady: number };
  prepOn: { stats(): { count: number; p50: number | null; p95: number | null; p99: number | null } };
  prepOff: { stats(): { count: number; p50: number | null; p95: number | null; p99: number | null } };
  zoomFactor(): number;
};

type CompareLike = {
  count: number;
  active: number;
  baseSlot: number | null;
  nextIndex: number;
  syncEnabled: boolean;
  sync: { centerX: number; centerY: number; zoomFactor: number };
  slots: Array<{
    assetId: string | null;
    width: number;
    height: number;
    url: string | null;
    isOriginal: boolean;
    detached: boolean;
    retainedUrl: string | null;
  }>;
  stats: { groups: number; swaps: number; requests: number };
  frames: { stats(): { count: number; p50: number | null; p95: number | null; max: number | null } };
  effectiveTransform(index: number): { scale: number; centerX: number; centerY: number };
};

function n(value: number | null | undefined, digits = 2): string {
  return typeof value === "number" ? value.toFixed(digits) : "-1";
}

function toBridgeMessage(value: unknown): string {
  return value instanceof Error ? `${value.message}` : String(value);
}

function scheme(url: string | null): string {
  if (url === null) {
    return "none";
  }
  const idx = url.indexOf("://");
  return idx > 0 ? url.slice(idx + 3, idx + 3 + 7) : "?";
}

function findButton(prefix: string): HTMLButtonElement | null {
  for (const button of Array.from(document.querySelectorAll("button"))) {
    if ((button.textContent ?? "").trim().startsWith(prefix)) {
      return button;
    }
  }
  return null;
}

function clickButton(prefix: string): void {
  findButton(prefix)?.click();
}

// ===== UIUX-1 E2E: メニューバー・左ペインタブ駆動 =====

/** 左ペインのタブを切替 (全パネル常駐のためhidden要素もclickで駆動可能)。 */
function clickLeftTab(id: "folders" | "collections" | "library"): void {
  document.querySelector<HTMLButtonElement>(`#left-tab-${id}`)?.click();
}

/** メニューバーの topLabel を開き itemPrefix で始まる項目をクリック (生産経路)。
 * (チェック付き項目はテキスト先頭に✓が付くため除去してから照合。
 *  既に開いているメニューをclickするとトグルで閉じるため、開いていない
 *  ときだけトップボタンをクリックする) */
function runMenuItem(topLabel: string, itemPrefix: string): void {
  const top = Array.from(
    document.querySelectorAll<HTMLButtonElement>(".menubar-button"),
  ).find((b) => (b.textContent ?? "").trim() === topLabel);
  if (top === undefined) {
    return;
  }
  if (top.getAttribute("aria-expanded") !== "true") {
    top.click();
  }
  setTimeout(() => {
    const item = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".menu-popover button"),
    ).find((b) => (b.textContent ?? "").replace("✓", "").trim().startsWith(itemPrefix));
    item?.click();
  }, 150);
}

/** 開いているメニューバーのメニューを閉じる (トリガ再クリック=トグル)。 */
function closeMenuBar(): void {
  document
    .querySelectorAll<HTMLButtonElement>(".menubar-button[aria-expanded='true']")
    .forEach((button) => button.click());
}

/** メニューバーの topLabel を開く (項目クリックはしない)。 */
function runMenuOpen(topLabel: string): void {
  const top = Array.from(
    document.querySelectorAll<HTMLButtonElement>(".menubar-button"),
  ).find((b) => (b.textContent ?? "").trim() === topLabel);
  top?.click();
}

/** openFirstHomeChild本体 (UIUX-1: フォルダタブ切替後に呼ばれる)。 */
function openFirstHomeChildNow(): void {
  const labels = Array.from(document.querySelectorAll<HTMLElement>(".tree-label"));
  const home = labels[0]?.title;
  if (home === undefined) {
    return;
  }
  const childPrefix = `${home}/`;
  const clickChild = (): void => {
    const rows = Array.from(document.querySelectorAll<HTMLElement>(".tree-label"));
    rows
      .slice(1)
      .find((label) => label.title.startsWith(childPrefix))
      ?.click();
  };
  const already = labels.slice(1).some((label) => label.title.startsWith(childPrefix));
  if (already) {
    clickChild();
    return;
  }
  // 折りたたみ中: 展開して子の遅延ロード後にクリック (冪等: 再実行は
  // already経路を通る)
  labels[0]
    .closest(".tree-row")
    ?.querySelector<HTMLButtonElement>(".tree-expander")
    ?.click();
  setTimeout(clickChild, 700);
}

/** 高速選別キー相当のkeydownをwindowへ合成 (ビューアのsvelte:windowリスナ経由)。 */
function pressTriageKey(key: string): void {
  window.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
}

function cellAt(index: number): HTMLElement | null {
  return document.querySelector(`[data-grid-cell][data-index="${index}"]`);
}

function clickCell(index: number, ctrl = false): void {
  const cell = cellAt(index);
  if (cell === null) {
    return;
  }
  if (!ctrl) {
    // WebKitGTK + Svelte 5委譲イベントでは合成MouseEventが届かない場合が
    // あるため、修飾キー不要なクリックはネイティブ click() を使う
    cell.click();
    return;
  }
  cell.dispatchEvent(
    new MouseEvent("click", {
      bubbles: true,
      cancelable: true,
      composed: true,
      view: window,
      ctrlKey: true,
    }),
  );
}

/** 開いているオーバーレイの操作面 (単一=stage、比較=slot0)。 */
function interactionEl(): HTMLElement | null {
  return (
    document.querySelector(".compare-overlay .slot[data-slot='0']") ??
    document.querySelector(".viewer-stage")
  );
}

function wheelAt(
  el: HTMLElement,
  xFrac: number,
  yFrac: number,
  notches: number,
  delta: number,
  mods: { alt?: boolean; ctrl?: boolean; meta?: boolean } = {},
): void {
  const rect = el.getBoundingClientRect();
  const x = rect.left + rect.width * xFrac;
  const y = rect.top + rect.height * yFrac;
  for (let i = 0; i < notches; i++) {
    el.dispatchEvent(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        deltaY: delta,
        altKey: mods.alt ?? false,
        ctrlKey: mods.ctrl ?? false,
        metaKey: mods.meta ?? false,
      }),
    );
  }
}

/** UIUX-3: ビューアstageへ合成wheel 3ノッチ (modifier指定可)。本物の
 * ImageViewer wheel listenerを通る。 */
function wheelViewerBridge(
  delta: number,
  mods: { alt?: boolean; ctrl?: boolean },
): void {
  const el = document.querySelector<HTMLElement>(".viewer-stage");
  if (el !== null) {
    wheelAt(el, 0.25, 0.35, 3, delta, mods);
  }
}

/**
 * FIX-VIEWERNAV-1: 実クリックのpointer配列を再現するclickヘルパ。
 * 素朴な .click() はpointerdown/upを経由しないため、祖先要素のpointer
 * captureがclickを壊すbugクラス (ビューア送りボタン) を検出できない。
 * 実ブラウザと同じ意味論で合成する: pointerdownで祖先がsetPointerCapture
 * を呼んだ場合はpointerupをcapture要素へリターゲットし、clickはdown/up
 * 両対象の最近共通祖先で発火させる (合成pointerIdへのNotFound回避のため
 * 操作中のみprototypeをpatch — dragSequenceと同じ規約)。
 */
function realClick(target: HTMLElement): void {
  const originalCapture = Element.prototype.setPointerCapture;
  let captureEl: Element | null = null;
  const noteCapture = (el: Element): void => {
    if (captureEl === null) {
      captureEl = el;
    }
  };
  Element.prototype.setPointerCapture = function patch(this: Element, pointerId: number) {
    void pointerId;
    noteCapture(this);
    return undefined;
  } as typeof Element.prototype.setPointerCapture;
  try {
    target.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: 9,
        isPrimary: true,
        button: 0,
        buttons: 1,
      }),
    );
    const upTarget = captureEl ?? target;
    upTarget.dispatchEvent(
      new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        composed: true,
        pointerId: 9,
        isPrimary: true,
        button: 0,
        buttons: 0,
      }),
    );
    // click対象 = down/up対象の最近共通祖先 (実ブラウザの決定規則)
    const bChain = new Set<Node>();
    for (let n: Node | null = upTarget; n !== null; n = n.parentNode) {
      bChain.add(n);
    }
    let clickTarget: Node = document.body;
    for (let n: Node | null = target; n !== null; n = n.parentNode) {
      if (bChain.has(n)) {
        clickTarget = n;
        break;
      }
    }
    clickTarget.dispatchEvent(
      new MouseEvent("click", {
        bubbles: true,
        cancelable: true,
        composed: true,
        view: window,
        button: 0,
      }),
    );
  } finally {
    Element.prototype.setPointerCapture = originalCapture;
  }
}

/** rAF間隔でpointerdown→移動→up。同期パンのフレーム計測ループを real frames で駆動する。 */
async function dragSequence(
  el: HTMLElement,
  xFrac: number,
  yFrac: number,
  dxTotal: number,
  dyTotal: number,
  steps: number,
): Promise<void> {
  const rect = el.getBoundingClientRect();
  const startX = rect.left + rect.width * xFrac;
  const startY = rect.top + rect.height * yFrac;
  const originalCapture = Element.prototype.setPointerCapture;
  let captured = 0;
  // 合成pointerIdに対するsetPointerCaptureのNotFoundError回避 (操作中のみ)
  Element.prototype.setPointerCapture = function patch(pointerId: number) {
    captured = pointerId;
    return undefined;
  } as typeof Element.prototype.setPointerCapture;
  try {
    el.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        pointerId: 7,
        isPrimary: true,
        button: 0,
        buttons: 1,
        clientX: startX,
        clientY: startY,
      }),
    );
    for (let i = 1; i <= steps; i++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      el.dispatchEvent(
        new PointerEvent("pointermove", {
          bubbles: true,
          cancelable: true,
          pointerId: 7,
          isPrimary: true,
          buttons: 1,
          clientX: startX + (dxTotal * i) / steps,
          clientY: startY + (dyTotal * i) / steps,
        }),
      );
    }
    el.dispatchEvent(
      new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        pointerId: 7,
        isPrimary: true,
        buttons: 0,
        clientX: startX + dxTotal,
        clientY: startY + dyTotal,
      }),
    );
  } finally {
    Element.prototype.setPointerCapture = originalCapture;
    void captured;
  }
}

export function installDevBridge(stores: DevBridgeStores): void {
  if (typeof window === "undefined" || "__gentoTest" in window) {
    return;
  }

  // FIXLIB-1: WebView内の未捕捉JS例外をdevtestチャネルへ露出する
  // (レンダリング効果のクラッシュはUI側で静かに呑まれるため)。
  let jsErrorCount = 0;
  let jsErrorLast = "";
  const noteJsError = (message: string): void => {
    jsErrorCount++;
    jsErrorLast = message.slice(0, 200);
  };
  window.addEventListener("error", (event) => {
    const top = event.error instanceof Error ? ` @ ${event.error.stack?.split("\n")[1]?.trim() ?? ""}` : "";
    noteJsError(`${event.message}${top}`);
  });
  window.addEventListener("unhandledrejection", (event) => {
    noteJsError(`unhandled: ${toBridgeMessage(event.reason)}`);
  });

  const readout = document.createElement("pre");
  readout.id = "devtest-readout";
  readout.style.cssText =
    "position:fixed;right:8px;top:8px;z-index:9999;pointer-events:none;" +
    "margin:0;padding:6px 10px;background:#000d;color:#7fe7a0;" +
    "font:13px/1.35 monospace;white-space:pre;border-radius:6px;";
  // DEVUX-1: 既定非表示。永続化された選択のみ復元 (localStorage不在環境はOFF)
  let readoutVisible = false;
  try {
    readoutVisible = parseDevReadoutVisible(
      window.localStorage.getItem(DEVTEST_READOUT_KEY),
    );
  } catch {
    // localStorage不在環境では既定OFFのまま
  }
  readout.style.display = readoutVisible ? "" : "none";
  document.body.appendChild(readout);

  const report = (line: string): void => {
    // ログチャネル喪失時でもUI計測 (readout/スクリーンショット) は継続する
    void stores.invoke("devtest_report", { payload: line }).catch(() => undefined);
  };

  let injectionState = 0; // 0=none 1=decode0.30 2=cache_write0.30
  // WP-6 E2E: コレクションコマンドモード (ctrl+shift+F1でトグル)
  let collCmdMode = false;
  let foCmdMode = false;
  // UIUX-1 E2E: メニュー/タブ/右クリック/パンくずコマンドモード (shift+F1で入る)
  let uxCmdMode = false;
  // UIUX-3 E2E: ビューアwheel/navコマンドモード (F12素押しで入る。
  // F-key修飾付きは既存WP-5/WP-7ブロックが使用済みのため素F12を採用)
  let vwCmdMode = false;
  // UIUX-3 E2E: 設定/検索折りたたみコマンドモード (素F1で入る。shift+字母は
  // この環境のXephyr/WebKitへ到達しないため小文字専用の別モードを用意)
  let ux3CmdMode = false;

  // UIUX-1 r1 E2E: 非アクティブタブパネル (hidden) 内のfocus試行結果。
  // 0=focus拒否 (hidden実効・正常) / 1=着地 (不良) / -1=未実施 or 対象なし。
  let uxHiddenFocusResult = -1;

  // WP-18-1 D3 E2E: バックアップ/復元の状態行 (foCmdModeの 6/7 で駆動)。
  // 本物のTauri command (BackupDialog/RestoreDialogと同一のinvoke) を
  // 経由するため、作成・復元とも生産経路で実行される。
  const bkState = {
    op: "idle",
    ok: -1,
    assets: -1,
    files: -1,
    colls: -1,
    applied: -1,
    skipped: -1,
    err: "",
  };
  const resetBk = (op: string): void => {
    bkState.op = op;
    bkState.ok = 0;
    bkState.assets = -1;
    bkState.files = -1;
    bkState.colls = -1;
    bkState.applied = -1;
    bkState.skipped = -1;
    bkState.err = "";
  };

  const actions: Record<string, () => void> = {
    // WP-4計測: WebViewのlocalStorage (レイアウト・フォルダ・セッション状態) を
    // 消してリロードする (計測ドライバの初期状態を決定論化)
    resetStorage: () => {
      try {
        for (const key of Object.keys(window.localStorage)) {
          if (key.startsWith("gento:")) {
            window.localStorage.removeItem(key);
          }
        }
      } catch {
        // localStorage不在環境では何もしない
      }
      window.location.reload();
    },
    // DEVUX-1: 画面読み出しの可視性トグル (既定OFF・localStorageへ永続化)。
    // ログ側のreportCompact送出はこのトグルの影響を受けない
    readoutToggle: () => {
      try {
        readoutVisible = toggleDevReadout(window.localStorage);
      } catch {
        // localStorage不在環境では今回のセッション内でのみ反転
        readoutVisible = !readoutVisible;
      }
      readout.style.display = readoutVisible ? "" : "none";
    },
    select2: () => {
      clickCell(0, false);
      clickCell(1, true);
    },
    select3: () => {
      clickCell(0, false);
      clickCell(1, true);
      clickCell(2, true);
    },
    openViewer: () => clickButton("開く"),
    prefetchToggle: () => {
      if (stores.getMode() === "single") {
        clickButton("先読み");
      } else {
        clickButton("同期:");
      }
    },
    resetStats: () => clickButton("統計リセット"),
    cycleChoice: () => {
      const order = ["単一", "2枚比較", "4枚比較"];
      const active = order.findIndex((label) => findButton(label)?.classList.contains("active"));
      clickButton(order[(active + 1) % order.length]);
    },
    scan: () => runMenuItem("ファイル", "一時走査を実行"),
    virtualMode: () => clickButton("仮想カタログ"),
    realMode: () => clickButton("実カタログ"),
    generateAll: () => clickButton("全件生成"),
    generateAllPreviews: () => {
      void stores
        .invoke("generate_all_thumbnails", { sizeClass: "preview2048" })
        .catch(() => undefined);
    },
    clearCache: () => clickButton("キャッシュ全削除"),
    focusGrid: () => stores.getGridComponent()?.focus(),
    jumpStart: () => stores.getGridComponent()?.jumpTo(0),
    jumpMiddle: () => stores.getGridComponent()?.jumpTo(0.5),
    jumpEnd: () => stores.getGridComponent()?.jumpTo(1),
    cycleInjection: () => {
      injectionState = (injectionState + 1) % 3;
      const modes: Array<["none" | "decode" | "cache_write", number]> = [
        ["none", 0],
        ["decode", 0.3],
        ["cache_write", 0.3],
      ];
      const [mode, rate] = modes[injectionState];
      void stores.invoke("set_failure_injection", { mode, rate }).catch(() => undefined);
    },
    // QA-WP4: 選別セッション操作 (実機E2Eの再開シナリオ用)
    sessionStartFolder: () => clickButton("現在の一覧で開始"),
    sessionResume: () => clickButton("再開 ("),
    sessionNext: () => clickButton("次へ →"),
    // QA-WP4: ソート順巡回 (toolbarのselectを本物のchangeイベントで駆動)
    cycleSort: () => {
      const select = document.querySelector<HTMLSelectElement>(".sort-label select");
      if (select === null) {
        return;
      }
      const order = ["name", "date_desc", "size_desc"];
      const next = order[(order.indexOf(select.value) + 1) % order.length] ?? "name";
      select.value = next;
      select.dispatchEvent(new Event("change", { bubbles: true }));
    },
    // QA-WP4: 走査キャンセル (メニューバー ファイル>一時走査を中止)
    cancelScan: () => runMenuItem("ファイル", "一時走査を中止"),
    // WP-8 E2E: 検索バーの駆動 (foCmdModeのh/j)
    srFocusInput: () => {
      const input = document.querySelector<HTMLInputElement>(
        '[data-testid="search-input"]',
      );
      input?.focus();
      input?.select();
    },
    srRun: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="search-run"]')
        ?.click();
    },
    srSave: () => {
      const search = stores.getSearch();
      if (search?.lastResult !== null && search !== null) {
        void search.save("E2E保存検索");
      }
    },
    srRefresh: () => {
      void stores.getSearch()?.refresh();
    },
    // WP-5: メタデータ編集 (InfoPanelの本物のボタン/DOMイベントで駆動)
    metaRate5: () => {
      document.querySelector<HTMLButtonElement>('[data-meta="rating"] button[aria-label="評価 5"]')
        ?.click();
    },
    metaReject: () => {
      const buttons = Array.from(
        document.querySelectorAll<HTMLButtonElement>('[data-meta="cull"] button'),
      );
      buttons.find((b) => (b.textContent ?? "").startsWith("除外"))?.click();
    },
    metaPick: () => {
      const buttons = Array.from(
        document.querySelectorAll<HTMLButtonElement>('[data-meta="cull"] button'),
      );
      buttons.find((b) => (b.textContent ?? "").startsWith("採用"))?.click();
    },
    metaUndo: () => {
      document.querySelector<HTMLButtonElement>('[data-meta="undo"] button')?.click();
    },
    metaBulkConfirm: () => {
      document.querySelector<HTMLButtonElement>(".bulk-confirm button.primary")?.click();
    },
    metaToggleTriage: () => clickButton("高速選別"),
    /** WP-9 E2E: 埋め込みメタデータの折りたたみを全展開
     * (巨大workflowのレンダリング検証。展開で本文明示レンダリング)。 */
    metaExpandAll: () => {
      document
        .querySelectorAll<HTMLButtonElement>(".fold-toggle")
        .forEach((button) => {
          if (button.getAttribute("aria-expanded") === "false") {
            button.click();
          }
        });
    },
    /** WP-9 E2E: workflow JSONのコピーボタンを押下。 */
    metaCopyWorkflow: () => {
      document
        .querySelector<HTMLButtonElement>('[data-meta="workflow-copy"]')
        ?.click();
    },
    /** 高速選別キー相当のkeydownをwindowへ合成 (ビューアのsvelte:windowリスナ経由)。 */
    pressKey0: () => pressTriageKey("0"),
    pressKey3: () => pressTriageKey("3"),
    pressKey5: () => pressTriageKey("5"),
    pressKeyX: () => pressTriageKey("x"),
    pressKeyP: () => pressTriageKey("p"),
    // WP-5 E2E: 実キー入力 (xdotool type) の着地ポイントへフォーカス
    // UIUX-1: LibraryPanelはライブラリタブ内 (タブを切替えてから着地)
    focusLibraryPathInput: () => {
      clickLeftTab("library");
      setTimeout(() => {
        document.querySelector<HTMLInputElement>(".roots-add input")?.focus();
      }, 200);
    },
    focusTagInput: () => {
      document.querySelector<HTMLInputElement>('[data-meta="tags"] input')?.focus();
    },
    focusNoteCaption: () => {
      document.querySelector<HTMLTextAreaElement>('[data-meta="caption-user_note"] textarea')
        ?.focus();
    },
    metaSaveCaption: () => {
      document
        .querySelector<HTMLButtonElement>('[data-meta="caption-user_note"] .caption-controls button')
        ?.click();
    },
    focusCell0: () => clickCell(0),
    focusCell1: () => clickCell(1),
    focusCell2: () => clickCell(2),
    focusCell3: () => clickCell(3),
    // WP-6 E2E: コレクション (本物のDOM要素 → 生産経路で駆動)
    // UIUX-1: CollectionsPaneはコレクションタブ内 (タブ切替後にフォーム操作)
    /** 「+ 手動」フォームを開き名前入力へフォーカス (xdotool typeの着地)。 */
    collCreateManualForm: () => {
      clickLeftTab("collections");
      setTimeout(() => {
        clickButton("+ 手動");
        setTimeout(() => {
          document.querySelector<HTMLInputElement>(".coll-create .coll-name-input")?.focus();
        }, 100);
      }, 150);
    },
    /** 「+ スマート」フォームを開き名前入力へフォーカス (既定条件=評価≥4)。 */
    collCreateSmartForm: () => {
      clickLeftTab("collections");
      setTimeout(() => {
        clickButton("+ スマート");
        setTimeout(() => {
          document.querySelector<HTMLInputElement>(".coll-create .coll-name-input")?.focus();
        }, 100);
      }, 150);
    },
    /** 次のコレクションを表示 (活性の次。非表示時は最初。一覧末尾で先頭へ戻る)。 */
    collShowNext: () => {
      const labels = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".coll-list .coll-label"),
      );
      if (labels.length === 0) {
        return;
      }
      const state = stores.getCollections();
      const activeId = state?.active?.id ?? null;
      const activeIndex =
        activeId === null ? -1 : (state?.list.findIndex((c) => c.id === activeId) ?? -1);
      labels[(activeIndex + 1) % labels.length]?.click();
    },
    /** cell0でコンテキストメニューを開く (コレクションへ追加の導線)。 */
    contextMenuCell0: () => {
      cellAt(0)?.dispatchEvent(
        new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
      );
    },
    /** フォーカス中セルでコンテキストメニューを開く (WP-7 E2E: 選択対象に
     * 操作したいセルがcell0以外の場合)。 */
    contextMenuFocused: () => {
      const sel = stores.getSelection();
      const index = sel?.focusIndex ?? -1;
      if (index < 0) {
        return;
      }
      cellAt(index)?.dispatchEvent(
        new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
      );
    },
    /** 開いたコンテキストメニューの「コレクションへ追加…」をクリック。 */
    clickCollMenuAdd: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").startsWith("コレクションへ追加"));
      item?.click();
    },
    /** ピッカーの最初の既存コレクションへ追加。 */
    pickerPickFirst: () => {
      document.querySelector<HTMLButtonElement>(".picker-row")?.click();
    },
    /** ピッカーの新規コレクション名入力へフォーカス。 */
    focusPickerNewInput: () => {
      document.querySelector<HTMLInputElement>(".picker-new input")?.focus();
    },
    /** 「選択を先頭へ」ボタン (manual表示中の並び替え)。 */
    collMoveFront: () => clickButton("選択を先頭へ"),
    /** 「選択を末尾へ」ボタン。 */
    collMoveBack: () => clickButton("選択を末尾へ"),
    /** スマート「再評価」ボタン。 */
    collRefresh: () => clickButton("再評価"),
    /** コレクション内メモのtextareaへフォーカス。 */
    focusCollectionMemo: () => {
      document.querySelector<HTMLTextAreaElement>('[data-meta="collection-memo"] textarea')
        ?.focus();
    },
    collSaveMemo: () => {
      document
        .querySelector<HTMLButtonElement>('[data-meta="collection-memo"] .caption-controls button')
        ?.click();
    },
    // QA-WP4: ツリー行クリック (xdotoolポインタが環境により不安定なため
    // 本物のtree-labelボタンをDOM clickで駆動。selectFolder→open_folder
    // の生産経路を通る)。UIUX-1: フォルダタブへ切替えてから操作。
    openTreeFsRoot: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        document.querySelector<HTMLButtonElement>('.tree-label[title="/"]')?.click();
      }, 150);
    },
    // WP-7 E2E: ファイル操作 (本物のDOM要素 → 生産経路で駆動)
    /** 宛先ダイアログ: 最初のノード (登録ルート) を選択して確定。 */
    foDestPickFirst: () => {
      document.querySelector<HTMLButtonElement>(".node-label")?.click();
      setTimeout(() => {
        document.querySelector<HTMLButtonElement>(".picker-actions .primary")?.click();
      }, 150);
    },
    /** 宛先ダイアログ: 最初のノードを展開し最初の子を選択して確定。 */
    foDestPickFirstSub: () => {
      document.querySelector<HTMLButtonElement>(".node-toggle")?.click();
      setTimeout(() => {
        document
          .querySelectorAll<HTMLButtonElement>(".node-label")[1]
          ?.click();
      }, 400);
      setTimeout(() => {
        document.querySelector<HTMLButtonElement>(".picker-actions .primary")?.click();
      }, 700);
    },
    foConflictKeepBoth: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="conflict-keep-both"]')
        ?.click();
    },
    foConflictOverwrite: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="conflict-overwrite"]')
        ?.click();
    },
    foConflictSkip: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="conflict-skip"]')
        ?.click();
    },
    foConflictApplyAll: () => {
      (document.querySelector('[data-testid="conflict-apply-all"]') as HTMLInputElement | null)
        ?.click();
    },
    foForeverCheck: () => {
      (document.querySelector('[data-testid="forever-check"]') as HTMLInputElement | null)
        ?.click();
    },
    foForeverConfirm: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="forever-confirm"]')
        ?.click();
    },
    foMenuCopy: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").startsWith("コピー"));
      item?.click();
    },
    foMenuMove: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").startsWith("移動"));
      item?.click();
    },
    foMenuTrash: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").startsWith("ゴミ箱"));
      item?.click();
    },
    /** コンテキストメニュー「リネーム」(F2相当 — dev bridgeがF2を使うため)。 */
    foMenuRename: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").startsWith("リネーム"));
      item?.click();
    },
    foRetryConfirm: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="retry-button"]')
        ?.click();
    },
    /** F2リネーム入力へフォーカス (xdotool typeの着地)。 */
    foFocusRename: () => {
      document.querySelector<HTMLInputElement>('[data-testid="rename-input"]')?.focus();
    },
    /**
     * WP-7 E2E (共有デスクトップでxdotool typeが不安定な環境向け):
     * 登録ルート追加フォームに --dataset のパスを設定して追加
     * (input値設定+inputイベント+追加ボタンclick = 生産経路)。
     * UIUX-1: フォームはライブラリタブ内。
     */
    foAddRootDataset: () => {
      clickLeftTab("library");
      setTimeout(() => {
        void (async () => {
          const health = await stores
            .invoke<{ dataset_path: string | null }>("health")
            .catch(() => null);
          const path = health?.dataset_path ?? null;
          const input = document.querySelector<HTMLInputElement>(".roots-add input");
          if (path === null || path === "" || input === null) {
            return;
          }
          input.value = path;
          input.dispatchEvent(new Event("input", { bubbles: true }));
          setTimeout(() => {
            document
              .querySelectorAll<HTMLButtonElement>(".roots-add button")
              .forEach((b) => {
                if ((b.textContent ?? "").includes("追加")) {
                  b.click();
                }
              });
          }, 100);
        })();
      }, 150);
    },
    /** UXLIB-1 E2E: 登録ルート行の切替ボタンclick (ライブラリ表示 = 生産経路)。 */
    foShowLibraryRow: () => {
      clickLeftTab("library");
      setTimeout(() => {
        document.querySelector<HTMLButtonElement>(".roots-list li .roots-open")?.click();
      }, 150);
    },
    /** UXLIB-1 E2E: 「ライブラリを表示」ボタンclick (生産経路)。 */
    foShowLibraryBtn: () => {
      clickLeftTab("library");
      setTimeout(() => {
        document
          .querySelectorAll<HTMLButtonElement>(".roots-controls button")
          .forEach((b) => {
            if ((b.textContent ?? "").includes("ライブラリを表示")) {
              b.click();
            }
          });
      }, 150);
    },
    /**
     * リネーム入力へ "will-revert.png" を設定して確定 (E2E固定値。
     * input値設定+inputイベント+Enter合成keydown = 生産経路)。
     */
    foRenameWillRevert: () => {
      const input = document.querySelector<HTMLInputElement>('[data-testid="rename-input"]');
      if (input === null) {
        return;
      }
      input.value = "will-revert.png";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      setTimeout(() => {
        input.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "Enter",
            bubbles: true,
            cancelable: true,
          }),
        );
      }, 120);
    },
    /** FG-WP7 E2E: 既存名 (lib-05.png) へのリネーム確定 — 上書き衝突の
     * 確認トークンフロー (計画→衝突ダイアログ→上書き) を駆動する。 */
    foRenameToExisting: () => {
      const input = document.querySelector<HTMLInputElement>('[data-testid="rename-input"]');
      if (input === null) {
        return;
      }
      input.value = "lib-05.png";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      setTimeout(() => {
        input.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "Enter",
            bubbles: true,
            cancelable: true,
          }),
        );
      }, 120);
    },
    /** Deleteキー相当 (ゴミ箱) の合成keydown。 */
    pressKeyDelete: () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Delete", bubbles: true, cancelable: true }),
      );
    },
    /** Shift+Delete (完全削除ダイアログ) の合成keydown。 */
    pressKeyShiftDelete: () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Delete",
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    },
    /** Ctrl+Z (Undo) の合成keydown。 */
    pressCtrlZ: () => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "z",
          ctrlKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    },
    openFirstHomeChild: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        openFirstHomeChildNow();
      }, 150);
    },
    /** UIUX-1 E2E: フォルダツリーの最初の (可視) 行で右クリックメニューを開く。 */
    treeContextFirst: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        document
          .querySelector<HTMLElement>(".tree-label")
          ?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
      }, 150);
    },
    /** UIUX-1 E2E: 選択中 (row-selected) のツリー行で右クリックメニューを開く。
     *  未選択時は最初の行へfallback (登録外フォルダ選択後の登録導線検証用)。 */
    treeContextCurrent: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        const label =
          document.querySelector<HTMLElement>(".tree-row.row-selected .tree-label") ??
          document.querySelector<HTMLElement>(".tree-label");
        label?.dispatchEvent(
          new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
        );
      }, 150);
    },
    /** UIUX-1 E2E: 開いているツリー右クリックメニューの「このフォルダーを登録ルートに追加」。 */
    treeMenuRegister: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").trim().startsWith("このフォルダーを登録ルートに追加"));
      item?.click();
    },
    /** UIUX-1 E2E: 開いているツリー右クリックメニューの「登録を解除」。 */
    treeMenuUnregister: () => {
      const item = Array.from(
        document.querySelectorAll<HTMLButtonElement>(".context-menu button"),
      ).find((b) => (b.textContent ?? "").trim().startsWith("登録を解除"));
      item?.click();
    },
    // WP-18-1 D3 E2E: 最初の登録ルートのポータブルバックアップを作成
    // (destDir=null = 既定dir。BackupDialogと同一のcommandと引数)。
    bkPortableCreateFirstRoot: () => {
      resetBk("create");
      void (async () => {
        try {
          const roots = await stores.invoke<Array<{ id: string; available: boolean }>>(
            "library_roots_list",
          );
          const root = roots.find((r) => r.available) ?? roots[0];
          if (root === undefined) {
            throw new Error("登録ルートが無い");
          }
          const created = await stores.invoke<{
            path: string;
            assetCount: number;
            fileCount: number;
            collectionCount: number;
          }>("backup_portable_create", { rootId: root.id, destDir: null });
          bkState.ok = 1;
          bkState.assets = created.assetCount;
          bkState.files = created.fileCount;
          bkState.colls = created.collectionCount;
        } catch (err) {
          bkState.ok = 0;
          bkState.err = toBridgeMessage(err);
        }
      })();
    },
    // WP-18-1 D3 E2E: 既定dir内の最新 `.gento` を復元 (plan→token→execute。
    // RestoreDialogと同一のcommand列。targetRootId=null = 経路自動照合)。
    bkRestoreLatest: () => {
      resetBk("restore");
      void (async () => {
        try {
          const base = await stores.invoke<string>("backup_default_dir");
          const list = await stores.invoke<{ entries: Array<{ name: string; path: string }> }>(
            "fs_list_dirs",
            { path: base },
          );
          const gento = list.entries
            .filter((e) => e.name.endsWith(".gento"))
            .sort((a, b) => a.name.localeCompare(b.name));
          const latest = gento[gento.length - 1];
          if (latest === undefined) {
            throw new Error(".gentoバックアップが見つからない");
          }
          const plan = await stores.invoke<{ confirmToken: string }>(
            "backup_restore_plan",
            { backupDir: latest.path, targetRootId: null },
          );
          const result = await stores.invoke<{
            applied: unknown[];
            skipped: unknown[];
          }>("backup_restore_execute", {
            backupDir: latest.path,
            targetRootId: null,
            confirmToken: plan.confirmToken,
          });
          bkState.ok = 1;
          bkState.applied = result.applied.length;
          bkState.skipped = result.skipped.length;
        } catch (err) {
          bkState.ok = 0;
          bkState.err = toBridgeMessage(err);
        }
      })();
    },
    // ===== UIUX-1 E2E: メニューバー実操作・タブ・パンくず入力 =====
    /** メニューバー「ファイル」を開く (開いたまま — 項目数はUX行で観測)。 */
    uxMenuFile: () => {
      closeMenuBar();
      runMenuOpen("ファイル");
    },
    /** メニューバー「表示」を開く。 */
    uxMenuView: () => {
      closeMenuBar();
      runMenuOpen("表示");
    },
    /** メニューバー「ツール」を開く。 */
    uxMenuTools: () => {
      closeMenuBar();
      runMenuOpen("ツール");
    },
    /** メニューバー「ヘルプ」を開く。 */
    uxMenuHelp: () => {
      closeMenuBar();
      runMenuOpen("ヘルプ");
    },
    /** 開いているメニューを閉じる。 */
    uxMenuClose: () => closeMenuBar(),
    /** ツール>「設定…」を開く (data-testid=settings-buttonと同一項目)。 */
    uxMenuSettings: () => runMenuItem("ツール", "設定…"),
    /** ヘルプ>「このアプリについて」(設定dialogのAboutタブ直開)。 */
    uxMenuAbout: () => runMenuItem("ヘルプ", "このアプリについて"),
    /** ツール>「ルートのバックアップ…」(1ルートならBackupDialog直開)。 */
    uxMenuBackupRoot: () => runMenuItem("ツール", "ルートのバックアップ…"),
    /** ファイル>「SD学習用エクスポート…」(無効条件の検証用 — 無効なら不発火)。 */
    uxMenuExport: () => runMenuItem("ファイル", "SD学習用エクスポート…"),
    /** ファイル>「一時走査を実行」。 */
    uxMenuScan: () => runMenuItem("ファイル", "一時走査を実行"),
    /** ファイル>「一時走査を中止」(scanning中のみ項目が存在)。 */
    uxMenuCancelScan: () => runMenuItem("ファイル", "一時走査を中止"),
    /** ファイル>「終了」— 実window close経路 (プロセス終了のE2E用)。 */
    uxMenuQuit: () => runMenuItem("ファイル", "終了"),
    /** 表示>「左ペイン」(表示切替)。 */
    uxMenuToggleLeft: () => runMenuItem("表示", "左ペイン"),
    /** 表示>「右ペイン」(表示切替)。 */
    uxMenuToggleRight: () => runMenuItem("表示", "右ペイン"),
    /** ツール>「AIパネル…」。 */
    uxMenuAiPanel: () => runMenuItem("ツール", "AIパネル…"),
    /** ツール>「開発パネル」(devBridgeActive時のみ項目が存在)。 */
    uxMenuDevPanel: () => runMenuItem("ツール", "開発パネル"),
    /** ツール>「全データベースをバックアップ」(即時実行・完了は通知)。 */
    uxMenuBackupDb: () => runMenuItem("ツール", "全データベースをバックアップ"),
    /** ツール>「バックアップから復元…」(RestoreDialogを開く)。 */
    uxMenuRestore: () => runMenuItem("ツール", "バックアップから復元…"),
    /** メニューバー先頭 (ファイル) ボタンへフォーカス (実キー操作の着地)。 */
    uxFocusMenubar: () => {
      document.querySelector<HTMLButtonElement>(".menubar-button")?.focus();
    },
    /**
     * 非アクティブタブパネル内の最初の操作要素へfocusを試みて結果を記録
     * (hidden属性の実効性 — 本物のWebKitでは拒否=0が正常)。
     */
    uxTryFocusHiddenPane: () => {
      const activeTab = document.querySelector(
        ".left-tabs [role='tab'][aria-selected='true']",
      );
      const activePanelId =
        activeTab instanceof HTMLElement ? `left-panel-${activeTab.id.replace("left-tab-", "")}` : "";
      const hidden = Array.from(
        document.querySelectorAll<HTMLElement>(".left-tabpanel"),
      ).find((panel) => panel.id !== activePanelId && panel.hidden);
      const control =
        hidden?.querySelector<HTMLElement>("button, input, textarea, select, [tabindex]") ??
        null;
      if (control === null) {
        uxHiddenFocusResult = -1;
        return;
      }
      const before = document.activeElement;
      control.focus();
      uxHiddenFocusResult = document.activeElement === control && before !== control ? 1 : 0;
    },
    /** 「+ スマート」フォームを開き名前入力へフォーカス (ドラフト生存検証)。 */
    uxCollSmartForm: () => {
      clickLeftTab("collections");
      setTimeout(() => {
        clickButton("+ スマート");
        setTimeout(() => {
          document.querySelector<HTMLInputElement>(".coll-create .coll-name-input")?.focus();
        }, 120);
      }, 150);
    },
    /** ライブラリタブのルート追加path入力へフォーカス (ドラフト生存検証)。 */
    uxFocusRootPathInput: () => {
      clickLeftTab("library");
      setTimeout(() => {
        document.querySelector<HTMLInputElement>(".roots-add input")?.focus();
      }, 200);
    },
    /** 登録ルート行 (「登録ルート」バッジ持ち行) で右クリックメニューを開く
     * (登録済みパスはクリック表示がlibrary固定のため選択状態に依存しない)。 */
    treeContextRoot: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        const row = Array.from(document.querySelectorAll<HTMLElement>(".tree-row")).find(
          (r) => r.querySelector(".tree-badge")?.textContent === "登録ルート",
        );
        row
          ?.querySelector<HTMLElement>(".tree-label")
          ?.dispatchEvent(
            new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
          );
      }, 150);
    },
    /** UIUX-2 E2E: ファイルシステム (/) 行で右クリック (危険パス登録の
     *  確認ダイアログ検証用 — 登録導線のunrelated状態)。 */
    treeContextFsRoot: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        document
          .querySelector<HTMLElement>('.tree-label[title="/"]')
          ?.dispatchEvent(
            new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
          );
      }, 150);
    },
    /** 登録ルート行の展開矢印をクリック (配下行の出現)。 */
    treeExpandRoot: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        const row = Array.from(document.querySelectorAll<HTMLElement>(".tree-row")).find(
          (r) => r.querySelector(".tree-badge")?.textContent === "登録ルート",
        );
        row?.querySelector<HTMLElement>(".tree-expander")?.click();
      }, 150);
    },
    /** 登録内バッジ (登録ルート配下) の行で右クリック (メニュー不出現の検証)。 */
    treeContextInside: () => {
      clickLeftTab("folders");
      setTimeout(() => {
        const row = Array.from(document.querySelectorAll<HTMLElement>(".tree-row")).find(
          (r) => r.querySelector(".tree-badge")?.textContent === "登録内",
        );
        row
          ?.querySelector<HTMLElement>(".tree-label")
          ?.dispatchEvent(
            new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
          );
      }, 150);
    },
    /** 左ペインタブ切替 (ドラフト生存検証: 切替えても入力値が残る)。 */
    uxTabFolders: () => clickLeftTab("folders"),
    uxTabCollections: () => clickLeftTab("collections"),
    uxTabLibrary: () => clickLeftTab("library"),
    /** コレクション作成フォームを開き名前入力へフォーカス (xdotool typeの着地)。 */
    uxCollCreateForm: () => {
      clickLeftTab("collections");
      setTimeout(() => {
        clickButton("+ 手動");
        setTimeout(() => {
          document.querySelector<HTMLInputElement>(".coll-create .coll-name-input")?.focus();
        }, 120);
      }, 150);
    },
    /** パンくずの「パスを入力」ボタンを開きinputへフォーカス (xdotool typeの着地)。 */
    uxFocusBreadcrumb: () => {
      document
        .querySelector<HTMLButtonElement>('[data-testid="breadcrumb-path-button"]')
        ?.click();
      setTimeout(() => {
        document
          .querySelector<HTMLInputElement>('[data-testid="breadcrumb-path-input"]')
          ?.focus();
      }, 150);
    },
    /** ツールバーのスクロール速度スライダーへフォーカス (矢印キー=本物の操作)。 */
    uxFocusSpeedSlider: () => {
      document
        .querySelector<HTMLInputElement>('[data-testid="scroll-speed-slider"]')
        ?.focus();
    },
    /** グリッドのスクロール要素へ合成wheelを3ノッチ dispatch (速度係数の実測用。
     * 実環境のX button4/5がWebKitGTKでホイール処理されないため、ビューアの
     * wheelAtと同じ方針 — 本物のconditionalWheelリスナを通る)。 */
    uxWheelGrid: () => {
      const el = document.querySelector<HTMLElement>(".grid-scroll");
      if (el === null) {
        return;
      }
      for (let i = 0; i < 3; i++) {
        el.dispatchEvent(
          new WheelEvent("wheel", {
            bubbles: true,
            cancelable: true,
            deltaMode: 0,
            deltaY: 200,
          }),
        );
      }
    },
    /** UIUX-3 E2E: ツール>設定… を開き**表示タブ**へ切替 (速度スライダーは
     *  dialog表示タブ内に移設されたため、xsp=読み取りはこのアクション後に
     *  行う契約。閉時のxsp=-1は正常値)。 */
    uxSettingsDisplay: () => {
      runMenuItem("ツール", "設定…");
      setTimeout(() => {
        const tab = Array.from(
          document.querySelectorAll<HTMLButtonElement>(".settings-picker .tabs button"),
        ).find((b) => (b.textContent ?? "").trim() === "表示");
        tab?.click();
      }, 350);
    },
    /** UIUX-3 E2E: 検索行の折りたたみトグル (data-testid=search-toggle)。 */
    uxSearchToggle: () => {
      document.querySelector<HTMLButtonElement>('[data-testid="search-toggle"]')?.click();
    },
    /** UIUX-3 E2E: 設定表示タブの「既定に戻す (2.0)」ボタン。 */
    uxScrollSpeedReset: () => {
      document.querySelector<HTMLButtonElement>('[data-testid="scroll-speed-reset"]')?.click();
    },
    /** UIUX-3 E2E: 折りたたみ行の「✕ 検索を解除」ボタン。 */
    uxSearchClear: () => {
      document.querySelector<HTMLButtonElement>('[data-testid="search-clear"]')?.click();
    },
    // ===== UIUX-3 E2E: ビューアwheel/navの分離アクション (V-4橋分離) =====
    // 平常wheel=送り・Alt/Ctrl(modifier)wheel=ズーム。viewer-stageへ本物の
    // listener (ImageViewer) を通る合成WheelEvent。3ノッチ連続dispatchは
    // accumulatorにより1ジェスチャ=1送りになる (生産semanticsの検証を兼ねる)。
    /** viewerWheelNext: 平常wheel (deltaY+) で次の画像へ1枚。 */
    viewerWheelNext: () => wheelViewerBridge(100, {}),
    /** viewerWheelPrev: 平常wheel (deltaY-) で前の画像へ1枚。 */
    viewerWheelPrev: () => wheelViewerBridge(-100, {}),
    /** viewerAltZoomIn / Out: Alt+wheelズーム (V-4の主要ズーム経路)。 */
    viewerAltZoomIn: () => wheelViewerBridge(-100, { alt: true }),
    viewerAltZoomOut: () => wheelViewerBridge(100, { alt: true }),
    /** viewerPinchIn / Out: ctrl+wheel (トラックパッドpinch) のズーム保存検証。 */
    viewerPinchIn: () => wheelViewerBridge(-100, { ctrl: true }),
    viewerPinchOut: () => wheelViewerBridge(100, { ctrl: true }),
    /** 比較ビューslot0への平常wheel (比較ズームはUIUX-3で不変 — 確認用)。 */
    compareWheelZoom: () => {
      const el = document.querySelector<HTMLElement>(".compare-overlay .slot[data-slot='0']");
      if (el !== null) {
        wheelAt(el, 0.25, 0.35, 3, -100);
      }
    },
    /** ビューア左右端の送りボタン (V-4)。実クリック配列 (pointerdown→
     *  pointerup→click) で駆動 — FIX-VIEWERNAV-1: stageのpointer captureが
     *  ボタンのclickを壊す回帰をE2Eでも検出できるようにする。 */
    viewerNavPrevBtn: () => {
      const btn = document.querySelector<HTMLButtonElement>(
        '[data-testid="viewer-nav-prev"]',
      );
      if (btn !== null) {
        realClick(btn);
      }
    },
    viewerNavNextBtn: () => {
      const btn = document.querySelector<HTMLButtonElement>(
        '[data-testid="viewer-nav-next"]',
      );
      if (btn !== null) {
        realClick(btn);
      }
    },
  };

  function gridLines(): string[] {
    const grid = stores.getGrid();
    if (grid === null) {
      return [];
    }
    const cells = Array.from(document.querySelectorAll<HTMLElement>("[data-grid-cell]"));
    const dom = cells.length;
    const ready0 = grid.summary !== null && grid.summary.total > 0 && grid.thumbUrlFor("0") !== null ? 1 : 0;
    // FIXLIB-1: セル単位の<img>描画統計。「要求/ready受理は進むのに描画されない」
    // 系の欠陥で、統計値 (req/rdy) が正常に見える偽陽性をDOM実測で切り分ける。
    // フィールド名は既存ドライバのdevtest_value先頭一致 (CL n= 等) を
    // 奪わないよう命名済み (n→img, err→bad)。rdy=はG行より後ろにあるため
    // G行の値が先頭一致する (従来通り)。
    // img=セル内img総数 / ok=デコード成功 / bad=ロード完了だが破損
    // / pend=ロード中 / none=imgを持たないセル / rdy=ストアがURLを保持するセル
    // / it=itemを解決できるセル / t0=先頭セルのtitle文字数 (0=item未描画)
    let imgTotal = 0;
    let imgOk = 0;
    let imgErr = 0;
    let imgPend = 0;
    let cellNoImg = 0;
    let cellUrlReady = 0;
    let cellItem = 0;
    const firstTitle = cells[0]?.getAttribute("title") ?? "";
    for (const cell of cells) {
      const img = cell.querySelector("img");
      if (img === null) {
        cellNoImg++;
      } else {
        imgTotal++;
        if (img.complete && img.naturalWidth > 0) {
          imgOk++;
        } else if (img.complete) {
          imgErr++;
        } else {
          imgPend++;
        }
      }
      const index = Number(cell.dataset.index);
      const item = Number.isInteger(index) ? grid.getItem(index) : null;
      if (item !== null) {
        cellItem++;
      }
      if (item !== null && grid.thumbUrlFor(item.id) !== null) {
        cellUrlReady++;
      }
    }
    const scrollEl = document.querySelector<HTMLElement>(".grid-scroll");
    const ae = document.activeElement;
    const sel = stores.getSelection();
    const s = grid.stats;
    const gridLinesOut = [
      `G tot=${grid.summary?.total ?? 0} mode=${grid.summary?.mode ?? "-"}` +
        ` scn=${grid.summary?.scanning ? 1 : 0}` +
        ` srt=${grid.summary?.sort ?? "-"}` +
        ` req=${s.requested} bat=${s.batches} rej=${s.rejectedFull}` +
        ` rdy=${s.readyReceived} fail=${s.failedReceived} can=${s.cancelled} canB=${s.cancelBatches}`,
      `G pf=${grid.pager.stats.fetched} pe=${grid.pager.stats.errors}` +
        ` pgen=${grid.pager.generation ?? -1} dom=${dom} r0=${ready0}` +
        ` st=${Math.round(scrollEl?.scrollTop ?? -1)}` +
        ` ae=${ae === null ? "-" : `${ae.tagName}.${String(ae.className).split(" ")[0]}`}` +
        ` selN=${sel?.ids.length ?? -1} foc=${sel?.focusIndex ?? -1}`,
      `IM img=${imgTotal} ok=${imgOk} bad=${imgErr} pend=${imgPend}` +
        ` none=${cellNoImg} rdy=${cellUrlReady} it=${cellItem} t0=${firstTitle.length}` +
        ` jse=${jsErrorCount}`,
    ];
    if (jsErrorLast !== "") {
      gridLinesOut.push(`JSE last="${jsErrorLast.replaceAll('"', "'")}"`);
    }
    return gridLinesOut;
  }

  /** QA-WP4: 選別セッション状態行 (再開シナリオの実機E2E用)。 */
  function sessionLines(): string[] {
    const session = stores.getSession();
    if (session === null) {
      return [];
    }
    const sel = stores.getSelection();
    const grid = stores.getGrid();
    const focusItem =
      sel !== null && sel.focusIndex !== null ? grid?.getItem(sel.focusIndex) : null;
    // WP-5: 採用/除外/保留の実集計 (DB triage → pagerキャッシュ)
    let keep = -1;
    let reject = -1;
    let undecided = -1;
    if (grid !== null && session.session !== null) {
      keep = 0;
      reject = 0;
      undecided = 0;
      const wanted = new Set(session.session.ids);
      const resolved = grid.pager.findIndicesByIds(wanted);
      for (const id of session.session.ids) {
        const index = resolved.get(id);
        const triage = index !== undefined ? grid.getItem(index)?.triage : undefined;
        if (triage === "keep") {
          keep++;
        } else if (triage === "reject") {
          reject++;
        } else {
          undecided++;
        }
      }
    }
    return [
      `S act=${session.active ? 1 : 0} pos=${session.session?.position ?? -1}` +
        ` tot=${session.session?.ids.length ?? 0} cur=${session.currentId() ?? "-"}` +
        ` foc=${sel?.focusIndex ?? -1} fn=${focusItem?.displayName ?? "-"}` +
        ` k=${keep} r=${reject} u=${undecided}` +
        ` note=${stores.getSessionNotice().slice(0, 48)}`,
    ];
  }

  /** WP-6: コレクション状態行 (E2Eの機械検証用。比較表示C行と区別するためCL接頭辞)。 */
  function collectionLines(): string[] {
    const collections = stores.getCollections();
    if (collections === null) {
      return [];
    }
    const a = collections.active;
    const exportButtons = document.querySelectorAll("button.export-entry[disabled]").length;
    const picker = document.querySelectorAll(".picker").length;
    return [
      `CL n=${collections.list.length} act=${a === null ? "-" : a.id}` +
        ` kind=${a === null ? "-" : a.kind === "smart" ? "s" : "m"}` +
        ` cnt=${a?.itemCount ?? -1} memo=${collections.focusMemo === null ? "-" : "1"}` +
        ` exp=${exportButtons} pick=${picker} cmd=${collCmdMode ? 1 : 0}` +
        ` list=${collections.list.map((c) => `${c.kind === "smart" ? "s" : "m"}${c.itemCount}`).join(",")}`,
    ];
  }

  /** WP-7(MVP): ファイル操作状態行 (E2Eの機械検証用。FO接頭辞)。 */
  function fileOpsLines(): string[] {
    const fo = stores.getFileOps();
    if (fo === null) {
      return [];
    }
    return [
      `FO busy=${fo.busy ? 1 : 0}` +
        ` prog=${fo.progress === null ? "-" : `${fo.progress.index + 1}/${fo.progress.total}`}` +
        ` dst=${fo.destPicker ? 1 : 0} cf=${fo.conflict ? 1 : 0} fv=${fo.forever ? 1 : 0}` +
        ` rt=${fo.retry ? 1 : 0} rn=${fo.renaming ? 1 : 0}` +
        ` undo=${fo.undoDepth}:${fo.undoLabel ?? "-"}` +
        ` note=${fo.notice.slice(0, 40)} err=${fo.error === null ? "-" : fo.error.slice(0, 40)}`,
    ];
  }

  /** WP-18-1: バックアップ/復元の状態行 (BK接頭辞)。bok=-1は未実行。
   * フィールドはb接頭辞 — 既存行の IM ok= / V ap= / FO err= との
   * devtest_value先頭一致の奪い合いを避ける (命名規約はgridLines参照)。 */
  function bkLines(): string[] {
    return [
      `BK bop=${bkState.op} bok=${bkState.ok} ba=${bkState.assets} bf=${bkState.files}` +
        ` bc=${bkState.colls} bap=${bkState.applied} bsk=${bkState.skipped}` +
        ` berr=${bkState.err === "" ? "-" : bkState.err.slice(0, 60)}`,
    ];
  }

  /** UIUX-1: メニューバー・左ペインタブ・パンくず入力の状態行 (UX接頭辞)。
   * フィールドはx接頭辞で既存行の先頭一致を奪わない (命名規約はbkLines参照)。
   * xtab=アクティブタブ(f/c/l/-) / xmen=開中メニュー(f/v/t/h/-) / xmi=項目数
   * / xdlg=picker系dialog数 / xab=設定About表示 / xcr=コレクション作成
   * 入力文字数 / xcm=作成モード(n/なし m=手動 s=スマート) / xbi=パンくず入力中
   * / xbc=crumb数 / xed=エクスポート項目の無効(1/0/-1=メニュー閉)
   * / xerr=toolbarエラー通知 / xsp=スクロール速度係数 (スライダーvalue)
   * / xrole=activeElementのrole (実キー操作のfocus検証)
   * / xlp=左ペイン可視 / xrp=右ペイン可視 / xhid=非アクティブパネルhidden
   * / xhf=hidden内focus試行 (0=拒否が正常) / xrn=リネーム入力文字数
   * / xra=ルート追加path入力文字数 / xes=エクスポートdialog入力元(c/s/-)
   * / xsc=ファイルメニュー走査項目(r=実行/c=中止/-) / xti=登録内バッジ数
   * / xdd=開発パネル / xai=AIパネル / xln=DBバックアップ完了通知
    * / xrc=危険パス登録確認dialog / xrt=登録ルート行数 / xtt=省略タブ数
    * (UIUX-2) / xsec=検索行展開(1/0/-1)・xsq=折りたたみ検索語文字数(-1=展開中)
    * / xssp=scope badge種(l/f/c/-) / xvn=ビューア送りボタン数(2/-1)
    * / xvd=送りボタンdisabled bit(0=両方有効 1=前無効 2=次無効 3=両方/-1)
    * / xgs=開発パネルグリッド統計total(-1=未表示) / xcl=折りたたみ✕操作(1/0)
    * / xol=旧grid-statsオーバーレイ残存(0=正常) (UIUX-3) */
  function uiuxLines(): string[] {
    const tabBtn = document.querySelector<HTMLButtonElement>(
      ".left-tabs [role='tab'][aria-selected='true']",
    );
    const xtab = tabBtn === null ? "-" : tabBtn.id.replace("left-tab-", "").slice(0, 1);
    const openTop = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".menubar-button"),
    ).findIndex((b) => b.getAttribute("aria-expanded") === "true");
    const xmen = openTop === -1 ? "-" : ["f", "v", "t", "h"][openTop] ?? "-";
    const popover = document.querySelector(".menu-popover");
    const xmi = popover === null ? 0 : popover.querySelectorAll("button").length;
    const xdlg = document.querySelectorAll(".picker-backdrop").length;
    const xab = document.querySelector("[data-testid='settings-about']") !== null ? 1 : 0;
    const createForm = document.querySelector(".coll-create");
    const collName = document.querySelector<HTMLInputElement>(".coll-create .coll-name-input");
    const xcm = createForm === null ? "n" : createForm.querySelector(".coll-builder") === null ? "m" : "s";
    const xcr = collName === null ? -1 : collName.value.length;
    const xbi =
      document.querySelector('[data-testid="breadcrumb-path-input"]') !== null ? 1 : 0;
    const xbc = document.querySelectorAll(".breadcrumb .crumb").length;
    const xerr = document.querySelector(".toolbar-notice.error") !== null ? 1 : 0;
    const speedSlider = document.querySelector<HTMLInputElement>(
      '[data-testid="scroll-speed-slider"]',
    );
    // UIUX-3: スライダーは設定dialog表示タブ内へ移設。dialog閉時は-1
    // (読み取りは「S: uxSettingsDisplay」からの契約)。
    const xsp = speedSlider === null ? "-1" : speedSlider.value;
    const active = document.activeElement;
    const xrole =
      active === null || active === document.body
        ? "-"
        : active.getAttribute("role") ?? active.tagName.toLowerCase();
    const xlp = document.querySelector(".left-tabs") !== null ? 1 : 0;
    const xrp = document.querySelector(".side-pane.right") !== null ? 1 : 0;
    const anyHidden = Array.from(document.querySelectorAll<HTMLElement>(".left-tabpanel")).find(
      (panel) => panel.hidden,
    );
    const xhid = tabBtn === null ? -1 : anyHidden === undefined ? 0 : 1;
    const xrn =
      document.querySelector<HTMLInputElement>('[data-testid="rename-input"]')?.value.length ??
      -1;
    const xra = document.querySelector<HTMLInputElement>(".roots-add input")?.value.length ?? -1;
    const exportSource = document.querySelector("[data-testid='export-source']");
    const xes =
      exportSource === null
        ? "-"
        : (exportSource.textContent ?? "").includes("コレクション")
          ? "c"
          : (exportSource.textContent ?? "").includes("選択")
            ? "s"
            : "-";
    let xsc = "-";
    if (popover !== null) {
      const first = popover.querySelectorAll<HTMLButtonElement>("button")[0];
      if (first !== undefined) {
        const label = (first.textContent ?? "").trim();
        if (label.startsWith("一時走査を実行")) {
          xsc = "r";
        } else if (label.startsWith("一時走査を中止")) {
          xsc = "c";
        }
      }
    }
    const xti = Array.from(document.querySelectorAll(".tree-badge")).filter(
      (badge) => badge.textContent === "登録内",
    ).length;
    const xdd = document.querySelector(".dev-drawer") !== null ? 1 : 0;
    const xai = document.querySelector('[role="dialog"][aria-label="AI設定"]') !== null ? 1 : 0;
    const xln =
      (document.querySelector(".roots-notice")?.textContent ?? "").includes("DBバックアップを作成しました")
        ? 1
        : 0;
    // UIUX-2: 危険パス登録確認dialog・登録ルート行数・タブラベル省略
    const xrc = document.querySelector('[data-testid="register-confirm"]') !== null ? 1 : 0;
    const xrt = document.querySelectorAll(".roots-list li").length;
    const xtt = Array.from(
      document.querySelectorAll<HTMLButtonElement>(".left-tabs button"),
    ).filter((tab) => tab.scrollWidth > tab.clientWidth).length;
    let xed = -1;
    if (popover !== null) {
      const exportItem = Array.from(
        popover.querySelectorAll<HTMLButtonElement>("button"),
      ).find((b) => (b.textContent ?? "").trim().startsWith("SD学習用エクスポート"));
      if (exportItem !== undefined) {
        xed = exportItem.disabled ? 1 : 0;
      }
    }
    // UIUX-3: 検索折りたたみ (xsec=1展開/0折りたたみ/-1検索行なし) と
    // 折りたたみ要約 (xsq=検索語文字数・xssp=scope badge種 l/f/c/-)
    const searchBar = document.querySelector<HTMLElement>('[data-testid="search-bar"]');
    const xsec = searchBar === null ? -1 : searchBar.querySelector('[data-testid="search-input"]') !== null ? 1 : 0;
    const summaryQuery = document.querySelector<HTMLElement>('[data-testid="search-summary-query"]');
    const xsq = summaryQuery === null ? -1 : (summaryQuery.textContent ?? "").length;
    const scopeBadge = document.querySelector<HTMLElement>('[data-testid="search-scope-badge"]');
    const xssp = scopeBadge === null ? "-" : scopeBadge.dataset.scopeKind === "folder" ? "f" : scopeBadge.dataset.scopeKind === "collection" ? "c" : "l";
    // UIUX-3: ビューア送りボタン (xvn=数/xvd=disabled bit0=前 bit1=次)
    const navPrev = document.querySelector<HTMLButtonElement>('[data-testid="viewer-nav-prev"]');
    const navNext = document.querySelector<HTMLButtonElement>('[data-testid="viewer-nav-next"]');
    const xvn = navPrev === null || navNext === null ? -1 : 2;
    const xvd =
      navPrev === null || navNext === null ? -1 : (navPrev.disabled ? 1 : 0) + (navNext.disabled ? 2 : 0);
    // UIUX-3: 開発パネルのグリッド統計 (xgs=total値。未表示/-1)
    const devStatsTotal = document.querySelector<HTMLElement>('[data-testid="dev-grid-stats-total"]');
    const xgs = devStatsTotal === null ? -1 : Number((devStatsTotal.textContent ?? "").replaceAll(",", ""));
    // UIUX-3: 折りたたみ行のクリア (✕) 操作の存在 (V-3 acceptance)
    const xcl = document.querySelector('[data-testid="search-clear"]') !== null ? 1 : 0;
    // UIUX-3: 旧グリッド統計オーバーレイ (.grid-stats) は主UIから撤去済みが正常
    const xol = document.querySelector(".grid-stats") !== null ? 1 : 0;
    return [
      `UX xtab=${xtab} xmen=${xmen} xmi=${xmi} xdlg=${xdlg} xab=${xab}` +
        ` xcr=${xcr} xcm=${xcm} xbi=${xbi} xbc=${xbc} xed=${xed} xerr=${xerr} xsp=${xsp}` +
        ` xrole=${xrole} xlp=${xlp} xrp=${xrp} xhid=${xhid} xhf=${uxHiddenFocusResult}` +
        ` xrn=${xrn} xra=${xra} xes=${xes} xsc=${xsc} xti=${xti} xdd=${xdd} xai=${xai} xln=${xln}` +
        ` xrc=${xrc} xrt=${xrt} xtt=${xtt}` +
        ` xsec=${xsec} xsq=${xsq} xssp=${xssp} xvn=${xvn} xvd=${xvd} xgs=${xgs}` +
        ` xcl=${xcl} xol=${xol}`,
    ];
  }

  /** WP-5: メタデータ編集状態行 (E2Eの機械検証用)。 */
  function metaLines(): string[] {
    const meta = stores.getMeta();
    if (meta === null) {
      return [];
    }
    const d = meta.detail;
    const f = meta.focusItem;
    const tri = (value: string | undefined): string =>
      value === "keep" || value === "picked" ? "k" : value === "reject" || value === "rejected" ? "r" : "u";
    // グリッドDOM上のバッジ数 (星・♥・採用/除外) とタグ候補数
    const badges = document.querySelectorAll("[data-grid-cell] .badge").length;
    const suggests = document.querySelectorAll("[data-meta='tag-suggest'] .tag-suggest-item").length;
    // UXLIB-1: InfoPanel編集ロックの理由 (h=ハッシュ待ち / u=未登録フォルダ)
    const lockText = (document.querySelector(".placeholder-note")?.textContent ?? "").trim();
    const lock = lockText.includes("ハッシュ計算待ち")
      ? "h"
      : lockText.includes("未登録フォルダ")
        ? "u"
        : "-";
    return [
      `M edit=${meta.canEdit ? 1 : 0} rat=${d?.rating ?? f?.rating ?? -1}` +
        ` frat=${f?.rating ?? -1} ftri=${tri(f?.triage)} fid=${f?.id ?? "-"} fdb=${f?.dbState ?? "-"}` +
        ` tri=${tri(d?.triage ?? f?.triage)}` +
        ` fav=${(d?.favorite ?? f?.favorite ?? false) ? 1 : 0}` +
        ` tags=${d?.tags.length ?? -1} caps=${d?.captions.length ?? -1}` +
        ` undo=${meta.undoDepth} tmode=${meta.triageMode ? 1 : 0}` +
        ` badge=${badges} sug=${suggests} lock=${lock}`,
    ];
  }

  function thumbLines(): string[] {
    const thumb = stores.getThumb();
    const stats = thumb?.stats;
    if (!stats) {
      return [];
    }
    const q = stats.queue_depths;
    return [
      `T gen=${stats.generated} hit=${stats.hits} mis=${stats.misses}` +
        ` hr=${n(stats.hit_rate, 3)} fail=${stats.failures} ev=${stats.evictions}` +
        ` cb=${stats.cache_bytes} pa=${stats.paused ? 1 : 0}`,
      `T q=${stats.queue_depth} qi=${q.interactive} qv=${q.viewport} qp=${q.prefetch}` +
        ` qo=${q.overscan} qb=${q.background}` +
        ` p50=${n(stats.p50_ms, 1)} p95=${n(stats.p95_ms, 1)} p99=${n(stats.p99_ms, 1)}`,
      `T inj=${stats.injection_mode ?? "-"},${n(stats.injection_rate ?? null, 2)}` +
        ` proto=${stats.proto_requests} pp50=${n(stats.proto_p50_ms, 1)}` +
        ` pp95=${n(stats.proto_p95_ms, 1)} pp99=${n(stats.proto_p99_ms, 1)}`,
    ];
  }

  function perfLines(): string[] {
    const p = stores.getPerf();
    const d = p.startupDurations();
    return [
      `P lt=${p.longTasks} ltMax=${n(p.worstLongTaskMs, 1)} ltObs=${p.longTaskObserverSupported === null ? "?" : p.longTaskObserverSupported ? 1 : 0}` +
        ` lf=${p.longFrames} lfMax=${n(p.worstLongFrameMs, 1)}`,
      `P mnt=${n(p.marks.mountedAt, 0)} hok=${n(p.marks.healthOkAt, 0)} pg1=${n(p.marks.firstPageAt, 0)}` +
        ` dH=${n(d.mountToHealthMs, 0)} dP=${n(d.mountToFirstPageMs, 0)}`,
    ];
  }

  /** WP-4 UIシェル状態行 (メニュー・クイックプレビュー・通知の機械検証用)。 */
  function uiLines(): string[] {
    const menuEl = document.querySelector<HTMLElement>(".context-menu");
    const menu = document.querySelectorAll(".context-menu").length;
    const qp = document.querySelectorAll(".quick-preview-backdrop").length;
    const notice = (document.querySelector(".header-notice")?.textContent ?? "-")
      .trim()
      .slice(0, 48);
    const buttons = menuEl ? menuEl.querySelectorAll("button").length : 0;
    const rect = menuEl?.getBoundingClientRect();
    const mrect = rect
      ? `${Math.round(rect.x)},${Math.round(rect.y)},${Math.round(rect.width)},${Math.round(rect.height)}`
      : "0,0,0,0";
    return [
      `U menu=${menu} mbtn=${buttons} mrect=${mrect} qp=${qp}` +
        ` ro=${readoutVisible ? 1 : 0} note=${notice}`,
    ];
  }

  /** V/C行 (ビューア・比較) の構築。dump表示とログ報告の両方で使う。 */
  function dumpLines(): string[] {
    const lines: string[] = [];
    const mode = stores.getMode();
    const v = mode === "single" ? (stores.getViewer() as ViewerLike | null) : null;
    if (v !== null) {
      const on = v.prepOn.stats();
      const off = v.prepOff.stats();
      const cur = v.current;
      lines.push(
        `V idx=${v.displayIndex} id=${cur?.assetId ?? "-"} dim=${cur?.width ?? 0}x${cur?.height ?? 0}`,
        `V url=${scheme(cur?.url ?? null)} orig=${cur?.isOriginal ? 1 : 0} ret=${v.retained?.assetId ?? "-"}`,
        `V gen=${v.generation} ap=${v.stats.applied} stale=${v.stats.staleDropped}` +
          ` req=${v.stats.requests} preq=${v.stats.prefetchRequests} prdy=${v.stats.prefetchReady}`,
        `V pre=${v.prefetchEnabled ? 1 : 0}` +
          ` onN=${on.count} onP50=${n(on.p50, 1)} onP95=${n(on.p95, 1)} onP99=${n(on.p99, 1)}` +
          ` offN=${off.count} offP50=${n(off.p50, 1)} offP95=${n(off.p95, 1)} offP99=${n(off.p99, 1)}`,
        `V t sc=${n(v.transform.scale, 3)} cx=${n(v.transform.centerX, 4)} cy=${n(v.transform.centerY, 4)}` +
          ` md=${v.transform.mode} zf=${n(v.zoomFactor(), 3)} vp=${v.viewport.width}x${v.viewport.height}`,
      );
    }
    const c = mode !== null && mode !== "single" ? (stores.getCompare() as CompareLike | null) : null;
    if (c !== null) {
      lines.push(
        `C n=${c.count} sync=${c.syncEnabled ? 1 : 0} c=(${n(c.sync.centerX, 4)},${n(c.sync.centerY, 4)})` +
          ` zf=${n(c.sync.zoomFactor, 3)} base=${c.baseSlot ?? -1} act=${c.active} next=${c.nextIndex} grp=${c.stats.groups}`,
      );
      c.slots.forEach((slot, index) => {
        const t = c.effectiveTransform(index);
        lines.push(
          `C s${index} id=${slot.assetId ?? "-"} dim=${slot.width}x${slot.height}` +
            ` url=${scheme(slot.url)} orig=${slot.isOriginal ? 1 : 0} det=${slot.detached ? 1 : 0}` +
            ` ret=${slot.retainedUrl ? 1 : 0} c=(${n(t.centerX, 4)},${n(t.centerY, 4)}) sc=${n(t.scale, 3)}`,
        );
      });
      const f = c.frames.stats();
      lines.push(
        `C frN=${f.count} frP50=${n(f.p50, 1)} frP95=${n(f.p95, 1)} frMax=${n(f.max, 1)}` +
          ` sw=${c.stats.swaps} creq=${c.stats.requests}`,
      );
    }
    lines.push(
      ...gridLines(),
      ...thumbLines(),
      ...perfLines(),
      ...uiLines(),
      ...sessionLines(),
      ...metaLines(),
      ...assetMetaLines(),
      ...collectionLines(),
      ...fileOpsLines(),
      ...bkLines(),
      ...uiuxLines(),
    );
    if (!lines.some((line) => line.startsWith("V ") || line.startsWith("C "))) {
      lines.push("idle (viewer closed)");
    }
    return lines;
  }

  function dump(): void {
    readout.textContent = dumpLines().join("\n");
  }

  /** 機械可読チャネル: 1行テキストを devtest: ログと document.title へ送る。 */
  /** WP-8: 検索状態の機械可読1行 (E2E検証用)。 */
  function searchLines(): string[] {
    const search = stores.getSearch();
    if (search === null) {
      return [];
    }
    const result = search.lastResult;
    return [
      `S act=${result === null ? 0 : 1} q="${search.lastQuery}"` +
        ` tot=${result?.total ?? -1} shown=${result?.shown ?? -1}` +
        ` ms=${result?.elapsedMs ?? -1} warn=${result?.warnings.length ?? 0}` +
        ` ev=${search.evidenceCount} run=${search.running ? 1 : 0}`,
    ];
  }

  /** WP-9: 埋め込み画像メタデータ状態行 (E2E検証用。X接頭辞)。
   * DOM観測ベース (InfoPanelのdata-meta-section属性) — ストアを経由しない
   * ため表示そのものを検証できる。 */
  function assetMetaLines(): string[] {
    const photo = document.querySelector('[data-meta-section="photo"]') !== null ? 1 : 0;
    const gen = document.querySelector('[data-meta-section="generation"]') !== null ? 1 : 0;
    const raw = document.querySelector('[data-meta-section="raw"]') !== null ? 1 : 0;
    const posFold = document.querySelector('[data-meta-fold="positive-prompt"]');
    const negFold = document.querySelector('[data-meta-fold="negative-prompt"]');
    const wfFold = document.querySelector('[data-meta-fold="workflow"]');
    const wfNote = document.querySelector('[data-meta="workflow-note"]');
    const rawFolds = document.querySelectorAll('[data-meta-section="raw"] .meta-fold').length;
    const chars = (el: Element | null): string =>
      el?.textContent?.match(/\(([\d,]+)文字\)/)?.[1] ?? "-";
    const copy = document.body.dataset.gentoCopy ?? "-";
    return [
      `X photo=${photo} gen=${gen} raw=${raw}` +
        ` pos=${posFold ? chars(posFold) : "-"} neg=${negFold ? chars(negFold) : "-"}` +
        ` wf=${wfFold ? chars(wfFold) : "-"} wfx=${wfNote ? 1 : 0} rawn=${rawFolds}` +
        ` cpy=${copy}`,
    ];
  }

  function reportCompact(): void {
    const single = [
      ...dumpLines(),
      ...gridLines(),
      ...searchLines(),
      ...assetMetaLines(),
      ...thumbLines(),
      ...perfLines(),
    ]
      .join(" | ")
      .replaceAll("\n", " ");
    try {
      document.title = `GENTO ${single}`;
    } catch {
      // title変更が環境によって失敗してもログチャネルは生きる
    }
    report(single);
  }

  window.addEventListener(
    "keydown",
    (event) => {
      // WP-6 E2E: コレクションコマンドモード (ctrl+shift+F1で入る)。
      // モード中の数字/記号キーは**1回のアクション実行で自動退出**する
      // (モーメンタリ方式 — モード残留による入力食いを構造的に防ぐ。
      // 3修飾キーctrl+shift+altはWM/WebKitへ到達しないための代替チャネル)。
      if (foCmdMode) {
        const foKeys: Record<string, string> = {
          "0": "foAddRootDataset",
          "1": "foShowLibraryRow",
          "2": "foShowLibraryBtn",
          "4": "contextMenuCell0",
          "5": "contextMenuFocused",
          "3": "focusCell3",
          c: "foMenuCopy",
          v: "foMenuMove",
          t: "foMenuTrash",
          e: "foMenuRename",
          w: "foRenameWillRevert",
          q: "foRenameToExisting",
          x: "pressKeyDelete",
          y: "pressKeyShiftDelete",
          z: "pressCtrlZ",
          d: "foDestPickFirst",
          b: "foDestPickFirstSub",
          k: "foConflictKeepBoth",
          o: "foConflictOverwrite",
          s: "foConflictSkip",
          a: "foConflictApplyAll",
          f: "foForeverCheck",
          g: "foForeverConfirm",
          r: "foRetryConfirm",
          n: "foFocusRename",
          // DEVUX-1: 読み出しトグル (rはfoRetryConfirmが使用済みのためm)
          m: "readoutToggle",
          h: "srFocusInput",
          j: "srRun",
          l: "srRefresh",
          u: "srSave",
          // WP-9 E2E: 埋め込みメタデータ (折りたたみ全展開・workflowコピー)
          p: "metaExpandAll",
          i: "metaCopyWorkflow",
          // WP-18-1 D3 E2E: バックアップ作成 (6) と最新.gento復元 (7)
          "6": "bkPortableCreateFirstRoot",
          "7": "bkRestoreLatest",
        };
        const action = foKeys[event.key];
        if (action !== undefined) {
          actions[action]?.();
        }
        foCmdMode = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
        return;
      }
      if (uxCmdMode) {
        const uxKeys: Record<string, string> = {
          "1": "uxMenuFile",
          "2": "uxMenuTools",
          "3": "uxMenuHelp",
          "4": "uxMenuClose",
          "5": "uxTabFolders",
          "6": "uxTabCollections",
          "7": "uxTabLibrary",
          "8": "uxCollCreateForm",
          "9": "uxFocusBreadcrumb",
          "0": "treeContextCurrent",
          a: "treeMenuRegister",
          u: "treeMenuUnregister",
          d: "treeContextInside",
          t: "treeExpandRoot",
          h: "treeContextRoot",
          l: "treeContextFsRoot",
          s: "uxMenuSettings",
          b: "uxMenuAbout",
          e: "uxMenuExport",
          k: "uxMenuBackupRoot",
          c: "uxMenuScan",
          n: "uxMenuCancelScan",
          q: "uxMenuQuit",
          m: "uxMenuView",
          p: "uxMenuToggleLeft",
          j: "uxMenuToggleRight",
          i: "uxMenuAiPanel",
          g: "uxMenuDevPanel",
          f: "uxMenuBackupDb",
          r: "uxMenuRestore",
          z: "uxFocusMenubar",
          x: "uxTryFocusHiddenPane",
          y: "uxCollSmartForm",
          o: "uxFocusRootPathInput",
          v: "uxFocusSpeedSlider",
          w: "uxWheelGrid",
          // UIUX-3: 大文字 (shift+字母) 割当 — Xephyr環境ではshift+字母が
          // WebKitへ到達しないためdriverはux3CmdMode (素F1) を使う。
          // 実ブラウザ寄りの環境向けに残置
          S: "uxSettingsDisplay",
          E: "uxSearchToggle",
          R: "uxScrollSpeedReset",
          C: "uxSearchClear",
        };
        const action = uxKeys[event.key];
        if (action !== undefined) {
          actions[action]?.();
        }
        uxCmdMode = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
        return;
      }
      if (vwCmdMode) {
        // UIUX-3: ビューアwheel/nav分離アクション (モーメンタリ。viewer要open)
        const vwKeys: Record<string, string> = {
          n: "viewerWheelNext",
          p: "viewerWheelPrev",
          i: "viewerAltZoomIn",
          o: "viewerAltZoomOut",
          c: "viewerPinchIn",
          d: "viewerPinchOut",
          b: "viewerNavPrevBtn",
          f: "viewerNavNextBtn",
          m: "compareWheelZoom",
        };
        const action = vwKeys[event.key];
        if (action !== undefined) {
          actions[action]?.();
        }
        vwCmdMode = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
        return;
      }
      if (ux3CmdMode) {
        // UIUX-3: 設定dialog速度・検索折りたたみ (モーメンタリ)
        const ux3Keys: Record<string, string> = {
          s: "uxSettingsDisplay",
          e: "uxSearchToggle",
          r: "uxScrollSpeedReset",
          c: "uxSearchClear",
        };
        const action = ux3Keys[event.key];
        if (action !== undefined) {
          actions[action]?.();
        }
        ux3CmdMode = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
        return;
      }
      if (collCmdMode) {
        const collKeys: Record<string, string> = {
          "1": "collCreateManualForm",
          "2": "collCreateSmartForm",
          "3": "collShowNext",
          "4": "contextMenuCell0",
          "5": "clickCollMenuAdd",
          "6": "pickerPickFirst",
          "7": "focusPickerNewInput",
          "8": "collMoveFront",
          "9": "collMoveBack",
          "0": "collRefresh",
          m: "focusCollectionMemo",
          s: "collSaveMemo",
        };
        const action = collKeys[event.key];
        if (action !== undefined) {
          actions[action]?.();
        }
        collCmdMode = false;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
        return;
      }
      if (!event.key.startsWith("F")) {
        return;
      }
      const key = event.key;
      // WP-6 E2E: コマンドモードのトグル本体。
      if (event.ctrlKey && event.shiftKey && !event.altKey && key === "F1") {
        collCmdMode = true;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 300);
        return;
      }
      // WP-7 E2E: ファイル操作コマンドモード (ctrl+shift+F12で入る。
      // F2はリネーム本体に取られているため)。
      if (event.ctrlKey && event.shiftKey && !event.altKey && key === "F12") {
        foCmdMode = true;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 300);
        return;
      }
      // UIUX-1 E2E: メニュー/タブ/右クリック/パンくずコマンドモード
      // (shift+F1で入る。ctrl+shift+F1はcollCmdModeが使用済みのため)。
      if (event.shiftKey && !event.ctrlKey && !event.altKey && key === "F1") {
        uxCmdMode = true;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 300);
        return;
      }
      // UIUX-3 E2E: ビューアwheel/navコマンドモード (素F12で入る。
      // ctrl+F12=jumpEnd・shift+F12=metaToggleTriage・ctrl+shift+F12=foCmdMode
      // が使用済みのため修飾なしF12を採用)。
      if (!event.ctrlKey && !event.shiftKey && !event.altKey && key === "F12") {
        vwCmdMode = true;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 300);
        return;
      }
      // UIUX-3 E2E: 設定速度/検索折りたたみコマンドモード (素F1で入る。
      // shift+字母キーが環境 (Xephyr/WebKit) へ到達しないため小文字専用モード)。
      if (!event.ctrlKey && !event.shiftKey && !event.altKey && key === "F1") {
        ux3CmdMode = true;
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 300);
        return;
      }
      if (event.altKey) {
        return;
      }
      let handled = true;
      // WP-5 E2E: ctrl+shift+F2..F8 はメタデータ編集の着地フォーカス等。
      // 未割り当てなら通常割り当て (F9..F12のctrl+shift) へ落とす
      if (event.ctrlKey && event.shiftKey && key >= "F2" && key <= "F8") {
        switch (key) {
          case "F2":
            actions.focusLibraryPathInput();
            break;
          case "F3":
            actions.focusTagInput();
            break;
          case "F4":
            actions.focusCell0();
            break;
          case "F5":
            actions.focusCell1();
            break;
          case "F6":
            actions.focusCell2();
            break;
          case "F7":
            actions.focusNoteCaption();
            break;
          case "F8":
            actions.metaSaveCaption();
            break;
          default:
            handled = false;
        }
        if (handled) {
          event.preventDefault();
          event.stopImmediatePropagation();
          setTimeout(dump, 600);
        }
        return;
      }
      switch (key) {
        case "F1":
          if (event.ctrlKey) {
            actions.resetStorage();
          } else {
            handled = false;
          }
          break;
        case "F2":
          if (event.ctrlKey) {
            actions.focusGrid();
          } else if (event.shiftKey) {
            actions.sessionStartFolder();
          } else {
            actions.select2();
          }
          break;
        case "F3":
          if (event.ctrlKey) {
            actions.select3();
          } else if (event.shiftKey) {
            actions.sessionResume();
          } else {
            actions.openViewer();
          }
          break;
        case "F4": {
          // UIUX-3: ctrl+F4=metaRate5 (wp7/wp8/uxlib1ドライバが使用) は不変。
          // 旧実装はここに `event.ctrlKey ? 100 : -100` のctrl分岐を置いていたが、
          // ctrlKey=trueなら上のmetaRate5へrouting済みで**到達不能な死に分岐**だった
          // (wp16_s4_viewer.pyの「ctrl+F4=wheelAt zoom」主張と不一致 — F4bug)。
          // 平常wheelの意味変更 (V-4) に伴い: 単一ビューアのズーム検証は
          // vwCmdMode (F12) のAlt/pinchズームへ、送りは同n/pへ分離した。
          // 比較ビュー (interactionEl=slot0) は別listenerのままズーム不変。
          if (event.ctrlKey) {
            actions.metaRate5();
            break;
          }
          if (event.shiftKey) {
            actions.sessionNext();
            break;
          }
          const el = interactionEl();
          if (el !== null) {
            wheelAt(el, 0.25, 0.35, 3, -100);
          }
          break;
        }
        case "F5": {
          if (event.ctrlKey) {
            actions.metaUndo();
            break;
          }
          if (event.shiftKey) {
            actions.cycleSort();
            break;
          }
          const el = interactionEl();
          if (el !== null) {
            void dragSequence(el, 0.3, 0.5, 240, 160, 24);
          }
          break;
        }
        case "F6":
          if (event.ctrlKey) {
            actions.resetStats();
          } else if (event.shiftKey) {
            actions.cancelScan();
          } else {
            actions.prefetchToggle();
          }
          break;
        case "F7":
          if (event.ctrlKey) {
            actions.openTreeFsRoot();
          } else if (event.shiftKey) {
            actions.openFirstHomeChild();
          } else {
            actions.cycleChoice();
          }
          break;
        case "F8":
          if (event.shiftKey) {
            actions.generateAllPreviews();
          } else if (event.ctrlKey) {
            actions.generateAll();
          } else {
            actions.scan();
          }
          break;
        case "F9":
          if (event.ctrlKey && event.shiftKey) {
            actions.pressKey3();
          } else if (event.shiftKey) {
            actions.metaPick();
          } else if (event.ctrlKey) {
            actions.clearCache();
          } else {
            actions.virtualMode();
          }
          break;
        case "F10":
          if (event.ctrlKey && event.shiftKey) {
            actions.pressKey5();
          } else if (event.shiftKey) {
            actions.metaReject();
          } else if (event.ctrlKey) {
            actions.jumpStart();
          } else {
            actions.realMode();
          }
          break;
        case "F11":
          if (event.ctrlKey && event.shiftKey) {
            actions.pressKeyX();
          } else if (event.shiftKey) {
            actions.metaBulkConfirm();
          } else if (event.ctrlKey) {
            actions.jumpMiddle();
          } else {
            actions.cycleInjection();
          }
          break;
        case "F12":
          if (event.ctrlKey && event.shiftKey) {
            actions.pressKeyP();
          } else if (event.shiftKey) {
            actions.metaToggleTriage();
          } else if (event.ctrlKey) {
            actions.jumpEnd();
          } else {
            handled = false;
          }
          break;
        default:
          handled = false;
      }
      if (handled) {
        event.preventDefault();
        event.stopImmediatePropagation();
        setTimeout(dump, 600);
      }
    },
    true,
  );

  const timer = window.setInterval(dump, 400);
  const reportTimer = window.setInterval(reportCompact, 1000);
  dump();

  (window as unknown as Record<string, unknown>).__gentoTest = {
    actions,
    dump,
    dispose: () => {
      window.clearInterval(timer);
      window.clearInterval(reportTimer);
      readout.remove();
    },
  };
}
