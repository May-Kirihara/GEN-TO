<script lang="ts">
  /**
    * 危険なパス (`/` 等の広域ルート) の登録確認ダイアログ (UIUX-2)。
    *
    * `needsRegistrationConfirm` がtrueを返すパスを登録する全入口
    * (FolderTree右クリック・LibraryPanelフォーム) から開く。
    * Cancel = 登録中止 (roots不変)。
    */
  import { focusTrap } from "./focus-trap";

  let {
    path,
    onConfirm,
    onCancel,
  }: {
    path: string;
    onConfirm: () => void;
    onCancel: () => void;
  } = $props();

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" role="presentation">
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-label="広域パスの登録確認"
    use:focusTrap
    data-testid="register-confirm"
  >
    <h2>この場所を登録しますか?</h2>
    <p class="path" data-testid="register-confirm-path">{path}</p>
    <p class="sub">
      この場所は規模が大きい可能性があります (例: ファイルシステム全体)。
      最初の走査に長時間かかり、システム領域へアクセスする場合があります。
      登録しますか?
    </p>
    <div class="actions">
      <button onclick={onCancel} data-testid="register-confirm-cancel">キャンセル</button>
      <button class="primary" onclick={onConfirm} data-testid="register-confirm-ok">
        登録する
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 660;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .dialog {
    width: min(30rem, 90vw);
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    background: light-dark(#ffffff, #1c2128);
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.6rem;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    padding: 0.9rem 1rem;
    font-size: 0.85rem;
  }

  h2 {
    font-size: 1rem;
    margin: 0;
  }

  .path {
    margin: 0;
    font-family: monospace;
    font-size: 0.78rem;
    overflow-wrap: anywhere;
    background: light-dark(#eaeef2, #21262d);
    border-radius: 0.25rem;
    padding: 0.2rem 0.35rem;
  }

  .sub {
    margin: 0;
  }

  .actions {
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
</style>
