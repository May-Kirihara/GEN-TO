// @vitest-environment happy-dom
/**
 * FirstRunGuide.svelte コンポーネントテスト (WP-18 E3)。
 *
 * deriveFirstRunGuide の結果ステップを描画し、題名・状態がテキストで
 * 読み取れること (色のみに依存しない) を検証する。
 */

import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import FirstRunGuide from "./FirstRunGuide.svelte";
import { deriveFirstRunGuide } from "./first-run";

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("FirstRunGuide", () => {
  it("ウェルカム見出しと3ステップをテキストで表示する", () => {
    const steps = deriveFirstRunGuide({
      rootsCount: 0,
      aiProfilesCount: 1,
      model: { modelsDir: "/data/models", verifiedCount: 0 },
    })!;
    target = document.createElement("div");
    document.body.appendChild(target);
    component = mount(FirstRunGuide, { target, props: { steps } });
    flushSync();

    const root = target.querySelector<HTMLElement>("[data-testid='first-run-guide']");
    expect(root).not.toBeNull();
    expect(root!.textContent).toContain("ようこそ");
    expect(root!.textContent).toContain("登録ルート");
    expect(root!.textContent).toContain("未実施");
    expect(root!.textContent).toContain("登録済み");
    expect(root!.textContent).toContain("未配置");
    expect(root!.textContent).toContain("/data/models");
  });
});
