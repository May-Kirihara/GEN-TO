// @vitest-environment node
/**
 * RegistrationConfirmGate 協調テスト (UIUX-3 N2)。
 *
 * 危険パス登録の確認ゲート: 確認が必要なパスでは dialog確定/取消まで
 * request のPromiseが保留され、確定登録でも呼び出し側 (LibraryPanel) の
 * 「戻り値trueで入力クリア」が機能することを保証する。
 */
import { describe, expect, it, vi } from "vitest";
import { RegistrationConfirmGate } from "./registration-confirm";

/** 1マイクロタスク+1タイマー分待つ (pendingが維持されることの傍証用)。 */
async function tick(): Promise<void> {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("RegistrationConfirmGate (UIUX-3 N2)", () => {
  it("確認不要パスはゲートを通らず即登録に流れる", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const result = await gate.request("/home/user/pictures", register);
    expect(result).toBe(true);
    expect(register).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledWith("/home/user/pictures");
    expect(gate.pendingPath).toBeNull();
  });

  it("危険パスはdialog確定までpendingし、登録成功でtrueに解決する", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    let settled = false;
    const request = gate.request("/home/..", register).then((ok) => {
      settled = true;
      return ok;
    });

    // dialog表示中: 登録は未実行・Promiseは未解決のまま
    expect(gate.pendingPath).toBe("/home/..");
    expect(register).not.toHaveBeenCalled();
    await tick();
    expect(settled).toBe(false);

    // dialog確定: pendingPathは即座に外れ (dialogが閉じる)、登録へ進む
    gate.dialogConfirmed(register);
    expect(gate.pendingPath).toBeNull();
    await expect(request).resolves.toBe(true);
    expect(settled).toBe(true);
    expect(register).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledWith("/home/..");
  });

  it("dialog取消は登録せずfalseに解決する (入力クリアしない契約)", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const pending = gate.request("/", register);
    gate.dialogCancelled();
    await expect(pending).resolves.toBe(false);
    expect(register).not.toHaveBeenCalled();
    expect(gate.pendingPath).toBeNull();
  });

  it("登録が失敗すればfalseに解決する", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(false);
    const pending = gate.request("/home", register);
    gate.dialogConfirmed(register);
    await expect(pending).resolves.toBe(false);
    expect(register).toHaveBeenCalledTimes(1);
  });

  it("registerがrejectしてもfalseに解決する (未処理拒否を残さない)", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockRejectedValue(new Error("ipc down"));
    const pending = gate.request("/home", register);
    gate.dialogConfirmed(register);
    await expect(pending).resolves.toBe(false);
  });

  it("取消後に続く新規要求は正常に機能する", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const first = gate.request("/home", register);
    gate.dialogCancelled();
    await expect(first).resolves.toBe(false);
    const second = gate.request("/home", register);
    gate.dialogConfirmed(register);
    await expect(second).resolves.toBe(true);
  });
});
