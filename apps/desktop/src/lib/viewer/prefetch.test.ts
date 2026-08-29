import { describe, expect, it } from "vitest";
import { planPrefetch } from "./prefetch";

describe("planPrefetch", () => {
  it("進行方向側を優先し次に反対側を返す", () => {
    expect(planPrefetch(10, 100, 2, 1)).toEqual([
      { index: 11, relation: "next" },
      { index: 12, relation: "next" },
      { index: 9, relation: "prev" },
      { index: 8, relation: "prev" },
    ]);
    expect(planPrefetch(10, 100, 2, -1)).toEqual([
      { index: 9, relation: "prev" },
      { index: 8, relation: "prev" },
      { index: 11, relation: "next" },
      { index: 12, relation: "next" },
    ]);
  });

  it("方向なしはnext側を先に", () => {
    expect(planPrefetch(10, 100, 1, 0)).toEqual([
      { index: 11, relation: "next" },
      { index: 10 - 1, relation: "prev" },
    ]);
  });

  it("範囲外のindexを返さない (境界)", () => {
    expect(planPrefetch(0, 10, 2, 1)).toEqual([
      { index: 1, relation: "next" },
      { index: 2, relation: "next" },
    ]);
    expect(planPrefetch(9, 10, 2, 1)).toEqual([
      { index: 8, relation: "prev" },
      { index: 7, relation: "prev" },
    ]);
    expect(planPrefetch(0, 1, 1, 1)).toEqual([]);
  });

  it("count=0は空", () => {
    expect(planPrefetch(5, 10, 0, 1)).toEqual([]);
  });
});
