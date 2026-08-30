import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_KEYMAP,
  KeyDispatcher,
  isEditableTarget,
  matchBinding,
  type KeyAction,
} from "./keys";

/** DOM無し環境 (node) でも動くKeyboardEvent風オブジェクト。 */
function keyEvent(
  init: Partial<KeyboardEvent> & { key: string },
): KeyboardEvent {
  let defaultPrevented = false;
  return {
    key: init.key,
    ctrlKey: init.ctrlKey ?? false,
    metaKey: init.metaKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: init.shiftKey ?? false,
    target: init.target ?? null,
    get defaultPrevented(): boolean {
      return defaultPrevented;
    },
    preventDefault(): void {
      defaultPrevented = true;
    },
    stopPropagation: vi.fn(),
  } as unknown as KeyboardEvent;
}

describe("DEFAULT_KEYMAP", () => {
  it("必須割り当て (Space/Enter/Esc/F2/Delete) が揃っている", () => {
    expect(DEFAULT_KEYMAP.quickPreview.key).toBe("Space");
    expect(DEFAULT_KEYMAP.openViewer.key).toBe("Enter");
    expect(DEFAULT_KEYMAP.back.key).toBe("Escape");
    expect(DEFAULT_KEYMAP.rename.key).toBe("F2");
    expect(DEFAULT_KEYMAP.trash.key).toBe("Delete");
    expect(DEFAULT_KEYMAP.prevImage.key).toBe("ArrowLeft");
    expect(DEFAULT_KEYMAP.nextImage.key).toBe("ArrowRight");
  });
});

describe("matchBinding", () => {
  const cases: Array<[string, KeyAction | null]> = [
    // ブラウザは空白キーを " " として返す → "Space" 割り当てに正規化一致
    [" ", "quickPreview"],
    ["Space", "quickPreview"],
    ["Enter", "openViewer"],
    ["Escape", "back"],
    ["F2", "rename"],
    ["Delete", "trash"],
    ["ArrowLeft", "prevImage"],
    ["a", null],
    ["F8", null],
  ];

  it.each(cases)("key=%s -> %s", (key, expected) => {
    expect(matchBinding(DEFAULT_KEYMAP, keyEvent({ key }))).toBe(expected);
  });

  it("修飾キー付きは対象外", () => {
    expect(matchBinding(DEFAULT_KEYMAP, keyEvent({ key: "Space", ctrlKey: true }))).toBeNull();
    expect(matchBinding(DEFAULT_KEYMAP, keyEvent({ key: "Enter", metaKey: true }))).toBeNull();
    expect(matchBinding(DEFAULT_KEYMAP, keyEvent({ key: "Escape", altKey: true }))).toBeNull();
    // shiftは許容 (Shift+Space等を潰さない)
    expect(matchBinding(DEFAULT_KEYMAP, keyEvent({ key: "Space", shiftKey: true }))).toBe(
      "quickPreview",
    );
  });
});

describe("KeyDispatcher", () => {
  it("登録したactionのハンドラだけが呼ばれ、イベントを消費する", () => {
    const dispatcher = new KeyDispatcher();
    const viewer = vi.fn();
    dispatcher.register("openViewer", viewer);
    const event = keyEvent({ key: "Enter" });
    expect(dispatcher.handleEvent(event)).toBe(true);
    expect(viewer).toHaveBeenCalledWith(event);
    expect(event.defaultPrevented).toBe(true);
  });

  it("未登録のactionは素通し (ハンドラ無し・消費なし)", () => {
    const dispatcher = new KeyDispatcher();
    const event = keyEvent({ key: "Enter" });
    expect(dispatcher.handleEvent(event)).toBe(false);
    expect(event.defaultPrevented).toBe(false);
  });

  it("ハンドラがfalseを返せばイベントを通す (文脈によって不成立)", () => {
    const dispatcher = new KeyDispatcher();
    dispatcher.register("prevImage", () => false);
    const event = keyEvent({ key: "ArrowLeft" });
    expect(dispatcher.handleEvent(event)).toBe(false);
    expect(event.defaultPrevented).toBe(false);
  });

  it("defaultPrevented済みのイベントは無視する", () => {
    const dispatcher = new KeyDispatcher();
    const viewer = vi.fn();
    dispatcher.register("openViewer", viewer);
    const event = keyEvent({ key: "Enter" });
    event.preventDefault();
    expect(dispatcher.handleEvent(event)).toBe(false);
    expect(viewer).not.toHaveBeenCalled();
  });

  it("編集可能要素への入力はショートカットにしない", () => {
    const dispatcher = new KeyDispatcher();
    const viewer = vi.fn();
    dispatcher.register("openViewer", viewer);
    const inputTarget = { tagName: "INPUT" };
    const event = keyEvent({ key: "Enter", target: inputTarget as unknown as EventTarget });
    expect(dispatcher.handleEvent(event)).toBe(false);
    expect(viewer).not.toHaveBeenCalled();
    expect(isEditableTarget(inputTarget as unknown as EventTarget)).toBe(true);
    expect(isEditableTarget({ tagName: "DIV" } as unknown as EventTarget)).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
    expect(
      isEditableTarget({ isContentEditable: true } as unknown as EventTarget),
    ).toBe(true);
  });

  it("registerの解除関数で登録を外せる", () => {
    const dispatcher = new KeyDispatcher();
    const viewer = vi.fn();
    const unregister = dispatcher.register("openViewer", viewer);
    expect(dispatcher.hasHandler("openViewer")).toBe(true);
    unregister();
    expect(dispatcher.hasHandler("openViewer")).toBe(false);
    expect(dispatcher.handleEvent(keyEvent({ key: "Enter" }))).toBe(false);
    expect(viewer).not.toHaveBeenCalled();
  });

  it("installはcaptureリスナーを付け、解除で外れる", () => {
    const dispatcher = new KeyDispatcher();
    const add = vi.fn();
    const remove = vi.fn();
    const target = { addEventListener: add, removeEventListener: remove };
    const uninstall = dispatcher.install(target);
    expect(add).toHaveBeenCalledWith("keydown", expect.any(Function), true);
    const listener = add.mock.calls[0][1] as (event: KeyboardEvent) => void;
    dispatcher.register("back", () => undefined);
    const event = keyEvent({ key: "Escape" });
    listener(event);
    expect(event.defaultPrevented).toBe(true);
    uninstall();
    expect(remove).toHaveBeenCalledWith("keydown", listener, true);
  });

  it("空白キー (event.key=' ') は既定の表でもクイックプレビューとして扱う", () => {
    // 正規化により DEFAULT_KEYMAP の "Space" 割り当てに " " が一致する
    const dispatcher = new KeyDispatcher();
    const preview = vi.fn();
    dispatcher.register("quickPreview", preview);
    expect(dispatcher.handleEvent(keyEvent({ key: " " }))).toBe(true);
    expect(preview).toHaveBeenCalledOnce();
  });
});
