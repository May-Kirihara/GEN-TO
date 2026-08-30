import { describe, expect, it } from "vitest";
import { fillSlots, nextGroup, swapSlots } from "./slots";

type SrcItem = { id: string; width: number; height: number; state: string };

function catalog(items: SrcItem[]) {
  return {
    total: () => items.length,
    get: (index: number) => {
      const item = items[index];
      if (!item) {
        return null;
      }
      return {
        id: item.id,
        displayName: `name-${item.id}`,
        width: item.state === "ready" ? item.width : null,
        height: item.state === "ready" ? item.height : null,
        state: item.state,
        version: 300_000 + index,
      };
    },
  };
}

const OK = (id: string): SrcItem => ({ id, width: 1000, height: 800, state: "ready" });

describe("fillSlots", () => {
  it("選択の先頭N件をスロットへ充填する", () => {
    const cat = catalog([OK("1"), OK("2"), OK("3"), OK("4"), OK("5")]);
    const result = fillSlots({
      selectedIds: ["2", "4", "5"],
      count: 2,
      focusIndex: 0,
      total: cat.total(),
      get: cat.get,
      findIndexById: (id) => ["1", "2", "3", "4", "5"].indexOf(id),
    });
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["2", "4"]);
    expect(result.nextIndex).toBe(4); // 最大充填index 3 ("4") + 1
  });

  it("選択不足分は現在位置から連続補充する", () => {
    const cat = catalog([OK("1"), OK("2"), OK("3"), OK("4"), OK("5")]);
    const result = fillSlots({
      selectedIds: ["5"],
      count: 3,
      focusIndex: 1,
      total: cat.total(),
      get: cat.get,
      findIndexById: (id) => ["1", "2", "3", "4", "5"].indexOf(id),
    });
    // 選択の"5" (index4) + フォーカス位置1から連続で 1,2 (重複除外)
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["5", "2", "3"]);
    expect(result.nextIndex).toBe(5);
  });

  it("選択・フォーカスなしは先頭から充填", () => {
    const cat = catalog([OK("1"), OK("2"), OK("3")]);
    const result = fillSlots({
      selectedIds: [],
      count: 2,
      focusIndex: null,
      total: cat.total(),
      get: cat.get,
      findIndexById: () => null,
    });
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["1", "2"]);
    expect(result.nextIndex).toBe(2);
  });

  it("非ready項目とキャッシュ上未知の選択IDはスキップされる", () => {
    const cat = catalog([OK("1"), { id: "2", width: 0, height: 0, state: "corrupt" }, OK("3"), OK("4")]);
    const result = fillSlots({
      selectedIds: ["99", "2", "3"],
      count: 2,
      focusIndex: 0,
      total: cat.total(),
      get: cat.get,
      findIndexById: (id) => (id === "99" ? null : Number(id) - 1),
    });
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["3", "1"]);
  });

  it("カタログより多いスロットはnullで埋まる", () => {
    const cat = catalog([OK("1")]);
    const result = fillSlots({
      selectedIds: [],
      count: 4,
      focusIndex: null,
      total: cat.total(),
      get: cat.get,
      findIndexById: () => null,
    });
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["1", null, null, null]);
    expect(result.nextIndex).toBe(1);
  });
});

describe("nextGroup (基準スロット固定)", () => {
  const cat = catalog([OK("1"), OK("2"), OK("3"), OK("4"), OK("5"), OK("6"), OK("7"), OK("8")]);

  function slotsOf(ids: Array<string | null>) {
    return ids.map((id) =>
      id === null
        ? null
        : {
            assetId: id,
            displayName: `name-${id}`,
            width: 1000,
            height: 800,
            version: null,
          },
    );
  }

  it("基準スロットは画像を保持し他スロットだけ進む", () => {
    let result = fillSlots({
      selectedIds: ["1", "2", "3", "4"],
      count: 4,
      focusIndex: 0,
      total: cat.total(),
      get: cat.get,
      findIndexById: (id) => Number(id) - 1,
    });
    result = nextGroup(result.slots, result.nextIndex, 1, cat.total(), cat.get);
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["5", "2", "6", "7"]);
    expect(result.nextIndex).toBe(7);
    // 基準 (slot1) は再度進めても不変。残り1件 ("8") は基準以外へ充填
    const again = nextGroup(result.slots, result.nextIndex, 1, cat.total(), cat.get);
    expect(again.slots[1]?.assetId).toBe("2");
    expect(again.slots.map((s) => s?.assetId ?? null)).toEqual(["8", "2", "6", "7"]);
    expect(again.nextIndex).toBe(8);
  });

  it("基準なしなら全スロットが進む", () => {
    let result = fillSlots({
      selectedIds: ["1", "2"],
      count: 2,
      focusIndex: 0,
      total: cat.total(),
      get: cat.get,
      findIndexById: (id) => Number(id) - 1,
    });
    result = nextGroup(result.slots, result.nextIndex, null, cat.total(), cat.get);
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["3", "4"]);
  });

  it("カタログ終端では打切り (既存を維持)", () => {
    const result = nextGroup(slotsOf(["7", "8"]), 8, null, cat.total(), cat.get);
    expect(result.slots.map((s) => s?.assetId ?? null)).toEqual(["7", "8"]);
    expect(result.nextIndex).toBe(8);
  });
});

describe("swapSlots", () => {
  it("2枚の左右を入れ替える", () => {
    expect(swapSlots(["a", "b"])).toEqual(["b", "a"]);
  });
});
