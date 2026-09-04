<script lang="ts">
  /**
    * 左ペインのタブストリップ (UIUX-1 / R-B)。
    *
    * - フォルダ / コレクション / ライブラリ の3タブ (APG tabs模式)
    * - パネル本体 (全マウント維持・アクセシブル隠蔽) はApp側が持ち、
    *   このコンポーネントは選択状態の表示と切替通知のみを担う
    * - キーボード: 矢印左右 (Home/End) でタブ間移動 (自動活性化)
    */
  import type { LeftPaneTab } from "./layout-prefs";

  let {
    active,
    onChange,
  }: {
    active: LeftPaneTab;
    onChange: (tab: LeftPaneTab) => void;
  } = $props();

  const TABS: Array<{ id: LeftPaneTab; label: string }> = [
    { id: "folders", label: "フォルダ" },
    { id: "collections", label: "コレクション" },
    { id: "library", label: "ライブラリ" },
  ];

  function select(tab: LeftPaneTab): void {
    if (tab !== active) {
      onChange(tab);
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    const delta =
      event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : null;
    if (delta === null) {
      if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        select(TABS[event.key === "Home" ? 0 : TABS.length - 1]!.id);
      }
      return;
    }
    event.preventDefault();
    const current = TABS.findIndex((tab) => tab.id === active);
    const next = TABS[(current + delta + TABS.length) % TABS.length]!;
    select(next.id);
    // 自動活性化に伴いフォーカスも追従させる
    requestAnimationFrame(() => {
      document.getElementById(`left-tab-${next.id}`)?.focus();
    });
  }
</script>

<div
  class="left-tabs"
  role="tablist"
  aria-label="左ペインの表示内容"
  tabindex="-1"
  onkeydown={handleKeydown}
>
  {#each TABS as tab (tab.id)}
    <button
      type="button"
      role="tab"
      id="left-tab-{tab.id}"
      class:active={active === tab.id}
      aria-selected={active === tab.id}
      aria-controls="left-panel-{tab.id}"
      tabindex={active === tab.id ? 0 : -1}
      onclick={() => select(tab.id)}
    >
      {tab.label}
    </button>
  {/each}
</div>

<style>
  .left-tabs {
    flex: none;
    display: flex;
    gap: 0.15rem;
    padding: 0.2rem 0.3rem 0;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
    font-size: 0.78rem;
  }

  .left-tabs button {
    /* UIUX-2 (C2): コンテンツ幅を基準に伸縮する — 既定240pxの左ペインで
     * 「コレクション」が省略されない (均等flexだと最長ラベルが切れる。
     * 余白は均等配分、狭い幅では長い方から順に縮む)。 */
    flex: 1 1 auto;
    min-width: 0;
    border: 1px solid transparent;
    border-bottom: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.2rem 0.3rem;
    border-radius: 0.375rem 0.375rem 0 0;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .left-tabs button:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .left-tabs button.active {
    background: light-dark(#ddf4ff, #121d2f);
    border-color: light-dark(#0969da, #1f6feb);
    font-weight: 600;
  }
</style>
