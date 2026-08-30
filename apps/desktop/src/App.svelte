<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { normalizeHealth, type HealthInfo, type HealthUiState } from "./lib/health";
  import {
    CatalogStore,
    type CatalogIpc,
    type CatalogPage,
    type CatalogSummary,
    type ScanProgressEvent,
  } from "./lib/catalog";
  import {
    ThumbnailStore,
    formatBytes,
    formatHitRate,
    formatMs,
    type RequestAccepted,
    type ThumbnailIpc,
    type ThumbnailStats,
  } from "./lib/thumbnails";
  import { originalUrl, previewUrl, thumbUrl } from "./lib/protocol";
  import { GridStore, type GridIpc } from "./lib/grid/grid-store";
  import { ScrollPositionStore } from "./lib/grid/scroll-position";
  import type { ThumbCellClass } from "./lib/grid/layout";
  import ThumbnailGrid from "./lib/grid/ThumbnailGrid.svelte";
  import { ViewerStore, type ViewerIpc } from "./lib/viewer/viewer-store";
  import ImageViewer from "./lib/viewer/ImageViewer.svelte";
  import { CompareStore, type CompareIpc } from "./lib/compare/compare-store";
  import CompareView from "./lib/compare/CompareView.svelte";
  import { perf } from "./lib/perf";

  let healthStatus: HealthUiState | null = $state(null);
  let rawHealth: HealthInfo | null = $state(null);
  let fetchedAt = $state("");
  let scanNotice = $state("");
  // 計測ブリッジ (dev bridge) が有効なビルド/セッションか。仮想カタログや
  // エラー注入など開発専用UIの表示に使う (WP-0)。
  let devBridgeActive = $state(false);

  // カタログストアはプレーンなTSクラス ($stateはクラスインスタンスを深く
  // プロキシしないため、変更通知でローカル$state変数へ同期する)。
  const ipc: CatalogIpc = { invoke, listen };
  const catalog = new CatalogStore(ipc);

  let summary: CatalogSummary | null = $state(null);
  let progress: ScanProgressEvent | null = $state(null);
  let catalogError: string | null = $state(null);

  // サムネイル開発パネル (WP-3): 統計ポーリング + 全件生成/一時停止/全削除。
  const thumbIpc: ThumbnailIpc = { invoke, listen };
  const thumbnails = new ThumbnailStore(thumbIpc);
  let thumbStats: ThumbnailStats | null = $state(null);
  let thumbNotice = $state("");
  let thumbError: string | null = $state(null);
  let thumbEventLog: string[] = $state([]);
  let thumbBusy = $state(false);
  let injectionBusy = $state(false);

  // プロトコルスモークパネル (WP-4): gento-thumb/preview/original の
  // 表示とネガティブケースをアプリ内で証明する (結果はOCR可能なテキスト行)。
  let smokeLines: string[] = $state([]);
  let smokeRunning = $state(false);
  let smokeResult = $state("");

  // 仮想サムネイルグリッド (WP-5)。
  const grid = new GridStore({ invoke, listen } satisfies GridIpc);
  const scrollPositions = new ScrollPositionStore(localStorage);
  let cellClass = $state<ThumbCellClass>("s256");
  let overscanChoice = $state(2);
  let gridComponent: ThumbnailGrid | null = $state(null);
  let gridModeKey = $state("real");
  let restoreNotice = $state("");

  // ビューア/比較 (WP-6): グリッドのEnter/ダブルクリックまたはボタンで開く。
  type ViewerMode = "single" | "compare2" | "compare4";
  let viewerChoice: ViewerMode = $state("single");
  let viewerMode: ViewerMode | null = $state(null);
  let viewerStore: ViewerStore | null = $state(null);
  let compareStore: CompareStore | null = $state(null);
  let viewerNotice = $state("");
  let gridSelection: { ids: string[]; focusIndex: number | null } = { ids: [], focusIndex: null };

  const viewerIpc: ViewerIpc = { invoke, listen };
  const compareIpc: CompareIpc = { invoke, listen };
  const viewerItems = {
    total: () => grid.summary?.total ?? 0,
    get: (index: number) => grid.getItem(index),
    ensureRange: (start: number, end: number) => grid.pager.ensureRange(start, end),
  };
  const compareItems = {
    ...viewerItems,
    findIndexById: (id: string) => grid.pager.findIndicesByIds(new Set([id])).get(id) ?? null,
  };

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
        const store = new CompareStore(compareIpc, compareItems);
        compareStore = store;
        viewerMode = viewerChoice;
        await store.init();
        await store.open(count, gridSelection.ids, focus);
      }
    } catch (error) {
      viewerNotice = toErrorMessage(error);
    }
  }

  function setViewerChoice(choice: ViewerMode): void {
    viewerChoice = choice;
  }

  const stopChangeNotification = catalog.onChange(() => {
    summary = catalog.summary;
    progress = catalog.progress;
    catalogError = catalog.lastError;
    if (catalog.summary !== null) {
      gridModeKey = catalog.summary.mode;
      grid.syncSummary(catalog.summary);
    }
  });

  // 起動タイムマーク: 初回ページ受信 (WP-7 / §16)。ページ取得はgridの
  // ensureRange完了通知 (grid.onChange) で観測する
  const stopGridNotifyForPerf = grid.onChange(() => {
    if (!markedFirstPage && grid.pager.stats.fetched > 0) {
      markedFirstPage = true;
      perf.markFirstPage();
    }
  });

  const stopThumbNotification = thumbnails.onChange(() => {
    thumbStats = thumbnails.stats;
    thumbNotice = thumbnails.lastNotice;
    thumbError = thumbnails.lastError;
    thumbEventLog = thumbnails.eventLog;
  });

  let disposeCatalog: (() => void) | null = null;
  let disposeThumbnails: (() => void) | null = null;
  let disposeGrid: (() => void) | null = null;
  let markedFirstPage = false;

  function toErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  async function refreshHealth(): Promise<void> {
    try {
      const response = await invoke<HealthInfo>("health");
      rawHealth = response;
      healthStatus = normalizeHealth(response);
      perf.markHealthOk();
    } catch (error) {
      rawHealth = null;
      healthStatus = { kind: "error", message: toErrorMessage(error) };
    }
    fetchedAt = new Date().toLocaleTimeString();
  }

  async function startScan(): Promise<void> {
    scanNotice = "";
    const ok = await catalog.startScan();
    if (!ok) {
      scanNotice = catalog.lastError ?? "スキャン開始に失敗しました";
    }
  }

  async function switchMode(virtual: boolean): Promise<void> {
    scanNotice = "";
    const ok = await catalog.setMode(virtual);
    if (!ok) {
      scanNotice = catalog.lastError ?? "モード切替に失敗しました";
    }
  }

  async function generateAllThumbs(): Promise<void> {
    thumbBusy = true;
    await thumbnails.generateAll("s256");
    thumbBusy = false;
  }

  async function togglePause(): Promise<void> {
    thumbBusy = true;
    await thumbnails.setPaused(!(thumbStats?.paused ?? false));
    thumbBusy = false;
  }

  async function clearThumbCache(): Promise<void> {
    thumbBusy = true;
    const cacheGeneration = await thumbnails.clearCache();
    if (cacheGeneration !== null) {
      grid.noteCacheCleared(cacheGeneration);
    }
    thumbBusy = false;
  }

  async function setInjection(
    mode: "none" | "decode" | "cache_write",
    rate: number,
  ): Promise<void> {
    injectionBusy = true;
    await thumbnails.setInjection(mode, rate);
    injectionBusy = false;
  }

  function setCellClass(next: ThumbCellClass): void {
    cellClass = next;
    grid.setSizeClass(next);
  }

  function restoreScrollPosition(): void {
    restoreNotice = "";
    if (gridComponent === null) {
      restoreNotice = "グリッドが未初期化です";
      return;
    }
    gridComponent.restoreFromStorage();
    restoreNotice = "位置を復元しました";
  }

  // ===== プロトコルスモーク (WP-4) =====

  type LoadResult = { ok: boolean; detail: string };

  function loadImage(url: string, timeoutMs = 20000): Promise<LoadResult> {
    return new Promise((resolve) => {
      const img = new Image();
      let settled = false;
      const finish = (result: LoadResult) => {
        if (!settled) {
          settled = true;
          clearTimeout(timer);
          img.onload = null;
          img.onerror = null;
          resolve(result);
        }
      };
      const timer = setTimeout(() => finish({ ok: false, detail: "TIMEOUT" }), timeoutMs);
      img.onload = () =>
        finish(
          img.naturalWidth > 0
            ? { ok: true, detail: `${img.naturalWidth}x${img.naturalHeight}` }
            : { ok: false, detail: "naturalWidth=0" },
        );
      img.onerror = () => finish({ ok: false, detail: "onerror" });
      img.src = url;
    });
  }

  /** 生成を要求し、その資産の thumb:ready を待つ (versionを返す)。 */
  async function requestAndWaitReady(
    assetId: string,
    sizeClass: string,
  ): Promise<{ version: number | null; note: string }> {
    let unlisten: (() => void) | null = null;
    const readyPromise = new Promise<number | null>((resolve) => {
      let done = false;
      const timer = setTimeout(() => {
        done = true;
        resolve(null);
      }, 90000);
      void listen<{
        items: Array<{
          asset_id: string;
          size_class: string;
          cache_generation: number;
          version: number;
        }>;
      }>("thumb:ready", (event) => {
        if (done) {
          return;
        }
        const hit = event.payload.items.find(
          (item) => item.asset_id === assetId && item.size_class === sizeClass,
        );
        if (hit) {
          done = true;
          clearTimeout(timer);
          resolve(hit.version);
        }
      }).then((un) => {
        unlisten = un;
        if (done) {
          un();
        }
      });
    });
    const accepted = await invoke<RequestAccepted>("request_thumbnails", {
      ids: [assetId],
      sizeClass,
      priority: "interactive",
    });
    const version = await readyPromise;
    // CFAはthenコールバック内の代入を追跡しないため、明示的に読み直す
    const stop = unlisten as (() => void) | null;
    stop?.();
    const note = `受付 q=${accepted.queued} dup=${accepted.rejected_dup} hit=${accepted.cache_hits}`;
    return { version, note };
  }

  /** 先頭のreadyなカタログ項目を1件取得する (staleGenerationは1回だけ再試行)。 */
  async function fetchFirstReadyItem(): Promise<{ id: string; name: string } | null> {
    for (let attempt = 0; attempt < 2; attempt++) {
      const summary = await invoke<CatalogSummary>("get_catalog_summary");
      try {
        const page = await invoke<CatalogPage>("get_catalog_page", {
          generation: summary.generation,
          offset: 0,
          limit: 5,
        });
        const item = page.items.find((entry) => entry.state === "ready");
        return item ? { id: item.id, name: item.displayName } : null;
      } catch {
        // staleGeneration: summaryを引き直して再試行
      }
    }
    return null;
  }

  async function runProtocolSmoke(): Promise<void> {
    if (smokeRunning) {
      return;
    }
    smokeRunning = true;
    smokeResult = "";
    const lines: string[] = [];
    const checks: Array<{ label: string; pass: boolean }> = [];
    const push = (line: string) => {
      lines.push(line);
      smokeLines = [...lines];
    };

    try {
      const target = await fetchFirstReadyItem();
      if (!target) {
        push("target: カタログにready項目なし (スキャン後に再実行すること)");
        push("SMOKE RESULT: FAIL (0/0)");
        smokeResult = "SMOKE RESULT: FAIL (カタログ空)";
        return;
      }
      push(`target: id=${target.id} (${target.name})`);

      // S1: サムネイル (s256) — 要求 → thumb:ready → URL更新 → 表示切替
      const s1 = await requestAndWaitReady(target.id, "s256");
      if (s1.version === null) {
        checks.push({ label: "S1", pass: false });
        push(`[S1] thumb s256 → FAIL (ready未受信 ${s1.note})`);
      } else {
        const r = await loadImage(thumbUrl(target.id, "s256", s1.version));
        checks.push({ label: "S1", pass: r.ok });
        push(
          `[S1] thumb s256 v=${s1.version} ${s1.note} → ${r.ok ? "LOAD OK" : "LOAD FAIL"} (${r.detail})`,
        );
      }

      // S2: preview2048
      const s2 = await requestAndWaitReady(target.id, "preview2048");
      if (s2.version === null) {
        checks.push({ label: "S2", pass: false });
        push(`[S2] preview2048 → FAIL (ready未受信 ${s2.note})`);
      } else {
        const r = await loadImage(previewUrl(target.id, s2.version));
        checks.push({ label: "S2", pass: r.ok });
        push(
          `[S2] preview2048 v=${s2.version} ${s2.note} → ${r.ok ? "LOAD OK" : "LOAD FAIL"} (${r.detail})`,
        );
      }

      // S3: original (元ファイル。生成不要。vは内容感応versionを流用)
      const versionForOriginal = s2.version ?? s1.version ?? 1;
      const r3 = await loadImage(originalUrl(target.id, versionForOriginal));
      checks.push({ label: "S3", pass: r3.ok });
      push(
        `[S3] original v=${versionForOriginal} → ${r3.ok ? "LOAD OK" : "LOAD FAIL"} (${r3.detail})`,
      );

      // ネガティブ: いずれも onerror にならなければならない
      const negatives: Array<[string, string]> = [
        ["N1a 未登録ID (localhost形式)", "gento-thumb://localhost/asset/999999999/s256?v=1"],
        ["N1b 未登録ID (WP指定形式)", "gento-thumb://asset/999999999/s256"],
        ["N2 トラバーサル", "gento-thumb://../../../etc/passwd"],
        ["N3a 未知クエリ (localhost形式)", `gento-thumb://localhost/asset/${target.id}/s256?x=1`],
        ["N3b 未知クエリ (WP指定形式)", `gento-thumb://asset/${target.id}/s256?x=1`],
      ];
      for (const [label, url] of negatives) {
        const r = await loadImage(url, 15000);
        checks.push({ label, pass: !r.ok });
        push(`[${label}] → ${!r.ok ? "ERROR OK" : "BAD: LOADED"} (${r.detail})`);
      }

      const passed = checks.filter((c) => c.pass).length;
      const allPass = passed === checks.length && checks.length > 0;
      push(`SMOKE RESULT: ${allPass ? "PASS" : "FAIL"} (${passed}/${checks.length})`);
      smokeResult = `SMOKE RESULT: ${allPass ? "PASS" : "FAIL"} (${passed}/${checks.length})`;
    } catch (error) {
      push(`SMOKE RESULT: FAIL (例外: ${toErrorMessage(error)})`);
      smokeResult = `SMOKE RESULT: FAIL (${toErrorMessage(error)})`;
    } finally {
      smokeRunning = false;
    }
  }

  onMount(() => {
    perf.markMounted();
    perf.startLongTaskObserver();
    perf.startFrameMonitor();
    void refreshHealth();
    void catalog.init().then((dispose) => {
      disposeCatalog = dispose;
    });
    void thumbnails.init().then((dispose) => {
      disposeThumbnails = dispose;
    });
    void grid.init().then((dispose) => {
      disposeGrid = dispose;
    });
    // 実機検証ブリッジ (WP-6/WP-7)。dev build、またはdevtoolsビルドのreleaseで
    // GENTO_DEV_BRIDGE=1 のときのみ有効 (WP-0)。backend側にcommandが無い
    // (通常release) 場合はinvokeがrejectされ、ブリッジも開発パネル (仮想
    // カタログ・エラー注入) も無効のままになる
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
    stopThumbNotification();
    stopGridNotifyForPerf();
    disposeCatalog?.();
    disposeThumbnails?.();
    disposeGrid?.();
    perf.stop();
  });
</script>

<main>
  <h1>GEN-TO 技術検証プロトタイプ</h1>

  <section>
    <h2>Rustバックエンド接続状態</h2>
    {#if healthStatus === null}
      <p>接続確認中…</p>
    {:else if healthStatus.kind === "ok"}
      <p class="ok">接続OK</p>
      <p>
        データセット:
        {healthStatus.datasetPath ?? "(未設定)"}
        ({healthStatus.datasetPresent ? "あり" : "なし"})
      </p>
      <p>
        データベース:
        {healthStatus.dbOpen ? "接続" : "エラー (詳細はdb_status)"}
      </p>
    {:else}
      <p class="error">接続エラー: {healthStatus.message}</p>
    {/if}
    <button onclick={() => void refreshHealth()}>再取得</button>
  </section>

  <section>
    <h2>スキャン</h2>
    <button onclick={() => void startScan()} disabled={summary?.scanning ?? false}>
      スキャン開始
    </button>
    {#if scanNotice}
      <p class="error">{scanNotice}</p>
    {/if}
    {#if progress}
      <p>
        走査: {progress.scanned} 件 /
        発見: {progress.found} 件 /
        エラー: {progress.errors} 件 /
        経過: {progress.elapsed_ms} ms
        {progress.done ? "(完了)" : ""}
      </p>
    {/if}
  </section>

  <section>
    <h2>カタログ</h2>
    {#if summary}
      <p>
        generation: {summary.generation} /
        モード: {summary.mode === "virtual" ? "仮想" : "実"} /
        総件数: {summary.total.toLocaleString()}
        {summary.scanning ? "/ 走査中" : ""}
      </p>
      <p>
        最終走査: {summary.scanned_count.toLocaleString()} 件スキャン,
        エラー {summary.error_count.toLocaleString()} 件,
        所要 {summary.last_scan_elapsed_ms ?? "-"} ms
      </p>
    {:else}
      <p>カタログ情報未取得</p>
    {/if}
    {#if devBridgeActive}
      <!-- 仮想5万件カタログは計測用 (devtools機能)。通常releaseビルドでは
           backendのset_catalog_modeが存在しないため表示しない -->
      <button
        onclick={() => void switchMode(false)}
        disabled={summary?.scanning ?? false}
      >
        実カタログ
      </button>
      <button
        onclick={() => void switchMode(true)}
        disabled={summary?.scanning ?? false}
      >
        仮想カタログ (5万件)
      </button>
    {/if}
    {#if catalogError}
      <p class="error">エラー: {catalogError}</p>
    {/if}
  </section>

  <section>
    <h2>サムネイルグリッド (WP-5)</h2>
    <div class="grid-controls">
      <button class:active={cellClass === "s256"} onclick={() => setCellClass("s256")}>
        セル小 (128px / s256)
      </button>
      <button class:active={cellClass === "s512"} onclick={() => setCellClass("s512")}>
        セル大 (256px / s512)
      </button>
      <label>
        overscan
        <select
          value={overscanChoice}
          onchange={(event) => {
            overscanChoice = Number(event.currentTarget.value);
          }}
        >
          {#each [0, 2, 5, 10] as value (value)}
            <option value={value}>{value}</option>
          {/each}
        </select>
      </label>
      <button onclick={() => restoreScrollPosition()}>位置復元</button>
      {#if restoreNotice}
        <span class="notice">{restoreNotice}</span>
      {/if}
    </div>
    <div class="grid-controls">
      <span class="notice">ビューア (Enter/ダブルクリックで開く):</span>
      <button class:active={viewerChoice === "single"} onclick={() => setViewerChoice("single")}>
        単一
      </button>
      <button class:active={viewerChoice === "compare2"} onclick={() => setViewerChoice("compare2")}>
        2枚比較
      </button>
      <button class:active={viewerChoice === "compare4"} onclick={() => setViewerChoice("compare4")}>
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
      {#if viewerNotice}
        <span class="error">{viewerNotice}</span>
      {/if}
    </div>
    {#key `${gridModeKey}-${cellClass}`}
      <ThumbnailGrid
        bind:this={gridComponent}
        {grid}
        scrollStore={scrollPositions}
        modeKey={gridModeKey}
        {cellClass}
        overscanRows={overscanChoice}
        onOpen={(index) => void openViewerForChoice(index)}
        onSelectionChange={(snapshot) => {
          gridSelection = snapshot;
        }}
      />
    {/key}
  </section>

  <section>
    <h2>サムネイル生成 (WP-3検証パネル)</h2>
    <div class="thumb-buttons">
      <button onclick={() => void generateAllThumbs()} disabled={thumbBusy}>
        全件生成 (S256)
      </button>
      <button onclick={() => void togglePause()} disabled={thumbBusy}>
        {thumbStats?.paused ? "再開" : "一時停止"}
      </button>
      <button onclick={() => void clearThumbCache()} disabled={thumbBusy}>
        キャッシュ全削除
      </button>
    </div>
    {#if devBridgeActive}
      <!-- エラー注入は計測用 (devtools機能)。通常releaseビルドではbackendの
           set_failure_injectionが存在しないため表示しない -->
      <div class="thumb-buttons">
        <span class="notice">エラー注入 (WP-7):</span>
        <button
          class:active={thumbStats?.injection_mode === "none"}
          onclick={() => void setInjection("none", 0)}
          disabled={injectionBusy}
        >
          なし
        </button>
        <button
          class:active={thumbStats?.injection_mode === "decode"}
          onclick={() => void setInjection("decode", 0.3)}
          disabled={injectionBusy}
        >
          decode失敗 30%
        </button>
        <button
          class:active={thumbStats?.injection_mode === "cache_write"}
          onclick={() => void setInjection("cache_write", 0.3)}
          disabled={injectionBusy}
        >
          キャッシュ書込失敗 30%
        </button>
      </div>
    {/if}
    {#if thumbNotice}
      <p class="notice">{thumbNotice}</p>
    {/if}
    {#if thumbError}
      <p class="error">{thumbError}</p>
    {/if}
    {#if thumbStats}
      <div class="thumb-stats">
        <div>
          生成 <strong>{thumbStats.generated.toLocaleString()}</strong> 件 /
          ヒット <strong>{thumbStats.hits.toLocaleString()}</strong> /
          ミス <strong>{thumbStats.misses.toLocaleString()}</strong> /
          ヒット率 <strong>{formatHitRate(thumbStats.hit_rate)}</strong>
        </div>
        <div>
          失敗 {thumbStats.failures.toLocaleString()} /
          退避 {thumbStats.evictions.toLocaleString()} /
          キュー {thumbStats.queue_depth.toLocaleString()}
          (i {thumbStats.queue_depths.interactive} /
          v {thumbStats.queue_depths.viewport} /
          p {thumbStats.queue_depths.prefetch} /
          o {thumbStats.queue_depths.overscan} /
          b {thumbStats.queue_depths.background}) /
          容量 {formatBytes(thumbStats.cache_bytes)}
          {thumbStats.paused ? " / 一時停止中" : ""}
        </div>
        <div>
          生成時間 p50 {formatMs(thumbStats.p50_ms)} /
          p95 {formatMs(thumbStats.p95_ms)} /
          p99 {formatMs(thumbStats.p99_ms)}
        </div>
        <div>
          プロトコル {thumbStats.proto_requests.toLocaleString()} 件 /
          OK {thumbStats.proto_ok.toLocaleString()} /
          転送 {formatBytes(thumbStats.proto_bytes_sent)} /
          p50 {formatMs(thumbStats.proto_p50_ms)} /
          p95 {formatMs(thumbStats.proto_p95_ms)} /
          p99 {formatMs(thumbStats.proto_p99_ms)}
        </div>
      </div>
    {:else}
      <p>統計未取得</p>
    {/if}
    {#if thumbEventLog.length > 0}
      <div class="event-log">
        {#each thumbEventLog.slice(0, 8) as line (line)}
          <div class="log-line">{line}</div>
        {/each}
      </div>
    {/if}
  </section>

  <section>
    <h2>プロトコルスモーク (WP-4)</h2>
    <div class="thumb-buttons">
      <button onclick={() => void runProtocolSmoke()} disabled={smokeRunning}>
        {smokeRunning ? "スモーク実行中…" : "スモーク実行"}
      </button>
    </div>
    <p class="notice">
      手順: スキャン完了後に実行。thumb(s256) / preview2048 / original の表示と
      ネガティブケース (未登録ID・トラバーサル・未知クエリ) を検証する。
    </p>
    {#if smokeResult}
      <p class={smokeResult.includes("PASS") ? "ok" : "error"}>{smokeResult}</p>
    {/if}
    {#if smokeLines.length > 0}
      <pre class="smoke-log">{smokeLines.join("\n")}</pre>
    {/if}
  </section>

  <section>
    <h2>パフォーマンス (WP-7計測)</h2>
    <div class="thumb-stats">
      <div>
        long task {perf.longTasks.toLocaleString()} 件
        (最悪 {perf.worstLongTaskMs === null ? "—" : `${Math.round(perf.worstLongTaskMs)} ms`})
        / observer {perf.longTaskObserverSupported === null ? "未開始" : perf.longTaskObserverSupported ? "対応" : "非対応"}
      </div>
      <div>
        long frame (≥50ms) {perf.longFrames.toLocaleString()} 件
        (最悪 {perf.worstLongFrameMs === null ? "—" : `${Math.round(perf.worstLongFrameMs)} ms`})
      </div>
      <div>
        起動: mount {Math.round(perf.marks.mountedAt ?? 0)} ms /
        health OK +{perf.startupDurations().mountToHealthMs === null ? "—" : `${Math.round(perf.startupDurations().mountToHealthMs as number)} ms`} /
        初回ページ +{perf.startupDurations().mountToFirstPageMs === null ? "—" : `${Math.round(perf.startupDurations().mountToFirstPageMs as number)} ms`}
      </div>
    </div>
  </section>

  <section>
    <h2>開発用ステータス</h2>
    <p>取得時刻: {fetchedAt || "-"}</p>
    <pre>{rawHealth ? JSON.stringify(rawHealth, null, 2) : "{}"}</pre>
  </section>
</main>

{#if viewerMode === "single" && viewerStore !== null}
  <ImageViewer store={viewerStore} onClose={closeViewer} />
{:else if (viewerMode === "compare2" || viewerMode === "compare4") && compareStore !== null}
  <CompareView store={compareStore} onClose={closeViewer} />
{/if}

<style>
  main {
    max-width: 60rem;
    margin: 0 auto;
    padding: 1.5rem;
  }

  h1 {
    font-size: 1.5rem;
  }

  section {
    margin-top: 1.5rem;
  }

  pre {
    background: light-dark(#f5f5f5, #1e1e1e);
    padding: 0.75rem;
    border-radius: 0.5rem;
    overflow-x: auto;
  }

  .grid-controls {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }

  .grid-controls label {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-size: 0.875rem;
  }

  .grid-controls button.active {
    border-color: light-dark(#0969da, #58a6ff);
    font-weight: bold;
  }

  .ok {
    color: light-dark(#1a7f37, #3fb950);
    font-weight: bold;
  }

  .error {
    color: light-dark(#cf222e, #f85149);
    font-weight: bold;
  }

  .notice {
    color: light-dark(#57606a, #8b949e);
  }

  .thumb-buttons {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }

  .thumb-stats {
    display: grid;
    gap: 0.25rem;
    font-size: 0.875rem;
  }

  .event-log {
    margin-top: 0.5rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.5rem;
    max-height: 10rem;
    overflow-y: auto;
    padding: 0.5rem;
    font-size: 0.75rem;
  }

  .log-line {
    font-family: monospace;
    white-space: nowrap;
  }

  .smoke-log {
    font-family: monospace;
    font-size: 0.875rem;
    white-space: pre-wrap;
  }

  button {
    margin-right: 0.5rem;
  }

  button:disabled {
    opacity: 0.5;
  }
</style>
