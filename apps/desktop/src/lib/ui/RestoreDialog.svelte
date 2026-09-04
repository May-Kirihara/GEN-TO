<script lang="ts">
  /**
    * 復元ダイアログ (MVP WP-13)。
    *
    * 3段階の確認フロー:
    * 1. `.gento` バックアップディレクトリ選択 (フォルダツリー)
    * 2. plan (dry-run) 結果表示 — 差分・衝突・行方不明・曖昧をレポート
    * 3. 確認付き実行 (単発confirmToken) → 結果レポート
    *
    * 確認ゲートは backend側で構造強制されている (トークン無し/不一致は拒否)。
    */
  import { invoke } from "@tauri-apps/api/core";
  import type { LibraryRoot } from "../library";
  import {
    actionLabel,
    looksLikeGentoBackup,
    matchLabel,
    planHasWork,
    planSummaryLines,
    planWarnings,
    resultSummaryLines,
    type RestorePlanDto,
    type RestoreResultDto,
  } from "../backup";
  import { focusTrap } from "./focus-trap";

  let {
    roots,
    onClose,
  }: {
    roots: LibraryRoot[];
    onClose: () => void;
  } = $props();

  type Node = {
    path: string;
    name: string;
    expanded: boolean;
    children: Node[] | null;
    loading: boolean;
    gento: boolean;
  };

  // 短命なモーダル: オープン時点のルート一覧から固定で構築する
  // (DestPickerと同じ規約 — 変更は次回オープンで反映。svelte-checkの
  // 初期値捕捉警告は意図的)。
  let nodes = $state<Node[]>(
    roots
      .filter((r) => r.available)
      .map((r) => ({
        path: r.path,
        name: r.path,
        expanded: false,
        children: null,
        loading: false,
        gento: looksLikeGentoBackup(r.path.split("/").pop() ?? ""),
      })),
  );
  let selected = $state<string | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let plan = $state<RestorePlanDto | null>(null);
  let result = $state<RestoreResultDto | null>(null);

  async function loadChildren(node: Node): Promise<void> {
    if (node.children !== null || node.loading) {
      return;
    }
    node.loading = true;
    try {
      const list = await invoke<{ entries: Array<{ name: string; path: string }> }>(
        "fs_list_dirs",
        { path: node.path },
      );
      node.children = list.entries.map((entry) => ({
        path: entry.path,
        name: entry.name,
        expanded: false,
        children: null,
        loading: false,
        gento: looksLikeGentoBackup(entry.name),
      }));
    } catch (err) {
      node.children = [];
      error = err instanceof Error ? err.message : String(err);
    } finally {
      node.loading = false;
    }
  }

  function toggle(node: Node): void {
    node.expanded = !node.expanded;
    if (node.expanded) {
      void loadChildren(node);
    }
  }

  function select(node: Node): void {
    selected = node.gento ? node.path : null;
  }

  async function runPlan(): Promise<void> {
    if (selected === null) {
      return;
    }
    busy = true;
    error = null;
    try {
      plan = await invoke<RestorePlanDto>("backup_restore_plan", {
        backupDir: selected,
        targetRootId: null,
      });
      result = null;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function runExecute(): Promise<void> {
    if (plan === null || selected === null) {
      return;
    }
    busy = true;
    error = null;
    try {
      result = await invoke<RestoreResultDto>("backup_restore_execute", {
        backupDir: selected,
        confirmToken: plan.confirmToken,
        targetRootId: null,
      });
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function restart(): void {
    plan = null;
    result = null;
    selected = null;
    error = null;
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  function handleBackdropPointerDown(event: PointerEvent): void {
    if (event.target === event.currentTarget) {
      onClose();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="picker-backdrop" onpointerdown={handleBackdropPointerDown} role="presentation">
  <div class="picker wide" role="dialog" aria-modal="true" aria-label="バックアップ復元" use:focusTrap>
    <h2>バックアップ復元</h2>
    {#if error}
      <p class="picker-error" role="alert" data-testid="restore-error">{error}</p>
    {/if}

    {#if result !== null && plan !== null}
      <!-- 結果レポート -->
      <p class="picker-sub">
        復元が完了しました (バックアップ: {plan.rootDisplayPath} /
        作成 {plan.createdAt})
      </p>
      <ul class="result-lines" data-testid="restore-result">
        {#each resultSummaryLines(result) as line (line)}
          <li>{line}</li>
        {/each}
      </ul>
      {#if result.skipped.length > 0}
        <details>
          <summary>スキップした項目 ({result.skipped.length.toLocaleString()}件)</summary>
          <ul class="skip-list">
            {#each result.skipped as s (s.assetUuid)}
              <li>
                <span class="skip-match">{matchLabel(s.matched)}</span>
                {s.reason}
              </li>
            {/each}
          </ul>
        </details>
      {/if}
      <div class="picker-actions">
        <button onclick={restart}>別のバックアップを復元</button>
        <button class="primary" onclick={onClose}>閉じる</button>
      </div>
    {:else if plan !== null}
      <!-- plan確認 -->
      <p class="picker-sub">
        バックアップ <span class="mono">{plan.rootDisplayPath}</span>
        (作成 {plan.createdAt}) を
        <span class="mono">{plan.targetRootPath}</span> へ復元します。
      </p>
      <ul class="result-lines" data-testid="restore-plan-summary">
        {#each planSummaryLines(plan) as line (line)}
          <li>{line}</li>
        {/each}
      </ul>
      {#if planWarnings(plan).length > 0}
        <div class="warnings" role="alert">
          <p class="warnings-title">確認が必要な項目 (スキップされます)</p>
          <ul class="skip-list" data-testid="restore-plan-warnings">
            {#each planWarnings(plan) as warning (warning)}
              <li>{warning}</li>
            {/each}
          </ul>
        </div>
      {/if}
      <details>
        <summary>項目ごとの内訳 ({plan.entries.length.toLocaleString()}件)</summary>
        <ul class="entry-list">
          {#each plan.entries as entry (entry.assetUuid)}
            <li>
              <span class="entry-action" data-action={entry.action}>
                {actionLabel(entry.action)}
              </span>
              <span class="entry-match">{matchLabel(entry.matched)}</span>
              <span class="entry-name" title={entry.reason}>{entry.displayName}</span>
            </li>
          {/each}
        </ul>
      </details>
      <div class="picker-actions">
        <button onclick={restart}>戻る</button>
        <button
          class="primary"
          class:danger={plan.summary.update + plan.summary.add > 0}
          disabled={busy || !planHasWork(plan)}
          data-testid="restore-execute"
          onclick={() => void runExecute()}
        >
          {busy ? "実行中…" : "復元を実行"}
        </button>
      </div>
    {:else}
      <!-- .gento選択 -->
      <p class="picker-sub">
        復元する `.gento` バックアップディレクトリを選択してください
        (画像本体は対象外 — メタデータを現DBへ復元します)
      </p>
      <div class="tree" role="tree">
        {#each nodes as node (node.path)}
          {@render renderNode(node, 0)}
        {/each}
      </div>
      <p class="picker-path" data-testid="restore-selected">
        選択中: {selected ?? "—"}
      </p>
      <div class="picker-actions">
        <button onclick={onClose}>キャンセル</button>
        <button
          class="primary"
          disabled={busy || selected === null}
          data-testid="restore-plan"
          onclick={() => void runPlan()}
        >
          {busy ? "確認中…" : "復元内容を確認"}
        </button>
      </div>
    {/if}
  </div>
</div>

{#snippet renderNode(node: Node, depth: number)}
  <div
    class="node-row"
    role="treeitem"
    aria-selected={selected === node.path}
    tabindex={-1}
    style="padding-left:{depth * 14 + 6}px"
  >
    <button
      class="node-toggle"
      aria-label={node.expanded ? "折りたたむ" : "展開する"}
      onclick={() => toggle(node)}
    >
      {node.children === null && node.loading ? "…" : node.expanded ? "▾" : "▸"}
    </button>
    <button
      class="node-label"
      class:node-selected={selected === node.path}
      class:node-gento={node.gento}
      onclick={() => select(node)}
      ondblclick={() => {
        toggle(node);
        select(node);
      }}
      title={node.gento ? "バックアップとして選択" : node.path}
    >
      {node.name}{node.gento ? " ◈" : ""}
    </button>
  </div>
  {#if node.expanded && node.children !== null}
    {#each node.children as child (child.path)}
      {@render renderNode(child, depth + 1)}
    {/each}
  {/if}
{/snippet}

<style>
  .picker-backdrop {
    position: fixed;
    inset: 0;
    z-index: 650;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .picker {
    width: min(34rem, 90vw);
    max-height: 82vh;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.6rem;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: 0.9rem 1rem;
    font-size: 0.85rem;
  }

  .picker.wide {
    width: min(40rem, 92vw);
  }

  .picker h2 {
    font-size: 1rem;
    margin: 0;
  }

  .picker-sub {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: monospace;
    font-size: 0.72rem;
  }

  .picker-error {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .tree {
    overflow-y: auto;
    min-height: 6rem;
    max-height: 18rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.375rem;
    padding: 0.25rem;
  }

  .node-row {
    display: flex;
    align-items: center;
    gap: 0.15rem;
  }

  .node-toggle {
    width: 1.2rem;
    border: none;
    background: transparent;
    color: inherit;
    cursor: pointer;
    padding: 0;
  }

  .node-label {
    flex: 1;
    min-width: 0;
    text-align: left;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.15rem 0.3rem;
    border-radius: 0.25rem;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-label:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .node-selected {
    background: light-dark(#ddf4ff, #121d2f);
    font-weight: 600;
  }

  .node-gento {
    font-weight: 600;
  }

  .picker-path {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .result-lines {
    margin: 0;
    padding-left: 1.2rem;
    font-size: 0.8rem;
  }

  .warnings {
    border: 1px solid light-dark(#d4a72c, #9e6a03);
    border-radius: 0.375rem;
    padding: 0.4rem 0.6rem;
    background: light-dark(#fff8c5, #282115);
  }

  .warnings-title {
    margin: 0 0 0.2rem;
    font-size: 0.78rem;
    font-weight: 600;
  }

  .skip-list {
    margin: 0;
    padding-left: 1.1rem;
    font-size: 0.72rem;
    max-height: 9rem;
    overflow-y: auto;
    overflow-wrap: anywhere;
  }

  .skip-match {
    font-weight: 600;
    margin-right: 0.3rem;
  }

  .entry-list {
    margin: 0;
    padding-left: 1.1rem;
    font-size: 0.72rem;
    max-height: 12rem;
    overflow-y: auto;
  }

  .entry-list li {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
  }

  .entry-action {
    flex: none;
    min-width: 4rem;
    text-align: center;
    font-size: 0.66rem;
    border-radius: 0.25rem;
    padding: 0 0.2rem;
    background: light-dark(#eaeef2, #21262d);
  }

  .entry-action[data-action="update"],
  .entry-action[data-action="add"] {
    background: light-dark(#ddf4ff, #121d2f);
  }

  .entry-match {
    flex: none;
    font-size: 0.66rem;
    color: light-dark(#57606a, #8b949e);
  }

  .entry-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  details summary {
    font-size: 0.75rem;
    cursor: pointer;
  }

  .picker-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .primary {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
    border: none;
    border-radius: 0.375rem;
    padding: 0.3rem 0.8rem;
    cursor: pointer;
  }

  .primary.danger {
    background: light-dark(#cf222e, #da3633);
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
