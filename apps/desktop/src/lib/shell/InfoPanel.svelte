<script lang="ts">
  /**
   * 情報パネル (MVP WP-4 / 仕様§8の3ペイン目)。
   *
   * - 選択 (フォーカス) 項目のファイル情報: 名前・パス・寸法・容量・形式・
   *   更新日時・DB状態 (登録/Asset有無)・スキャン状態
   * - 選別セッションの開始・再開・進捗 (順序・位置・カウント。WP-5への足場)
   * - 評価・タグ編集はWP-5のプレースホルダ領域
   */
  import type { ImageListItem } from "../catalog-types";
  import type { SessionCounts } from "../session/selection-session";
  import { formatBytes } from "../thumbnails";
  import { formatTimestamp } from "../browse";

  let {
    item,
    selectionCount,
    libraryMode,
    sessionCounts,
    sessionOrigin,
    resumable,
    sessionNotice,
    onStartFolder,
    onStartSelection,
    onResume,
    onEnd,
    onSessionMove,
  }: {
    item: ImageListItem | null;
    selectionCount: number;
    libraryMode: boolean;
    sessionCounts: SessionCounts | null;
    sessionOrigin: string | null;
    /** 非アクティブ時の再開候補。 */
    resumable: { origin: string; count: number } | null;
    sessionNotice: string;
    onStartFolder: () => void;
    onStartSelection: () => void;
    onResume: () => void;
    onEnd: () => void;
    onSessionMove: (delta: number) => void;
  } = $props();

  const DB_STATE_LABELS: Record<string, string> = {
    asset: "登録済み (Asset)",
    provisional: "登録済み (ハッシュ計算待ち)",
    adhoc: "未登録 (DB外)",
  };

  const SCAN_STATE_LABELS: Record<string, string> = {
    ready: "表示可能",
    unreadable: "読み取り不可",
    unsupported: "未対応形式",
    corrupt: "破損",
  };

  function dbStateLabel(item: ImageListItem): string {
    if (libraryMode && item.dbState === "adhoc") {
      return "未登録 (DB外)";
    }
    return DB_STATE_LABELS[item.dbState] ?? item.dbState;
  }
</script>

<div class="info-pane">
  <section class="info-section">
    <h3>ファイル情報 {selectionCount > 1 ? ` (${selectionCount.toLocaleString()}件選択)` : ""}</h3>
    {#if item === null}
      <p class="info-empty">画像を選択すると詳細を表示します</p>
    {:else}
      <dl class="info-list">
        <dt>ファイル名</dt>
        <dd title={item.displayName}>{item.displayName}</dd>
        <dt>パス</dt>
        <dd class="info-path" title={item.path}>{item.path}</dd>
        <dt>寸法</dt>
        <dd>{item.width !== null && item.height !== null ? `${item.width} × ${item.height}` : "—"}</dd>
        <dt>容量</dt>
        <dd>{formatBytes(item.byteSize)}</dd>
        <dt>形式</dt>
        <dd>{item.mimeType ?? "—"}</dd>
        <dt>更新日時</dt>
        <dd>{formatTimestamp(item.mtimeMs)}</dd>
        <dt>DB状態</dt>
        <dd>{dbStateLabel(item)}</dd>
        <dt>スキャン状態</dt>
        <dd>{SCAN_STATE_LABELS[item.state] ?? item.state}</dd>
        <dt>選別</dt>
        <dd>{item.triage === "keep" ? "採用" : item.triage === "reject" ? "除外" : "未選別"}</dd>
      </dl>
    {/if}
  </section>

  <section class="info-section">
    <h3>選別セッション</h3>
    {#if sessionCounts !== null}
      <div class="session-progress">
        <div class="session-line">
          位置 <strong>{sessionCounts.position + 1}</strong>/{sessionCounts.total.toLocaleString()}
          · 確認済み <strong>{sessionCounts.confirmed.toLocaleString()}</strong>
        </div>
        <div class="session-line">
          採用 {sessionCounts.keep.toLocaleString()} /
          除外 {sessionCounts.reject.toLocaleString()} /
          保留 {sessionCounts.undecided.toLocaleString()}
        </div>
        <p class="session-origin">{sessionOrigin}</p>
        <div class="session-controls">
          <button onclick={() => onSessionMove(-1)}>← 前へ</button>
          <button onclick={() => onSessionMove(1)}>次へ →</button>
          <button class="danger" onclick={onEnd}>終了</button>
        </div>
        <p class="session-note">採用・除外のマーク付けはWP-5で追加されます</p>
      </div>
    {:else}
      <div class="session-controls">
        <button onclick={onStartFolder}>現在の一覧で開始</button>
        <button onclick={onStartSelection} disabled={selectionCount === 0}>
          選択画像で開始
        </button>
        {#if resumable !== null}
          <button onclick={onResume} title={resumable.origin}>
            再開 ({resumable.count.toLocaleString()}件)
          </button>
        {/if}
      </div>
      {#if resumable !== null}
        <p class="session-origin">前回: {resumable.origin}</p>
      {/if}
    {/if}
    {#if sessionNotice}
      <p class="info-error">{sessionNotice}</p>
    {/if}
  </section>

  <section class="info-section info-placeholder">
    <h3>評価・タグ・キャプション</h3>
    <p class="placeholder-note">WP-5で実装予定 (同一Assetで共有されます)</p>
  </section>
</div>

<style>
  .info-pane {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    height: 100%;
    min-height: 0;
    overflow-y: auto;
    padding: 0.5rem 0.75rem;
    font-size: 0.85rem;
  }

  .info-section h3 {
    font-size: 0.85rem;
    margin: 0 0 0.4rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
    padding-bottom: 0.25rem;
  }

  .info-empty {
    color: light-dark(#57606a, #8b949e);
    margin: 0.5rem 0;
  }

  .info-list {
    display: grid;
    grid-template-columns: 5.5rem 1fr;
    gap: 0.2rem 0.5rem;
    margin: 0;
  }

  .info-list dt {
    color: light-dark(#57606a, #8b949e);
  }

  .info-list dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .info-path {
    font-family: monospace;
    font-size: 0.78rem;
  }

  .session-progress {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .session-line {
    font-size: 0.85rem;
  }

  .session-origin {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .session-controls {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .session-note {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#8b949e, #6e7681);
  }

  .info-error {
    margin: 0.3rem 0 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
  }

  .info-placeholder {
    opacity: 0.7;
  }

  .placeholder-note {
    margin: 0.2rem 0 0;
    font-size: 0.75rem;
    color: light-dark(#8b949e, #6e7681);
  }

  button.danger {
    border-color: light-dark(#cf222e, #f85149);
    color: light-dark(#cf222e, #f85149);
  }
</style>
