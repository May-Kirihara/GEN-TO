<script lang="ts">
  /**
    * アプリヘッダー (UIUX-5/WP7aでApp.svelteから抽出)。
    * 状態・storeはApp残置 — 素値をpropsで受け取り操作はcallbackで返す。
    * 閉じ判定 (popover外クリック・Escape) はWP2/WP3と同じaction手法を
    * 使用側で保持 (window listenerのためscoped制約なし)。
    */
  import type { HealthInfo, HealthUiState } from "../health";
  import type { CatalogSummary, ScanProgressEvent } from "../catalog";
  import { indexProgressText, type VectorIndexProgress } from "../vector-index";

  let {
    healthStatus,
    rawHealth,
    catalogSource,
    searchActive,
    headerStatusOpen,
    headerStatusBusy,
    headerStatusWarn,
    headerStatusHasRows,
    scanBusy,
    progress,
    summary,
    pendingHash,
    watchDegraded,
    indexProgress,
    metaNotice,
    osNotice,
    onToggleStatus,
    onCloseStatus,
  }: {
    healthStatus: HealthUiState | null;
    rawHealth: HealthInfo | null;
    catalogSource: "temp" | "library";
    searchActive: boolean;
    /** ステータスpopoverの開閉state (正本はApp)。 */
    headerStatusOpen: boolean;
    headerStatusBusy: boolean;
    headerStatusWarn: boolean;
    /** popover内に表示すべき行があるか (無い場合は空き状態案内)。 */
    headerStatusHasRows: boolean;
    scanBusy: boolean;
    progress: ScanProgressEvent | null;
    summary: CatalogSummary | null;
    /** BLAKE3ハッシュ計算の残件数 (library.status.pending_hash)。 */
    pendingHash: number;
    /** フォルダ監視の劣化フラグ (library.status.watch_degraded)。 */
    watchDegraded: boolean;
    indexProgress: VectorIndexProgress | null;
    metaNotice: string;
    osNotice: string;
    /** ステータスボタンの開閉トグル。 */
    onToggleStatus: () => void;
    /** popoverを閉じる (外クリック・Escape)。 */
    onCloseStatus: () => void;
  } = $props();

  /** popoverの外クリック判定用 (ステータスボタン上では閉じない)。 */
  let headerStatusToggleEl: HTMLElement | null = $state(null);

  /**
   * UIUX-5/WP4: ヘッダーステータスpopoverの閉じ取りまとめ (WP2/WP3と同じ
   * pointerdown capture+Escapeパターン)。popoverは開いている間のみ描画される
   * ため、リスナーも開いている間のみ存在する。ステータスボタン上は無視 —
   * 外クリックcloseとclickトグルの二重効きを防ぐ。
   */
  function headerStatusPopover(node: HTMLElement): { destroy: () => void } {
    function onPointerDown(event: PointerEvent): void {
      if (!headerStatusOpen) {
        return;
      }
      const target = event.target;
      if (
        target instanceof Node &&
        (node.contains(target) || headerStatusToggleEl?.contains(target) === true)
      ) {
        return;
      }
      onCloseStatus();
    }
    function onKeydown(event: KeyboardEvent): void {
      if (!headerStatusOpen) {
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseStatus();
      }
    }
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeydown, true);
    return {
      destroy: () => {
        window.removeEventListener("pointerdown", onPointerDown, true);
        window.removeEventListener("keydown", onKeydown, true);
      },
    };
  }
</script>

<header class="app-header">
  <h1 class="app-title">GEN-TO</h1>
  <span
    class="health-dot"
    class:ok={healthStatus?.kind === "ok"}
    class:ng={healthStatus?.kind === "error"}
    role="status"
    aria-label={healthStatus?.kind === "ok"
      ? `DB: ${healthStatus.dbOpen ? "接続" : "未接続"}`
      : healthStatus?.kind === "error"
        ? `エラー: ${healthStatus.message}`
        : "接続確認中"}
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
  <div class="header-spacer"></div>
  <!-- UIUX-5/WP4: 走査/警告インジケータ+ヘッダーnoticeを1個のステータス
       ボタン+popoverへ集約 (health-dot・source-badge・app-titleは維持)。
       各行は元インジケータのtitle/文言を移設。 -->
  <div class="header-status-wrap">
    <button
      bind:this={headerStatusToggleEl}
      class="header-status"
      class:busy={headerStatusBusy}
      class:warn={headerStatusWarn}
      onclick={() => onToggleStatus()}
      aria-expanded={headerStatusOpen}
      aria-haspopup="true"
      aria-label="アプリの状態と通知"
      title="アプリの状態と通知 (スキャン・索引・監視・通知)"
    ><span aria-hidden="true">◐</span></button>
    {#if headerStatusOpen}
      <div class="header-status-popover" use:headerStatusPopover>
        {#if !headerStatusHasRows}
          <p class="header-status-empty">現在表示できる状態情報はありません</p>
        {/if}
        {#if scanBusy}
          <p class="header-status-row" title="スキャン進行中">
            スキャン中
            {progress?.found.toLocaleString() ?? summary?.scanned_count.toLocaleString() ?? 0}件
          </p>
        {/if}
        {#if pendingHash > 0}
          <p class="header-status-row" title="BLAKE3ハッシュ計算の残件数">
            ハッシュ待ち {pendingHash.toLocaleString()}
          </p>
        {/if}
        {#if indexProgress !== null}
          <p
            class="header-status-row"
            title="USearch索引のバックグラウンド構築進捗 (検索・閲覧は引き続き利用できます)"
            data-testid="vector-index-progress"
          >
            {indexProgressText(indexProgress)}
          </p>
        {/if}
        {#if watchDegraded}
          <p class="header-status-row warn">⚠ 監視劣化</p>
          <p class="header-status-detail">
            フォルダ監視が未確立の登録ルートがあります (ポーリング再スキャンで運用中)
          </p>
        {/if}
        {#if metaNotice}
          <span class="header-notice">{metaNotice}</span>
        {/if}
        {#if osNotice}
          <span class="header-notice">{osNotice}</span>
        {/if}
      </div>
    {/if}
  </div>
  <!-- UIUX-1 (R-A): 操作ボタン類はメニューバーへ移動。headerは状態表示のみ。 -->
</header>

<style>
  .app-header {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--space-60);
    padding: 0.4rem 0.75rem;
    border-bottom: 1px solid var(--color-border-strong);
    font-size: var(--font-size-85);
    /* UIUX-1 (R-A): 状態表示のみになったheader。狭幅では折返し (省略なし) */
    flex-wrap: wrap;
  }

  .app-title {
    font-size: var(--font-size-100);
    margin: 0;
  }

  .health-dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: var(--color-text-subtle);
  }

  .health-dot.ok {
    background: var(--color-success);
  }

  .health-dot.ng {
    background: var(--color-danger);
  }

  .source-badge {
    padding: 0.1rem 0.5rem;
    border-radius: var(--radius-pill);
    font-size: var(--font-size-75);
    border: 1px solid var(--color-border-strong);
    color: var(--color-text-muted);
  }

  .source-badge.library {
    border-color: var(--color-success-emphasis);
    color: var(--color-success);
  }

  .source-badge.search {
    border-color: light-dark(#8250df, #a371f7);
    color: light-dark(#8250df, #a371f7);
  }

  /* UIUX-5/WP4: ヘッダー状態を1個のステータスボタン+popoverへ集約。 */
  .header-status-wrap {
    position: relative;
    flex: none;
  }

  .header-status {
    font: inherit;
    font-size: var(--font-size-85);
    line-height: 1;
    padding: var(--space-15) var(--space-30);
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    background: transparent;
    color: var(--color-text-subtle);
    cursor: pointer;
  }

  .header-status:hover {
    background: var(--color-bg-subtle);
  }

  /* 忙碌 (スキャン/ハッシュ/索引構築中) = accent色。警告は忙碌より優先。 */
  .header-status.busy {
    color: var(--color-accent);
  }

  .header-status.warn {
    color: var(--color-warning);
    font-weight: bold;
  }

  .header-status-popover {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: var(--space-40);
    min-width: 16rem;
    max-width: 28rem;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-25);
    padding: var(--space-60);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background: var(--color-surface-raised);
    box-shadow: var(--shadow-popover);
    z-index: 600;
    font-size: var(--font-size-75);
  }

  .header-status-row {
    margin: 0;
    color: var(--color-accent);
  }

  .header-status-row.warn {
    color: var(--color-warning);
    font-weight: bold;
  }

  .header-status-detail {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-75);
  }

  .header-status-empty {
    margin: 0;
    color: var(--color-text-muted);
  }

  .header-spacer {
    flex: 1;
  }

  .header-notice {
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
    max-width: 24rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。 */
  button {
    font-size: var(--font-size-80);
  }
</style>
