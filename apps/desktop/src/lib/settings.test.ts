/**
 * 設定・診断ユーティリティのテスト (MVP WP-15)。
 *
 * - endpoint分類: 各クラスのfixture URL (ローカル/プライベート/外部/不正)
 * - formatBytes / mibToBytes
 * - normalizeSettings: 破損応答でも既定値へ落ちる (設定破損時の安全な既定値)
 */
import { describe, expect, it } from "vitest";
import {
  classifyEndpoint,
  defaultSettings,
  endpointClassLabel,
  formatBytes,
  mibToBytes,
  normalizeSettings,
} from "./settings";

describe("classifyEndpoint", () => {
  it("ローカル: localhost / 127.0.0.1 / ::1", () => {
    expect(classifyEndpoint("http://localhost:8080/v1")).toBe("local");
    expect(classifyEndpoint("http://127.0.0.1:8080/v1")).toBe("local");
    expect(classifyEndpoint("http://[::1]:8080/v1")).toBe("local");
    expect(classifyEndpoint("https://localhost/v1")).toBe("local");
  });

  it("プライベート: RFC1918 + link-local + IPv6 ULA/link-local", () => {
    expect(classifyEndpoint("http://10.0.0.5:11434/v1")).toBe("private");
    expect(classifyEndpoint("http://10.255.255.255/v1")).toBe("private");
    expect(classifyEndpoint("http://172.16.0.1/v1")).toBe("private");
    expect(classifyEndpoint("http://172.31.255.254/v1")).toBe("private");
    expect(classifyEndpoint("http://192.168.1.20:8080/v1")).toBe("private");
    expect(classifyEndpoint("http://169.254.7.7/v1")).toBe("private");
    expect(classifyEndpoint("http://[fe80::1]:8080/v1")).toBe("private");
    expect(classifyEndpoint("http://[fd00::123]/v1")).toBe("private");
  });

  it("外部: パブリックhost", () => {
    expect(classifyEndpoint("https://api.openai.com/v1")).toBe("external");
    expect(classifyEndpoint("http://8.8.8.8/v1")).toBe("external");
    // 172.32/172.15はRFC1918 (172.16〜172.31) の範囲外
    expect(classifyEndpoint("http://172.32.0.1/v1")).toBe("external");
    expect(classifyEndpoint("http://172.15.0.1/v1")).toBe("external");
    expect(classifyEndpoint("http://[2001:db8::1]/v1")).toBe("external");
    expect(classifyEndpoint("http://llama.example.local/v1")).toBe("external");
  });

  it("不正URL", () => {
    expect(classifyEndpoint("")).toBe("invalid");
    expect(classifyEndpoint("not a url")).toBe("invalid");
    expect(classifyEndpoint("::::")).toBe("invalid");
  });

  it("表示ラベル", () => {
    expect(endpointClassLabel("local")).toBe("ローカル");
    expect(endpointClassLabel("private")).toBe("プライベートネットワーク");
    expect(endpointClassLabel("external")).toBe("外部");
    expect(endpointClassLabel("invalid")).toBe("不明なURL");
  });
});

describe("formatBytes / mibToBytes", () => {
  it("formatBytes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(2 * 1024 ** 3)).toBe("2.0 GiB");
    expect(formatBytes(-1)).toBe("-");
    expect(formatBytes(Number.NaN)).toBe("-");
  });

  it("mibToBytes", () => {
    expect(mibToBytes(512)).toBe(512 * 1024 * 1024);
    expect(mibToBytes(2048)).toBe(2 * 1024 ** 3);
  });
});

describe("normalizeSettings", () => {
  it("正常応答をそのまま通す", () => {
    const dto = normalizeSettings({
      schemaVersion: 1,
      corruptKeys: [],
      general: { logLevel: "warn" },
      cache: { thumbsMaxBytes: 1024 ** 3, usedBytes: 4096, maxBytes: 1024 ** 3 },
    });
    expect(dto.general.logLevel).toBe("warn");
    expect(dto.cache.thumbsMaxBytes).toBe(1024 ** 3);
    expect(dto.cache.usedBytes).toBe(4096);
  });

  it("破損応答・非オブジェクトは既定値 (info / 2GiB) へ", () => {
    expect(normalizeSettings(null)).toEqual(defaultSettings());
    expect(normalizeSettings("oops")).toEqual(defaultSettings());
    expect(normalizeSettings({}).general.logLevel).toBe("info");
    expect(normalizeSettings({}).cache.thumbsMaxBytes).toBe(2 * 1024 ** 3);
    // 一部欠損時は既定値で補完
    const partial = normalizeSettings({ general: {}, cache: { usedBytes: 10 } });
    expect(partial.general.logLevel).toBe("info");
    expect(partial.cache.thumbsMaxBytes).toBe(2 * 1024 ** 3);
    expect(partial.cache.usedBytes).toBe(10);
    // corruptKeysに非文字列が混んでいても文字列のみ残す
    expect(
      normalizeSettings({ corruptKeys: ["general.log_level", 3, null] }).corruptKeys,
    ).toEqual(["general.log_level"]);
  });
});
