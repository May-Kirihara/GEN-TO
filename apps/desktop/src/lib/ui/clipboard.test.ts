import { describe, expect, it, vi } from "vitest";
import { copyText } from "./clipboard";

describe("copyText", () => {
  it("clipboard APIを優先して使う", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    const execCopy = vi.fn().mockReturnValue(true);
    const ok = await copyText("/a/b.png", { writeText, execCopy });
    expect(ok).toBe(true);
    expect(writeText).toHaveBeenCalledWith("/a/b.png");
    expect(execCopy).not.toHaveBeenCalled();
  });

  it("clipboard APIが失敗したらexecCommandフォールバック", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    const execCopy = vi.fn().mockReturnValue(true);
    const ok = await copyText("/a/b.png", { writeText, execCopy });
    expect(ok).toBe(true);
    expect(execCopy).toHaveBeenCalledWith("/a/b.png");
  });

  it("clipboard API不在でもフォールバックで書き込む", async () => {
    const execCopy = vi.fn().mockReturnValue(true);
    const ok = await copyText("/a/b.png", { execCopy });
    expect(ok).toBe(true);
    expect(execCopy).toHaveBeenCalledOnce();
  });

  it("両経路失敗時はfalse", async () => {
    const ok = await copyText("/a/b.png", {
      writeText: () => Promise.reject(new Error("x")),
      execCopy: () => false,
    });
    expect(ok).toBe(false);
  });

  it("clipboard APIが応答しない (timeout) 場合もフォールバックで書き込む", async () => {
    const never = new Promise<void>(() => undefined);
    const execCopy = vi.fn().mockReturnValue(true);
    const ok = await copyText("/a/b.png", {
      writeText: () => never,
      execCopy,
    });
    expect(ok).toBe(true);
    expect(execCopy).toHaveBeenCalledWith("/a/b.png");
  });
});
