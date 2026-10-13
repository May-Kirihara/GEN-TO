import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ScheduleApply, type ApplyTimerHooks } from "./schedule-apply";

describe("ScheduleApply — trailingデバウンス (WP-UX-FILTER)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("スケジュール直後は実行せず、遅延経過後にちょうど1回だけ実行する", () => {
    const apply = new ScheduleApply(150);
    const fn = vi.fn();
    apply.schedule(fn);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(150);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("連続scheduleは最終のfnのみ1回に収束する (フィルタ連打・チップ連続削除)", () => {
    const apply = new ScheduleApply(150);
    const first = vi.fn();
    const second = vi.fn();
    const third = vi.fn();
    apply.schedule(first);
    vi.advanceTimersByTime(100);
    apply.schedule(second);
    vi.advanceTimersByTime(100);
    apply.schedule(third);
    vi.advanceTimersByTime(150);
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();
    expect(third).toHaveBeenCalledTimes(1);
  });

  it("遅延途中の再scheduleは待ち時間を引き直す (残時間を引き継がない)", () => {
    const apply = new ScheduleApply(150);
    const fn = vi.fn();
    apply.schedule(fn);
    vi.advanceTimersByTime(149);
    apply.schedule(fn);
    vi.advanceTimersByTime(149);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("cancelは保留中の実行を握り潰す", () => {
    const apply = new ScheduleApply(150);
    const fn = vi.fn();
    apply.schedule(fn);
    apply.cancel();
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
  });

  it("実行済み・未スケジュール状態でのcancelは何もしない (安全な再呼び出し)", () => {
    const apply = new ScheduleApply(150);
    const fn = vi.fn();
    apply.cancel();
    apply.schedule(fn);
    vi.advanceTimersByTime(150);
    apply.cancel();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("isPendingはschedule→実行/cancelで遷移する", () => {
    const apply = new ScheduleApply(150);
    expect(apply.isPending()).toBe(false);
    apply.schedule(() => {});
    expect(apply.isPending()).toBe(true);
    apply.cancel();
    expect(apply.isPending()).toBe(false);
    apply.schedule(() => {});
    vi.advanceTimersByTime(150);
    expect(apply.isPending()).toBe(false);
  });
});

describe("ScheduleApply — タイマー注入", () => {
  it("注入タイマーに遅延値をそのまま渡し、handler実行でfnが走りhandleを解放する", () => {
    const handlers: (() => void)[] = [];
    const cleared: unknown[] = [];
    const delays: number[] = [];
    const hooks: ApplyTimerHooks = {
      setTimeout: (handler, ms) => {
        handlers.push(handler);
        delays.push(ms);
        return handlers.length;
      },
      clearTimeout: (handle) => {
        cleared.push(handle);
      },
    };
    const apply = new ScheduleApply(250, hooks);
    const fn = vi.fn();
    apply.schedule(fn);
    expect(delays).toEqual([250]);
    expect(apply.isPending()).toBe(true);
    // 再scheduleは旧handleを明示破棄してから新設する
    apply.schedule(fn);
    expect(cleared).toEqual([1]);
    handlers[1]!();
    expect(fn).toHaveBeenCalledTimes(1);
    expect(apply.isPending()).toBe(false);
    // 実行解放後の再scheduleはclearTimeout不要
    apply.schedule(fn);
    expect(cleared).toEqual([1]);
  });
});
