/**
 * semantic-suggest.ts 純関数テスト (UIUX-7/WP1 — test-first。
 * WP3で却下済みquery・実行中条件を追加)。
 *
 * サジェストチップ「🧠 意味検索を試す」の表示条件 (すべて満たすときtrue):
 * ①semanticAvailable ②weightPercent===0 (キーワードモード)
 * ③query.trim()非empty ④構文トークン ([A-Za-z_]+:) を含まない
 * ⑤query !== dismissedQuery (チップclickで却下済みの同一クエリ)
 * ⑥running===false (検索実行中は再実行点滅を防ぐため出さない)
 * ⑦searchActive!==false (検索結果表示中のみ — 終了後はlastSearchMetaが
 * 残っていても出さない / WP4)
 */
import { describe, expect, it } from "vitest";
import { suggestsSemantic } from "./semantic-suggest";

describe("suggestsSemantic — サジェストチップ表示判定", () => {
  it("全条件を満たすときtrue", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true)).toBe(true);
    expect(suggestsSemantic("青い髪の少女", 0, true)).toBe(true);
  });

  it("意味検索不可ならfalse", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, false)).toBe(false);
  });

  it("weightPercent>0 (キーワードモード以外の実行) ならfalse", () => {
    expect(suggestsSemantic("夕暮れの海岸", 50, true)).toBe(false);
    expect(suggestsSemantic("夕暮れの海岸", 100, true)).toBe(false);
  });

  it("空・空白のみのクエリならfalse", () => {
    expect(suggestsSemantic("", 0, true)).toBe(false);
    expect(suggestsSemantic("   ", 0, true)).toBe(false);
  });

  it("構文トークン ([A-Za-z_]+:) を含むならfalse", () => {
    expect(suggestsSemantic("tag:猫", 0, true)).toBe(false);
    expect(suggestsSemantic("rating:>=3", 0, true)).toBe(false);
    expect(suggestsSemantic("type:png", 0, true)).toBe(false);
    expect(suggestsSemantic("added:2026-01-01..2026-06-30", 0, true)).toBe(false);
    expect(suggestsSemantic('model:"sdxl"', 0, true)).toBe(false);
    // 自然文+構文の混在も構文入力中と同様に出さない
    expect(suggestsSemantic("少女 tag:night", 0, true)).toBe(false);
  });

  it("コロンを含まない自然文はtrue (構文言語の単語だけでは抑止しない)", () => {
    expect(suggestsSemantic("rating が高い少女", 0, true)).toBe(true);
    expect(suggestsSemantic("海の見える街", 0, true)).toBe(true);
  });

  it("却下済みクエリ (dismissedQuery) と同一ならfalse (WP3)", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { dismissedQuery: "夕暮れの海岸" })).toBe(
      false,
    );
  });

  it("却下済みでも別クエリならtrue (新しい検索で自然に再表示)", () => {
    expect(suggestsSemantic("青い髪の少女", 0, true, { dismissedQuery: "夕暮れの海岸" })).toBe(
      true,
    );
  });

  it("dismissedQueryがnull (未却下) なら従来どおりtrue", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { dismissedQuery: null })).toBe(true);
  });

  it("実行中 (running) はfalse — 他条件を満たしていても出さない (WP3)", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { running: true })).toBe(false);
    expect(
      suggestsSemantic("夕暮れの海岸", 0, true, { dismissedQuery: null, running: true }),
    ).toBe(false);
  });

  it("実行完了後 (running=false) は条件次第でtrueに戻る", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { running: false })).toBe(true);
  });

  it("検索終了後 (searchActive=false) は他条件を満たしていてもfalse (WP4)", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { searchActive: false })).toBe(false);
    expect(
      suggestsSemantic("夕暮れの海岸", 0, true, { dismissedQuery: null, searchActive: false }),
    ).toBe(false);
  });

  it("検索結果表示中 (searchActive=true) は従来どおり条件次第でtrue (WP4)", () => {
    expect(suggestsSemantic("夕暮れの海岸", 0, true, { searchActive: true })).toBe(true);
  });
});
