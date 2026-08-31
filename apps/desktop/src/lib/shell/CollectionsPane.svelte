<script lang="ts">
  /**
    * 左ペインのコレクション一覧 (MVP WP-6 / 仕様§8「フォルダツリー、
    * コレクション、保存済み検索」のコレクション領域)。
    *
    * - manual / smart の区別アイコンと件数付き一覧
    * - manual作成 (名前入力) と smart作成 (最小条件ビルダー:
    *   評価≥/選別/お気に入り/タグ/形式のドロップダウン+値、AND結合のみ)
    * - 選択で中央グリッドへ表示 (Appがcollection_showへ接続)
    * - 改名・削除・再評価 (smart)。エクスポート入口はコレクション表示中の
    *   ツールバー側 (WP-14で実装)
    */
  import { onDestroy } from "svelte";
  import {
    buildQueryJson,
    CONDITION_TYPES,
    MIME_CHOICES,
    parseQueryJson,
    type CollectionDto,
    type ConditionDraft,
    type CollectionsStore,
  } from "../collections";

  let {
    store,
    activeCollectionId,
    onSelect,
  }: {
    store: CollectionsStore;
    activeCollectionId: string | null;
    onSelect: (collection: CollectionDto) => void;
  } = $props();

  let storeTick = $state(0);
  const stopNotify = store.onChange(() => {
    storeTick++;
  });

  onDestroy(() => {
    stopNotify();
  });

  function collections(): CollectionDto[] {
    void storeTick;
    return store.collections;
  }

  function isSelected(id: string): boolean {
    void storeTick;
    return activeCollectionId === id;
  }

  // ===== 作成フォーム (manual / smart) =====

  let createMode = $state<"none" | "manual" | "smart">("none");
  let newName = $state("");
  let drafts = $state<ConditionDraft[]>([]);
  let busy = $state(false);
  let notice = $state("");
  let builderError = $state("");

  function startCreate(mode: "manual" | "smart"): void {
    createMode = mode;
    newName = "";
    builderError = "";
    if (mode === "smart" && drafts.length === 0) {
      drafts = [{ type: "rating_ge", value: "4" }];
    }
  }

  function addDraft(): void {
    drafts = [...drafts, { type: "has_tag", value: "" }];
  }

  function removeDraft(index: number): void {
    drafts = drafts.filter((_, i) => i !== index);
  }

  function setDraftType(index: number, type: ConditionDraft["type"]): void {
    const defaults: Record<ConditionDraft["type"], string> = {
      rating_ge: "4",
      cull_state: "picked",
      favorite: "true",
      has_tag: "",
      mime_type: "image/png",
    };
    drafts = drafts.map((draft, i) =>
      i === index ? { type, value: defaults[type] } : draft,
    );
  }

  function setDraftValue(index: number, value: string): void {
    drafts = drafts.map((draft, i) => (i === index ? { ...draft, value } : draft));
  }

  async function submitCreate(): Promise<void> {
    notice = "";
    builderError = "";
    const name = newName.trim();
    if (name === "") {
      builderError = "名前を入力してください";
      return;
    }
    if (createMode === "smart") {
      const queryJson = buildQueryJson(drafts);
      if (queryJson === null) {
        builderError = "条件の値が不正です (評価は0〜5・タグは空欄不可)";
        return;
      }
      await run(() => store.createSmart(name, queryJson));
    } else {
      await run(() => store.createManual(name));
    }
  }

  async function run(action: () => Promise<unknown>): Promise<void> {
    busy = true;
    notice = "";
    try {
      await action();
      createMode = "none";
    } catch (error) {
      notice = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  }

  // ===== 個別操作 =====

  let renaming = $state<{ id: string; name: string } | null>(null);
  let renameInput: HTMLInputElement | null = $state(null);

  // 改名入力へフォーカス (autofocusはa11y警告のため$effectで)
  $effect(() => {
    if (renaming !== null) {
      renameInput?.focus();
    }
  });

  function startRename(collection: CollectionDto): void {
    renaming = { id: collection.id, name: collection.name };
  }

  async function submitRename(): Promise<void> {
    if (renaming === null) {
      return;
    }
    const target = renaming;
    renaming = null;
    await run(() => store.rename(target.id, target.name));
  }

  async function removeCollection(collection: CollectionDto): Promise<void> {
    const kindLabel = collection.kind === "smart" ? "スマートコレクション" : "コレクション";
    if (!window.confirm(`${kindLabel}「${collection.name}」を削除しますか? (元画像は影響しません)`)) {
      return;
    }
    await run(() => store.remove(collection.id));
  }

  async function refreshSmart(collection: CollectionDto): Promise<void> {
    await run(() => store.refreshSmart(collection.id));
  }

  function conditionsSummary(collection: CollectionDto): string {
    const drafts = parseQueryJson(collection.query_json);
    if (drafts === null) {
      return collection.kind === "smart" ? "(条件表示不可)" : "";
    }
    if (drafts.length === 0) {
      return "すべて";
    }
    return drafts
      .map((draft) => {
        switch (draft.type) {
          case "rating_ge":
            return `★≥${draft.value}`;
          case "cull_state":
            return draft.value === "picked" ? "採用" : draft.value === "rejected" ? "除外" : "未選別";
          case "favorite":
            return draft.value === "true" ? "お気に入り" : "非お気に入り";
          case "has_tag":
            return `タグ:${draft.value}`;
          case "mime_type":
            return draft.value.replace("image/", "");
        }
      })
      .join(" かつ ");
  }

  // 条件ビルダーの値入力候補 (選択式。タグは自由入力+候補は省略 — WP-8で接続)
  function valueChoices(draft: ConditionDraft): Array<{ value: string; label: string }> {
    switch (draft.type) {
      case "rating_ge":
        return [0, 1, 2, 3, 4, 5].map((n) => ({ value: String(n), label: `★${n}` }));
      case "cull_state":
        return [
          { value: "picked", label: "採用" },
          { value: "rejected", label: "除外" },
          { value: "unselected", label: "未選別" },
        ];
      case "favorite":
        return [
          { value: "true", label: "ON" },
          { value: "false", label: "OFF" },
        ];
      case "mime_type":
        return MIME_CHOICES.map((mime) => ({ value: mime, label: mime.replace("image/", "") }));
      default:
        return [];
    }
  }
</script>

<div class="coll-pane">
  <div class="coll-header">
    <h3>コレクション</h3>
    <span class="coll-actions">
      <button
        class:active={createMode === "manual"}
        title="手動コレクションを作成 (画像を任意順でまとめる仮想分類)"
        onclick={() => (createMode === "manual" ? (createMode = "none") : startCreate("manual"))}
      >
        + 手動
      </button>
      <button
        class:active={createMode === "smart"}
        title="スマートコレクションを作成 (検索条件で自動的にまとまる)"
        onclick={() => (createMode === "smart" ? (createMode = "none") : startCreate("smart"))}
      >
        + スマート
      </button>
    </span>
  </div>

  {#if createMode !== "none"}
    <div class="coll-create">
      <input
        class="coll-name-input"
        placeholder={createMode === "manual" ? "コレクション名" : "スマートコレクション名"}
        value={newName}
        oninput={(event) => (newName = event.currentTarget.value)}
        onkeydown={(event) => {
          if (event.key === "Enter") {
            void submitCreate();
          }
        }}
      />
      {#if createMode === "smart"}
        <div class="coll-builder" role="group" aria-label="検索条件 (AND結合)">
          {#each drafts as draft, index (index)}
            <div class="coll-condition">
              <select
                aria-label="条件の種類"
                value={draft.type}
                onchange={(event) => setDraftType(index, event.currentTarget.value as ConditionDraft["type"])}
              >
                {#each CONDITION_TYPES as choice (choice.type)}
                  <option value={choice.type}>{choice.label}</option>
                {/each}
              </select>
              {#if draft.type === "has_tag"}
                <input
                  class="coll-tag-input"
                  placeholder="タグ名"
                  value={draft.value}
                  oninput={(event) => setDraftValue(index, event.currentTarget.value)}
                />
              {:else}
                <select
                  aria-label="条件の値"
                  value={draft.value}
                  onchange={(event) => setDraftValue(index, event.currentTarget.value)}
                >
                  {#each valueChoices(draft) as choice (choice.value)}
                    <option value={choice.value}>{choice.label}</option>
                  {/each}
                </select>
              {/if}
              <button
                class="coll-condition-remove"
                title="この条件を削除"
                onclick={() => removeDraft(index)}
              >
                ✕
              </button>
            </div>
          {/each}
          <button class="coll-condition-add" onclick={addDraft}>+ 条件を追加</button>
          <p class="coll-builder-hint">
            条件はすべて AND (かつ) で結合されます。評価の変更は「再評価」で反映されます。
          </p>
        </div>
      {/if}
      {#if builderError}
        <p class="coll-error">{builderError}</p>
      {/if}
      <div class="coll-create-actions">
        <button disabled={busy} onclick={() => void submitCreate()}>作成</button>
        <button onclick={() => (createMode = "none")}>取消</button>
      </div>
    </div>
  {/if}

  {#if notice}
    <p class="coll-error" role="status">{notice}</p>
  {/if}

  <div class="coll-list" role="list" aria-label="コレクション一覧">
    {#each collections() as collection (collection.id)}
      <div
        class="coll-row"
        class:row-selected={isSelected(collection.id)}
        role="listitem"
      >
        {#if renaming?.id === collection.id}
          <input
            bind:this={renameInput}
            class="coll-name-input rename"
            value={renaming.name}
            oninput={(event) =>
              (renaming = renaming === null
                ? null
                : { id: renaming.id, name: event.currentTarget.value })}
            onkeydown={(event) => {
              if (event.key === "Enter") {
                void submitRename();
              } else if (event.key === "Escape") {
                renaming = null;
              }
            }}
            onblur={() => void submitRename()}
          />
        {:else}
          <button
            class="coll-label"
            title={collection.kind === "smart"
              ? `スマート: ${conditionsSummary(collection)}`
              : collection.name}
            onclick={() => onSelect(collection)}
          >
            <span class="coll-kind" aria-hidden="true">
              {collection.kind === "smart" ? "✦" : "▤"}
            </span>
            <span class="coll-name">{collection.name}</span>
            <span class="coll-count">{collection.item_count.toLocaleString()}</span>
          </button>
          <span class="coll-row-actions">
            {#if collection.kind === "smart"}
              <button
                class="coll-icon-button"
                title="再評価 (メタデータ変更後のスマート内容更新)"
                onclick={() => void refreshSmart(collection)}
              >
                ⟳
              </button>
            {/if}
            <button
              class="coll-icon-button"
              title="改名"
              onclick={() => startRename(collection)}
            >
              ✎
            </button>
            <button
              class="coll-icon-button"
              title="削除 (コレクション定義のみ。元画像は影響しません)"
              onclick={() => void removeCollection(collection)}
            >
              🗑
            </button>
          </span>
        {/if}
        {#if collection.kind === "smart"}
          <div class="coll-query" title={collection.query_json ?? ""}>
            {conditionsSummary(collection)}
          </div>
        {/if}
      </div>
    {:else}
      <p class="coll-empty">
        コレクションはまだありません。右の「+ 手動 / + スマート」から作成できます。
      </p>
    {/each}
  </div>
</div>

<style>
  .coll-pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-top: 1px solid light-dark(#d0d7de, #30363d);
    padding: 0.35rem 0.5rem 0.5rem;
    font-size: 0.8rem;
  }

  .coll-header {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .coll-header h3 {
    font-size: 0.8rem;
    margin: 0.2rem 0;
    flex: 1;
  }

  .coll-actions {
    display: inline-flex;
    gap: 0.25rem;
  }

  button {
    font-size: 0.75rem;
  }

  .coll-create {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    padding: 0.35rem 0;
  }

  .coll-name-input {
    font: inherit;
    font-size: 0.8rem;
    width: 100%;
    box-sizing: border-box;
  }

  .coll-name-input.rename {
    flex: 1;
    min-width: 0;
  }

  .coll-builder {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .coll-condition {
    display: flex;
    align-items: center;
    gap: 0.25rem;
  }

  .coll-condition select {
    font-size: 0.75rem;
    min-width: 0;
    flex: 1;
  }

  .coll-tag-input {
    font: inherit;
    font-size: 0.75rem;
    flex: 1;
    min-width: 0;
  }

  .coll-condition-remove {
    flex: none;
    padding: 0 0.3rem;
  }

  .coll-condition-add {
    align-self: flex-start;
  }

  .coll-builder-hint {
    margin: 0;
    font-size: 0.68rem;
    color: light-dark(#57606a, #8b949e);
  }

  .coll-create-actions {
    display: flex;
    gap: 0.3rem;
  }

  .coll-error {
    margin: 0.2rem 0;
    font-size: 0.72rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .coll-list {
    overflow-y: auto;
    min-height: 0;
    max-height: 16rem;
  }

  .coll-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
  }

  .coll-row.row-selected .coll-label {
    background: light-dark(#ddf4ff, #121d2f);
    font-weight: bold;
  }

  .coll-label {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    flex: 1;
    min-width: 0;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    padding: 0.15rem 0.25rem;
    border-radius: 0.25rem;
    cursor: pointer;
    overflow: hidden;
  }

  .coll-label:hover {
    background: light-dark(#eaeef2, #21262d);
  }

  .coll-kind {
    flex: none;
    width: 1rem;
    text-align: center;
    color: light-dark(#0969da, #58a6ff);
  }

  .coll-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
  }

  .coll-count {
    flex: none;
    font-size: 0.68rem;
    color: light-dark(#57606a, #8b949e);
  }

  .coll-row-actions {
    display: none;
    flex: none;
    gap: 0.1rem;
  }

  .coll-row:hover .coll-row-actions,
  .coll-row.row-selected .coll-row-actions {
    display: inline-flex;
  }

  .coll-icon-button {
    border: none;
    background: transparent;
    color: light-dark(#57606a, #8b949e);
    cursor: pointer;
    padding: 0.05rem 0.2rem;
    font-size: 0.75rem;
    border-radius: 0.25rem;
  }

  .coll-icon-button:hover {
    background: light-dark(#eaeef2, #21262d);
    color: inherit;
  }

  .coll-query {
    flex-basis: 100%;
    padding: 0 0.25rem 0.15rem 1.6rem;
    font-size: 0.68rem;
    color: light-dark(#57606a, #8b949e);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .coll-empty {
    margin: 0.3rem 0;
    font-size: 0.72rem;
    color: light-dark(#8b949e, #6e7681);
  }
</style>
