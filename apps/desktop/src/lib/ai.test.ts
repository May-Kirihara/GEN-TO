import { describe, expect, test } from "vitest";
import {
  AI_TESTS,
  AiStore,
  apiKeyForSave,
  apiKeyPlaceholder,
  apiKeyStoreLabel,
  endpointIsLocal,
  jobCancellable,
  jobStatusLabel,
  jobTypeLabel,
  statusSummary,
  type AiIpc,
  type AiJobsEvent,
} from "./ai";

/** invokeを記録するmock IPC。 */
function mockIpc(
  handlers: Record<string, (args?: Record<string, unknown>) => unknown> = {},
): AiIpc & { calls: Array<{ cmd: string; args?: Record<string, unknown> }> } {
  const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
  return {
    calls,
    invoke: async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ cmd, args });
      const handler = handlers[cmd];
      if (handler === undefined) {
        throw new Error(`unmocked command: ${cmd}`);
      }
      return handler(args) as T;
    },
    listen: async () => () => {},
  };
}

describe("jobStatusLabel / jobTypeLabel", () => {
  test("全状態の日本語ラベル", () => {
    expect(jobStatusLabel("queued")).toBe("待機中");
    expect(jobStatusLabel("running")).toBe("処理中");
    expect(jobStatusLabel("retry_waiting")).toBe("再試行待ち");
    expect(jobStatusLabel("succeeded")).toBe("成功");
    expect(jobStatusLabel("failed")).toBe("失敗");
    expect(jobStatusLabel("cancelled")).toBe("キャンセル");
  });

  test("ジョブ種別ラベル", () => {
    expect(jobTypeLabel("ai_caption_tags")).toBe("タグ・キャプション生成");
    expect(jobTypeLabel("ai_reparse")).toBe("メタデータ再解析");
    expect(jobTypeLabel("unknown_future")).toBe("unknown_future");
  });
});

describe("jobCancellable", () => {
  test("queued/retry_waitingのみキャンセル可", () => {
    expect(jobCancellable("queued")).toBe(true);
    expect(jobCancellable("retry_waiting")).toBe(true);
    expect(jobCancellable("running")).toBe(false);
    expect(jobCancellable("succeeded")).toBe(false);
    expect(jobCancellable("failed")).toBe(false);
    expect(jobCancellable("cancelled")).toBe(false);
  });
});

describe("statusSummary", () => {
  test("件数の0埋めと順序", () => {
    const summary = statusSummary({ queued: 3, failed: 1 });
    expect(summary.map((s) => `${s.label}:${s.count}`)).toEqual([
      "待機中:3",
      "処理中:0",
      "再試行待ち:0",
      "成功:0",
      "失敗:1",
      "キャンセル:0",
    ]);
  });
});

describe("apiKeyForSave", () => {
  test("未編集はundefined (変更なし)", () => {
    expect(apiKeyForSave({ value: "", edited: false }, true)).toBeUndefined();
    expect(apiKeyForSave({ value: "", edited: false }, false)).toBeUndefined();
  });

  test("空入力+既存鍵は削除 (空文字)", () => {
    expect(apiKeyForSave({ value: "", edited: true }, true)).toBe("");
  });

  test("空入力+既存なしは何もしない", () => {
    expect(apiKeyForSave({ value: "", edited: true }, false)).toBeUndefined();
  });

  test("入力値はそのまま渡す", () => {
    expect(apiKeyForSave({ value: "sk-abc", edited: true }, false)).toBe("sk-abc");
    // 既存があっても入力あれば更新
    expect(apiKeyForSave({ value: "sk-new", edited: true }, true)).toBe("sk-new");
  });
});

describe("apiKeyPlaceholder / apiKeyStoreLabel", () => {
  test("プレースホルダ", () => {
    expect(apiKeyPlaceholder(true)).toContain("保存済み");
    expect(apiKeyPlaceholder(false)).toContain("空欄で可");
  });

  test("保存先ラベル", () => {
    expect(apiKeyStoreLabel("keyring")).toBe("資格情報ストアに保存");
    expect(apiKeyStoreLabel("file")).toContain("平文");
    expect(apiKeyStoreLabel(null)).toBe("APIキーなし");
  });
});

describe("endpointIsLocal", () => {
  test("localhost/127.0.0.1はローカル", () => {
    expect(endpointIsLocal("http://127.0.0.1:8080/v1")).toBe(true);
    expect(endpointIsLocal("http://localhost:3000/v1")).toBe(true);
  });

  test("外部はローカルでない", () => {
    expect(endpointIsLocal("https://api.openai.com/v1")).toBe(false);
    expect(endpointIsLocal("https://example.com/v1")).toBe(false);
  });

  test("不正URLはfalse", () => {
    expect(endpointIsLocal("not a url")).toBe(false);
  });
});

describe("AI_TESTS", () => {
  test("4種のテスト", () => {
    expect(AI_TESTS.map((t) => t.label)).toEqual(["接続", "テキスト", "Vision", "構造化JSON"]);
  });
});

describe("AiStore", () => {
  test("enqueueはforceを渡す", async () => {
    const ipc = mockIpc({ ai_jobs_enqueue: () => 2 });
    const store = new AiStore(ipc);
    const n = await store.enqueue(["1", "2"], true);
    expect(n).toBe(2);
    expect(ipc.calls[0]).toEqual({ cmd: "ai_jobs_enqueue", args: { assetIds: ["1", "2"], force: true } });
  });

  test("saveProfileはapiKey未編集なら鍵を送らない", async () => {
    const ipc = mockIpc({ ai_profile_save: () => ({}) });
    const store = new AiStore(ipc);
    await store.saveProfile(
      {
        displayName: "x",
        baseUrl: "http://127.0.0.1:1/v1",
        model: "m",
        visionCapable: true,
        enabled: true,
        timeoutMs: 60000,
        concurrency: 2,
        extra: {
          send_exif: false,
          send_gps: false,
          max_image_edge: 1024,
          send_response_format: false,
        },
      },
      undefined,
    );
    expect(ipc.calls[0]?.args?.apiKey).toBeUndefined();
    expect(ipc.calls[0]?.args?.profile).toBeDefined();

    await store.saveProfile(
      {
        displayName: "x",
        baseUrl: "http://127.0.0.1:1/v1",
        model: "m",
        visionCapable: true,
        enabled: true,
        timeoutMs: 60000,
        concurrency: 2,
        extra: {
          send_exif: false,
          send_gps: false,
          max_image_edge: 1024,
          send_response_format: false,
        },
      },
      "",
    );
    expect(ipc.calls[1]?.args?.apiKey).toBe("");
  });

  test("runTestは種別ごとのコマンドへ振り分け", async () => {
    const ipc = mockIpc({
      ai_profile_test_connection: () => ({ ok: true, detail: "", elapsedMs: 1, status: 200 }),
      ai_profile_test_structured: () => ({ ok: false, detail: "", elapsedMs: 1, status: null }),
    });
    const store = new AiStore(ipc);
    expect((await store.runTest("7", "connection")).ok).toBe(true);
    expect((await store.runTest("7", "structured")).ok).toBe(false);
    expect(ipc.calls.map((c) => c.cmd)).toEqual([
      "ai_profile_test_connection",
      "ai_profile_test_structured",
    ]);
  });

  test("subscribeはai:jobsイベントをハンドラへ渡す", async () => {
    const captured: Array<(event: { payload: AiJobsEvent }) => void> = [];
    const ipc: AiIpc = {
      invoke: async <T,>() => ({}) as T,
      listen: async <T,>(_event: string, handler: (event: { payload: T }) => void) => {
        captured.push(handler as (event: { payload: AiJobsEvent }) => void);
        return () => {};
      },
    };
    const store = new AiStore(ipc);
    const received: AiJobsEvent[] = [];
    await store.subscribe((event) => received.push(event));
    expect(captured.length).toBe(1);
    captured[0]?.({ payload: { changes: [], counts: { queued: 1 }, paused: false } });
    expect(received).toHaveLength(1);
    expect(received[0]?.counts.queued).toBe(1);
  });

  test("エラーはlastErrorへ保持して再送出", async () => {
    const ipc = mockIpc({
      ai_jobs_pause: () => {
        throw new Error("DBなし");
      },
    });
    const store = new AiStore(ipc);
    await expect(store.pause()).rejects.toThrow("DBなし");
    expect(store.lastError).toBe("DBなし");
  });
});
