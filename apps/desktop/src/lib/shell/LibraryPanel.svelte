<script lang="ts">
  /**
   * 登録ルート管理ペイン (MVP WP-2資産の製品UI組み込み、WP-4)。
   *
   * - パス入力での追加 (親子重複警告はLibraryStoreが保持)
   * - 有効/無効切替・解除・ライブラリ再スキャン・スキャン停止
   * - 不在ルート (アンマウント等) は「不在」表示 (WP-2仕様)
   */
  import type { LibraryRoot, LibraryStore } from "../library";

  let {
    library,
    roots,
    notice,
    error,
    scanBusy,
    onRefresh,
  }: {
    library: LibraryStore;
    roots: LibraryRoot[];
    notice: string;
    error: string | null;
    scanBusy: boolean;
    onRefresh: () => void;
  } = $props();

  let newRootPath = $state("");

  async function addRoot(): Promise<void> {
    if (newRootPath.trim() === "") {
      return;
    }
    const ok = await library.addRoot(newRootPath.trim());
    if (ok) {
      newRootPath = "";
    }
  }
</script>

<div class="roots-pane">
  <h3>登録ルート</h3>
  <div class="roots-add">
    <input
      type="text"
      placeholder="/path/to/images"
      bind:value={newRootPath}
      onkeydown={(event) => {
        if (event.key === "Enter") {
          void addRoot();
        }
      }}
    />
    <button onclick={() => void addRoot()}>追加</button>
  </div>
  {#if notice}
    <p class="roots-notice">{notice}</p>
  {/if}
  {#if error}
    <p class="roots-error">{error}</p>
  {/if}
  {#if roots.length === 0}
    <p class="roots-notice">登録されたルートはありません</p>
  {:else}
    <ul class="roots-list">
      {#each roots as root (root.id)}
        <li class:root-unavailable={!root.available}>
          <span class="roots-path" title={root.path}>{root.path}</span>
          <span class="roots-state">
            {root.enabled ? (root.available ? "有効" : "不在") : "無効"}
          </span>
          <span class="roots-actions">
            <button
              onclick={() => void library.setEnabled(root.id, !root.enabled)}
              title={root.enabled ? "無効化" : "有効化"}
            >
              {root.enabled ? "無効化" : "有効化"}
            </button>
            <button onclick={() => void library.removeRoot(root.id)}>解除</button>
          </span>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="roots-controls">
    <button onclick={() => void library.startScan()} disabled={scanBusy}>
      ライブラリ再スキャン
    </button>
    <button onclick={() => void library.stopScan()} disabled={!scanBusy}>停止</button>
    <button onclick={onRefresh}>再取得</button>
  </div>
  <p class="roots-status">
    {library.status.phase ?? "待機"} /
    発見 {library.status.found.toLocaleString()} /
    変更 {library.status.updated.toLocaleString()} /
    ハッシュ待ち {library.status.pending_hash.toLocaleString()} /
    エラー {library.status.errors.toLocaleString()}
  </p>
</div>

<style>
  .roots-pane {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.5rem 0.75rem;
    border-top: 1px solid light-dark(#d0d7de, #30363d);
    font-size: 0.8rem;
  }

  .roots-pane h3 {
    font-size: 0.8rem;
    margin: 0;
  }

  .roots-add {
    display: flex;
    gap: 0.3rem;
  }

  .roots-add input {
    flex: 1;
    min-width: 0;
    font-size: 0.78rem;
  }

  .roots-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    max-height: 10rem;
    overflow-y: auto;
  }

  .roots-list li {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    padding: 0.25rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.375rem;
  }

  .roots-list li.root-unavailable {
    border-color: light-dark(#bf8700, #d29922);
  }

  .roots-path {
    font-family: monospace;
    font-size: 0.72rem;
    overflow-wrap: anywhere;
  }

  .roots-state {
    font-size: 0.7rem;
    color: light-dark(#57606a, #8b949e);
  }

  .root-unavailable .roots-state {
    color: light-dark(#9a6700, #d29922);
    font-weight: bold;
  }

  .roots-actions {
    display: flex;
    gap: 0.3rem;
  }

  .roots-actions button {
    font-size: 0.7rem;
    padding: 0.05rem 0.4rem;
  }

  .roots-controls {
    display: flex;
    gap: 0.3rem;
    flex-wrap: wrap;
  }

  .roots-status {
    margin: 0;
    font-size: 0.7rem;
    color: light-dark(#57606a, #8b949e);
  }

  .roots-notice {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
  }

  .roots-error {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#cf222e, #f85149);
  }
</style>
