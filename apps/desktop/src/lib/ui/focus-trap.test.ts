// @vitest-environment happy-dom
/**
 * focus-trap action (WP-18 E5) のテスト。
 *
 * - マウント時に最初のフォーカス可能要素へフォーカスを移す
 * - Tabで最後→最初へラップ、Shift+Tabで最初→最後へラップする
 * - destroy後にリスナーが外れる
 */

import { afterEach, describe, expect, it } from "vitest";
import { focusTrap } from "./focus-trap";

let cleanup: Array<() => void> = [];

afterEach(() => {
  cleanup.forEach((fn) => fn());
  cleanup = [];
  document.body.innerHTML = "";
});

function key(target: Element, key: string, shiftKey = false): void {
  target.dispatchEvent(
    new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, shiftKey }),
  );
}

describe("focusTrap", () => {
  it("適用時に最初のフォーカス可能要素へフォーカスする", () => {
    const div = document.createElement("div");
    div.innerHTML = "<button>1</button><button>2</button><button>3</button>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    cleanup.push(action.destroy);
    expect(document.activeElement?.textContent).toBe("1");
  });

  it("フォーカス可能要素が無いときはコンテナ自身へフォーカスする", () => {
    const div = document.createElement("div");
    div.innerHTML = "<p>テキストのみ</p>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    cleanup.push(action.destroy);
    expect(document.activeElement).toBe(div);
  });

  it("Tab: 最後の要素から最初へラップする", () => {
    const div = document.createElement("div");
    div.innerHTML = "<button>1</button><button>2</button><button>3</button>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    cleanup.push(action.destroy);
    const buttons = Array.from(div.querySelectorAll("button"));
    buttons[2]!.focus();
    key(buttons[2]!, "Tab");
    expect(document.activeElement?.textContent).toBe("1");
  });

  it("Shift+Tab: 最初の要素から最後へラップする", () => {
    const div = document.createElement("div");
    div.innerHTML = "<button>1</button><button>2</button><button>3</button>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    cleanup.push(action.destroy);
    const buttons = Array.from(div.querySelectorAll("button"));
    buttons[0]!.focus();
    key(buttons[0]!, "Tab", true);
    expect(document.activeElement?.textContent).toBe("3");
  });

  it("Tab以外のキーは介入しない", () => {
    const div = document.createElement("div");
    div.innerHTML = "<button>1</button><button>2</button>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    cleanup.push(action.destroy);
    const buttons = Array.from(div.querySelectorAll("button"));
    buttons[0]!.focus();
    key(buttons[0]!, "Escape");
    expect(document.activeElement?.textContent).toBe("1");
  });

  it("destroy後にTabラップが無効になる", () => {
    const div = document.createElement("div");
    div.innerHTML = "<button>1</button><button>2</button>";
    document.body.appendChild(div);
    const action = focusTrap(div);
    const buttons = Array.from(div.querySelectorAll("button"));
    buttons[1]!.focus();
    action.destroy();
    key(buttons[1]!, "Tab");
    // リスナー解除後は既定動作 (フォーカス移動なし in happy-dom)
    expect(document.activeElement?.textContent).toBe("2");
  });
});
