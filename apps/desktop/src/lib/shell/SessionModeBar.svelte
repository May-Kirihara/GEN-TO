<script lang="ts">
  /**
    * 選別セッションのモードバー (UIUX-5/WP6。WP7aでApp.svelteから抽出)。
    * 表示のon/offはApp呼び出し側の `{#if sessionSnapshot.active}` が正本
    * (DOM描画条件の位置保存)。操作はApp残置の関数へのcallbackのみ。
    */
  import type { SessionCounts } from "../session/selection-session";

  let {
    sessionCounts,
    sessionNotice,
    onSessionMove,
    onSessionTriage,
    onEndSession,
  }: {
    sessionCounts: SessionCounts | null;
    /** 案内/エラーのnotice (InfoPanel表示に加えバー内にも出す — WP8)。 */
    sessionNotice: string;
    /** 前へ/次へ (AppのsessionMove — フォーカス同期込み)。 */
    onSessionMove: (delta: number) => void;
    /** 採用/除外 (AppのsessionTriage — 成功時のみ次へ進む)。 */
    onSessionTriage: (value: "picked" | "rejected") => Promise<void>;
    /** セッション終了 (AppのendSession)。 */
    onEndSession: () => void;
  } = $props();
</script>

<div class="session-mode-bar" role="group" aria-label="選別セッション操作バー">
  <button
    onclick={() => onSessionMove(-1)}
    disabled={sessionCounts !== null && sessionCounts.position <= 0}
  >◀ 前へ</button>
  <button
    onclick={() => void onSessionTriage("picked")}
    title="ビューアの高速選別モードでは P / X キーでも同じ操作ができます"
  >採用</button>
  <button
    onclick={() => void onSessionTriage("rejected")}
    title="ビューアの高速選別モードでは P / X キーでも同じ操作ができます"
  >除外</button>
  <button
    onclick={() => onSessionMove(1)}
    disabled={sessionCounts !== null && sessionCounts.position >= sessionCounts.total - 1}
  >次へ ▶</button>
  <button class="danger" onclick={onEndSession}>終了</button>
  {#if sessionCounts !== null}
    <span class="session-mode-progress">
      {(sessionCounts.position ?? 0) + 1}/{sessionCounts.total.toLocaleString()}
      · 確認済み {sessionCounts.confirmed.toLocaleString()}
      · 採用 {sessionCounts.keep.toLocaleString()}・除外 {sessionCounts.reject.toLocaleString()}
    </span>
  {/if}
  {#if sessionNotice !== ""}
    <span class="session-mode-notice" role="status">{sessionNotice}</span>
  {/if}
</div>

<style>
  /* UIUX-5/WP6: 選別セッションのモードバー (セッション中のみ・1行の帯)。 */
  .session-mode-bar {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-50);
    padding: var(--space-15) var(--space-60);
    font-size: var(--font-size-75);
    background: var(--color-bg-subtle);
  }

  .session-mode-bar .session-mode-progress {
    color: var(--color-text-muted);
  }

  /* 案内とエラーが混合するためwarning寄りの色で出す (WP8)。 */
  .session-mode-bar .session-mode-notice {
    color: var(--color-warning);
  }

  .session-mode-bar button.danger {
    border-color: var(--color-danger);
    color: var(--color-danger);
  }

  /* App.svelteの全button共通則と同一値 (scoped CSSのためcomponent毎に必要)。 */
  button {
    font-size: var(--font-size-80);
  }
</style>
