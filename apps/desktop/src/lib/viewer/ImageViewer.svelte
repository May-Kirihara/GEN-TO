<script lang="ts">
  /**
    * 単一画像ビューアのオーバーレイ (WP-6 / フェーズ6 + WP-5高速選別)。
    *
    * - 表示: 現行レイヤー + 保持 (retained) レイヤーの積層。新画像のonloadまで
    *   旧画像を表示し続ける (空白を出さない)
    * - 操作: ホイール=画像送り (UIUX-3 V-4: 平常wheelで前/次。
    *   1ジェスチャ=1送りのaccumulator)。Alt/Ctrl/Meta+ホイール=ズーム
    *   (ポインタ中心・pinch=ctrl+wheelを保存)、左右端の‹ ›ボタン=送り
    *   (端でdisabled)、ドラッグ=パン、ダブルクリック=fit⇔actual、
    *   ←/→=前/次画像、+/-/0/1/r、Esc=閉じる
    * - WP-5 高速選別モード (トグル): 数字1-5=星評価、0=評価解除、X=除外、
    *   P/A=採用。入力後自動的に次の画像へ送る。OFF中は既存のズームキー
    *   (0=fit / 1=actual) が優先される。
    * - 画像要素は論理寸法 (実寸) で配置しCSS transformで操作する
    *   (preview→original差し替えで幾何不変)
    * - 開発統計: 表示準備時間 p50/p95/p99 (先読みon/off別)、center/zoomFactor数値
    */
  import { onDestroy, onMount } from "svelte";
  import { toCss, type ViewTransform } from "./transform";
  import { advanceWheelNav, initialWheelNavState, type WheelNavState } from "./wheel-nav";
  import {
    backgroundCss,
    backgroundLabel,
    loadBackground,
    nextBackground,
    safeLocalStorage,
    saveBackground,
    type ViewerBackground,
  } from "./background";
  import { parseTriageKey, triageActionLabel, type TriageKeyAction } from "../triage-keys";
  import type { ViewerStore } from "./viewer-store";

  let {
    store,
    onClose,
    triageMode = false,
    onToggleTriageMode,
    onTriageAction,
  }: {
    store: ViewerStore;
    onClose: () => void;
    /** 高速選別モード (Appが保持。ビューアを開いても引き継ぐ)。 */
    triageMode?: boolean;
    onToggleTriageMode?: () => void;
    /** 高速選別キーの適用 (AppがMetadataStore/undo/自動次送りを担う)。
     *  true=処理済み (自動次送りする)、false=編集不可。 */
    onTriageAction?: (action: TriageKeyAction) => boolean;
  } = $props();

  let overlayEl: HTMLElement | null = $state(null);
  let stageEl: HTMLElement | null = $state(null);
  let viewerTick = $state(0);
  /** 高速選別の直近の適用結果 (一瞬表示するフィードバック)。 */
  let triageFeedback: { label: string; ok: boolean } | null = $state(null);
  let triageFeedbackTimer: ReturnType<typeof setTimeout> | null = null;
  // 透過背景 (市松/白/黒)。localStorageから復元し切替時に永続化 (WP-3)
  let background: ViewerBackground = $state(loadBackground(safeLocalStorage()));
  const stopNotify = store.onChange(() => {
    viewerTick++;
  });

  function cycleBackground(): void {
    background = nextBackground(background);
    saveBackground(safeLocalStorage(), background);
    viewerTick++;
  }

  function currentBackgroundLabel(): string {
    void viewerTick;
    return backgroundLabel(background);
  }

  function stageBackgroundCss(): string {
    void viewerTick;
    return backgroundCss(background);
  }

  // ビューポート追従 (fitの再計算)
  $effect(() => {
    const element = stageEl;
    if (element === null || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) {
        store.setViewport(Math.round(rect.width), Math.round(rect.height));
      }
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  });

  // ホイール (UIUX-3 V-4): 修飾キー (Alt/Ctrl/Meta) 付き=ズーム (pinch=ctrl保存)、
  // 平常=画像送り (accumulator: 1ジェスチャ=1送り)。ビューアがジェスチャを
  // 握る間は常にpreventDefault (passive:falseで手動登録)。
  // CompareView (2/4枚比較) のwheelは別listenerで本変更の対象外。
  let wheelNav: WheelNavState = initialWheelNavState();
  $effect(() => {
    const element = stageEl;
    if (element === null) {
      return;
    }
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      if (event.altKey || event.ctrlKey || event.metaKey) {
        // modifierジェスチャはplain累積を引き継がない
        wheelNav = initialWheelNavState();
        const rect = element.getBoundingClientRect();
        store.zoomAt(
          { x: event.clientX - rect.left, y: event.clientY - rect.top },
          Math.exp(-event.deltaY * 0.0015),
        );
        return;
      }
      const out = advanceWheelNav(wheelNav, event.deltaY, performance.now());
      wheelNav = out.state;
      if (out.step !== 0) {
        store.nav(out.step);
      }
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      element.removeEventListener("wheel", onWheel);
    };
  });

  // ===== ドラッグパン =====

  let drag: { pointerId: number; lastX: number; lastY: number } | null = null;

  function onPointerDown(event: PointerEvent): void {
    if (event.button !== 0) {
      return;
    }
    drag = { pointerId: event.pointerId, lastX: event.clientX, lastY: event.clientY };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onPointerMove(event: PointerEvent): void {
    if (drag === null || drag.pointerId !== event.pointerId) {
      return;
    }
    store.panBy(event.clientX - drag.lastX, event.clientY - drag.lastY);
    drag.lastX = event.clientX;
    drag.lastY = event.clientY;
  }

  function onPointerUp(event: PointerEvent): void {
    if (drag !== null && drag.pointerId === event.pointerId) {
      drag = null;
    }
  }

  // ===== キーボード =====

  function isFormTarget(event: KeyboardEvent): boolean {
    const target = event.target as HTMLElement | null;
    return (
      target !== null &&
      (target.tagName === "INPUT" || target.tagName === "SELECT" || target.tagName === "TEXTAREA")
    );
  }

  function onKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (isFormTarget(event)) {
      return;
    }
    // 高速選別モード (WP-5): 1-5/0/X/P/A を選別操作へ優先適用し、
    // 成功時は自動次送りする。ズームキー (0=fit/1=actual) はモードOFF中のみ。
    if (triageMode) {
      const action = parseTriageKey(event.key, {
        ctrlKey: event.ctrlKey,
        metaKey: event.metaKey,
        altKey: event.altKey,
      });
      if (action !== null) {
        event.preventDefault();
        applyTriageAction(action);
        return;
      }
    }
    switch (event.key) {
      case "ArrowLeft":
        event.preventDefault();
        store.nav(-1);
        break;
      case "ArrowRight":
        event.preventDefault();
        store.nav(1);
        break;
      case "f":
      case "F":
        event.preventDefault();
        toggleFullscreen();
        break;
      case "+":
      case "=":
        event.preventDefault();
        store.zoomAt(centerPointer(), 1.25);
        break;
      case "-":
      case "_":
        event.preventDefault();
        store.zoomAt(centerPointer(), 0.8);
        break;
      case "0":
        event.preventDefault();
        store.setMode("fit");
        break;
      case "1":
        event.preventDefault();
        store.setMode("actual");
        break;
      case "r":
      case "R":
        event.preventDefault();
        store.setMode("fit");
        break;
    }
  }

  /** 高速選別キーの適用 + フィードバック表示。次送りはApp側で行う。 */
  function applyTriageAction(action: TriageKeyAction): void {
    const handled = onTriageAction?.(action) ?? false;
    const label = triageActionLabel(action);
    if (triageFeedbackTimer !== null) {
      clearTimeout(triageFeedbackTimer);
    }
    triageFeedback = { label: handled ? label : `${label} (編集不可)`, ok: handled };
    triageFeedbackTimer = setTimeout(() => {
      triageFeedback = null;
    }, 900);
  }

  function centerPointer(): { x: number; y: number } {
    return { x: store.viewport.width / 2, y: store.viewport.height / 2 };
  }

  /** 全画面切替 (§9.1。オーバーレイ要素をフルスクリーン化)。 */
  function toggleFullscreen(): void {
    const element = overlayEl;
    if (element === null) {
      return;
    }
    if (document.fullscreenElement === element) {
      void document.exitFullscreen().catch(() => undefined);
    } else {
      void element.requestFullscreen().catch(() => undefined);
    }
  }

  function isFullscreen(): boolean {
    return document.fullscreenElement === overlayEl;
  }

  function layerStyle(
    transform: ViewTransform,
    dims: { width: number; height: number },
  ): string {
    const css = toCss(transform, dims);
    return (
      `position:absolute; left:50%; top:50%; width:${dims.width}px; height:${dims.height}px;` +
      `transform: translate(-50%, -50%) translate(${css.tx}px, ${css.ty}px) scale(${css.scale});`
    );
  }

  // ===== テンプレート用getter (viewerTickを読み変更を伝える。WP-2/5の教訓) =====

  function currentImg() {
    void viewerTick;
    return store.current;
  }

  function retainedImg() {
    void viewerTick;
    return store.retained;
  }

  function retainedT() {
    void viewerTick;
    return store.retainedTransform;
  }

  function transformNow() {
    void viewerTick;
    return store.transform;
  }

  function prefetchOn() {
    void viewerTick;
    return store.prefetchEnabled;
  }

  function formatNum(value: number | null, digits = 3): string {
    return value === null ? "—" : value.toFixed(digits);
  }

  function latencyLine(label: string, log: { stats(): { count: number; p50: number | null; p95: number | null; p99: number | null } }): string {
    void viewerTick;
    const s = log.stats();
    return (
      `${label} n=${s.count} p50=${formatNum(s.p50, 1)}ms p95=${formatNum(s.p95, 1)}ms` +
      ` p99=${formatNum(s.p99, 1)}ms`
    );
  }

  function pendingLabel(): string {
    void viewerTick;
    return `読み込み中… (${store.displayIndex + 1})`;
  }

  /** UIUX-3 V-4: 送りボタンの端disabled (navと同じ非wrap clamp)。 */
  function navTotal(): number {
    void viewerTick;
    return store.total();
  }

  function navIndex(): number {
    void viewerTick;
    return store.displayIndex;
  }

  function statusLine(): string {
    void viewerTick;
    return (
      `applied=${store.stats.applied} stale=${store.stats.staleDropped}` +
      ` req=${store.stats.requests} preq=${store.stats.prefetchRequests}` +
      ` gen=${store.generation} index=${store.displayIndex}` +
      ` id=${store.current?.assetId ?? "—"}` +
      ` ${store.current?.isOriginal ? "ORIG" : "PREV"}` +
      `${store.current?.isAnimated ? "+ANIM" : ""}`
    );
  }

  function transformLine(): string {
    void viewerTick;
    return (
      `center=(${formatNum(store.transform.centerX)}, ${formatNum(store.transform.centerY)})` +
      ` zf=${formatNum(store.zoomFactor(), 2)} scale=${formatNum(store.transform.scale, 2)}` +
      ` mode=${store.transform.mode}`
    );
  }

  onMount(() => {
    overlayEl?.focus();
  });

  onDestroy(() => {
    stopNotify();
    if (triageFeedbackTimer !== null) {
      clearTimeout(triageFeedbackTimer);
    }
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="viewer-overlay" bind:this={overlayEl} tabindex="-1">
  <div class="viewer-toolbar">
    <button
      class:triage-on={triageMode}
      onclick={() => onToggleTriageMode?.()}
      title="数字1-5=評価 / 0=評価解除 / X=除外 / P=採用。入力後自動で次へ進みます"
    >
      高速選別: {triageMode ? "ON" : "OFF"}
    </button>
    <button onclick={() => store.setMode("fit")}>fit {triageMode ? "" : "(0)"}</button>
    <button onclick={() => store.setMode("actual")}>原寸 {triageMode ? "" : "(1)"}</button>
    <button onclick={() => store.setMode("fit")}>リセット (r)</button>
    <button onclick={() => toggleFullscreen()}>
      全画面 (f){isFullscreen() ? " ✓" : ""}
    </button>
    <button
      onclick={() => store.setOriginal(!(currentImg()?.isOriginal ?? false))}
      disabled={currentImg()?.isAnimated ?? false}
      title={currentImg()?.isAnimated ? "アニメーション画像は常にoriginal表示" : undefined}
    >
      {currentImg()?.isOriginal ? "previewへ戻す" : "original昇格"}
    </button>
    <button onclick={() => cycleBackground()}>背景: {currentBackgroundLabel()}</button>
    <button onclick={() => store.togglePrefetch()}>
      先読み: {prefetchOn() ? "ON" : "OFF"}
    </button>
    <button onclick={() => store.resetMetrics()}>統計リセット</button>
    <span class="spacer"></span>
    <button class="close" onclick={onClose}>閉じる (Esc)</button>
  </div>

  <!-- a11yは技術検証の対象外 (本体計画 §18)。ズーム/パン面はキーボード操作も別途提供 -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="viewer-stage"
    style={`background: ${stageBackgroundCss()};`}
    bind:this={stageEl}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    ondblclick={() => store.toggleFitActual()}
  >
    {#if retainedImg() !== null && retainedImg()!.url !== null && retainedT() !== null && retainedImg()!.width > 0}
      <img
        class="layer retained"
        src={retainedImg()!.url}
        alt=""
        draggable="false"
        style={layerStyle(retainedT()!, {
          width: retainedImg()!.width,
          height: retainedImg()!.height,
        })}
      />
    {/if}
    {#if currentImg() !== null && currentImg()!.url !== null}
      {#key currentImg()!.key}
        <img
          class="layer"
          src={currentImg()!.url}
          alt={currentImg()!.displayName}
          draggable="false"
          style={layerStyle(transformNow(), {
            width: currentImg()!.width,
            height: currentImg()!.height,
          })}
          onload={() => store.noteLoaded(currentImg()!.key)}
          onerror={() => store.noteLoadError(currentImg()!.key)}
        />
      {/key}
    {:else if currentImg() !== null}
      <div class="pending">{pendingLabel()}</div>
    {/if}
    {#if currentImg()?.note}
      <div class="note">{currentImg()!.note}</div>
    {/if}
    {#if triageFeedback !== null}
      <div class="triage-feedback" class:triage-ng={!triageFeedback.ok}>{triageFeedback.label}</div>
    {/if}
    <!-- UIUX-3 V-4: 左右端の送りボタン (半透明・大的確。フルスクリーンでも表示)。
         total≤1では両方disabledで描画 (枚数による出現/消滅のちらつきを避ける) -->
    <button
      class="viewer-nav viewer-nav-prev"
      data-testid="viewer-nav-prev"
      aria-label="前の画像"
      disabled={navIndex() <= 0}
      onclick={() => store.nav(-1)}
    >‹</button>
    <button
      class="viewer-nav viewer-nav-next"
      data-testid="viewer-nav-next"
      aria-label="次の画像"
      disabled={navIndex() >= navTotal() - 1}
      onclick={() => store.nav(1)}
    >›</button>
  </div>

  <div class="viewer-stats">
    <span>{latencyLine("prep(先読みON)", store.prepOn)}</span>
    <span>{latencyLine("prep(先読みOFF)", store.prepOff)}</span>
    <span>{statusLine()}</span>
    <span>{transformLine()}</span>
  </div>
</div>

<style>
  .viewer-overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    flex-direction: column;
    background: light-dark(#f0f0f0, #101014);
    outline: none;
  }

  .viewer-toolbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
  }

  .viewer-toolbar .spacer {
    flex: 1;
  }

  .viewer-stage {
    position: relative;
    flex: 1;
    overflow: hidden;
    cursor: grab;
    /* backgroundは背景切替 (市松/白/黒) でstyle属性から上書きされる。
       ここは初期フォールバック */
    background:
      repeating-conic-gradient(light-dark(#e3e6ea, #171a21) 0% 25%, light-dark(#eceff3, #1d212b) 0% 50%)
      0 / 24px 24px;
    touch-action: none;
    user-select: none;
  }

  .viewer-stage:active {
    cursor: grabbing;
  }

  .layer {
    max-width: none;
    pointer-events: none;
    transform-origin: center;
    will-change: transform;
  }

  .layer.retained {
    z-index: 0;
  }

  .layer:not(.retained) {
    z-index: 1;
  }

  .pending,
  .note {
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    z-index: 2;
    padding: 0.5rem 1rem;
    border-radius: 0.5rem;
    background: light-dark(#ffffffdd, #1e1e1edd);
    font-size: 0.9rem;
  }

  /* WP-5: 高速選別キーの適用フィードバック (ステージ右上) */
  .triage-feedback {
    position: absolute;
    right: 12px;
    top: 12px;
    z-index: 2;
    padding: 0.3rem 0.8rem;
    border-radius: 0.5rem;
    background: light-dark(#1a7f37dd, #238636dd);
    color: #ffffff;
    font-size: 0.85rem;
    pointer-events: none;
  }

  /* UIUX-3 V-4: 左右端の送りボタン。半透明の大的確ヒットエリアでホバー時強調 */
  .viewer-nav {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 18%;
    min-width: 64px;
    z-index: 1;
    border: none;
    background: transparent;
    color: light-dark(#ffffff, #f0f6fc);
    font-size: 3rem;
    line-height: 1;
    opacity: 0.35;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    text-shadow: 0 1px 6px rgba(0, 0, 0, 0.8);
    transition: opacity 120ms ease;
    user-select: none;
    padding: 0;
  }

  .viewer-nav:not(:disabled):hover,
  .viewer-nav:not(:disabled):focus-visible {
    opacity: 0.9;
  }

  .viewer-nav:not(:disabled):hover {
    background: linear-gradient(to right, rgba(0, 0, 0, 0.28), transparent);
  }

  .viewer-nav-next:not(:disabled):hover {
    background: linear-gradient(to left, rgba(0, 0, 0, 0.28), transparent);
  }

  .viewer-nav:disabled {
    opacity: 0.08;
    cursor: default;
  }

  .viewer-nav-prev {
    left: 0;
  }

  .viewer-nav-next {
    right: 0;
  }

  .triage-feedback.triage-ng {
    background: light-dark(#9a6700dd, #9e6a03dd);
  }

  .triage-on {
    border-color: light-dark(#1a7f37, #238636);
    font-weight: bold;
  }

  .viewer-stats {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    padding: 0.35rem 0.75rem;
    border-top: 1px solid light-dark(#d0d7de, #30363d);
    font-family: monospace;
    font-size: 0.75rem;
    white-space: nowrap;
  }
</style>
