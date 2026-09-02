<script lang="ts">
  /**
   * 開発パネル (MVP WP-4。dev bridge有効時のみ表示)。
   *
   * スパイク由来の検証UI (仮想カタログ・サムネイル統計・エラー注入・
   * プロトコルスモーク・パフォーマンス・生ヘルス) を製品シェルの外へ退避する。
   * dev bridgeの計測ドライバがボタン語句 (スキャン開始/全件生成/…) を
   * 辿るため、主要ボタン語句は維持すること。
   */
  import { onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import type { CatalogStore, CatalogPage, CatalogSummary } from "../catalog";
  import {
    ThumbnailStore,
    formatBytes,
    formatHitRate,
    formatMs,
    type RequestAccepted,
    type ThumbnailStats,
  } from "../thumbnails";
  import type { HealthInfo } from "../health";
  import type { PerfMetrics } from "../perf";
  import { previewUrl, thumbUrl, originalUrl } from "../protocol";

  let {
    catalog,
    thumbnails,
    perf,
    rawHealth,
    fetchedAt,
    onRefreshHealth,
    onCacheCleared,
  }: {
    catalog: CatalogStore;
    thumbnails: ThumbnailStore;
    perf: PerfMetrics;
    rawHealth: HealthInfo | null;
    fetchedAt: string;
    onRefreshHealth: () => void;
    onCacheCleared: (cacheGeneration: number) => void;
  } = $props();

  let open = $state(true);
  let scanNotice = $state("");
  let thumbBusy = $state(false);
  let injectionBusy = $state(false);
  let thumbStats: ThumbnailStats | null = $state(null);
  let thumbNotice = $state("");
  let thumbError: string | null = $state(null);
  let thumbEventLog: string[] = $state([]);
  let smokeLines: string[] = $state([]);
  let smokeRunning = $state(false);
  let smokeResult = $state("");

  // svelte-ignore state_referenced_locally
  const stopThumbNotification = thumbnails.onChange(() => {
    thumbStats = thumbnails.stats;
    thumbNotice = thumbnails.lastNotice;
    thumbError = thumbnails.lastError;
    thumbEventLog = thumbnails.eventLog;
  });

  onDestroy(() => {
    stopThumbNotification();
  });

  function toErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
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
      onCacheCleared(cacheGeneration);
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

  // ===== プロトコルスモーク (WP-4スパイク資産の移植) =====

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

      const versionForOriginal = s2.version ?? s1.version ?? 1;
      const r3 = await loadImage(originalUrl(target.id, versionForOriginal));
      checks.push({ label: "S3", pass: r3.ok });
      push(
        `[S3] original v=${versionForOriginal} → ${r3.ok ? "LOAD OK" : "LOAD FAIL"} (${r3.detail})`,
      );

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

  function summaryNow() {
    return catalog.summary;
  }
</script>

<div class="dev-drawer" class:collapsed={!open}>
  <div class="dev-header">
    <button class="dev-toggle" onclick={() => (open = !open)}>
      {open ? "▼" : "▲"} 開発パネル (dev bridge)
    </button>
  </div>
  {#if open}
    <div class="dev-body">
      <section>
        <h4>カタログ</h4>
        {#if summaryNow() !== null}
          <p class="dev-line">
            generation: {summaryNow()?.generation} /
            モード: {summaryNow()?.mode === "virtual" ? "仮想" : summaryNow()?.mode === "library" ? "ライブラリ" : "実"} /
            総件数: {summaryNow()?.total.toLocaleString()}
            {summaryNow()?.scanning ? "/ 走査中" : ""}
          </p>
        {:else}
          <p class="dev-line">カタログ情報未取得</p>
        {/if}
        <!-- 仮想5万件カタログは計測用 (devtools機能)。通常releaseビルドでは
             backendのset_catalog_modeが存在しないため表示しない -->
        <button onclick={() => void switchMode(false)} disabled={summaryNow()?.scanning ?? false}>
          実カタログ
        </button>
        <button onclick={() => void switchMode(true)} disabled={summaryNow()?.scanning ?? false}>
          仮想カタログ (5万件)
        </button>
        {#if scanNotice}
          <p class="dev-error">{scanNotice}</p>
        {/if}
      </section>

      <section>
        <h4>サムネイル生成 (WP-3検証パネル)</h4>
        <button onclick={() => void generateAllThumbs()} disabled={thumbBusy}>
          全件生成 (S256)
        </button>
        <button onclick={() => void togglePause()} disabled={thumbBusy}>
          {thumbStats?.paused ? "再開" : "一時停止"}
        </button>
        <button onclick={() => void clearThumbCache()} disabled={thumbBusy}>
          キャッシュ全削除
        </button>
        <!-- エラー注入は計測用 (devtools機能)。通常releaseビルドではbackendの
             set_failure_injectionが存在しないため表示しない -->
        <div class="dev-line">
          <span>エラー注入 (WP-7):</span>
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
        {#if thumbNotice}
          <p class="dev-line">{thumbNotice}</p>
        {/if}
        {#if thumbError}
          <p class="dev-error">{thumbError}</p>
        {/if}
        {#if thumbStats}
          <div class="dev-stats">
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
          <p class="dev-line">統計未取得</p>
        {/if}
        {#if thumbEventLog.length > 0}
          <div class="dev-log">
            <!-- キーは行位置: ログ行は同一内容になり得る (同一秒・同件数の
                 ready等) ため内容キーだとeach_key_duplicateで効果が落ちる -->
            {#each thumbEventLog.slice(0, 8) as line, logIndex (logIndex)}
              <div class="dev-log-line">{line}</div>
            {/each}
          </div>
        {/if}
      </section>

      <section>
        <h4>プロトコルスモーク</h4>
        <button onclick={() => void runProtocolSmoke()} disabled={smokeRunning}>
          {smokeRunning ? "スモーク実行中…" : "スモーク実行"}
        </button>
        <p class="dev-line">
          thumb(s256) / preview2048 / original の表示とネガティブケース
          (未登録ID・トラバーサル・未知クエリ) を検証する。
        </p>
        {#if smokeResult}
          <p class={smokeResult.includes("PASS") ? "dev-ok" : "dev-error"}>{smokeResult}</p>
        {/if}
        {#if smokeLines.length > 0}
          <pre class="dev-pre">{smokeLines.join("\n")}</pre>
        {/if}
      </section>

      <section>
        <h4>パフォーマンス</h4>
        <div class="dev-stats">
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
        <h4>開発用ステータス</h4>
        <button onclick={onRefreshHealth}>再取得</button>
        <p class="dev-line">取得時刻: {fetchedAt || "-"}</p>
        <pre class="dev-pre">{rawHealth ? JSON.stringify(rawHealth, null, 2) : "{}"}</pre>
      </section>
    </div>
  {/if}
</div>

<style>
  .dev-drawer {
    border-top: 1px solid light-dark(#d0d7de, #30363d);
    background: light-dark(#f6f8fa, #0d1117);
    font-size: 0.8rem;
    flex: none;
    max-height: 40vh;
    display: flex;
    flex-direction: column;
  }

  .dev-drawer.collapsed {
    max-height: 2rem;
  }

  .dev-header {
    flex: none;
    padding: 0.15rem 0.5rem;
  }

  .dev-toggle {
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
    padding: 0.15rem 0;
  }

  .dev-body {
    overflow-y: auto;
    padding: 0 0.75rem 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
  }

  .dev-body h4 {
    margin: 0.2rem 0;
    font-size: 0.8rem;
  }

  .dev-line {
    margin: 0.2rem 0;
  }

  .dev-stats {
    display: grid;
    gap: 0.25rem;
    font-size: 0.75rem;
  }

  .dev-pre {
    background: light-dark(#f5f5f5, #1e1e1e);
    padding: 0.5rem;
    border-radius: 0.5rem;
    overflow-x: auto;
    font-size: 0.72rem;
    margin: 0.25rem 0;
  }

  .dev-log {
    margin-top: 0.5rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.5rem;
    max-height: 8rem;
    overflow-y: auto;
    padding: 0.5rem;
    font-size: 0.7rem;
  }

  .dev-log-line {
    font-family: monospace;
    white-space: nowrap;
  }

  .dev-ok {
    color: light-dark(#1a7f37, #3fb950);
    font-weight: bold;
  }

  .dev-error {
    color: light-dark(#cf222e, #f85149);
    font-weight: bold;
  }

  button.active {
    border-color: light-dark(#0969da, #58a6ff);
    font-weight: bold;
  }

  button {
    margin-right: 0.4rem;
  }
</style>
