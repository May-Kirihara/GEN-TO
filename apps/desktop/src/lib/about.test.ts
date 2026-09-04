// @vitest-environment happy-dom
/**
 * about.ts (WP-18 E2: バージョン・ビルド情報) の純粋計算テスト。
 *
 * - normalizeAboutInfo: backend `about_info` 応答の寛容な正規化
 * - buildProfileLabel: debug/release + devtools feature の表示ラベル
 * - CHANGELOG_TEXT: bundle用CHANGELOG.md の埋め込み
 */

import { describe, expect, it } from "vitest";
import { CHANGELOG_TEXT, buildProfileLabel, normalizeAboutInfo } from "./about";

describe("normalizeAboutInfo", () => {
  it("正しい形状をそのまま通す", () => {
    const info = normalizeAboutInfo({
      version: "0.1.0",
      apiVersion: "3",
      commit: "b6e7858abcdef",
      commitShort: "b6e7858",
      profile: "release",
      devtoolsBuild: false,
    });
    expect(info).toEqual({
      version: "0.1.0",
      apiVersion: "3",
      commit: "b6e7858abcdef",
      commitShort: "b6e7858",
      profile: "release",
      devtoolsBuild: false,
    });
  });

  it("文字列でない値や未知の形状にはnullを返す (描画側で取得失敗表示へ)", () => {
    expect(normalizeAboutInfo(null)).toBeNull();
    expect(normalizeAboutInfo("0.1.0")).toBeNull();
    expect(normalizeAboutInfo({ version: 1 })).toBeNull();
    expect(normalizeAboutInfo({})).toBeNull();
  });
});

describe("buildProfileLabel", () => {
  it("debug/releaseをそのまま返す", () => {
    expect(buildProfileLabel("debug", false)).toBe("debug");
    expect(buildProfileLabel("release", false)).toBe("release");
  });

  it("devtools featureを含むビルドには+devtoolsを付ける (開発パネル表示と区別)", () => {
    expect(buildProfileLabel("release", true)).toBe("release (+devtools)");
    expect(buildProfileLabel("debug", true)).toBe("debug (+devtools)");
  });
});

describe("CHANGELOG_TEXT", () => {
  it("bundle対象のCHANGELOG.md 本文を埋め込んでいる", () => {
    expect(CHANGELOG_TEXT).toContain("0.1.0");
    expect(CHANGELOG_TEXT.length).toBeGreaterThan(100);
  });
});
