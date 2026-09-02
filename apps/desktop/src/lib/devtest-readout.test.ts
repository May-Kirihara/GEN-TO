import { describe, expect, it } from "vitest";
import {
  DEVTEST_READOUT_KEY,
  parseDevReadoutVisible,
  toggleDevReadout,
} from "./devtest-readout";

type StorageLike = { getItem(k: string): string | null; setItem(k: string, v: string): void };

function makeStorage(initial = new Map<string, string>()): StorageLike & { data: Map<string, string> } {
  return {
    data: initial,
    getItem: (k) => initial.get(k) ?? null,
    setItem: (k, v) => {
      initial.set(k, v);
    },
  };
}

describe("devtest読み出し可視性 (DEVUX-1)", () => {
  it("未設定・0・破損値は既定の非表示", () => {
    expect(parseDevReadoutVisible(null)).toBe(false);
    expect(parseDevReadoutVisible("0")).toBe(false);
    expect(parseDevReadoutVisible("")).toBe(false);
    expect(parseDevReadoutVisible("true")).toBe(false);
  });

  it('"1"のときのみ可視', () => {
    expect(parseDevReadoutVisible("1")).toBe(true);
  });

  it("トグルは反転して永続化 (既定OFF → ON → OFF)", () => {
    const storage = makeStorage();
    expect(toggleDevReadout(storage)).toBe(true);
    expect(storage.data.get(DEVTEST_READOUT_KEY)).toBe("1");
    expect(toggleDevReadout(storage)).toBe(false);
    expect(storage.data.get(DEVTEST_READOUT_KEY)).toBe("0");
  });

  it("保存済みONは次回起動で復元 (選択の維持)", () => {
    const storage = makeStorage(new Map([[DEVTEST_READOUT_KEY, "1"]]));
    expect(parseDevReadoutVisible(storage.getItem(DEVTEST_READOUT_KEY))).toBe(true);
  });

  it("キーはgento:接頭辞 (resetStorageで既定OFFへ戻る契約)", () => {
    expect(DEVTEST_READOUT_KEY.startsWith("gento:")).toBe(true);
  });
});
