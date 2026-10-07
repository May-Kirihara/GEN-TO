// @vitest-environment happy-dom
/**
 * SessionModeBar.svelte コンポーネントテスト (UIUX-5/WP8)。
 *
 * - 前へ/次へボタンのposition連動disabled (先頭/最終)
 * - sessionNoticeのバー内表示 (role=status — InfoPanelを開かなくても見える)
 * - 操作callback (前へ/次へ/採用/除外/終了) のclick発火
 * - role=group・aria-label="選別セッション操作バー"
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import SessionModeBar from "./SessionModeBar.svelte";
import type { SessionCounts } from "../session/selection-session";

let target: HTMLElement | null = null;
let component: Record<string, unknown> | null = null;
let moved: number[] = [];
let triaged: Array<"picked" | "rejected"> = [];
let ended = 0;

function counts(position: number): SessionCounts {
  return { total: 3, position, confirmed: 0, keep: 0, reject: 0, undecided: 3 };
}

function mountBar(position = 1, sessionNotice = ""): void {
  // 同一ファイル内で複数回mountするため、旧targetを先に除去する
  target?.remove();
  target = document.createElement("div");
  document.body.appendChild(target);
  component = mount(SessionModeBar, {
    target,
    props: {
      sessionCounts: counts(position),
      sessionNotice,
      onSessionMove: (delta: number) => moved.push(delta),
      onSessionTriage: async (value: "picked" | "rejected") => {
        triaged.push(value);
      },
      onEndSession: () => (ended += 1),
    },
  });
  flushSync();
}

/** DOM順: [◀ 前へ, 採用, 除外, 次へ ▶, 終了]。 */
function buttons(): HTMLButtonElement[] {
  return Array.from(target!.querySelectorAll<HTMLButtonElement>("button"));
}

beforeEach(() => {
  moved = [];
  triaged = [];
  ended = 0;
});

afterEach(() => {
  if (component) {
    unmount(component);
    component = null;
  }
  target?.remove();
  target = null;
});

describe("SessionModeBar", () => {
  it("先頭位置 (position=0) では前へdisabled・次へenabled", () => {
    mountBar(0);
    expect(buttons()[0]!.disabled).toBe(true);
    expect(buttons()[3]!.disabled).toBe(false);
  });

  it("最終位置 (position=2/total=3) では前へenabled・次へdisabled", () => {
    mountBar(2);
    expect(buttons()[0]!.disabled).toBe(false);
    expect(buttons()[3]!.disabled).toBe(true);
  });

  it("sessionNoticeをバー内にrole=statusで表示する (空なら非表示)", () => {
    mountBar(1, "エラー");
    const notice = target!.querySelector('[role="status"]');
    expect(notice?.textContent).toBe("エラー");
    mountBar(1, "");
    expect(target!.querySelector('[role="status"]')).toBeNull();
  });

  it("各ボタンのclickで対応するcallbackが発火する", () => {
    mountBar(1);
    buttons()[0]!.click();
    buttons()[1]!.click();
    buttons()[2]!.click();
    buttons()[3]!.click();
    buttons()[4]!.click();
    flushSync();
    expect(moved).toEqual([-1, 1]);
    expect(triaged).toEqual(["picked", "rejected"]);
    expect(ended).toBe(1);
  });

  it('role=group・aria-label="選別セッション操作バー"を持つ', () => {
    mountBar(1);
    expect(
      target!.querySelector('[role="group"][aria-label="選別セッション操作バー"]'),
    ).not.toBeNull();
  });
});
