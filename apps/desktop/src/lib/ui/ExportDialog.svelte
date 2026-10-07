<script lang="ts">
  /**
    * SD学習用エクスポートダイアログ (MVP WP-14)。
    *
    * - 入力源は呼び出し側で解決済みのasset id列 (選択/コレクション)。
    * - dry-run (export_preview) → 確認 → 実行 (export_execute) の順。
    *   実行にはdry-runが必須 (canExecute)。
    * - 実行中は `export:progress` で進捗表示し、キャンセル
    *   (export_cancel) できる。結果サマリと失敗/衝突を表示する。
    * - 出力先選択は BackupDialog と同一の テキスト入力 + フォルダツリー。
    */
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";
  import {
    canExecute,
    captionSourceLabel,
    createExportClient,
    existingLabel,
    previewSummaryLines,
    resultSummaryLines,
    structureLabel,
    type ExportPreviewDto,
    type ExportProgress,
    type ExportResultDto,
    type ExportStructure,
  } from "../export";
  import type { LibraryRoot } from "../library";
  import { focusTrap } from "./focus-trap";

  let {
    assetIds,
    sourceLabel,
    roots,
    onClose,
  }: {
    assetIds: string[];
    sourceLabel: string;
    roots: LibraryRoot[];
    onClose: () => void;
  } = $props();

  const client = createExportClient({ invoke });

  type Node = {
    path: string;
    name: string;
    expanded: boolean;
    children: Node[] | null;
    loading: boolean;
  };

  // 短命なモーダル: オープン時点のルート一覧から固定で構築する
  // (BackupDialogと同じ規約 — 変更は次回オープンで反映。svelte-checkの
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
      })),
  );
  let selected = $state<string | null>(null);
  let destInput = $state("");
  let structure = $state<ExportStructure>("preserve");
  let preview = $state<ExportPreviewDto | null>(null);
  let previewBusy = $state(false);
  let executing = $state(false);
  let error = $state<string | null>(null);
  let result = $state<ExportResultDto | null>(null);
  let progress = $state<ExportProgress | null>(null);

  let disposeProgress: (() => void) | null = null;
  void (async () => {
    disposeProgress = await listen<ExportProgress>("export:progress", (event) => {
      if (executing) {
        progress = event.payload;
      }
    });
  })();

  onDestroy(() => {
    disposeProgress?.();
  });

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
    selected = node.path;
    destInput = node.path;
  }

  function structureChanged(next: ExportStructure): void {
    if (structure !== next) {
      structure = next;
      // 条件が変われば従来のdry-runは無効 (再プレビューを要求する)
      preview = null;
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && !executing) {
      event.preventDefault();
      onClose();
    }
  }

  async function runPreview(): Promise<void> {
    const dest = destInput.trim();
    if (dest === "") {
      error = "出力先ディレクトリを入力または選択してください";
      return;
    }
    previewBusy = true;
    error = null;
    try {
      preview = await client.preview(assetIds, dest, structure);
    } catch (err) {
      preview = null;
      error = err instanceof Error ? err.message : String(err);
    } finally {
      previewBusy = false;
    }
  }

  async function runExport(): Promise<void> {
    const dest = destInput.trim();
    if (!canExecute(dest, preview)) {
      return;
    }
    executing = true;
    error = null;
    progress = null;
    try {
      result = await client.execute(assetIds, dest, structure);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      executing = false;
      progress = null;
    }
  }

  async function cancelExport(): Promise<void> {
    try {
      await client.cancel();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  function handleBackdropPointerDown(event: PointerEvent): void {
    if (event.target === event.currentTarget && !executing) {
      onClose();
    }
  }

  const progressText = $derived(
    progress === null
      ? ""
      : `${progress.processed.toLocaleString()}/${progress.total.toLocaleString()}件`,
  );
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="picker-backdrop" onpointerdown={handleBackdropPointerDown} role="presentation">
  <div class="picker" role="dialog" aria-modal="true" aria-label="SD学習用エクスポート" use:focusTrap>
    <h2>SD学習用エクスポート</h2>
    {#if result}
      <p class="picker-sub" data-testid="export-done">
        {result.cancelled ? "エクスポートを中断しました" : "エクスポートが完了しました"}
      </p>
      <ul class="summary" data-testid="export-result-summary">
        {#each resultSummaryLines(result) as line (line)}
          <li>{line}</li>
        {/each}
      </ul>
      <p class="picker-path">manifest: <span class="mono">{result.manifestPath}</span></p>
      {#if result.failures.length > 0}
        <div class="issues">
          <h3>失敗した項目 ({result.failures.length.toLocaleString()}件)</h3>
          <ul data-testid="export-failures">
            {#each result.failures as issue, index (`fail-${index}-${issue.assetUuid}`)}
              <li>
                <span class="mono">{issue.displayName || issue.assetUuid}</span>
                <span class="issue-reason">{issue.reason}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if result.conflicts.length > 0}
        <div class="issues">
          <h3>衝突でスキップした項目 ({result.conflicts.length.toLocaleString()}件)</h3>
          <ul data-testid="export-conflicts">
            {#each result.conflicts as issue, index (`conf-${index}-${issue.assetUuid}`)}
              <li>
                <span class="mono">{issue.displayName || issue.assetUuid}</span>
                <span class="issue-reason">{issue.reason}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if result.issuesTruncated}
        <p class="picker-sub">報告は先頭一部のみ。全量はmanifestを参照してください。</p>
      {/if}
      <div class="picker-actions">
        <button class="primary" onclick={onClose}>閉じる</button>
      </div>
    {:else}
      <p class="picker-sub" data-testid="export-source">
        入力: {sourceLabel} ({assetIds.length.toLocaleString()}件) —
        画像と同名の`.txt` (caption・無ければタグ列) を書き出します。元ファイルは変更しません。
      </p>
      {#if error}
        <p class="picker-error" role="alert" data-testid="export-error">{error}</p>
      {/if}
      <label class="dest-label">
        出力先ディレクトリ
        <input
          type="text"
          data-testid="export-dest"
          bind:value={destInput}
          disabled={executing}
          placeholder="/path/to/dataset"
        />
      </label>
      <div class="tree" role="tree">
        {#each nodes as node (node.path)}
          {@render renderNode(node, 0)}
        {/each}
      </div>
      <p class="picker-path" data-testid="export-selected">入力中: {destInput || "—"}</p>
      <fieldset class="structure" role="radiogroup" aria-label="フォルダ構造">
        <legend>フォルダ構造</legend>
        <label>
          <input
            type="radio"
            name="export-structure"
            value="preserve"
            checked={structure === "preserve"}
            onchange={() => structureChanged("preserve")}
            disabled={executing}
          />
          {structureLabel("preserve")}
        </label>
        <label>
          <input
            type="radio"
            name="export-structure"
            value="flat"
            checked={structure === "flat"}
            onchange={() => structureChanged("flat")}
            disabled={executing}
          />
          {structureLabel("flat")}
        </label>
      </fieldset>
      {#if preview}
        <p class="picker-sub" data-testid="export-preview-summary">
          {previewSummaryLines(preview).join(" / ")}
        </p>
        <div class="preview-table-wrap">
          <table class="preview-table" data-testid="export-preview-table">
            <thead>
              <tr>
                <th>画像</th>
                <th>.txt</th>
                <th>既存</th>
                <th>キャプション</th>
              </tr>
            </thead>
            <tbody>
              {#each preview.entries as entry, index (`${entry.assetId}-${index}`)}
                <tr class:row-error={entry.error !== null}>
                  <td class="mono" title={entry.sourcePath}>
                    {entry.error !== null ? (entry.displayName || entry.assetId) : entry.outputRelPath}
                  </td>
                  <td class="mono">{entry.txtRelPath}</td>
                  <td>
                    {entry.error !== null
                      ? "—"
                      : `${existingLabel(entry.imageExisting)} / ${existingLabel(entry.txtExisting)}`}
                  </td>
                  <td class="caption-cell" title={entry.captionPreview}>
                    {#if entry.error !== null}
                      <span class="issue-reason">{entry.error}</span>
                    {:else}
                      {captionSourceLabel(entry.captionSource)}:
                      {entry.captionPreview === "" ? "(空)" : entry.captionPreview}
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
      <div class="picker-actions">
        <button onclick={onClose} disabled={executing}>閉じる</button>
        <button
          data-testid="export-preview-button"
          disabled={previewBusy || executing || destInput.trim() === ""}
          onclick={() => void runPreview()}
        >
          {previewBusy ? "確認中…" : "プレビュー (dry-run)"}
        </button>
        {#if executing}
          <button data-testid="export-cancel-button" onclick={() => void cancelExport()}>
            キャンセル
          </button>
          <span class="progress" data-testid="export-progress">{progressText}</span>
        {:else}
          <button
            class="primary"
            data-testid="export-execute-button"
            disabled={!canExecute(destInput, preview) || previewBusy}
            onclick={() => void runExport()}
          >
            エクスポート実行
          </button>
        {/if}
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
      onclick={() => select(node)}
      ondblclick={() => {
        toggle(node);
        select(node);
      }}
      title={node.path}
    >
      {node.name}
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
    width: min(44rem, 92vw);
    max-height: 84vh;
    display: flex;
    flex-direction: column;
    gap: var(--space-50);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-xl);
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: var(--space-90) var(--space-100);
    font-size: var(--font-size-85);
    overflow-y: auto;
  }

  .picker h2 {
    font-size: var(--font-size-100);
    margin: 0;
  }

  .picker h3 {
    font-size: var(--font-size-80);
    margin: var(--space-20) 0;
  }

  .picker-sub {
    margin: 0;
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: monospace;
    font-size: var(--font-size-72);
  }

  .picker-error {
    margin: 0;
    font-size: var(--font-size-78);
    color: var(--color-danger);
    overflow-wrap: anywhere;
  }

  .dest-label {
    display: flex;
    flex-direction: column;
    gap: var(--space-20);
    font-size: var(--font-size-78);
  }

  .dest-label input {
    font: inherit;
    font-size: var(--font-size-80);
  }

  .tree {
    overflow-y: auto;
    min-height: 4rem;
    max-height: 10rem;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-sm);
    padding: var(--space-25);
  }

  .node-row {
    display: flex;
    align-items: center;
    gap: var(--space-15);
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
    padding: var(--space-15) var(--space-30);
    border-radius: var(--radius-xs);
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-label:hover {
    background: var(--color-bg-subtle);
  }

  .node-selected {
    background: var(--color-accent-subtle);
    font-weight: 600;
  }

  .picker-path {
    margin: 0;
    font-size: var(--font-size-72);
    color: var(--color-text-muted);
    overflow-wrap: anywhere;
  }

  .structure {
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-sm);
    padding: var(--space-30) var(--space-60);
    display: flex;
    gap: 1rem;
    font-size: var(--font-size-78);
  }

  .structure legend {
    font-size: var(--font-size-72);
    padding: 0 0.25rem;
  }

  .summary {
    margin: 0;
    padding-left: 1.2rem;
    font-size: var(--font-size-78);
  }

  .issues {
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-sm);
    padding: var(--space-30) var(--space-60);
    max-height: 10rem;
    overflow-y: auto;
  }

  .issues ul {
    margin: 0;
    padding-left: var(--space-110);
    font-size: var(--font-size-75);
  }

  .issue-reason {
    color: var(--color-danger);
    overflow-wrap: anywhere;
  }

  .preview-table-wrap {
    overflow: auto;
    max-height: 16rem;
    border: 1px solid var(--color-border-strong);
    border-radius: var(--radius-sm);
  }

  .preview-table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--font-size-72);
  }

  .preview-table th,
  .preview-table td {
    text-align: left;
    padding: 0.15rem 0.4rem;
    border-bottom: 1px solid var(--color-bg-subtle);
    vertical-align: top;
  }

  .preview-table th {
    position: sticky;
    top: 0;
    background: light-dark(#f6f8fa, #161b22);
  }

  .preview-table .row-error td {
    color: var(--color-danger);
  }

  .caption-cell {
    max-width: 18rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .picker-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-40);
  }

  .progress {
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
  }

  .primary {
    background: var(--color-accent-emphasis);
    color: #ffffff;
    border: none;
    border-radius: var(--radius-sm);
    padding: var(--space-30) var(--space-80);
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
