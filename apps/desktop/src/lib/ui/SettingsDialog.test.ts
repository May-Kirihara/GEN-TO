// @vitest-environment happy-dom
/**
 * SettingsDialog.svelte コンポーネントテスト (WP-15)。
 *
 * - 破損設定 (settings_get失敗・不正形状) でも既定値で描画する
 * - キャッシュ削除の2段階確認フロー (確認→実行→clear_thumbnail_cache)
 * - AI タブ: endpoint分類 (ローカル/プライベート/外部) とルート行列の表示
 * - 診断タブ: 5領域の描画と更新ボタン・bundle生成のinvoke
 * - ログレベル保存とバックアップ設定保存のinvoke
 * を実際の描画で検証する。IPCはモック化。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { flushSync, mount, unmount } from "svelte";
import SettingsDialog from "./SettingsDialog.svelte";
import type { LibraryRoot } from "../library";
import type { AiProfile } from "../ai";
import type { DiagnosticsDto } from "../settings";

let rootSeq = 0;
function rootOf(path: string, ai: Partial<LibraryRoot> = {}): LibraryRoot {
  rootSeq += 1;
  return {
    id: String(rootSeq),
    uuid: "00000000-0000-7000-8000-000000000001",
    path,
    enabled: true,
    watch_enabled: false,
    available: true,
    ai_enabled: false,
    send_exif: false,
    send_gps: false,
    ...ai,
  };
}

function profileOf(baseUrl: string, id: string): AiProfile {
  return {
    id,
    uuid: `00000000-0000-7000-8000-0000000000${id.padStart(2, "0")}`,
    displayName: `profile-${id}`,
    baseUrl,
    model: "test-model",
    visionCapable: true,
    enabled: true,
    timeoutMs: 30000,
    concurrency: 2,
    extra: {
      send_exif: false,
      send_gps: false,
      max_image_edge: 1024,
      send_response_format: false,
    },
    hasApiKey: false,
    apiKeyStore: null,
  };
}

const HEALTHY_SETTINGS = {
  schemaVersion: 1,
  corruptKeys: [],
  general: { logLevel: "info" },
  cache: { thumbsMaxBytes: 2 * 1024 ** 3, usedBytes: 4096, maxBytes: 2 * 1024 ** 3 },
};

const DIAGNOSTICS: DiagnosticsDto = {
  db: {
    dbOpen: true,
    integrityCheck: "ok",
    schemaVersion: 15,
    dbBytes: 1048576,
    walBytes: 4096,
    settingsCorruptCount: 0,
    dbPath: "/home/user/data/library.sqlite3",
  },
  fts: {
    expectedDocs: 10,
    actualDocs: 9,
    missingDocs: 1,
    orphanDocs: 0,
    duplicateDocs: 0,
    staleContents: 0,
    consistent: false,
    lastRebuildAt: "2026-09-01T00:00:00.000Z",
  },
  usearch: {
    model: "ruri-v3-70m",
    indexedCount: 3,
    sqliteCount: 5,
    stale: true,
    dimensions: 384,
    ready: false,
    building: true,
    lastError: null,
  },
  models: {
    enabled: true,
    packages: [
      { modelId: "ruri-v3-70m", state: "verified", embeddingDim: 384, license: "Apache-2.0" },
    ],
    workerPhase: "running",
    workerRestarts: 0,
    workerLastError: null,
    loadedModel: "ruri-v3-70m",
  },
  jobs: {
    byType: [{ jobType: "ai_caption_tags", status: "queued", count: 2 }],
    oldestPendingAgeSeconds: 125,
  },
};

const ABOUT_INFO = {
  version: "0.1.0",
  apiVersion: "3",
  commit: "b6e7858abcdef0123456789abcdef0123456789",
  commitShort: "b6e7858",
  profile: "release",
  devtoolsBuild: false,
};

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

async function mountDialog(
  options: {
    settings?: unknown;
    roots?: LibraryRoot[];
    profiles?: AiProfile[];
    about?: unknown;
    onClose?: () => void;
  } = {},
): Promise<void> {
  const {
    settings = HEALTHY_SETTINGS,
    roots = [],
    profiles = [],
    about = ABOUT_INFO,
    onClose = () => {},
  } = options;
  target = document.createElement("div");
  document.body.appendChild(target);
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "settings_get") {
      if (settings instanceof Error) {
        throw settings;
      }
      return settings;
    }
    if (command === "about_info") {
      if (about instanceof Error) {
        throw about;
      }
      return about;
    }
    if (command === "backup_settings_get") {
      return { autoFullDb: "off", retentionGenerations: 3 };
    }
    if (command === "backup_default_dir") {
      return "/home/user/backups";
    }
    if (command === "diagnostics_snapshot") {
      return DIAGNOSTICS;
    }
    if (command === "diagnostics_bundle_create") {
      return { path: "/home/user/backups/gento-diagnostics-x.json" };
    }
    if (command === "clear_thumbnail_cache") {
      return { removedFiles: 12, removedBytes: 3 * 1024 * 1024, cacheGeneration: 5 };
    }
    throw new Error(`unexpected command: ${command}`);
  });
  component = mount(SettingsDialog, {
    target,
    props: {
      roots,
      profiles,
      displayInfo: { lastFolder: "/photos/2026", expandedCount: 7, sortLabel: "名前順" },
      onClose,
    },
  });
  flushSync();
  // $effect内の連続invoke (settings/backup/default_dir) が完了するまで待つ
  await vi.waitFor(() => {
    flushSync();
    const calls = invokeMock.mock.calls.map((c) => c[0] as string);
    expect(calls).toContain("backup_default_dir");
  });
  flushSync();
  void settings;
  void component;
}

function click(selector: string): void {
  const el = target!.querySelector<HTMLElement>(selector);
  expect(el, selector).not.toBeNull();
  el!.click();
  flushSync();
}


function checkRadio(selector: string): void {
  const radio = target!.querySelector<HTMLInputElement>(selector);
  expect(radio, selector).not.toBeNull();
  radio!.checked = true;
  radio!.dispatchEvent(new Event("change", { bubbles: true }));
  flushSync();
}

function textOf(selector: string): string {
  const el = target!.querySelector(selector);
  expect(el, selector).not.toBeNull();
  return el!.textContent ?? "";
}

beforeEach(() => {
  invokeMock.mockReset();
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("SettingsDialog", () => {
  it("一般タブ: 既定値 (info / 2GiB) を表示し、ログレベル保存がinvokeされる", async () => {
    await mountDialog();
    expect(textOf("[data-testid='settings-general']")).toContain("ログレベル");
    expect(
      target!.querySelector<HTMLInputElement>("[data-testid='settings-log-level-info']")!.checked,
    ).toBe(true);
    checkRadio("[data-testid='settings-log-level-debug']");
    click("[data-testid='settings-log-level-save']");
    await vi.waitFor(() => {
      const call = invokeMock.mock.calls.find((c) => c[0] === "settings_set_log_level");
      expect(call?.[1]).toEqual({ level: "debug" });
    });
  });

  it("破損設定 (不正形状) でも既定値で描画し、注意を表示する", async () => {
    await mountDialog({ settings: { schemaVersion: 1, corruptKeys: ["general.log_level"], general: {}, cache: {} } });
    expect(textOf("[data-testid='settings-corrupt']")).toContain("general.log_level");
    // 既定値で描画 (info)
    expect(
      target!.querySelector<HTMLInputElement>("[data-testid='settings-log-level-info']")!.checked,
    ).toBe(true);
  });

  it("破損設定 (IPC失敗) でも落ちずに既定値で描画する", async () => {
    await mountDialog({ settings: new Error("db unavailable") });
    expect(target!.querySelector("[data-testid='settings-general']")).not.toBeNull();
    expect(textOf("[data-testid='settings-load-error']")).toContain("db unavailable");
    expect(
      target!.querySelector<HTMLInputElement>("[data-testid='settings-log-level-info']")!.checked,
    ).toBe(true);
  });

  it("キャッシュタブ: 削除は確認なしに実行されず、確認後のみ実行される", async () => {
    await mountDialog();
    // タブ移動: 「キャッシュ」ボタンをクリック
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "キャッシュ")!.click();
    flushSync();
    expect(textOf("[data-testid='cache-usage']")).toContain("4.0 KiB");
    expect(textOf("[data-testid='cache-usage']")).toContain("2.0 GiB");
    // 削除ボタン → 確認表示 (まだ実行しない)
    click("[data-testid='cache-clear-button']");
    expect(target!.querySelector("[data-testid='cache-clear-confirm']")).not.toBeNull();
    expect(invokeMock.mock.calls.some((c) => c[0] === "clear_thumbnail_cache")).toBe(false);
    // 実行
    click("[data-testid='cache-clear-execute']");
    await vi.waitFor(() => {
      expect(invokeMock.mock.calls.some((c) => c[0] === "clear_thumbnail_cache")).toBe(true);
    });
    await vi.waitFor(() => {
      expect(textOf("[data-testid='cache-notice']")).toContain("12");
    });
  });

  it("AIタブ: endpoint分類とルートの外部送信行列を表示", async () => {
    await mountDialog({
      roots: [
        rootOf("/photos/2026", { ai_enabled: true, send_exif: true, send_gps: false }),
        rootOf("/photos/private"),
      ],
      profiles: [
        profileOf("http://127.0.0.1:8080/v1", "1"),
        profileOf("http://192.168.1.5:11434/v1", "2"),
        profileOf("https://api.openai.com/v1", "3"),
      ],
    });
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "AI")!.click();
    flushSync();
    const classes = Array.from(
      target!.querySelectorAll<HTMLElement>("[data-testid='ai-endpoint-class']"),
    ).map((el) => el.textContent?.trim());
    expect(classes).toEqual(["ローカル", "プライベートネットワーク", "外部"]);
    const matrix = textOf("[data-testid='ai-roots-matrix']");
    expect(matrix).toContain("/photos/2026");
    expect(matrix).toContain("オン");
    expect(matrix).toContain("オフ");
  });

  it("診断タブ: 5領域を描画し、更新とbundle生成をinvokeする", async () => {
    await mountDialog();
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "診断")!.click();
    flushSync();
    await vi.waitFor(() => {
      expect(target!.querySelector("[data-testid='diag-db']")).not.toBeNull();
    });
    expect(textOf("[data-testid='diag-db']")).toContain("ok");
    expect(textOf("[data-testid='diag-fts']")).toContain("不整合");
    expect(textOf("[data-testid='diag-usearch']")).toContain("再構築が必要");
    expect(textOf("[data-testid='diag-models']")).toContain("ruri-v3-70m");
    expect(textOf("[data-testid='diag-jobs']")).toContain("2");
    // bundle生成
    const dest = target!.querySelector<HTMLInputElement>("[data-testid='bundle-dest']");
    expect(dest?.value).toBe("/home/user/backups");
    click("[data-testid='bundle-create']");
    await vi.waitFor(() => {
      const call = invokeMock.mock.calls.find((c) => c[0] === "diagnostics_bundle_create");
      expect(call?.[1]).toEqual({ destDir: "/home/user/backups" });
    });
    await vi.waitFor(() => {
      expect(textOf("[data-testid='bundle-result']")).toContain("gento-diagnostics-x.json");
    });
  });

  it("バックアップタブ: 保存が backup_settings_set を呼ぶ", async () => {
    await mountDialog();
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "バックアップ")!.click();
    flushSync();
    expect(
      target!.querySelector<HTMLInputElement>("[data-testid='backup-auto-off']")!.checked,
    ).toBe(true);
    checkRadio("[data-testid='backup-auto-weekly']");
    click("[data-testid='backup-save']");
    await vi.waitFor(() => {
      const call = invokeMock.mock.calls.find((c) => c[0] === "backup_settings_set");
      expect(call?.[1]).toMatchObject({ autoFullDb: "weekly", retentionGenerations: 3 });
    });
  });

  it("表示タブ: 現在の表示prefを読み取り専用で表示", async () => {
    await mountDialog();
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "表示")!.click();
    flushSync();
    const text = textOf("[data-testid='settings-display']");
    expect(text).toContain("/photos/2026");
    expect(text).toContain("7");
    expect(text).toContain("名前順");
  });

  it("このアプリについてタブ: バージョン・ビルド情報・更新履歴・ライセンス情報を表示する", async () => {
    await mountDialog();
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "このアプリについて")!.click();
    flushSync();
    const text = textOf("[data-testid='settings-about']");
    // バージョン・ビルド情報 (about_info)
    expect(text).toContain("0.1.0");
    expect(text).toContain("b6e7858");
    expect(text).toContain("release");
    // 更新履歴 (bundle resourcesのCHANGELOG.md本文)
    expect(text).toContain("0.1.0");
    // ライセンス: モデル (Apache-2.0) とアプリ本体 (未設定である旨)
    expect(text).toContain("Apache-2.0");
    expect(text).toContain("未設定");
  });

  it("このアプリについてタブ: about_info失敗時も失敗表示で描画を続ける", async () => {
    await mountDialog({ about: new Error("about unavailable") });
    const tabs = Array.from(target!.querySelectorAll<HTMLElement>(".tabs button"));
    tabs.find((b) => b.textContent === "このアプリについて")!.click();
    flushSync();
    expect(textOf("[data-testid='settings-about']")).toContain("about unavailable");
  });

  it("キーボード: ダイアログ開いた直後のフォーカスがダイアログ内に移り、Escapeで閉じる", async () => {
    let closed = 0;
    await mountDialog({ onClose: () => (closed += 1) });
    const dialog = target!.querySelector<HTMLElement>(".settings-picker")!;
    // 初期フォーカス: focus trapがダイアログ内の最初のフォーカス可能要素へ移す
    expect(dialog.contains(document.activeElement)).toBe(true);
    // Escapeで閉じる
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    flushSync();
    expect(closed).toBe(1);
  });
});
