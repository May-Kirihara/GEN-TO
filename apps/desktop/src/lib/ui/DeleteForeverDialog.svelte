<script lang="ts">
  /**
    * 完全削除の確認ダイアログ (MVP WP-7。spec §5.1「明示的な確認」)。
    *
    * 通常削除 (ゴミ箱) と異なり、確認ダイアログ + 「完全に削除する」
    * チェックボックスの2段階の明示を持つ (ダイアログ+入力確認は過剰と
    * いうPM判断)。実行ボタンはチェックが付くまで無効。
    */
  import { focusTrap } from "./focus-trap";

  let {
    count,
    onConfirm,
    onCancel,
  }: {
    count: number;
    onConfirm: () => void;
    onCancel: () => void;
  } = $props();

  let confirmed = $state(false);

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" role="presentation">
  <div class="dialog" role="dialog" aria-modal="true" aria-label="完全削除の確認" use:focusTrap>
    <h2>完全に削除しますか?</h2>
    <p class="sub">
      選択した {count.toLocaleString()}件を<strong>ゴミ箱を経由せず</strong>削除します。
      この操作は取り消せません。
    </p>
    <p class="note">
      通常の削除 (Deleteキー) はゴミ箱へ移動します。完全削除が必要な場合のみ続行してください。
    </p>
    <label class="confirm">
      <input type="checkbox" bind:checked={confirmed} data-testid="forever-check" />
      完全に削除する (元に戻せません)
    </label>
    <div class="actions">
      <button onclick={onCancel}>キャンセル</button>
      <button
        class="danger"
        disabled={!confirmed}
        onclick={onConfirm}
        data-testid="forever-confirm"
      >
        完全削除
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

  .sub {
    margin: 0;
  }

  .note {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .confirm {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82rem;
    color: light-dark(#cf222e, #f85149);
    font-weight: 600;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .danger {
    background: light-dark(#cf222e, #da3633);
    color: #ffffff;
    border: none;
    border-radius: 0.375rem;
    padding: 0.3rem 0.8rem;
    cursor: pointer;
  }

  .danger:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
