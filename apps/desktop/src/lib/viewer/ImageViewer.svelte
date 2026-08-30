<script lang="ts">
  /**
   * 単一画像ビューアのオーバーレイ (WP-6 / フェーズ6)。
   *
   * - 表示: 現行レイヤー + 保持 (retained) レイヤーの積層。新画像のonloadまで
   *   旧画像を表示し続ける (空白を出さない)
   * - 操作: ホイール=ズーム (ポインタ中心)、ドラッグ=パン、ダブルクリック=fit⇔actual、
   *   ←/→=前/次画像、+/-/0/1/r、Esc=閉じる
   * - 画像要素は論理寸法 (実寸) で配置しCSS transformで操作する
   *   (preview→original差し替えで幾何不変)
   * - 開発統計: 表示準備時間 p50/p95/p99 (先読みon/off別)、center/zoomFactor数値
   */
  import { onDestroy, onMount } from "svelte";
  import { toCss, type ViewTransform } from "./transform";
  import {
    backgroundCss,
    backgroundLabel,
    loadBackground,
    nextBackground,
    safeLocalStorage,
    saveBackground,
    type ViewerBackground,
  } from "./background";
  import type { ViewerStore } from "./viewer-store";

  let {
    store,
    onClose,
  }: {
    store: ViewerStore;
    onClose: () => void;
  } = $props();

  let overlayEl: HTMLElement | null = $state(null);
  let stageEl: HTMLElement | null = $state(null);
  let viewerTick = $state(0);
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

  // ホイールはpassive:falseで防止済みリスナーを手動登録する
  $effect(() => {
    const element = stageEl;
    if (element === null) {
      return;
    }
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      store.zoomAt(
        { x: event.clientX - rect.left, y: event.clientY - rect.top },
        Math.exp(-event.deltaY * 0.0015),
      );
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
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="viewer-overlay" bind:this={overlayEl} tabindex="-1">
  <div class="viewer-toolbar">
    <button onclick={() => store.setMode("fit")}>fit (0)</button>
    <button onclick={() => store.setMode("actual")}>原寸 (1)</button>
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
