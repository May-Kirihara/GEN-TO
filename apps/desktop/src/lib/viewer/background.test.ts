import { describe, expect, it } from "vitest";
import {
  DEFAULT_BACKGROUND,
  VIEWER_BACKGROUNDS,
  backgroundCss,
  backgroundLabel,
  loadBackground,
  nextBackground,
  normalizeBackground,
  safeLocalStorage,
  saveBackground,
  type ViewerBackground,
} from "./background";

/** localStorage互換のインメモリStorageモック。 */
function memoryStorage(initial: Record<string, string> = {}): Storage {
  const store = new Map<string, string>(Object.entries(initial));
  return {
    get length() {
      return store.size;
    },
    clear: () => store.clear(),
    getItem: (key: string) => store.get(key) ?? null,
    key: (index: number) => Array.from(store.keys())[index] ?? null,
    removeItem: (key: string) => void store.delete(key),
    setItem: (key: string, value: string) => void store.set(key, value),
  };
}

describe("viewer background: 透過背景切替 (WP-3)", () => {
  it("3種 (市松/白/黒) を循環する", () => {
    expect(VIEWER_BACKGROUNDS).toEqual(["checker", "white", "black"]);
    let current: ViewerBackground = "checker";
    const visited: ViewerBackground[] = [current];
    for (let i = 0; i < 3; i += 1) {
      current = nextBackground(current);
      visited.push(current);
    }
    expect(visited).toEqual(["checker", "white", "black", "checker"]);
  });

  it("localStorageへ永続化し、復元する", () => {
    const storage = memoryStorage();
    saveBackground(storage, "black");
    expect(storage.getItem("gento.viewer.background")).toBe("black");
    expect(loadBackground(storage)).toBe("black");

    saveBackground(storage, "white");
    expect(loadBackground(storage)).toBe("white");
  });

  it("不在・不正値は既定 (市松) に落ちる", () => {
    expect(loadBackground(null)).toBe(DEFAULT_BACKGROUND);
    expect(loadBackground(memoryStorage())).toBe("checker");
    expect(loadBackground(memoryStorage({ "gento.viewer.background": "polka" }))).toBe(
      "checker",
    );
    expect(normalizeBackground("checker")).toBe("checker");
    expect(normalizeBackground("green")).toBeNull();
    expect(normalizeBackground(null)).toBeNull();
  });

  it("storage=nullでも保存は例外にならない (テスト・SSR環境)", () => {
    expect(() => saveBackground(null, "white")).not.toThrow();
  });

  it("CSS値と表示名は全種類で空でない一意な値を返す", () => {
    const css = new Set(VIEWER_BACKGROUNDS.map(backgroundCss));
    const labels = new Set(VIEWER_BACKGROUNDS.map(backgroundLabel));
    expect(css.size).toBe(3);
    expect(labels.size).toBe(3);
    for (const kind of VIEWER_BACKGROUNDS) {
      expect(backgroundCss(kind).length).toBeGreaterThan(0);
      expect(backgroundLabel(kind).length).toBeGreaterThan(0);
    }
    expect(backgroundCss("white")).toBe("#ffffff");
    expect(backgroundCss("black")).toBe("#000000");
    expect(backgroundCss("checker")).toContain("repeating-conic-gradient");
  });

  it("safeLocalStorageはlocalStorage不在環境でnullを返す", () => {
    // node環境 (localStorage不在) ではnull。実ブラウザではStorage。
    const storage = safeLocalStorage();
    if (typeof window === "undefined") {
      expect(storage).toBeNull();
    } else {
      expect(storage).not.toBeNull();
    }
  });
});
