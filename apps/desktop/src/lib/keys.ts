/**
 * キーボードショートカットの一元化 (MVP WP-4)。
 *
 * - DEFAULT_KEYMAP が割り当ての唯一の正本。UIはこの表から表示ラベルと
 *   キー名を引く (カスタマイズUIはWP-15。表の差し替えで実現予定)。
 * - KeyDispatcher は1つのkeydownリスナー (capture) で表に従って
 *   登録済みハンドラへ振り分ける。ハンドラが false を返した場合は
 *   「この操作は今の文脈では不成立」と扱い、イベントを後段へ通す
 *   (例: 矢印キーはクイックプレビュー表示中のみここで扱い、通常時は
 *   グリッド自身の選択移動へ流す)。
 * - 編集可能要素 (input/textarea/select/contenteditable) への入力は
 *   ショートカットと判断しない。
 */

export type KeyAction =
  | "quickPreview"
  | "openViewer"
  | "back"
  | "rename"
  | "trash"
  | "deleteForever"
  | "prevImage"
  | "nextImage";

export type KeyBinding = {
  action: KeyAction;
  /** KeyboardEvent.key 値 ("Space" / "Enter" / "F2" ...)。 */
  key: string;
  label: string;
  /** `true` の場合Shift押下時のみ一致 (未指定はShift無しのみ)。 */
  shift?: boolean;
};

export const DEFAULT_KEYMAP: Record<KeyAction, KeyBinding> = {
  quickPreview: { action: "quickPreview", key: "Space", label: "クイックプレビュー" },
  openViewer: { action: "openViewer", key: "Enter", label: "ビューアで開く" },
  back: { action: "back", key: "Escape", label: "閉じる / 戻る" },
  // WP-7: F2=リネーム / Delete=ゴミ箱 / Shift+Delete=完全削除 (確認ダイアログ)。
  rename: { action: "rename", key: "F2", label: "リネーム" },
  trash: { action: "trash", key: "Delete", label: "ゴミ箱へ移動" },
  deleteForever: {
    action: "deleteForever",
    key: "Delete",
    shift: true,
    label: "完全削除 (確認あり)",
  },
  prevImage: { action: "prevImage", key: "ArrowLeft", label: "前の画像" },
  nextImage: { action: "nextImage", key: "ArrowRight", label: "次の画像" },
};

/** 編集可能要素への入力か (ショートカットの対象外にする)。
 * duck-typing判定 (テスト環境にDOMが無くても動作する)。 */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (target === null || typeof target !== "object") {
    return false;
  }
  const element = target as { tagName?: unknown; isContentEditable?: unknown };
  const tag = element.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    element.isContentEditable === true
  );
}

/**
 * イベントと割り当ての一致判定。修飾キー (ctrl/meta/alt) 付きは対象外
 * (OS・ブラウザ側の操作と衝突させない。dev bridgeのF-key操作とも分離)。
 *
 * 空白キーはブラウザ実装が event.key を " " (U+0020) で返すため、
 * 割り当て表の "Space" に正規化して比較する。
 */
export function matchBinding(
  bindings: Record<KeyAction, KeyBinding>,
  event: KeyboardEvent,
): KeyAction | null {
  if (event.ctrlKey || event.metaKey || event.altKey) {
    return null;
  }
  const key = event.key === " " ? "Space" : event.key;
  // Shift必須の割り当てを優先 (Shift+Delete=完全削除 等)。通常割り当ては
  // Shift押下でも従来どおり一致 (Shift+Space等を潰さない)。
  for (const binding of Object.values(bindings)) {
    if (binding.shift === true && event.shiftKey && binding.key === key) {
      return binding.action;
    }
  }
  for (const binding of Object.values(bindings)) {
    if (binding.shift !== true && binding.key === key) {
      return binding.action;
    }
  }
  return null;
}

export type KeyHandler = (event: KeyboardEvent) => boolean | void;

/** キー割り当て→ハンドラの振り分け器。 */
export class KeyDispatcher {
  private readonly handlers = new Map<KeyAction, KeyHandler>();
  private readonly listener: (event: KeyboardEvent) => void;

  constructor(
    private readonly bindings: () => Record<KeyAction, KeyBinding> = () => DEFAULT_KEYMAP,
  ) {
    this.listener = (event) => {
      this.handleEvent(event);
    };
  }

  /** actionへのハンドラ登録 (既存は置き換え)。解除関数を返す。 */
  register(action: KeyAction, handler: KeyHandler): () => void {
    this.handlers.set(action, handler);
    return () => {
      if (this.handlers.get(action) === handler) {
        this.handlers.delete(action);
      }
    };
  }

  /** テスト・デバッグ用: 登録の有無。 */
  hasHandler(action: KeyAction): boolean {
    return this.handlers.has(action);
  }

  /**
   * 1イベントの処理。ハンドラが存在し false を返さなければイベントを
   * 消費 (preventDefault + stopPropagation) してtrueを返す。
   */
  handleEvent(event: KeyboardEvent): boolean {
    if (event.defaultPrevented || isEditableTarget(event.target)) {
      return false;
    }
    const action = matchBinding(this.bindings(), event);
    if (action === null) {
      return false;
    }
    const handler = this.handlers.get(action);
    if (handler === undefined) {
      return false;
    }
    if (handler(event) === false) {
      return false;
    }
    event.preventDefault();
    event.stopPropagation();
    return true;
  }

  /** captureフェーズでkeydownを監視する。解除関数を返す。 */
  install(
    target: Pick<EventTarget, "addEventListener" | "removeEventListener"> = window,
  ): () => void {
    const listener = this.listener as EventListener;
    target.addEventListener("keydown", listener, true);
    return () => {
      target.removeEventListener("keydown", listener, true);
    };
  }
}
