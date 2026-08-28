<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { normalizeHealth, type HealthInfo, type HealthUiState } from "./lib/health";

  let healthStatus: HealthUiState | null = $state(null);
  let rawHealth: HealthInfo | null = $state(null);
  let fetchedAt = $state("");
  let scanNotice = $state("");

  function toErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  async function refreshHealth(): Promise<void> {
    try {
      const response = await invoke<HealthInfo>("health");
      rawHealth = response;
      healthStatus = normalizeHealth(response);
    } catch (error) {
      rawHealth = null;
      healthStatus = { kind: "error", message: toErrorMessage(error) };
    }
    fetchedAt = new Date().toLocaleTimeString();
  }

  function startScan(): void {
    scanNotice = "未実装 (Phase 2で実装)";
  }

  onMount(() => {
    void refreshHealth();
  });
</script>

<main>
  <h1>GEN-TO 技術検証プロトタイプ</h1>

  <section>
    <h2>Rustバックエンド接続状態</h2>
    {#if healthStatus === null}
      <p>接続確認中…</p>
    {:else if healthStatus.kind === "ok"}
      <p class="ok">接続OK</p>
      <p>
        データセット:
        {healthStatus.datasetPath ?? "(未設定)"}
        ({healthStatus.datasetPresent ? "あり" : "なし"})
      </p>
    {:else}
      <p class="error">接続エラー: {healthStatus.message}</p>
    {/if}
    <button onclick={() => void refreshHealth()}>再取得</button>
  </section>

  <section>
    <h2>スキャン</h2>
    <button onclick={startScan}>スキャン開始</button>
    {#if scanNotice}
      <p>{scanNotice}</p>
    {/if}
  </section>

  <section>
    <h2>開発用ステータス</h2>
    <p>取得時刻: {fetchedAt || "-"}</p>
    <pre>{rawHealth ? JSON.stringify(rawHealth, null, 2) : "{}"}</pre>
  </section>
</main>

<style>
  main {
    max-width: 60rem;
    margin: 0 auto;
    padding: 1.5rem;
  }

  h1 {
    font-size: 1.5rem;
  }

  section {
    margin-top: 1.5rem;
  }

  pre {
    background: light-dark(#f5f5f5, #1e1e1e);
    padding: 0.75rem;
    border-radius: 0.5rem;
    overflow-x: auto;
  }

  .ok {
    color: light-dark(#1a7f37, #3fb950);
    font-weight: bold;
  }

  .error {
    color: light-dark(#cf222e, #f85149);
    font-weight: bold;
  }
</style>
