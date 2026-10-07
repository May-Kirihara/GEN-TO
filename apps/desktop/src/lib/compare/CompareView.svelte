<script lang="ts">
  /**
    * 2枚・4枚比較のオーバーレイ (WP-6 / フェーズ7、spec §9.4 + WP-5選別)。
    *
    * - レイアウト: 2枚=横並び50/50、4枚=2x2
    * - 操作: クリック/1〜4/Tab=アクティブスロット、ドラッグ/ホイール=パン・ズーム
    *   (sync ON→共有状態、OFF/detached→対象スロットのみ)
    * - s=入れ替え (2枚)、p=昇格、d=一時解除、g=次グループ、b=基準固定、f=fit、Esc=閉じる
    * - WP-5: 各スロットに採用/除外ボタン。X=アクティブスロット除外、
    *   K=アクティブスロット採用 (p=昇格と衝突するためK。spec §9.4「1枚または
    *   複数枚の採用・残り画像の除外」の最小実装)
    * - 各スロットは旧画像保持レイヤーを持つ (URL差し替えで空白を出さない)
    * - 同期パン中はrAFのフレーム時間を計測する
    */
  import { onDestroy, onMount } from "svelte";
  import { toCss } from "../viewer/transform";
  import type { CompareStore } from "./compare-store";

  let {
    store,
    onClose,
    onCullSlot,
  }: {
    store: CompareStore;
    onClose: () => void;
    /** スロット単位の採用/除外 (WP-5。AppがMetadataStoreへ接続)。 */
    onCullSlot?: (slotIndex: number, value: "picked" | "rejected") => void;
  } = $props();

  let overlayEl: HTMLElement | null = $state(null);
  let compareTick = $state(0);
  const stopNotify = store.onChange(() => {
    compareTick++;
  });

  // 開いたらグリッド側のキー操作 (Enter/矢印) が背面で発火しないようフォーカスを移す
  onMount(() => {
    overlayEl?.focus();
  });

  // ===== スロット要素 (ビューポート計測・ホイール) =====

  let slotEls: Array<HTMLElement | null> = $state([]);

  $effect(() => {
    const elements = slotEls.filter((el): el is HTMLElement => el !== null);
    if (elements.length === 0 || typeof ResizeObserver === "undefined") {
      return;
    }
    const indexById = new Map(elements.map((el) => [el, Number(el.dataset.slot)]));
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const index = indexById.get(entry.target as HTMLElement);
        if (index !== undefined) {
          store.setSlotViewport(index, Math.round(entry.contentRect.width), Math.round(entry.contentRect.height));
        }
      }
    });
    for (const element of elements) {
      observer.observe(element);
    }
    return () => {
      observer.disconnect();
    };
  });

  $effect(() => {
    const elements = slotEls.filter((el): el is HTMLElement => el !== null);
    const cleanups: Array<() => void> = [];
    for (const element of elements) {
      const index = Number(element.dataset.slot);
      const onWheel = (event: WheelEvent) => {
        event.preventDefault();
        const rect = element.getBoundingClientRect();
        store.wheelZoom(
          index,
          { x: event.clientX - rect.left, y: event.clientY - rect.top },
          Math.exp(-event.deltaY * 0.0015),
        );
      };
      element.addEventListener("wheel", onWheel, { passive: false });
      cleanups.push(() => {
        element.removeEventListener("wheel", onWheel);
      });
    }
    return () => {
      for (const cleanup of cleanups) {
        cleanup();
      }
    };
  });

  // ===== ドラッグ (パン + フレーム時間計測) =====

  let drag: { slot: number; pointerId: number; lastX: number; lastY: number } | null = null;
  let rafId = 0;

  function startFrameLoop(): void {
    if (rafId !== 0) {
      return;
    }
    store.startFrames(performance.now());
    const loop = () => {
      store.tickFrame(performance.now());
      rafId = requestAnimationFrame(loop);
    };
    rafId = requestAnimationFrame(loop);
  }

  function stopFrameLoop(): void {
    if (rafId !== 0) {
      cancelAnimationFrame(rafId);
      rafId = 0;
      store.stopFrames();
    }
  }

  function onPointerDown(event: PointerEvent, slot: number): void {
    if (event.button !== 0) {
      return;
    }
    // slot内のインタラクティブ要素 (採用/除外ボタン) はパン対象外。captureすると
    // pointerup/clickがslotへリターゲットされ、ボタン自身のclickが発火
    // しなくなる (FIX-VIEWERNAV-1と同一原因 — FIX-VIEWERNAV-2)
    if ((event.target as Element).closest("button, a, input, select, textarea") !== null) {
      return;
    }
    store.setActive(slot);
    drag = { slot, pointerId: event.pointerId, lastX: event.clientX, lastY: event.clientY };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    startFrameLoop();
  }

  function onPointerMove(event: PointerEvent): void {
    if (drag === null || drag.pointerId !== event.pointerId) {
      return;
    }
    store.dragBy(drag.slot, event.clientX - drag.lastX, event.clientY - drag.lastY);
    drag.lastX = event.clientX;
    drag.lastY = event.clientY;
  }

  function onPointerUp(event: PointerEvent): void {
    if (drag !== null && drag.pointerId === event.pointerId) {
      drag = null;
      stopFrameLoop();
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
      case "1":
      case "2":
      case "3":
      case "4":
        event.preventDefault();
        store.setActive(Number(event.key) - 1);
        break;
      case "Tab":
        event.preventDefault();
        store.cycleActive();
        break;
      case "s":
      case "S":
        event.preventDefault();
        store.swap();
        break;
      case "p":
      case "P":
        event.preventDefault();
        store.promote(!store.slots[store.active]?.isOriginal);
        break;
      case "d":
      case "D":
        event.preventDefault();
        store.toggleDetached(store.active);
        break;
      case "g":
      case "G":
        event.preventDefault();
        void store.nextGroup();
        break;
      case "x":
      case "X":
        event.preventDefault();
        onCullSlot?.(store.active, "rejected");
        break;
      case "k":
      case "K":
        event.preventDefault();
        onCullSlot?.(store.active, "picked");
        break;
      case "b":
      case "B":
        event.preventDefault();
        store.toggleBase();
        break;
      case "f":
      case "F":
        event.preventDefault();
        store.setView("fit");
        break;
    }
  }

  // ===== 描画ヘルパー (compareTickを読み変更を伝える。WP-2/5の教訓) =====

  function layerStyle(
    transform: { scale: number; centerX: number; centerY: number },
    dims: { width: number; height: number },
  ): string {
    const css = toCss(
      { scale: transform.scale, centerX: transform.centerX, centerY: transform.centerY, mode: "custom" },
      dims,
    );
    return (
      `position:absolute; left:50%; top:50%; width:${dims.width}px; height:${dims.height}px;` +
      `transform: translate(-50%, -50%) translate(${css.tx}px, ${css.ty}px) scale(${css.scale});`
    );
  }

  function slotsSnapshot() {
    void compareTick;
    // スロットはプレーンクラス内で直接mutateされるため、通知ごとに浅い複製を
    // 作って各アイテムの値を変える (同一キー・同一値だとeach内容が再評価されず
    // url差し替え・昇格・次グループが画面へ反映されない — WP-6実機検証で発見)
    return store.slots.map((slot) => ({ ...slot }));
  }

  function countNow() {
    void compareTick;
    return store.count;
  }

  function syncOn() {
    void compareTick;
    return store.syncEnabled;
  }

  function baseNow() {
    void compareTick;
    return store.baseSlot;
  }

  function slotActive(index: number) {
    void compareTick;
    return store.active === index;
  }

  function activeIsOriginal() {
    void compareTick;
    return store.slots[store.active]?.isOriginal ?? false;
  }

  function retainedTransformAt(index: number, width: number, height: number) {
    void compareTick;
    return store.effectiveTransformFor(index, width, height);
  }

  function slotTransform(index: number) {
    void compareTick;
    return store.effectiveTransform(index);
  }

  function formatNum(value: number | null, digits = 3): string {
    return value === null ? "—" : value.toFixed(digits);
  }

  function syncLine(): string {
    void compareTick;
    return (
      `sync=${store.syncEnabled ? "ON" : "OFF"}` +
      ` c=(${formatNum(store.sync.centerX)},${formatNum(store.sync.centerY)})` +
      ` zf=${formatNum(store.sync.zoomFactor, 2)} base=${store.baseSlot ?? "—"} active=${store.active}`
    );
  }

  function slotStatLine(index: number): string {
    void compareTick;
    const slot = store.slots[index];
    if (!slot || slot.assetId === null) {
      return `s${index} id=—`;
    }
    const t = store.effectiveTransform(index);
    const zf = store.syncEnabled && !slot.detached ? store.sync.zoomFactor : t.scale;
    return (
      `s${index} id=${slot.assetId} ${slot.width}x${slot.height}` +
      ` c=(${formatNum(t.centerX)},${formatNum(t.centerY)}) zf=${formatNum(zf, 2)}` +
      ` scale=${formatNum(t.scale, 2)} ${slot.isOriginal ? "ORIG" : "PREV"}` +
      `${slot.detached ? " DET" : ""}${store.baseSlot === index ? " BASE" : ""}`
    );
  }

  function frameLine(): string {
    void compareTick;
    const s = store.frames.stats();
    return (
      `frame n=${s.count} p50=${formatNum(s.p50, 1)}ms p95=${formatNum(s.p95, 1)}ms` +
      ` max=${formatNum(s.max, 1)}ms | groups=${store.stats.groups}` +
      ` swaps=${store.stats.swaps} reqs=${store.stats.requests}`
    );
  }

  onDestroy(() => {
    stopFrameLoop();
    stopNotify();
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="compare-overlay" bind:this={overlayEl} tabindex="-1">
  <div class="compare-toolbar">
    <button class:active={countNow() === 2} onclick={() => void store.setCount(2)}>2枚</button>
    <button class:active={countNow() === 4} onclick={() => void store.setCount(4)}>4枚</button>
    <button onclick={() => store.setView("fit")}>全体表示 (f)</button>
    <button onclick={() => store.setView("actual")}>原寸</button>
    <button onclick={() => store.toggleSync()}>同期: {syncOn() ? "ON" : "OFF"}</button>
    <button class:active={baseNow() !== null} onclick={() => store.toggleBase()}>
      基準固定 (b)
    </button>
    <button onclick={() => store.promote(!activeIsOriginal())}>
      昇格 (p)
    </button>
    {#if countNow() === 2}
      <button onclick={() => store.swap()}>入れ替え (s)</button>
    {/if}
    <button onclick={() => void store.nextGroup()}>次グループ (g)</button>
    <button onclick={() => store.resetFrames()}>統計リセット</button>
    <span class="spacer"></span>
    <button class="close" onclick={onClose}>閉じる (Esc)</button>
  </div>

  <div class="compare-slots" class:four={countNow() === 4}>
    {#each slotsSnapshot() as slot, index (index)}
      {@const transform = slotTransform(index)}
      {@const retainedTransform =
        slot.retainedUrl !== null && slot.retainedWidth > 0
          ? retainedTransformAt(index, slot.retainedWidth, slot.retainedHeight)
          : null}
      <!-- a11yは技術検証の対象外 (本体計画 §18)。スロット選択は1〜4/Tabキーでも可能 -->
      <div
        class="slot"
        role="presentation"
        class:slot-active={slotActive(index)}
        data-slot={index}
        bind:this={slotEls[index]}
        onclick={() => store.setActive(index)}
        onpointerdown={(event) => onPointerDown(event, index)}
        onpointermove={onPointerMove}
        onpointerup={onPointerUp}
        onpointercancel={onPointerUp}
      >
        {#if slot.retainedUrl !== null && retainedTransform !== null}
          <img
            class="layer retained"
            src={slot.retainedUrl}
            alt=""
            draggable="false"
            style={layerStyle(retainedTransform, {
              width: slot.retainedWidth,
              height: slot.retainedHeight,
            })}
          />
        {/if}
        {#if slot.url !== null}
          {#key slot.key}
            <img
              class="layer"
              src={slot.url}
              alt={slot.displayName}
              draggable="false"
              style={layerStyle(transform, { width: slot.width, height: slot.height })}
              onload={() => store.noteLoaded(index, slot.key)}
              onerror={() => store.noteLoadError(index, slot.key)}
            />
          {/key}
        {:else}
          <div class="pending">読み込み中…</div>
        {/if}
        <div class="slot-label">
          s{index} {slot.displayName || "—"} {slot.width}x{slot.height}
          {slot.isOriginal ? "[ORIG]" : "[PREV]"}{slot.detached ? " [解除]" : ""}
          {baseNow() === index ? " [基準]" : ""}
        </div>
        {#if onCullSlot !== undefined}
          <div class="slot-cull">
            <button
              class="cull-pick"
              onclick={(event) => {
                event.stopPropagation();
                onCullSlot(index, "picked");
              }}
              title="この画像を採用 (K)"
            >採用</button>
            <button
              class="cull-reject"
              onclick={(event) => {
                event.stopPropagation();
                onCullSlot(index, "rejected");
              }}
              title="この画像を除外 (X)"
            >除外</button>
          </div>
        {/if}
        {#if slot.note}
          <div class="note">{slot.note}</div>
        {/if}
      </div>
    {/each}
  </div>

  <div class="compare-stats">
    <span>{syncLine()}</span>
    {#each [...slotsSnapshot().keys()] as index (index)}
      <span>{slotStatLine(index)}</span>
    {/each}
    <span>{frameLine()}</span>
  </div>
</div>

<style>
  .compare-overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    flex-direction: column;
    background: light-dark(#f0f0f0, #101014);
    outline: none;
  }

  .compare-toolbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-50);
    padding: var(--space-50) var(--space-75);
    border-bottom: 1px solid var(--color-border-strong);
  }

  .compare-toolbar .spacer {
    flex: 1;
  }

  .compare-toolbar button.active {
    border-color: var(--color-accent);
    font-weight: bold;
  }

  .compare-slots {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .compare-slots.four {
    display: grid;
    grid-template-columns: 1fr 1fr;
    grid-template-rows: 1fr 1fr;
  }

  .slot {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    border: 3px solid transparent;
    cursor: grab;
    background:
      repeating-conic-gradient(light-dark(#e3e6ea, #171a21) 0% 25%, light-dark(#eceff3, #1d212b) 0% 50%)
      0 / 24px 24px;
    touch-action: none;
    user-select: none;
  }

  .slot:active {
    cursor: grabbing;
  }

  .slot-active {
    border-color: var(--color-accent);
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
    padding: 0.25rem 0.75rem;
    border-radius: var(--radius-lg);
    background: light-dark(#ffffffdd, #1e1e1edd);
    font-size: var(--font-size-85);
  }

  .slot-label {
    position: absolute;
    left: 0.25rem;
    top: 0.25rem;
    z-index: 2;
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-xs);
    background: light-dark(#ffffffcc, #000000aa);
    font-family: monospace;
    font-size: var(--font-size-70);
    white-space: nowrap;
    pointer-events: none;
  }

  /* WP-5: スロット単位の採用/除外ボタン (右下) */
  .slot-cull {
    position: absolute;
    right: 0.35rem;
    bottom: 0.35rem;
    z-index: 2;
    display: flex;
    gap: var(--space-30);
  }

  .slot-cull button {
    font-size: var(--font-size-75);
    padding: 0.1rem 0.55rem;
  }

  .slot-cull .cull-pick {
    border-color: var(--color-success-emphasis);
    color: var(--color-success);
  }

  .slot-cull .cull-reject {
    border-color: var(--color-danger);
    color: var(--color-danger);
  }

  .compare-stats {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 1rem;
    padding: var(--space-35) var(--space-75);
    border-top: 1px solid var(--color-border-strong);
    font-family: monospace;
    font-size: var(--font-size-75);
    white-space: nowrap;
  }
</style>
