import { describe, expect, it } from "vitest";
import {
  FailureSet,
  ReadyMap,
  directionFromDelta,
  planDemand,
} from "./demand";
import type { ThumbReadyItem } from "../thumbnail-types";

function readyItem(id: string, gen = 1, sizeClass = "s256"): ThumbReadyItem {
  return { asset_id: id, size_class: sizeClass, cache_generation: gen };
}

describe("directionFromDelta", () => {
  it("閾値未満の差分は方向なし", () => {
    expect(directionFromDelta(100, 104)).toBe(0);
    expect(directionFromDelta(100, 96)).toBe(0);
    expect(directionFromDelta(100, 100)).toBe(0);
  });

  it("下向きスクロールは1、上向きは-1", () => {
    expect(directionFromDelta(100, 120)).toBe(1);
    expect(directionFromDelta(100, 80)).toBe(-1);
  });

  it("閾値は変更可能", () => {
    expect(directionFromDelta(100, 104, 2)).toBe(1);
  });
});

describe("ReadyMap", () => {
  it("受入れた項目をクラス別に記録する", () => {
    const map = new ReadyMap();
    expect(map.applyReady([readyItem("5", 1)])).toBe(1);
    expect(map.has("s256", "5")).toBe(true);
    expect(map.generation("s256", "5")).toBe(1);
    expect(map.has("s512", "5")).toBe(false);
    expect(map.size("s256")).toBe(1);
  });

  it("既知世代より古いイベントは破棄される", () => {
    const map = new ReadyMap();
    map.noteGeneration(2);
    expect(map.applyReady([readyItem("5", 1)])).toBe(0);
    expect(map.has("s256", "5")).toBe(false);
  });

  it("新しい世代のイベントは受入れ、古い世代の項目を一掃する", () => {
    const map = new ReadyMap();
    map.applyReady([readyItem("5", 1), readyItem("6", 1)]);
    expect(map.applyReady([readyItem("7", 3)])).toBe(1);
    expect(map.has("s256", "5")).toBe(false);
    expect(map.has("s256", "7")).toBe(true);
    // 世代2のまま届いた古い要求の完了も破棄
    expect(map.applyReady([readyItem("8", 2)])).toBe(0);
  });

  it("clearは全クラスを空にする (カタログgeneration変化)", () => {
    const map = new ReadyMap();
    map.applyReady([readyItem("5", 1), readyItem("6", 1, "s512")]);
    map.clear();
    expect(map.size("s256")).toBe(0);
    expect(map.size("s512")).toBe(0);
  });
});

describe("FailureSet", () => {
  it("クラス別に失敗を記録しクリアできる", () => {
    const failures = new FailureSet();
    failures.add("s256", ["1", "2"]);
    expect(failures.has("s256", "1")).toBe(true);
    expect(failures.has("s512", "1")).toBe(false);
    failures.clear();
    expect(failures.has("s256", "1")).toBe(false);
  });
});

function itemAtFrom(
  items: Array<{ id: string; state: string } | null>,
): (index: number) => { id: string; state: string } | null {
  return (index: number) => items[index] ?? null;
}

const baseInput = {
  visible: { start: 2, end: 4 }, // 3行
  overscanBefore: 1,
  overscanAfter: 1,
  direction: 1 as const,
  totalRows: 20,
  columns: 3,
  total: 60,
  prefetchRows: 2,
  maxBatch: 100,
  sizeClass: "s256",
};

describe("planDemand", () => {
  it("可視範囲の未生成IDをviewport優先度で抽出する", () => {
    const items = Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }));
    const plan = planDemand({
      ...baseInput,
      ready: new ReadyMap(),
      failures: new FailureSet(),
      pending: new Set(),
      itemAt: itemAtFrom(items),
    });
    // 行2..4 → index 6..14
    const viewport = plan.find((b) => b.priority === "viewport");
    expect(viewport?.ids).toEqual([
      "a6", "a7", "a8", "a9", "a10", "a11", "a12", "a13", "a14",
    ]);
    // 進行方向 (下) の先読み: 可視末尾+1から2行 → index 15..20
    const prefetch = plan.find((b) => b.priority === "prefetch");
    expect(prefetch?.ids).toEqual(["a15", "a16", "a17", "a18", "a19", "a20"]);
    // overscan: 上側1行 (index 3..5)。下側はprefetchに吸収される
    const overscan = plan.find((b) => b.priority === "overscan");
    expect(overscan?.ids).toEqual(["a3", "a4", "a5"]);
  });

  it("上向きスクロールでは先読みが可視範囲の前に置かれる", () => {
    const items = Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }));
    const plan = planDemand({
      ...baseInput,
      direction: -1,
      ready: new ReadyMap(),
      failures: new FailureSet(),
      pending: new Set(),
      itemAt: itemAtFrom(items),
    });
    // 先読み: 可視先頭-1から2行前 → index 3..0? 行1..0 → index 3,2,1,0, wait rows 1..0 descending rows → indices of row1 (3..5) and row0 (0..2)
    const prefetch = plan.find((b) => b.priority === "prefetch");
    expect(prefetch?.ids).toEqual(["a3", "a4", "a5", "a0", "a1", "a2"]);
    // overscan: 下側1行 (index 15..17)
    const overscan = plan.find((b) => b.priority === "overscan");
    expect(overscan?.ids).toEqual(["a15", "a16", "a17"]);
  });

  it("ready・failed・pending・未ロード・非ready状態の項目を除外する", () => {
    const ready = new ReadyMap();
    ready.applyReady([readyItem("a6"), readyItem("a7")]);
    const failures = new FailureSet();
    failures.add("s256", ["a8"]);
    const items: Array<{ id: string; state: string } | null> = Array.from(
      { length: 60 },
      (_, i) => ({ id: `a${i}`, state: "ready" }),
    );
    items[9] = { id: "a9", state: "unreadable" };
    items[10] = null;
    const plan = planDemand({
      ...baseInput,
      ready,
      failures,
      pending: new Set(["s256:a11"]),
      itemAt: itemAtFrom(items),
    });
    const viewport = plan.find((b) => b.priority === "viewport");
    expect(viewport?.ids).toEqual(["a12", "a13", "a14"]);
  });

  it("優先度ごとにバッチ上限を適用する", () => {
    const items = Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }));
    const plan = planDemand({
      ...baseInput,
      maxBatch: 4,
      ready: new ReadyMap(),
      failures: new FailureSet(),
      pending: new Set(),
      itemAt: itemAtFrom(items),
    });
    expect(plan.find((b) => b.priority === "viewport")?.ids).toEqual(["a6", "a7", "a8", "a9"]);
    expect(plan.find((b) => b.priority === "prefetch")?.ids).toEqual(["a15", "a16", "a17", "a18"]);
    expect(plan.find((b) => b.priority === "overscan")?.ids).toEqual(["a3", "a4", "a5"]);
  });

  it("すべて生成済みなら空の計画を返す", () => {
    const ready = new ReadyMap();
    ready.applyReady(Array.from({ length: 60 }, (_, i) => readyItem(`a${i}`)));
    const plan = planDemand({
      ...baseInput,
      ready,
      failures: new FailureSet(),
      pending: new Set(),
      itemAt: itemAtFrom(Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }))),
    });
    expect(plan).toEqual([]);
  });

  it("行範囲をtotalRows末尾でクランプする", () => {
    const items = Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }));
    const plan = planDemand({
      ...baseInput,
      visible: { start: 18, end: 19 },
      overscanBefore: 0,
      overscanAfter: 5,
      prefetchRows: 4,
      ready: new ReadyMap(),
      failures: new FailureSet(),
      pending: new Set(),
      itemAt: itemAtFrom(items),
    });
    // 行18..19 → index 54..59。以降の行はない
    const viewport = plan.find((b) => b.priority === "viewport");
    expect(viewport?.ids).toEqual(["a54", "a55", "a56", "a57", "a58", "a59"]);
    expect(plan.find((b) => b.priority === "prefetch")).toBeUndefined();
    expect(plan.find((b) => b.priority === "overscan")).toBeUndefined();
  });

  it("pendingのキーはサイズクラスを含む (クラス切替で再要求される)", () => {
    const items = Array.from({ length: 60 }, (_, i) => ({ id: `a${i}`, state: "ready" }));
    const plan = planDemand({
      ...baseInput,
      ready: new ReadyMap(),
      failures: new FailureSet(),
      pending: new Set(["s512:a6"]),
      itemAt: itemAtFrom(items),
    });
    expect(plan.find((b) => b.priority === "viewport")?.ids).toContain("a6");
  });
});
