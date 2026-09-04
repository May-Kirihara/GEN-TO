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

  // r2: 二重送信回帰 — 旧実装は2回目が #pending を無条件上書きし、
  // 1回目のPromiseを未解決のまま孤児化させていた (dialog確定でも解決不能)。
  it("二重送信: dialog未解決の2回目要求は即falseに解決し、1回目はdialog確定で解決する", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const first = gate.request("/", register);
    let firstSettled = false;
    void first.then(() => {
      firstSettled = true;
    });

    // 2回目 (1回目のdialogが未解決の間): 即false解決・dialogは置換しない
    await expect(gate.request("/home", register)).resolves.toBe(false);
    expect(gate.pendingPath).toBe("/");
    await tick();
    expect(firstSettled).toBe(false);
    expect(register).not.toHaveBeenCalled();

    // 1回目は通常どおりdialog確定の成否で解決する
    gate.dialogConfirmed(register);
    await expect(first).resolves.toBe(true);
    expect(register).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledWith("/");
  });

  it("二重送信: 拒否された2回目の後も取消意味論は1回目に適用される", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const first = gate.request("/", register);
    await expect(gate.request("/home", register)).resolves.toBe(false);

    gate.dialogCancelled();
    await expect(first).resolves.toBe(false);
    expect(register).not.toHaveBeenCalled();
    expect(gate.pendingPath).toBeNull();

    // busy解除後の新規要求は正常に機能する
    const third = gate.request("/", register);
    gate.dialogConfirmed(register);
    await expect(third).resolves.toBe(true);
  });

  // r3: busy検査はneedsRegistrationConfirm分類より先行しなければならない —
  // 危険path dialog保留中に来た確認不要path要求が分類を通過してregisterへ
  // 直达すると、表示中dialogの背後で並行登録が走り直列化契約をbypassする。
  it("二重送信: 危険path保留中の確認不要path要求も即falseで拒否される", async () => {
    const gate = new RegistrationConfirmGate();
    const register = vi.fn().mockResolvedValue(true);
    const first = gate.request("/", register);
    let firstSettled = false;
    void first.then(() => {
      firstSettled = true;
    });

    // 2回目は確認不要path: busy検査が分類に先行するためregisterへは流れない
    await expect(gate.request("/tmp/a/b/c", register)).resolves.toBe(false);
    expect(register).not.toHaveBeenCalled();
    expect(gate.pendingPath).toBe("/");
    await tick();
    expect(firstSettled).toBe(false);

    // 1回目の危険path要求は通常どおりdialog確定の成否で解決する
    gate.dialogConfirmed(register);
    await expect(first).resolves.toBe(true);
    expect(register).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledWith("/");
  });
});
