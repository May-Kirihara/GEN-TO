<script lang="ts">
  /**
    * 同名衝突ダイアログ (MVP WP-7。spec §5.3)。
    *
    * 表示: ファイル名・プレビュー・容量・更新日時・ハッシュ一致の有無。
    * 選択: 上書き / スキップ / 両方保持 (+ 以降すべてへ適用)。
    * 「新旧を比較」は2つのプレビュー並列表示で代替 (dstがカタログ内の
    * 場合はサムネイル、無い場合はメタ情報のみ)。
    */
  import type { ConflictSession, ConflictResolution } from "../fileops";
  import { focusTrap } from "./focus-trap";

  let {
    session,
    totalCount,
    srcThumbUrl,
    dstThumbUrl,
    onChoose,
    onCancel,
  }: {
    session: ConflictSession;
    /** 操作対象の総ファイル数 (「残り」表示の母数)。 */
    totalCount: number;
    /** 移動元のサムネイルURL (カタログ項目から)。 */
    srcThumbUrl: string | null;
    /** 衝突先のサムネイルURL (カタログ内の既知ファイルの場合のみ)。 */
    dstThumbUrl: string | null;
    onChoose: (choice: ConflictResolution, applyAll: boolean) => void;
    onCancel: () => void;
  } = $props();

  let applyAll = $state(false);

  const conflict = $derived(session.current?.conflict ?? null);
  const step = $derived(session.current);

  function formatBytes(size: number): string {
    if (size >= 1024 * 1024) {
      return `${(size / (1024 * 1024)).toFixed(1)} MiB`;
    }
    if (size >= 1024) {
      return `${(size / 1024).toFixed(1)} KiB`;
    }
    return `${size} バイト`;
  }

  function formatDate(ms: number): string {
    if (ms <= 0) {
      return "—";
    }
    return new Date(ms).toLocaleString();
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  }

  function choose(resolution: ConflictResolution): void {
    onChoose(resolution, applyAll);
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if conflict !== null && step !== null}
  <div class="backdrop" role="presentation">
    <div class="dialog" role="dialog" aria-modal="true" aria-label="同名ファイルの衝突" use:focusTrap>
      <h2>同名のファイルがあります</h2>
      <p class="sub">
        {step.display_name}
        {session.remaining > 0
          ? ` — 他 ${session.remaining.toLocaleString()} 件の衝突が続きます (対象 ${totalCount.toLocaleString()} 件中)`
          : ` (対象 ${totalCount.toLocaleString()} 件)`}
      </p>

      <div class="compare" data-testid="conflict-compare">
        <div class="pane">
          <div class="pane-title">移動・コピー元</div>
          {#if srcThumbUrl !== null}
            <img src={srcThumbUrl} alt={step.display_name} />
          {:else}
            <div class="no-preview">プレビューなし</div>
          {/if}
          <dl>
            <dt>容量</dt>
            <dd>{formatBytes(0)}</dd>
            <dt>パス</dt>
            <dd class="path">{step.src}</dd>
          </dl>
        </div>
        <div class="pane">
          <div class="pane-title">既存のファイル</div>
          {#if dstThumbUrl !== null}
            <img src={dstThumbUrl} alt="既存ファイル" />
          {:else}
            <div class="no-preview">プレビューなし (未登録ファイル)</div>
          {/if}
          <dl>
            <dt>容量</dt>
            <dd>{formatBytes(conflict.dst_byte_size)}</dd>
            <dt>更新日時</dt>
            <dd>{formatDate(conflict.dst_mtime_ms)}</dd>
            <dt>パス</dt>
            <dd class="path">{conflict.dst}</dd>
          </dl>
        </div>
      </div>

      <p class="hash" data-testid="conflict-hash">
        {#if conflict.hash_matches === true}
          ★ 内容ハッシュ一致 — 同一画像です
        {:else if conflict.hash_matches === false}
          内容ハッシュ不一致 — 異なる画像です
        {:else}
          内容ハッシュ未比較
        {/if}
      </p>
      <p class="keep-note">「両方保持」では新しい方を <code>{conflict.keep_both_name}</code> として置きます</p>

      <label class="apply-all">
        <input type="checkbox" bind:checked={applyAll} data-testid="conflict-apply-all" />
        以降の衝突すべてに同じ選択を適用
      </label>

      <div class="actions">
        <button onclick={onCancel}>中止</button>
        <button onclick={() => choose("skip")} data-testid="conflict-skip">スキップ</button>
        <button onclick={() => choose("keep_both")} data-testid="conflict-keep-both">両方保持</button>
        <button
          class="danger"
          onclick={() => choose("overwrite")}
          data-testid="conflict-overwrite"
        >
          上書き
        </button>
      </div>
    </div>
  </div>
{/if}

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
    width: min(44rem, 94vw);
    max-height: 86vh;
    overflow-y: auto;
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

  h2 {
    font-size: 1rem;
    margin: 0;
  }

  .sub {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .compare {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.6rem;
  }

  .pane {
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 0.375rem;
    padding: 0.4rem;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .pane-title {
    font-weight: 600;
    font-size: 0.78rem;
  }

  .pane img {
    width: 100%;
    height: 10rem;
    object-fit: contain;
    background: light-dark(#eaeef2, #21262d);
    border-radius: 0.25rem;
  }

  .no-preview {
    height: 10rem;
    display: grid;
    place-items: center;
    background: light-dark(#eaeef2, #21262d);
    border-radius: 0.25rem;
    color: light-dark(#8b949e, #6e7681);
    font-size: 0.75rem;
    text-align: center;
    padding: 0.4rem;
  }

  dl {
    margin: 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.1rem 0.4rem;
    font-size: 0.72rem;
  }

  dt {
    color: light-dark(#57606a, #8b949e);
  }

  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }

  .hash {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
  }

  .keep-note {
    margin: 0;
    font-size: 0.72rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .apply-all {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.8rem;
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
</style>
