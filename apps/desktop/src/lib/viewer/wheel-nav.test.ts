import { describe, expect, it } from "vitest";
import {
  WHEEL_NAV_GESTURE_TIMEOUT_MS,
  WHEEL_NAV_THRESHOLD,
  advanceWheelNav,
  initialWheelNavState,
} from "./wheel-nav";

/** 1イベントずつ進めるヘルパー。戻り値は各イベントのstep。 */
function feed(deltas: number[], startMs: number, stepMs: number): number[] {
  let state = initialWheelNavState();
  return deltas.map((delta, i) => {
    const out = advanceWheelNav(state, delta, startMs + i * stepMs);
    state = out.state;
    return out.step;
  });
}

describe("advanceWheelNav (UIUX-3 V-4: 平常wheel=画像送り)", () => {
  it("マウス1ノッチ (deltaY=±100) は即座に1送り", () => {
    // 上方向スクロール = 前の画像・下方向 = 次の画像
    expect(feed([-100], 1000, 0)).toEqual([-1]);
    expect(feed([100], 1000, 0)).toEqual([1]);
  });

  it("閾値は1ノッチ (100px) を下回る単発deltaでも発火できる値 (<=100)", () => {
    expect(WHEEL_NAV_THRESHOLD).toBeLessThanOrEqual(100);
  });

  it("トラックパッドの連続ストリーム (小delta多発) は1ジェスチャ=1送り (latch)", () => {
    // 8px × 30イベントを50ms間隔より短い高頻度で (1ジェスチャとして到達)
    const steps = feed(Array.from({ length: 30 }, () => -8), 1000, 5);
    const advanced = steps.filter((s) => s !== 0);
    expect(advanced).toEqual([-1]);
  });

  it("同ジェスチャ内の追いスクロール (ノッチ連打 <timeout) は追加送りしない", () => {
    // 3ノッチを100ms間隔 (gesture timeout未満) で — 1送りのみ
    const steps = feed([-100, -100, -100], 1000, 100);
    expect(steps.filter((s) => s !== 0)).toEqual([-1]);
  });

  it("静穏期間 (>timeout) を挟んだノッチはそれぞれ1送り (latch解除)", () => {
    const steps = feed(
      [-100, -100, -100],
      1000,
      WHEEL_NAV_GESTURE_TIMEOUT_MS + 50,
    );
    expect(steps).toEqual([-1, -1, -1]);
  });

  it("閾値未満の累積は送りなし・同一ジェスチャ内で累積が維持される", () => {
    const steps = feed([-10, -10, -10, -10], 1000, 50);
    expect(steps.every((s) => s === 0)).toBe(true);
    // 続けて閾値を超える分 (計60) → この時点で1送り
    const out = advanceWheelNav(
      advanceWheelNav(
        advanceWheelNav(
          advanceWheelNav(initialWheelNavState(), -10, 1000).state,
          -10,
          1050,
        ).state,
        -10,
        1100,
      ).state,
      -30,
      1150,
    );
    expect(out.step).toBe(-1);
  });

  it("静穏期間を跨ぐと累積はリセット (古い残余が新ジェスチャに持ち越されない)", () => {
    let state = advanceWheelNav(initialWheelNavState(), -50, 1000).state;
    state = advanceWheelNav(state, -50, 1000 + WHEEL_NAV_GESTURE_TIMEOUT_MS + 100).state;
    // 直前の-50は新しいジェスチャの第1イベントとして扱われ、単体では未発火
    // (閾値60未満のため)。ここではstateの累積が50にリセット済みであることを
    // 次イベントの挙動で観測する: +10では発火しない (旧実装の累積-100なら-1)。
    const next = advanceWheelNav(state, 10, 1000 + WHEEL_NAV_GESTURE_TIMEOUT_MS + 150);
    expect(next.step).toBe(0);
  });

  it("deltaY=0・非有限値は無視 (state不変・step=0)", () => {
    const before = initialWheelNavState();
    for (const bad of [0, Number.NaN, Number.POSITIVE_INFINITY]) {
      const out = advanceWheelNav(before, bad, 1000);
      expect(out.step).toBe(0);
      expect(out.state).toEqual(before);
    }
  });

  it("送り発火後の同一ジェスチャでは方向反転しても追加送りしない (静穏待ち)", () => {
    const steps = feed([-100, 100, -100], 1000, 40);
    expect(steps).toEqual([-1, 0, 0]);
  });
});
