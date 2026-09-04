// @vitest-environment happy-dom
/**
 * conditionalWheel Svelte action のリスナライフサイクルテスト (UIUX-1 r1)。
 *
 * - 係数1.0 (enabled=false) ではリスナを登録しない
 * - 有効時は {passive:false} で登録する
 * - updateは同一listener identityで除去→再登録 (二重登録なし)
 * - 無効化updateで除去。destroyで除去
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { conditionalWheel } from "./wheel-speed";

let node: HTMLElement;
let addSpy: ReturnType<typeof vi.spyOn>;
let removeSpy: ReturnType<typeof vi.spyOn>;
let handler: (event: WheelEvent) => void;

beforeEach(() => {
  node = document.createElement("div");
  document.body.appendChild(node);
  addSpy = vi.spyOn(node, "addEventListener");
  removeSpy = vi.spyOn(node, "removeEventListener");
  handler = () => {};
});

afterEach(() => {
  document.body.removeChild(node);
  vi.restoreAllMocks();
});

describe("conditionalWheel", () => {
  it("enabled=false (係数1.0相当) ではリスナを登録しない", () => {
    conditionalWheel(node, { enabled: false, handler });
    expect(addSpy).not.toHaveBeenCalled();
  });

  it("enabled=true は {passive:false} でwheelを登録する", () => {
    conditionalWheel(node, { enabled: true, handler });
    expect(addSpy).toHaveBeenCalledTimes(1);
    const [type, listener, options] = addSpy.mock.calls[0]!;
    expect(type).toBe("wheel");
    expect(typeof listener).toBe("function");
    expect(options).toEqual({ passive: false });
  });

  it("handlerは登録時でなくイベント時に評価される (updateで差し替わる)", () => {
    const events: string[] = [];
    conditionalWheel(node, {
      enabled: true,
      handler: () => events.push("first"),
    }).update({ enabled: true, handler: () => events.push("second") });
    node.dispatchEvent(new WheelEvent("wheel", { cancelable: true }));
    expect(events).toEqual(["second"]);
  });

  it("updateは同一identityで除去→再登録する (二重登録しない)", () => {
    const action = conditionalWheel(node, { enabled: true, handler });
    const first = addSpy.mock.calls[0]![1];
    addSpy.mockClear();
    action.update({ enabled: true, handler: () => {} });
    // 除去は登録時と同一の関数参照で行われる
    expect(removeSpy).toHaveBeenCalledTimes(1);
    expect(removeSpy.mock.calls[0]![1]).toBe(first);
    expect(addSpy).toHaveBeenCalledTimes(1);
    // 再登録されたidentityも登録時と同じ (action内部の一定listener)
    expect(addSpy.mock.calls[0]![1]).toBe(first);
  });

  it("enabled=true→false のupdateでリスナを除去する", () => {
    const action = conditionalWheel(node, { enabled: true, handler });
    const registered = addSpy.mock.calls[0]![1];
    action.update({ enabled: false, handler });
    expect(removeSpy).toHaveBeenCalledWith("wheel", registered);
    expect(addSpy).toHaveBeenCalledTimes(1);
  });

  it("destroyでリスナを除去する", () => {
    const action = conditionalWheel(node, { enabled: true, handler });
    const registered = addSpy.mock.calls[0]![1];
    action.destroy();
    expect(removeSpy).toHaveBeenCalledWith("wheel", registered);
  });
});
