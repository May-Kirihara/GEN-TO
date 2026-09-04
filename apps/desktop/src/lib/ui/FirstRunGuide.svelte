<script lang="ts">
  /**
    * 初回起動ガイドパネル (WP-18 E3)。
    *
    * 大がかりなウィザードではなく、グリッド空状態の充実として表示する。
    * ステップの導出は deriveFirstRunGuide (純粋計算) — 本体は描画のみ。
    * 状態はテキスト併記 (色のみに依存しない)。
    */
  import type { FirstRunStep } from "./first-run";

  let { steps }: { steps: FirstRunStep[] } = $props();
</script>

<section class="first-run" data-testid="first-run-guide" aria-label="初回起動ガイド">
  <h2>GEN-TOへようこそ</h2>
  <p class="lead">はじめての起動です。次の3ステップで使い始められます (2・3は任意です)。</p>
  <ol class="steps">
    {#each steps as step (step.key)}
      <li class="step" data-testid="first-run-step-{step.key}">
        <p class="step-title">{step.title}</p>
        <p class="step-body">{step.body}</p>
        <p class="step-status" class:done={step.done === true} class:todo={step.done === false}>
          現在の状態: {step.status}
        </p>
      </li>
    {/each}
  </ol>
</section>

<style>
  .first-run {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    text-align: center;
    padding: 2rem;
    pointer-events: none;
  }

  .first-run h2 {
    margin: 0;
    font-size: 1.1rem;
  }

  .lead {
    margin: 0;
    font-size: 0.82rem;
    color: light-dark(#57606a, #8b949e);
  }

  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    max-width: 34rem;
  }

  .step {
    border: 1px solid light-dark(#d0d7de, #444c56);
    border-radius: 0.5rem;
    padding: 0.55rem 0.8rem;
    background: light-dark(#ffffff, #1c2128);
    text-align: left;
  }

  .step-title {
    margin: 0 0 0.2rem;
    font-weight: 600;
    font-size: 0.85rem;
  }

  .step-body {
    margin: 0 0 0.25rem;
    font-size: 0.76rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .step-status {
    margin: 0;
    font-size: 0.74rem;
    font-weight: 600;
  }

  .step-status.done {
    color: light-dark(#1a7f37, #3fb950);
  }

  .step-status.todo {
    color: light-dark(#bc4c00, #d29922);
  }
</style>
