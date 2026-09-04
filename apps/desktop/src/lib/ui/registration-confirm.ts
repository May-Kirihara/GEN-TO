/**
 * 危険パス登録の確認ゲート (UIUX-2 / UIUX-3 N2)。
 *
 * `request` は確認不要パスを即座に登録へ流し、確認が必要なパスでは
 * 「確認dialogが確定 (登録実行) または取消されるまでpending」の
 * Promiseを返す。呼び出し側 (LibraryPanel等) は戻り値trueで入力を
 * クリアしてよい — dialog経由の登録でもクリアが効くことが保証される
 * (r1: 確認dialog経由では requestAddRoot が即falseを返し、入力が
 * 残るままになっていた)。
 */
import { needsRegistrationConfirm } from "../library";

/** 実登録 (確認済みパスでのみ呼ばれる)。成否を返す。 */
export type RegisterRoot = (path: string) => Promise<boolean>;

export class RegistrationConfirmGate {
  #pending: { path: string; settle: (ok: boolean) => void } | null = null;

  constructor(private readonly onChanged: () => void = () => {}) {}

  /** 確認待ちパス (dialogの表示対象)。保留中でなければnull。 */
  get pendingPath(): string | null {
    return this.#pending?.path ?? null;
  }

  /**
   * 登録を要求する。確認が必要なパスは保留状態に入り (onChangedが
   * 発火し `pendingPath` が立つ)、`dialogConfirmed`/`dialogCancelled`
   * まで解決されないPromiseを返す。
   */
  request(path: string, register: RegisterRoot): Promise<boolean> {
    if (!needsRegistrationConfirm(path)) {
      return register(path);
    }
    return new Promise<boolean>((settle) => {
      this.#pending = { path, settle };
      this.onChanged();
    });
  }

  /**
   * dialogの「登録する」: 保留を即座に外し (dialogが閉じる) てから
   * 登録を実行し、その成否で要求を解決する。
   */
  dialogConfirmed(register: RegisterRoot): void {
    const pending = this.#takePending();
    if (pending === null) {
      return;
    }
    void register(pending.path).then(
      (ok) => pending.settle(ok),
      () => pending.settle(false),
    );
  }

  /** dialogの取消: 登録せず要求をfalseで解決する (入力クリアしない)。 */
  dialogCancelled(): void {
    this.#takePending()?.settle(false);
  }

  #takePending(): { path: string; settle: (ok: boolean) => void } | null {
    const pending = this.#pending;
    this.#pending = null;
    if (pending !== null) {
      this.onChanged();
    }
    return pending;
  }
}
