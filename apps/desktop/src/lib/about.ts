/**
 * アプリのバージョン・ビルド情報 (WP-18 E2)。
 *
 * 正本は backend `about_info` (build.rsで埋め込んだcommit hash・profile)。
 * フロントエンドは表示ラベルの整形と、bundle resourcesに同梱する
 * CHANGELOG.md 本文の埋め込みのみを担う。
 */

import changelogText from "../../../../CHANGELOG.md?raw";

/** `about_info` 応答DTO (backend about::AboutInfoDto)。 */
export type AboutInfo = {
  version: string;
  apiVersion: string;
  commit: string;
  commitShort: string;
  profile: string;
  devtoolsBuild: boolean;
};

/**
 * `about_info` 応答を寛容に正規化する。不正形状ではnull
 * (描画側は「取得できません」表示へフォールバック)。
 */
export function normalizeAboutInfo(value: unknown): AboutInfo | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const v = value as Record<string, unknown>;
  if (
    typeof v.version !== "string" ||
    typeof v.apiVersion !== "string" ||
    typeof v.commit !== "string" ||
    typeof v.commitShort !== "string" ||
    typeof v.profile !== "string" ||
    typeof v.devtoolsBuild !== "boolean"
  ) {
    return null;
  }
  return {
    version: v.version,
    apiVersion: v.apiVersion,
    commit: v.commit,
    commitShort: v.commitShort,
    profile: v.profile,
    devtoolsBuild: v.devtoolsBuild,
  };
}

/**
 * ビルドprofileの表示ラベル。`+devtools` は計測・検証専用機能
 * (開発パネル) を含む計測用ビルドである旨 — 通常ビルドと区別する。
 */
export function buildProfileLabel(profile: string, devtoolsBuild: boolean): string {
  return devtoolsBuild ? `${profile} (+devtools)` : profile;
}

/** bundle resources同梱のCHANGELOG.md本文 (About表示用)。 */
export const CHANGELOG_TEXT: string = changelogText;
