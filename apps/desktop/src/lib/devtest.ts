/**
 * 実機検証用の開発限定ブリッジ (WP-6 + WP-7計測拡張)。
 *
 * 目的: 検証環境のデスクトップがセッション共有でポインタ自動操作
 * (xdotool mousemove/click) の到達が安定しないため、キーボード (F2〜F11)
 * から本物のDOMイベントを生成して生産コードパスを駆動し、
 * 機械可読な状態リードアウト (画面右上) で数値を実測する。
 *
 * - リリースビルドからは App.svelte の import.meta.env.DEV ガードで除外される
 *   (計測時のみ GENTO_DEV_BRIDGE=1 で Rust側 dev_bridge_enabled が有効化する)
 * - wheel/pointerイベントは実リスナー (store.zoomAt / dragBy) を通る。
 *   setPointerCaptureだけは合成pointerIdでNotFoundになるため
 *   操作中のみ一時的にパッチする (本来の呼び出しは維持)
 * - WP-7: グリッド/統計/パフォーマンス行 (G/T/P) を追加し、
 *   `devtest:` ログ行 (Rust経由) と document.title を機械可読チャネルとして
 *   1秒周期で送出する (bash側はログをgrepして数値を採取)
 */

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
  /** WP-7: invoke (エラー注入など)。App.svelteのcore invokeを流用。 */
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

type GridLike = {
  summary: { total: number; mode: string } | null;
  stats: {
    requested: number;
    batches: number;
    rejectedFull: number;
    readyReceived: number;
    failedReceived: number;
    cancelled: number;
    cancelBatches: number;
  };
  pager: { stats: { fetched: number; errors: number }; generation: number | null };
  thumbUrlFor(id: string): string | null;
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
    injection_mode: string;
    injection_rate: number;
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

function wheelAt(el: HTMLElement, xFrac: number, yFrac: number, notches: number, delta: number): void {
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
      }),
    );
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

  const readout = document.createElement("pre");
  readout.id = "devtest-readout";
  readout.style.cssText =
    "position:fixed;right:8px;top:8px;z-index:9999;pointer-events:none;" +
    "margin:0;padding:6px 10px;background:#000d;color:#7fe7a0;" +
    "font:13px/1.35 monospace;white-space:pre;border-radius:6px;";
  document.body.appendChild(readout);

  const report = (line: string): void => {
    // ログチャネル喪失時でもUI計測 (readout/スクリーンショット) は継続する
    void stores.invoke("devtest_report", { payload: line }).catch(() => undefined);
  };

  let injectionState = 0; // 0=none 1=decode0.30 2=cache_write0.30

  const actions: Record<string, () => void> = {
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
    scan: () => clickButton("スキャン開始"),
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
  };

  function gridLines(): string[] {
    const grid = stores.getGrid();
    if (grid === null) {
      return [];
    }
    const dom = document.querySelectorAll("[data-grid-cell]").length;
    const ready0 = grid.summary !== null && grid.summary.total > 0 && grid.thumbUrlFor("0") !== null ? 1 : 0;
    const scrollEl = document.querySelector<HTMLElement>(".grid-scroll");
    const ae = document.activeElement;
    const sel = stores.getSelection();
    const s = grid.stats;
    return [
      `G tot=${grid.summary?.total ?? 0} mode=${grid.summary?.mode ?? "-"}` +
        ` req=${s.requested} bat=${s.batches} rej=${s.rejectedFull}` +
        ` rdy=${s.readyReceived} fail=${s.failedReceived} can=${s.cancelled} canB=${s.cancelBatches}`,
      `G pf=${grid.pager.stats.fetched} pe=${grid.pager.stats.errors}` +
        ` pgen=${grid.pager.generation ?? -1} dom=${dom} r0=${ready0}` +
        ` st=${Math.round(scrollEl?.scrollTop ?? -1)}` +
        ` ae=${ae === null ? "-" : `${ae.tagName}.${String(ae.className).split(" ")[0]}`}` +
        ` selN=${sel?.ids.length ?? -1} foc=${sel?.focusIndex ?? -1}`,
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
      `T inj=${stats.injection_mode},${n(stats.injection_rate, 2)}` +
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
    lines.push(...gridLines(), ...thumbLines(), ...perfLines());
    if (!lines.some((line) => line.startsWith("V ") || line.startsWith("C "))) {
      lines.push("idle (viewer closed)");
    }
    return lines;
  }

  function dump(): void {
    readout.textContent = dumpLines().join("\n");
  }

  /** 機械可読チャネル: 1行テキストを devtest: ログと document.title へ送る。 */
  function reportCompact(): void {
    const single = [...dumpLines(), ...gridLines(), ...thumbLines(), ...perfLines()]
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
      if (!event.key.startsWith("F") || event.altKey) {
        return;
      }
      const key = event.key;
      let handled = true;
      switch (key) {
        case "F2":
          if (event.ctrlKey) {
            actions.focusGrid();
          } else {
            actions.select2();
          }
          break;
        case "F3":
          if (event.ctrlKey) {
            actions.select3();
          } else {
            actions.openViewer();
          }
          break;
        case "F4": {
          const el = interactionEl();
          if (el !== null) {
            wheelAt(el, 0.25, 0.35, 3, event.ctrlKey ? 100 : -100);
          }
          break;
        }
        case "F5": {
          const el = interactionEl();
          if (el !== null) {
            void dragSequence(el, 0.3, 0.5, 240, 160, 24);
          }
          break;
        }
        case "F6":
          if (event.ctrlKey) {
            actions.resetStats();
          } else {
            actions.prefetchToggle();
          }
          break;
        case "F7":
          actions.cycleChoice();
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
          if (event.ctrlKey) {
            actions.clearCache();
          } else {
            actions.virtualMode();
          }
          break;
        case "F10":
          if (event.ctrlKey) {
            actions.jumpStart();
          } else {
            actions.realMode();
          }
          break;
        case "F11":
          if (event.ctrlKey) {
            actions.jumpMiddle();
          } else {
            actions.cycleInjection();
          }
          break;
        case "F12":
          if (event.ctrlKey) {
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
