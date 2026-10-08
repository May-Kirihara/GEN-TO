<script lang="ts">
  /**
    * 1行notice strip (UIUX-5/WP4。WP7aでApp.svelteから抽出)。
    * 表示のon/offはApp呼び出し側のnoticeStripActive derivedが正本
    * (DOM描画条件の位置保存 — 活性エラー常駐契約はApp側で維持)。
    */
  import { describeKind, type ConflictSession, type ExecuteArgs } from "../fileops";

  let {
    searchNotice,
    folderNotice,
    collectionNotice,
    viewerError,
    fileOpsError,
    fileOpsBusy,
    fileOpsProgress,
    conflictOf,
    onCancelFileOps,
  }: {
    searchNotice: string;
    folderNotice: string;
    collectionNotice: string;
    /** ビューアオープン失敗の常駐エラー (UIUX-6/WP1。AppのviewerError)。 */
    viewerError: string | null;
    fileOpsError: string | null;
    fileOpsBusy: boolean;
    fileOpsProgress: { index: number; total: number; from: string } | null;
    conflictOf: { session: ConflictSession; args: ExecuteArgs; confirmToken: string | null } | null;
    /** ファイル操作のキャンセル (AppのcancelFileOps(fileOpsIpc))。 */
    onCancelFileOps: () => Promise<void>;
  } = $props();
</script>

<div class="notice-strip">
  {#if searchNotice}
    <p class="toolbar-notice error" role="alert" data-testid="search-notice">{searchNotice}</p>
  {/if}
  {#if folderNotice}
    <p class="toolbar-notice error">{folderNotice}</p>
  {/if}
  {#if collectionNotice}
    <p class="toolbar-notice error" role="status">{collectionNotice}</p>
  {/if}
  {#if viewerError !== null}
    <p class="toolbar-notice error" role="alert" data-testid="viewer-error">{viewerError}</p>
  {/if}
  {#if fileOpsError !== null}
    <p class="toolbar-notice error" role="alert" data-testid="fileops-error">{fileOpsError}</p>
  {/if}
  {#if fileOpsBusy && fileOpsProgress !== null}
    <div class="fileops-progress" role="status" data-testid="fileops-progress">
      <span>
        {describeKind(conflictOf?.args.kind ?? "copy")}中
        {fileOpsProgress.index + 1}/{fileOpsProgress.total}
      </span>
      <progress max={fileOpsProgress.total} value={fileOpsProgress.index + 1}></progress>
      <button onclick={() => void onCancelFileOps()}>キャンセル</button>
    </div>
  {/if}
</div>

<style>
  .toolbar-notice {
    flex: none;
    margin: 0;
    padding: 0 0.6rem;
    font-size: var(--font-size-75);
    color: var(--color-text-muted);
  }

  /* UIUX-5/WP4: notice 6行→1行strip (エラー最優先・flex-wrap可)。 */
  .notice-strip {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-60);
    padding: 0 var(--space-60);
    font-size: var(--font-size-75);
  }

  .notice-strip .toolbar-notice {
    padding: 0;
  }

  .fileops-progress {
    display: flex;
    align-items: center;
    gap: var(--space-40);
    font-size: var(--font-size-80);
  }

  .fileops-progress progress {
    width: 10rem;
  }

  .toolbar-notice.error {
    color: var(--color-danger);
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。 */
  button {
    font-size: var(--font-size-80);
  }
</style>
