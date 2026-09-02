<script lang="ts">
  /**
    * AIパネル (MVP WP-10。spec §11)。
    *
    * - プロファイル一覧・編集 (APIキーはpassword型・保存後マスク表示)
    * - 4種テスト (接続/テキスト/Vision/構造化JSON) の実行と結果表示
    * - ジョブキュー一覧 (状態・進捗・操作: キャンセル/一時停止/再開/失敗再試行)
    * - ルート毎の自動処理設定 (ai_enabled / send_exif / send_gps)
    * - 初回全件処理の確認フロー (件数提示 → 確認 → 一括投入)
    *
    * ビジネスロジック (ラベル・操作可否) は lib/ai.ts の純粋関数
    * (ai.test.tsで単体テスト済み)。このコンポーネントは表示と入力。
    */
  import {
    AI_TESTS,
    apiKeyForSave,
    apiKeyPlaceholder,
    apiKeyStoreLabel,
    endpointIsLocal,
    jobCancellable,
    jobStatusLabel,
    jobTypeLabel,
    statusSummary,
    type AiJobsEvent,
    type AiJobsSnapshot,
    AiStore,
    type AiProfile,
    type AiProfileExtra,
    type AiTestKind,
    type AiTestReport,
  } from "../ai";
  import type { LibraryRoot } from "../library";
  import {
    EmbeddingStore,
    MODEL_CHOICES,
    canEnable,
    enableConfirmText,
    modelLabel,
    packageStateLabel,
    progressRatio,
    progressText,
    workerPhaseLabel,
    type EmbeddingStatus,
  } from "../embedding";
  import { VectorIndexStore, type VectorIndexStatus } from "../vector-index";

  let {
    store,
    roots,
    onClose,
    onJobsChanged,
    ipc,
  }: {
    store: AiStore;
    roots: LibraryRoot[];
    onClose: () => void;
    /** ジョブ完了通知 (App側でInfoPanel再取得などに使う)。 */
    onJobsChanged: () => void;
    /** 埋め込みタブ用IPC注入 (Appから共有)。 */
    ipc?: { invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> };
  } = $props();

  let tab = $state<"profiles" | "jobs" | "auto" | "search">("profiles");
  let notice = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  // ===== プロファイル =====
  let profiles: AiProfile[] = $state([]);
  /** 編集中のプロファイル (null = 一覧表示)。 */
  let editing: {
    id?: string;
    displayName: string;
    baseUrl: string;
    model: string;
    visionCapable: boolean;
    enabled: boolean;
    timeoutMs: number;
    concurrency: number;
    extra: AiProfileExtra;
  } | null = $state(null);
  /** 鍵入力 (edited=false なら保存時に送らない)。 */
  let keyInput = $state({ value: "", edited: false });
  let editingHasKey = $state(false);
  /** テスト実行結果 (kind → report)。 */
  let testResults: Partial<Record<AiTestKind, AiTestReport>> = $state({});
  let testing: AiTestKind | null = $state(null);
  let deletingFor: AiProfile | null = $state(null);

  // ===== ジョブ =====
  let jobs: AiJobsSnapshot | null = $state(null);
  let statusFilter = $state("");
  let stopJobsEvent: (() => void) | null = null;
  let confirmAllFor: { root: LibraryRoot; count: number } | null = $state(null);

  // ===== 自動処理設定 =====
  /** rootId → 設定ドラフト (保存は明示ボタン)。 */
  let autoDrafts = $state<Record<string, { ai: boolean; exif: boolean; gps: boolean }>>({});

  // ===== 自然言語検索 (WP-11) =====
  let embeddingStore: EmbeddingStore | null = $state(null);
  let embeddingStatus: EmbeddingStatus | null = $state(null);
  /** WP-12: USearch索引状態 (自然言語検索タブの進捗表示)。 */
  let vectorIndexStore: VectorIndexStore | null = $state(null);
  let vectorIndexStatus: VectorIndexStatus | null = $state(null);
  let embeddingBusy = $state(false);
  let embeddingError = $state<string | null>(null);
  let embeddingNotice = $state("");
  let confirmEnable = $state(false);
  let confirmDeleteModel: string | null = $state(null);
  let pollStop: (() => void) | null = null;

  async function refreshVectorIndex(): Promise<void> {
    if (vectorIndexStore === null) {
      return;
    }
    try {
      vectorIndexStatus = await vectorIndexStore.status();
    } catch {
      // 状態取得失敗は表示を前回値のままにする
    }
  }

  async function refreshEmbedding(): Promise<void> {
    if (embeddingStore === null) {
      return;
    }
    try {
      embeddingStatus = await embeddingStore.status();
      embeddingError = null;
    } catch (e) {
      embeddingError = e instanceof Error ? e.message : String(e);
    }
  }

  async function doEnable(): Promise<void> {
    if (embeddingStore === null) {
      return;
    }
    embeddingBusy = true;
    embeddingError = null;
    confirmEnable = false;
    try {
      embeddingStatus = await embeddingStore.enable(embeddingStatus?.model);
      embeddingNotice = "自然言語検索を有効にしました";
      await refreshEmbedding();
    } catch (e) {
      embeddingError = e instanceof Error ? e.message : String(e);
    } finally {
      embeddingBusy = false;
    }
  }

  async function doDisable(): Promise<void> {
    if (embeddingStore === null) {
      return;
    }
    embeddingBusy = true;
    embeddingError = null;
    try {
      embeddingStatus = await embeddingStore.disable();
      embeddingNotice = "自然言語検索を無効にしました";
    } catch (e) {
      embeddingError = e instanceof Error ? e.message : String(e);
    } finally {
      embeddingBusy = false;
    }
  }

  async function doSelectModel(model: string): Promise<void> {
    if (embeddingStore === null || embeddingStatus === null) {
      return;
    }
    embeddingBusy = true;
    embeddingError = null;
    try {
      // モデル選択は有効化時のみ影響する (無効状態での選択切り替え)
      embeddingStatus = { ...embeddingStatus, model };
      if (embeddingStatus.enabled) {
        embeddingStatus = await embeddingStore.enable(model);
      }
      await refreshEmbedding();
    } catch (e) {
      embeddingError = e instanceof Error ? e.message : String(e);
    } finally {
      embeddingBusy = false;
    }
  }

  async function doDeleteModel(model: string): Promise<void> {
    if (embeddingStore === null) {
      return;
    }
    embeddingBusy = true;
    embeddingError = null;
    confirmDeleteModel = null;
    try {
      embeddingStatus = await embeddingStore.deleteModel(model);
      embeddingNotice = `モデル (${modelLabel(model)}) を削除しました`;
    } catch (e) {
      embeddingError = e instanceof Error ? e.message : String(e);
    } finally {
      embeddingBusy = false;
    }
  }

  async function refreshProfiles(): Promise<void> {
    try {
      profiles = await store.listProfiles();
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshJobs(): Promise<void> {
    try {
      jobs = await store.listJobs(statusFilter === "" ? undefined : statusFilter);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function startEditing(profile: AiProfile | null): void {
    testResults = {};
    notice = "";
    if (profile === null) {
      editing = {
        displayName: "",
        baseUrl: "http://127.0.0.1:8080/v1",
        model: "",
        visionCapable: true,
        enabled: true,
        timeoutMs: 60_000,
        concurrency: 2,
        extra: {
          send_exif: false,
          send_gps: false,
          max_image_edge: 1024,
          send_response_format: false,
        },
      };
      editingHasKey = false;
      keyInput = { value: "", edited: true }; // 新規は「編集済き」で空=鍵なし
    } else {
      editing = {
        id: profile.id,
        displayName: profile.displayName,
        baseUrl: profile.baseUrl,
        model: profile.model,
        visionCapable: profile.visionCapable,
        enabled: profile.enabled,
        timeoutMs: profile.timeoutMs,
        concurrency: profile.concurrency,
        extra: { ...profile.extra },
      };
      editingHasKey = profile.hasApiKey;
      keyInput = { value: "", edited: false };
    }
  }

  async function saveEditing(): Promise<void> {
    if (editing === null) {
      return;
    }
    busy = true;
    error = null;
    try {
      const saved = await store.saveProfile(editing, apiKeyForSave(keyInput, editingHasKey));
      notice = `「${saved.displayName}」を保存しました (${apiKeyStoreLabel(saved.apiKeyStore)})`;
      editing = null;
      await refreshProfiles();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function runTest(kind: AiTestKind): Promise<void> {
    if (editing?.id === undefined) {
      error = "テストは保存済みプロファイルに対して実行します (先に保存してください)";
      return;
    }
    testing = kind;
    error = null;
    try {
      const report = await store.runTest(editing.id, kind);
      testResults = { ...testResults, [kind]: report };
    } catch (e) {
      testResults = {
        ...testResults,
        [kind]: {
          ok: false,
          detail: e instanceof Error ? e.message : String(e),
          elapsedMs: 0,
          status: null,
        },
      };
    } finally {
      testing = null;
    }
  }

  async function deleteProfile(profile: AiProfile): Promise<void> {
    busy = true;
    try {
      await store.deleteProfile(profile.id);
      notice = `「${profile.displayName}」を削除しました`;
      deletingFor = null;
      await refreshProfiles();
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function pauseQueue(): Promise<void> {
    try {
      await store.pause();
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function resumeQueue(): Promise<void> {
    try {
      await store.resume();
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function retryFailed(): Promise<void> {
    try {
      const n = await store.retryFailed();
      notice = n > 0 ? `失敗ジョブ${n.toLocaleString()}件を再試行待ちへ戻しました` : "再試行する失敗ジョブはありません";
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function cancelJob(jobId: string): Promise<void> {
    try {
      await store.cancelJob(jobId);
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  /** 初回全件処理: 件数を提示して確認ダイアログへ。 */
  async function askEnqueueAll(root: LibraryRoot): Promise<void> {
    try {
      const count = await store.pendingCount(root.id);
      if (count === 0) {
        notice = `「${root.path}」に未処理の画像はありません`;
        return;
      }
      confirmAllFor = { root, count };
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function confirmEnqueueAll(): Promise<void> {
    const pending = confirmAllFor;
    confirmAllFor = null;
    if (pending === null) {
      return;
    }
    try {
      const n = await store.enqueueAllForRoot(pending.root.id, true);
      notice = `「${pending.root.path}」の${n.toLocaleString()}件をキューへ投入しました`;
      tab = "jobs";
      await refreshJobs();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function draftOf(root: LibraryRoot): { ai: boolean; exif: boolean; gps: boolean } {
    return (
      autoDrafts[root.id] ?? {
        ai: root.ai_enabled,
        exif: root.send_exif,
        gps: root.send_gps,
      }
    );
  }

  function setDraft(root: LibraryRoot, patch: Partial<{ ai: boolean; exif: boolean; gps: boolean }>): void {
    autoDrafts = {
      ...autoDrafts,
      [root.id]: { ...draftOf(root), ...patch },
    };
  }

  async function saveRootSettings(root: LibraryRoot): Promise<void> {
    const draft = draftOf(root);
    try {
      await store.setRootSettings(root.id, draft.ai, draft.exif, draft.gps);
      notice = `「${root.path}」の自動処理設定を保存しました`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onJobsEvent(event: AiJobsEvent): void {
    if (jobs === null) {
      return;
    }
    // イベントで到着した変更を一覧へマージ (簡易: 最新化は変更検知で再取得)
    const hasActive = event.changes.some((c) => c.status === "running");
    jobs = { ...jobs, paused: event.paused, counts: event.counts };
    void refreshJobs();
    if (event.changes.some((c) => c.status === "succeeded" || c.status === "failed")) {
      onJobsChanged();
    }
    if (hasActive) {
      onJobsChanged();
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (
      event.key === "Escape" &&
      confirmAllFor === null &&
      deletingFor === null &&
      !confirmEnable &&
      confirmDeleteModel === null
    ) {
      event.preventDefault();
      onClose();
    }
  }

  $effect(() => {
    void refreshProfiles();
    void refreshJobs();
    void store.subscribe(onJobsEvent).then((stop) => {
      stopJobsEvent = stop;
    });
    return () => {
      stopJobsEvent?.();
    };
  });

  $effect(() => {
    // 自然言語検索タブ: ストア生成 + 表示中の2秒ポーリング + 通知購読
    if (tab !== "search" || ipc === undefined) {
      pollStop?.();
      pollStop = null;
      return;
    }
    embeddingStore ??= new EmbeddingStore({ invoke: ipc.invoke, listen: () => Promise.resolve(() => {}) });
    vectorIndexStore ??= new VectorIndexStore({
      invoke: ipc.invoke,
      listen: () => Promise.resolve(() => {}),
    });
    void refreshEmbedding();
    void refreshVectorIndex();
    const timer = setInterval(() => {
      void refreshEmbedding();
      void refreshVectorIndex();
    }, 2000);
    void embeddingStore.subscribe((payload) => {
      embeddingNotice = payload;
      void refreshEmbedding();
    });
    pollStop = () => clearInterval(timer);
    return () => {
      clearInterval(timer);
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" role="presentation">
  <div class="panel" role="dialog" aria-modal="true" aria-label="AI設定">
    <header class="panel-header">
      <h2>AI</h2>
      <nav class="tabs" role="tablist">
        <button class:active={tab === "profiles"} onclick={() => (tab = "profiles")} role="tab">
          プロファイル
        </button>
        <button class:active={tab === "jobs"} onclick={() => (tab = "jobs")} role="tab">
          ジョブキュー
        </button>
        <button class:active={tab === "auto"} onclick={() => (tab = "auto")} role="tab">
          自動処理
        </button>
        <button class:active={tab === "search"} onclick={() => (tab = "search")} role="tab">
          自然言語検索
        </button>
      </nav>
      <button onclick={onClose} aria-label="閉じる">×</button>
    </header>

    {#if notice}
      <p class="notice" data-testid="ai-notice">{notice}</p>
    {/if}
    {#if error}
      <p class="error" data-testid="ai-error">{error}</p>
    {/if}

    <!-- ===== プロファイル ===== -->
    {#if tab === "profiles"}
      {#if editing === null}
        <div class="section">
          <div class="section-head">
            <h3>接続プロファイル</h3>
            <button onclick={() => startEditing(null)}>+ 新規プロファイル</button>
          </div>
          {#if profiles.length === 0}
            <p class="hint">
              プロファイルがありません。OpenAI互換endpoint (llama.cpp等) への接続を登録すると、
              タグ・キャプションの自動生成と手動再生成が利用できます。
            </p>
          {:else}
            <ul class="profile-list">
              {#each profiles as profile (profile.id)}
                <li class="profile-row" data-testid="ai-profile">
                  <div class="profile-main">
                    <strong>{profile.displayName}</strong>
                    <span class="profile-url" class:local={endpointIsLocal(profile.baseUrl)}>
                      {profile.baseUrl} · {profile.model}
                      {endpointIsLocal(profile.baseUrl) ? " (ローカル)" : " (外部)"}
                    </span>
                    <span class="profile-key">
                      {apiKeyStoreLabel(profile.apiKeyStore)}
                      {#if !profile.enabled}· 無効{/if}
                      {#if !profile.visionCapable}· Vision非対応{/if}
                    </span>
                  </div>
                  <div class="profile-actions">
                    <button onclick={() => startEditing(profile)}>編集</button>
                    <button class="danger" onclick={() => (deletingFor = profile)}>削除</button>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {:else}
        <div class="section edit-form" data-testid="ai-profile-form">
          <h3>{editing.id === undefined ? "新規プロファイル" : "プロファイル編集"}</h3>
          <label class="field">
            表示名
            <input type="text" bind:value={editing.displayName} placeholder="例: ローカルllama.cpp" />
          </label>
          <label class="field">
            Base URL
            <input type="text" bind:value={editing.baseUrl} placeholder="http://127.0.0.1:8080/v1" />
          </label>
          <label class="field">
            モデル名
            <input type="text" bind:value={editing.model} placeholder="例: llava-v1.5-7b" />
          </label>
          <label class="field">
            APIキー
            <input
              type="password"
              bind:value={keyInput.value}
              oninput={() => (keyInput.edited = true)}
              placeholder={apiKeyPlaceholder(editingHasKey)}
              autocomplete="off"
            />
          </label>
          <p class="hint">
            {#if editingHasKey}
              {apiKeyStoreLabel(profiles.find((p) => p.id === editing?.id)?.apiKeyStore ?? null)}
              — 平文でDBに保存されることはありません。
            {:else}
              APIキーはOS資格情報ストアへ保存されます (DB・ログには書きません)。
              ローカルendpointでは空欄で構いません。
            {/if}
          </p>
          <div class="field-row">
            <label class="field">
              タイムアウト (ms)
              <input type="number" min="1000" max="600000" step="1000" bind:value={editing.timeoutMs} />
            </label>
            <label class="field">
              同時実行数
              <input type="number" min="1" max="8" bind:value={editing.concurrency} />
            </label>
          </div>
          <div class="field-row">
            <label class="check">
              <input type="checkbox" bind:checked={editing.visionCapable} />
              Vision対応 (画像を送信)
            </label>
            <label class="check">
              <input type="checkbox" bind:checked={editing.enabled} />
              有効 (自動処理で使用)
            </label>
          </div>
          <fieldset class="privacy">
            <legend>送信プライバシー (登録ルート設定とのAND)</legend>
            <label class="check">
              <input type="checkbox" bind:checked={editing.extra.send_exif} />
              EXIF撮影情報をプロンプトへ含める
            </label>
            <label class="check">
              <input type="checkbox" bind:checked={editing.extra.send_gps} />
              GPS座標をプロンプトへ含める (独立したopt-in)
            </label>
            <label class="field">
              送信画像の最大辺 (px。64〜2048 — 送信は最大2048pxまで)
              <input type="number" min="64" max="2048" step="64" bind:value={editing.extra.max_image_edge} />
            </label>
            <label class="check">
              <input type="checkbox" bind:checked={editing.extra.send_response_format} />
              response_format (json_object) を送る (対応確認済みの場合のみ)
            </label>
          </fieldset>
          <div class="test-row" data-testid="ai-tests">
            {#each AI_TESTS as test (test.id)}
              <button
                disabled={editing.id === undefined || testing !== null}
                onclick={() => void runTest(test.id)}
                title="保存済みプロファイルに対して実行"
              >
                {testing === test.id ? "実行中…" : `${test.label}テスト`}
              </button>
            {/each}
          </div>
          {#each AI_TESTS as test (test.id)}
            {#if testResults[test.id]}
              {@const report = testResults[test.id]!}
              <p class="test-result" class:ok={report.ok} class:ng={!report.ok} data-testid={`ai-test-${test.id}`}>
                {test.label}: {report.ok ? "OK" : "失敗"} — {report.detail}
                ({report.elapsedMs.toLocaleString()}ms)
              </p>
            {/if}
          {/each}
          <div class="actions">
            <button onclick={() => (editing = null)}>キャンセル</button>
            <button class="primary" disabled={busy || editing.displayName.trim() === ""} onclick={() => void saveEditing()}>
              保存
            </button>
          </div>
        </div>
      {/if}
    {/if}

    <!-- ===== ジョブキュー ===== -->
    {#if tab === "jobs"}
      <div class="section" data-testid="ai-jobs">
        <div class="section-head">
          <h3>ジョブキュー</h3>
          <div class="queue-actions">
            {#if jobs?.paused}
              <button onclick={() => void resumeQueue()}>再開</button>
            {:else}
              <button onclick={() => void pauseQueue()}>一時停止</button>
            {/if}
            <button onclick={() => void retryFailed()}>失敗を再試行</button>
          </div>
        </div>
        {#if jobs !== null}
          <p class="counts">
            {#each statusSummary(jobs.counts) as item (item.label)}
              {item.label} {item.count.toLocaleString()}·
            {/each}
            {jobs.paused ? "一時停止中" : "実行中"}
          </p>
          <label class="field">
            状態絞り込み
            <select bind:value={statusFilter} onchange={() => void refreshJobs()}>
              <option value="">すべて</option>
              <option value="queued">待機中</option>
              <option value="running">処理中</option>
              <option value="retry_waiting">再試行待ち</option>
              <option value="succeeded">成功</option>
              <option value="failed">失敗</option>
              <option value="cancelled">キャンセル</option>
            </select>
          </label>
          {#if jobs.jobs.length === 0}
            <p class="hint">ジョブはありません。画像を選択して「AI生成」ボタン、または自動処理設定から投入できます。</p>
          {:else}
            <ul class="job-list">
              {#each jobs.jobs as job (job.id)}
                <li class="job-row" data-testid="ai-job">
                  <span class="job-type">{jobTypeLabel(job.jobType)}</span>
                  <span class="job-target">asset {job.targetId}</span>
                  <span class="job-status" data-status={job.status}>
                    {jobStatusLabel(job.status)}
                    {#if job.status === "retry_waiting" || job.status === "failed"}
                      ({job.attempts}/{job.maxAttempts})
                    {/if}
                  </span>
                  {#if job.error}
                    <span class="job-error" title={job.error}>{job.error}</span>
                  {/if}
                  {#if jobCancellable(job.status)}
                    <button onclick={() => void cancelJob(job.id)}>キャンセル</button>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>
    {/if}

    <!-- ===== 自動処理 ===== -->
    {#if tab === "auto"}
      <div class="section" data-testid="ai-auto">
        <h3>登録ルートごとの自動処理</h3>
        <p class="hint">
          新規画像のスキャン確定時に自動でタグ・キャプション生成を投入します。
          EXIF/GPSの送信はルート設定とプロファイル設定の両方が有効な場合のみ行われます
          (初期値はすべてOFF)。
        </p>
        {#if roots.length === 0}
          <p class="hint">登録ルートがありません。</p>
        {:else}
          <ul class="root-list">
            {#each roots as root (root.id)}
              {@const draft = draftOf(root)}
              <li class="root-row" data-testid="ai-root">
                <div class="root-main">
                  <strong title={root.path}>{root.path}</strong>
                </div>
                <div class="root-toggles">
                  <label class="check">
                    <input type="checkbox" checked={draft.ai} onchange={(e) => setDraft(root, { ai: e.currentTarget.checked })} />
                    自動処理
                  </label>
                  <label class="check">
                    <input type="checkbox" checked={draft.exif} onchange={(e) => setDraft(root, { exif: e.currentTarget.checked })} />
                    EXIF送信
                  </label>
                  <label class="check">
                    <input type="checkbox" checked={draft.gps} onchange={(e) => setDraft(root, { gps: e.currentTarget.checked })} />
                    GPS送信
                  </label>
                  <button onclick={() => void saveRootSettings(root)}>保存</button>
                  <button
                    title="このルート内の未処理画像すべてをキューへ投入します (確認あり)"
                    onclick={() => void askEnqueueAll(root)}
                  >
                    全件処理…
                  </button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}

    <!-- ===== 自然言語検索 (WP-11) ===== -->
    {#if tab === "search"}
      <div class="section" data-testid="embedding-panel">
        <div class="section-head">
          <h3>自然言語検索 (ローカル埋め込み)</h3>
          {#if embeddingStatus?.enabled}
            <button disabled={embeddingBusy} onclick={() => void doDisable()}>無効化</button>
          {:else}
            <button
              class="primary"
              disabled={embeddingBusy || embeddingStatus === null || (embeddingStatus !== null && !canEnable(embeddingStatus))}
              onclick={() => (confirmEnable = true)}
            >
              有効化…
            </button>
          {/if}
        </div>
        <p class="hint">
          画像のキャプション・タグ・プロンプトから意味の近さで検索するための
          ローカル埋め込みモデル (ruri-v3) です。外部へは送信されません。
          検索バーの重みスライダー (キーワード⇔意味) と「類似画像を検索」
          から利用できます。
        </p>
        {#if embeddingNotice}
          <p class="notice">{embeddingNotice}</p>
        {/if}
        {#if embeddingError}
          <p class="error" data-testid="embedding-error">{embeddingError}</p>
        {/if}
        {#if embeddingStatus !== null}
          {#if embeddingStatus.warning}
            <p class="error" data-testid="embedding-warning">{embeddingStatus.warning}</p>
          {/if}
          <label class="field">
            モデル
            <select
              value={embeddingStatus.model}
              disabled={embeddingBusy || embeddingStatus.enabled}
              onchange={(e) => void doSelectModel(e.currentTarget.value)}
            >
              {#each MODEL_CHOICES as choice (choice.id)}
                <option value={choice.id}>{choice.label} — {choice.hint}</option>
              {/each}
            </select>
          </label>
          <p class="counts">
            状態: {embeddingStatus.enabled ? "有効" : "無効"} ·
            ワーカー: {workerPhaseLabel(embeddingStatus.worker.phase)}
            {#if embeddingStatus.worker.restartCount > 0}
              (再起動 {embeddingStatus.worker.restartCount}回)
            {/if}
            {#if embeddingStatus.loadedModel !== null}
              · ロード済み: {modelLabel(embeddingStatus.loadedModel)}
            {/if}
          </p>
          <p class="counts" data-testid="embedding-progress">
            {progressText(embeddingStatus)}
            ({Math.round(progressRatio(embeddingStatus) * 100)}%)
          </p>
          {#if vectorIndexStatus !== null}
            <p class="counts" data-testid="vector-index-status">
              索引: {vectorIndexStatus.building
                ? `構築中 (${vectorIndexStatus.indexedCount.toLocaleString()} / ${vectorIndexStatus.sqliteCount.toLocaleString()})`
                : vectorIndexStatus.ready
                  ? `準備完了 (${vectorIndexStatus.indexedCount.toLocaleString()}件)`
                  : vectorIndexStatus.sqliteCount === 0
                    ? "未構築 (埋め込み待ち)"
                    : "準備中"}
              {#if vectorIndexStatus.lastError !== null}
                · エラーあり
              {/if}
            </p>
          {/if}
          {#if embeddingStatus.enabled && progressRatio(embeddingStatus) < 1}
            <progress max="1" value={progressRatio(embeddingStatus)}></progress>
          {/if}
          <ul class="profile-list">
            {#each embeddingStatus.packages as pkg (pkg.modelId)}
              <li class="profile-row" data-testid="embedding-package">
                <div class="profile-main">
                  <strong>{modelLabel(pkg.modelId)}</strong>
                  <span class="profile-url">{pkg.packageDir}</span>
                  <span class="profile-key">
                    {packageStateLabel(pkg.state)} · {pkg.embeddingDim}次元 · {pkg.license}
                  </span>
                  {#if pkg.state !== "verified"}
                    <span class="profile-key">{pkg.detail}</span>
                  {/if}
                </div>
                <div class="profile-actions">
                  {#if pkg.state !== "missing"}
                    <button
                      class="danger"
                      disabled={embeddingBusy || (embeddingStatus.enabled && embeddingStatus.model === pkg.modelId)}
                      title={embeddingStatus.enabled && embeddingStatus.model === pkg.modelId ? "使用中のため無効化してから削除してください" : "パッケージとベクトルを削除"}
                      onclick={() => (confirmDeleteModel = pkg.modelId)}
                    >
                      削除
                    </button>
                  {/if}
                </div>
              </li>
            {/each}
          </ul>
          <p class="hint">モデルの保存先: {embeddingStatus.modelsDir}</p>
        {:else if embeddingError === null}
          <p class="hint">状態を取得中…</p>
        {/if}
      </div>
    {/if}

    <!-- ===== 確認ダイアログ (埋め込み有効化 / モデル削除) ===== -->
    {#if confirmEnable && embeddingStatus !== null}
      <div class="inline-dialog" role="alertdialog" aria-label="自然言語検索の有効化確認">
        <p style="white-space: pre-line">{enableConfirmText(embeddingStatus)}</p>
        <div class="actions">
          <button onclick={() => (confirmEnable = false)}>キャンセル</button>
          <button class="primary" disabled={embeddingBusy} onclick={() => void doEnable()} data-testid="embedding-confirm-enable">
            有効にする
          </button>
        </div>
      </div>
    {/if}
    {#if confirmDeleteModel !== null}
      <div class="inline-dialog" role="alertdialog" aria-label="モデル削除の確認">
        <p>
          モデル ({modelLabel(confirmDeleteModel)}) のパッケージと埋め込みベクトルを削除します。
          通常のキーワード・タグ・条件検索には影響しません。
        </p>
        <div class="actions">
          <button onclick={() => (confirmDeleteModel = null)}>キャンセル</button>
          <button class="danger" disabled={embeddingBusy} onclick={() => void doDeleteModel(confirmDeleteModel!)}>削除</button>
        </div>
      </div>
    {/if}

    <!-- ===== 確認ダイアログ (初回全件処理 / プロファイル削除) ===== -->
    {#if confirmAllFor !== null}
      <div class="inline-dialog" role="alertdialog" aria-label="全件処理の確認">
        <p>
          「{confirmAllFor.root.path}」の未処理画像
          <strong>{confirmAllFor.count.toLocaleString()}件</strong>
          をAI処理のキューへ投入します。よろしいですか?
        </p>
        <div class="actions">
          <button onclick={() => (confirmAllFor = null)}>キャンセル</button>
          <button class="primary" onclick={() => void confirmEnqueueAll()} data-testid="ai-confirm-all">
            投入する
          </button>
        </div>
      </div>
    {/if}
    {#if deletingFor !== null}
      <div class="inline-dialog" role="alertdialog" aria-label="プロファイル削除の確認">
        <p>
          プロファイル「{deletingFor.displayName}」を削除します。
          待機中のジョブはキャンセルされます (実行履歴は残ります)。
        </p>
        <div class="actions">
          <button onclick={() => (deletingFor = null)}>キャンセル</button>
          <button class="danger" onclick={() => void deleteProfile(deletingFor!)}>削除</button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 640;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .panel {
    width: min(44rem, 92vw);
    max-height: 86vh;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.6rem;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: 0.9rem 1rem;
    font-size: 0.85rem;
    overflow-y: auto;
  }

  .panel-header {
    display: flex;
    align-items: center;
    gap: 0.8rem;
  }

  .panel-header h2 {
    font-size: 1rem;
    margin: 0;
  }

  .tabs {
    display: flex;
    gap: 0.25rem;
    flex: 1;
  }

  .tabs button.active {
    border-color: light-dark(#0969da, #58a6ff);
    font-weight: 600;
  }

  .notice {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#1a7f37, #3fb950);
  }

  .error {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.4rem;
    padding: 0.6rem;
  }

  .section h3 {
    font-size: 0.85rem;
    margin: 0;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .hint {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .profile-list,
  .job-list,
  .root-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .profile-row,
  .job-row,
  .root-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.35rem;
    padding: 0.35rem 0.5rem;
  }

  .profile-main,
  .root-main {
    flex: 1;
    min-width: 12rem;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }

  .profile-url {
    font-family: monospace;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
  }

  .profile-url.local {
    color: light-dark(#1a7f37, #3fb950);
  }

  .profile-key {
    font-size: 0.72rem;
    color: light-dark(#8b949e, #6e7681);
  }

  .edit-form .field {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    font-size: 0.78rem;
    color: light-dark(#57606a, #8b949e);
  }

  .field input,
  .field select {
    font: inherit;
    font-size: 0.82rem;
    color: light-dark(#1f2328, #e6edf3);
  }

  .field-row {
    display: flex;
    gap: 0.8rem;
    flex-wrap: wrap;
  }

  .field-row .field {
    flex: 1;
    min-width: 8rem;
  }

  .check {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.78rem;
  }

  .privacy {
    border: 1px dashed light-dark(#d0d7de, #30363d);
    border-radius: 0.35rem;
    padding: 0.4rem 0.6rem;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .privacy legend {
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    padding: 0 0.25rem;
  }

  .test-row {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
  }

  .test-result {
    margin: 0;
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }

  .test-result.ok {
    color: light-dark(#1a7f37, #3fb950);
  }

  .test-result.ng {
    color: light-dark(#cf222e, #f85149);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  progress {
    width: 100%;
    height: 0.5rem;
  }

  .counts {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#57606a, #8b949e);
  }

  .job-row {
    font-size: 0.78rem;
  }

  .job-status {
    font-weight: 600;
  }

  .job-status[data-status="failed"] {
    color: light-dark(#cf222e, #f85149);
  }

  .job-status[data-status="running"] {
    color: light-dark(#0969da, #58a6ff);
  }

  .job-error {
    flex-basis: 100%;
    font-size: 0.72rem;
    color: light-dark(#8b949e, #6e7681);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .job-target,
  .job-type {
    color: light-dark(#57606a, #8b949e);
  }

  .root-toggles {
    display: flex;
    gap: 0.6rem;
    align-items: center;
    flex-wrap: wrap;
  }

  .inline-dialog {
    border: 1px solid light-dark(#9a6700, #d29922);
    border-radius: 0.4rem;
    padding: 0.5rem 0.6rem;
    background: light-dark(#fff8c5, #25170a);
  }

  .inline-dialog p {
    margin: 0 0 0.4rem;
    overflow-wrap: anywhere;
  }

  button.danger {
    border-color: light-dark(#cf222e, #f85149);
    color: light-dark(#cf222e, #f85149);
  }

  button.primary {
    border-color: light-dark(#1a7f37, #238636);
    color: light-dark(#1a7f37, #3fb950);
    font-weight: bold;
  }
</style>
