import { describe, expect, it } from "vitest";
import { EMPTY_FILTERS, filtersActive, type GuiFilters } from "../search";
import { filterChips, withoutFilter } from "./filter-chips";

function filters(overrides: Partial<GuiFilters> = {}): GuiFilters {
  return { ...EMPTY_FILTERS, ...overrides };
}

describe("filterChips (UIUX-5/WP2: フィルタチップ)", () => {
  it("未設定 (EMPTY_FILTERS) は空配列", () => {
    expect(filterChips(filters())).toEqual([]);
  });

  it("評価下限は「★N以上」", () => {
    expect(filterChips(filters({ ratingMin: 3 }))).toEqual([
      { key: "ratingMin", label: "★3以上" },
    ]);
    expect(filterChips(filters({ ratingMin: 1 }))).toEqual([
      { key: "ratingMin", label: "★1以上" },
    ]);
  });

  it("選別は採用/除外/未選別", () => {
    expect(filterChips(filters({ cull: "picked" }))).toEqual([{ key: "cull", label: "採用" }]);
    expect(filterChips(filters({ cull: "rejected" }))).toEqual([{ key: "cull", label: "除外" }]);
    expect(filterChips(filters({ cull: "unselected" }))).toEqual([
      { key: "cull", label: "未選別" },
    ]);
  });

  it("お気に入りは true=♥ お気に入り / false=♥ お気に入り以外 (どちらもフィルタ有効)", () => {
    expect(filterChips(filters({ favorite: true }))).toEqual([
      { key: "favorite", label: "♥ お気に入り" },
    ]);
    expect(filterChips(filters({ favorite: false }))).toEqual([
      { key: "favorite", label: "♥ お気に入り以外" },
    ]);
  });

  it("形式は大文字化", () => {
    expect(filterChips(filters({ type: "png" }))).toEqual([{ key: "type", label: "PNG" }]);
    expect(filterChips(filters({ type: "webp" }))).toEqual([{ key: "type", label: "WEBP" }]);
  });

  it("タグは「タグ:xxx」(前後空白は除去)", () => {
    expect(filterChips(filters({ tag: "night" }))).toEqual([{ key: "tag", label: "タグ:night" }]);
    expect(filterChips(filters({ tag: " night " }))).toEqual([
      { key: "tag", label: "タグ:night" },
    ]);
  });

  it("追加日は両端で1チップ「追加日 a〜b」(範囲=1フィルタ単位)", () => {
    expect(filterChips(filters({ addedFrom: "2026-01-01", addedTo: "2026-06-30" }))).toEqual([
      { key: "addedRange", label: "追加日 2026-01-01〜2026-06-30" },
    ]);
  });

  it("追加日は片側のみなら片側だけの〜表記", () => {
    expect(filterChips(filters({ addedFrom: "2026-01-01" }))).toEqual([
      { key: "addedRange", label: "追加日 2026-01-01〜" },
    ]);
    expect(filterChips(filters({ addedTo: "2026-06-30" }))).toEqual([
      { key: "addedRange", label: "追加日 〜2026-06-30" },
    ]);
  });

  it("複合は固定順 (評価→選別→♥→形式→タグ→追加日) で全チップを列挙", () => {
    expect(
      filterChips(
        filters({
          tag: "night",
          addedTo: "2026-06-30",
          type: "png",
          favorite: true,
          cull: "picked",
          ratingMin: 3,
          addedFrom: "2026-01-01",
        }),
      ),
    ).toEqual([
      { key: "ratingMin", label: "★3以上" },
      { key: "cull", label: "採用" },
      { key: "favorite", label: "♥ お気に入り" },
      { key: "type", label: "PNG" },
      { key: "tag", label: "タグ:night" },
      { key: "addedRange", label: "追加日 2026-01-01〜2026-06-30" },
    ]);
  });

  it("フィルタ有効 (filtersActive) とチップありは一致する", () => {
    const cases: GuiFilters[] = [
      filters(),
      filters({ ratingMin: 5 }),
      filters({ cull: "unselected" }),
      filters({ favorite: false }),
      filters({ type: "avif" }),
      filters({ tag: "x" }),
      filters({ addedFrom: "2026-01-01" }),
      filters({ addedTo: "2026-01-01" }),
      filters({ ratingMin: 2, tag: "y", addedFrom: "2026-01-01", addedTo: "2026-12-31" }),
    ];
    for (const caseFilters of cases) {
      expect(filterChips(caseFilters).length > 0).toBe(filtersActive(caseFilters));
    }
  });
});

describe("withoutFilter (UIUX-5/WP2: チップ×で当該フィルタのみ解除)", () => {
  it("各キーは当該フィールドのみ既定値へ戻し他は保持する", () => {
    const full = filters({
      ratingMin: 3,
      cull: "picked",
      favorite: true,
      type: "png",
      tag: "night",
      addedFrom: "2026-01-01",
      addedTo: "2026-06-30",
    });
    expect(withoutFilter(full, "ratingMin")).toEqual(
      filters({
        cull: "picked",
        favorite: true,
        type: "png",
        tag: "night",
        addedFrom: "2026-01-01",
        addedTo: "2026-06-30",
      }),
    );
    expect(withoutFilter(full, "cull")).toEqual(
      filters({
        ratingMin: 3,
        favorite: true,
        type: "png",
        tag: "night",
        addedFrom: "2026-01-01",
        addedTo: "2026-06-30",
      }),
    );
    expect(withoutFilter(full, "favorite")).toEqual(
      filters({
        ratingMin: 3,
        cull: "picked",
        type: "png",
        tag: "night",
        addedFrom: "2026-01-01",
        addedTo: "2026-06-30",
      }),
    );
    expect(withoutFilter(full, "type")).toEqual(
      filters({
        ratingMin: 3,
        cull: "picked",
        favorite: true,
        tag: "night",
        addedFrom: "2026-01-01",
        addedTo: "2026-06-30",
      }),
    );
    expect(withoutFilter(full, "tag")).toEqual(
      filters({
        ratingMin: 3,
        cull: "picked",
        favorite: true,
        type: "png",
        addedFrom: "2026-01-01",
        addedTo: "2026-06-30",
      }),
    );
  });

  it("addedRange は追加日範囲 (from/to両方) を解除する", () => {
    const full = filters({
      ratingMin: 3,
      tag: "night",
      addedFrom: "2026-01-01",
      addedTo: "2026-06-30",
    });
    expect(withoutFilter(full, "addedRange")).toEqual(filters({ ratingMin: 3, tag: "night" }));
  });

  it("単独フィルタを解除すると未設定に戻る", () => {
    expect(withoutFilter(filters({ tag: "night" }), "tag")).toEqual(filters());
    expect(withoutFilter(filters({ addedTo: "2026-06-30" }), "addedRange")).toEqual(filters());
  });
});
