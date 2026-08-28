<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { normalizeHealth, type HealthInfo, type HealthUiState } from "./lib/health";
  import {
    CatalogStore,
    type CatalogIpc,
    type CatalogSummary,
    type ImageListItem,
    type ScanProgressEvent,
  } from "./lib/catalog";
  import {
    ThumbnailStore,
    formatBytes,
    formatHitRate,
    formatMs,
    type ThumbnailIpc,
    type ThumbnailStats,
  } from "./lib/thumbnails";

  let healthStatus: HealthUiState | null = $state(null);
  let rawHealth: HealthInfo | null = $state(null);
  let fetchedAt = $state("");
  let scanNotice = $state("");

  // カタログストアはプレーンなTSクラス ($stateはクラスインスタンスを深く
  // プロキシしないため、変更通知でローカル$state変数へ同期する)。
  const ipc: CatalogIpc = { invoke, listen };
  const catalog = new CatalogStore(ipc);

  let summary: CatalogSummary | null = $state(null);
  let progress: ScanProgressEvent | null = $state(null);
  let pageItems: ImageListItem[] = $state([]);
  let pageOffset = $state(0);
  let pageHasMore = $state(false);
  let catalogError: string | null = $state(null);

  // サムネイル開発パネル (WP-3): 統計ポーリング + 全件生成/一時停止/全削除。
  // 表示本体はWP-4/5。イベントはログ表示のみ。
  const thumbIpc: ThumbnailIpc = { invoke, listen };
  const thumbnails = new ThumbnailStore(thumbIpc);
  let thumbStats: ThumbnailStats | null = $state(null);
  let thumbNotice = $state("");
  let thumbError: string | null = $state(null);
  let thumbEventLog: string[] = $state([]);
  let thumbBusy = $state(false);

  const stopChangeNotification = catalog.onChange(() => {
    summary = catalog.summary;
    progress = catalog.progress;
    pageItems = catalog.pageItems;
    pageOffset = catalog.pageOffset;
    pageHasMore = catalog.pageHasMore;
    catalogError = catalog.lastError;
  });

  const stopThumbNotification = thumbnails.onChange(() => {
    thumbStats = thumbnails.stats;
    thumbNotice = thumbnails.lastNotice;
    thumbError = thumbnails.lastError;
    thumbEventLog = thumbnails.eventLog;
  });

  let disposeCatalog: (() => void) | null = null;
  let disposeThumbnails: (() => void) | null = null;

  function toErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  async function refreshHealth(): Promise<void> {
    try {
      const response = await invoke<HealthInfo>("health");
      rawHealth = response;
      healthStatus = normalizeHealth(response);
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
    await thumbnails.clearCache();
    thumbBusy = false;
  }

  function formatDimensions(width: number | null, height: number | null): string {
    if (width === null || height === null) {
      return "?";
    }
    return `${width}×${height}`;
  }

  onMount(() => {
    void refreshHealth();
    void catalog.init().then((dispose) => {
      disposeCatalog = dispose;
    });
    void thumbnails.init().then((dispose) => {
      disposeThumbnails = dispose;
    });
  });

  onDestroy(() => {
    stopChangeNotification();
    stopThumbNotification();
    disposeCatalog?.();
    disposeThumbnails?.();
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
    {#if catalogError}
      <p class="error">エラー: {catalogError}</p>
    {/if}
  </section>

  <section>
    <h2>ページプレビュー (先頭200件・単純テキスト)</h2>
    <p>offset {pageOffset} / 全 {summary?.total ?? 0} 件 {pageHasMore ? "(続きあり)" : ""}</p>
    <div class="page-preview">
      {#if pageItems.length === 0}
        <p>(項目なし。スキャン開始で表示されます)</p>
      {:else}
        {#each pageItems as item (item.id)}
          <div class="page-row">
            <span class="row-name">{item.displayName}</span>
            <span class="row-meta">
              {formatDimensions(item.width, item.height)} /
              {formatBytes(item.byteSize)} /
              {item.state}
            </span>
          </div>
        {/each}
      {/if}
    </div>
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
          キュー {thumbStats.queue_depth.toLocaleString()} /
          容量 {formatBytes(thumbStats.cache_bytes)}
          {thumbStats.paused ? " / 一時停止中" : ""}
        </div>
        <div>
          生成時間 p50 {formatMs(thumbStats.p50_ms)} /
          p95 {formatMs(thumbStats.p95_ms)} /
          p99 {formatMs(thumbStats.p99_ms)}
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
    <h2>開発用ステータス</h2>
    <p>取得時刻: {fetchedAt || "-"}</p>
    <pre>{rawHealth ? JSON.stringify(rawHealth, null, 2) : "{}"}</pre>
  </section>
</main>

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

  .page-preview {
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.5rem;
    max-height: 20rem;
    overflow-y: auto;
    padding: 0.5rem;
  }

  .page-row {
    display: flex;
    gap: 0.75rem;
    justify-content: space-between;
    font-size: 0.875rem;
    padding: 0.125rem 0.25rem;
  }

  .page-row:nth-child(odd) {
    background: light-dark(#fafbfc, #161b22);
  }

  .row-name {
    overflow-wrap: anywhere;
  }

  .row-meta {
    white-space: nowrap;
    color: light-dark(#57606a, #8b949e);
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

  button {
    margin-right: 0.5rem;
  }

  button:disabled {
    opacity: 0.5;
  }
</style>
