<script lang="ts">
  /**
    * 情報パネル (MVP WP-4 / 仕様§8の3ペイン目 + WP-5メタデータ編集)。
    *
    * - 選択 (フォーカス) 項目のファイル情報: 名前・パス・寸法・容量・形式・
    *   更新日時・DB状態 (登録/Asset有無)・スキャン状態
    * - 選別セッションの開始・再開・進捗 (順序・位置・カウント)
    * - WP-5: 星0-5・採用/未選別/除外・お気に入り・タグ編集 (候補・使用数)・
    *   キャプション3種 (description/sd_tags/user_note)。複数選択時は
    *   一括編集 (確認表示付き)。adhoc (未登録) では編集UIを無効化。
    */
  import { untrack } from "svelte";
  import type { ImageListItem } from "../catalog-types";
  import type { SessionCounts } from "../session/selection-session";
  import { formatBytes } from "../thumbnails";
  import { formatTimestamp } from "../browse";
  import type { SearchFieldMatch } from "../search";
  import {
    CAPTION_KINDS,
    canEditMetadata,
    editLockedReason,
    type AssetDetailDto,
    type CullValue,
    type TagDto,
  } from "../metadata";
  import {
    generationRows,
    hasGenerationSection,
    hasPhotoSection,
    hasRawSection,
    parserLabel,
    photoRows,
    preview,
    rawSlots,
    type AssetMetadataDto,
    type MetaRow,
  } from "../asset-metadata";
  import { copyText } from "../ui/clipboard";
  import { semanticPercent } from "../search";
  import type { AiRun } from "../ai";

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
    detail,
    metaBusy,
    metaError,
    actions,
    suggestTags,
    undoLabel,
    bulkTargetIds = [],
    collectionMemoContext = null,
    collectionMemoNote = null,
    onSaveCollectionMemo = undefined,
    searchEvidence = null,
    semanticScore = null,
    semanticLabel = "意味",
    canSimilarSearch = false,
    onSimilarSearch = undefined,
    assetMetadata = null,
    aiRuns = [],
    onEnqueueAi = undefined,
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
    /** フォーカス項目のメタデータ詳細 (asset_detail)。 */
    detail: AssetDetailDto | null;
    metaBusy: boolean;
    metaError: string | null;
    /** メタデータ編集操作 (Appが構築。idsは呼び出し側で選択へ拡張)。 */
    actions: {
      setRating: (ids: string[], rating: number) => Promise<void>;
      setCull: (ids: string[], value: CullValue) => Promise<void>;
      setFavorite: (ids: string[], favorite: boolean) => Promise<void>;
      addTags: (ids: string[], names: string[]) => Promise<void>;
      removeTag: (assetId: string, tagId: string) => Promise<void>;
      setCaption: (assetId: string, kind: "description" | "sd_tags" | "user_note", body: string) => Promise<void>;
      undo: () => void;
    };
    suggestTags: (prefix: string) => Promise<TagDto[]>;
    /** 次に巻き戻せる操作のラベル (無しnull)。 */
    undoLabel: string | null;
    /** 一括適用対象のID列 (Appが選択から供給)。 */
    bulkTargetIds?: string[];
    /** WP-6: 表示中manualコレクション名 (編集可能なときのみ。null=非表示)。 */
    collectionMemoContext?: string | null;
    /** WP-6: フォーカス項目のコレクション内メモ現在値。 */
    collectionMemoNote?: string | null;
    /** WP-6: コレクション内メモの保存。 */
    onSaveCollectionMemo?: (note: string) => void;
    /** WP-8: 検索一致根拠 (検索結果表示中のみ。null=非表示)。 */
    searchEvidence?: SearchFieldMatch[] | null;
    /** WP-12: 意味スコア (cosine類似度。null=非表示)。 */
    semanticScore?: number | null;
    /** WP-12: スコアの表示ラベル (「意味」/「類似度」)。 */
    semanticLabel?: string;
    /** WP-12: 類似画像検索が実行可能か。 */
    canSimilarSearch?: boolean;
    /** WP-12: 類似画像検索の実行 (Appが構築)。 */
    onSimilarSearch?: () => void;
    /** WP-9: 埋め込み画像メタデータ (asset_metadata_get。null=未取得/なし)。 */
    assetMetadata?: AssetMetadataDto | null;
    /** WP-10: フォーカス項目のAI実行履歴 (空=非表示)。 */
    aiRuns?: AiRun[];
    /** WP-10: 選択項目へのAI生成ジョブ投入。 */
    onEnqueueAi?: () => void;
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

  /** WP-8: 検索一致根拠のフィールド表示名。 */
  const EVIDENCE_FIELD_LABELS: Record<string, string> = {
    filename: "ファイル名",
    path: "パス",
    tags: "タグ",
    captions: "キャプション",
    note: "メモ",
  };
  function dbStateLabel(item: ImageListItem): string {
    if (libraryMode && item.dbState === "adhoc") {
      return "未登録 (DB外)";
    }
    return DB_STATE_LABELS[item.dbState] ?? item.dbState;
  }

  // ===== WP-5: 編集可否・一括適用 =====

  /** 編集可能 = library系表示 (Library・コレクション・検索結果) でAsset確定済み
   * (仕様§4.1: 未登録は編集不可。規則の正本は lib/metadata.ts canEditMetadata)。 */
  const canEdit = $derived(canEditMetadata(libraryMode, item));
  /** 編集不可の理由文案 (UXLIB-1。ハッシュ待ち/未登録フォルダを案内)。 */
  const lockReason = $derived(editLockedReason(libraryMode, item));
  const bulk = $derived(selectionCount > 1 && canEdit);

  /** 確認待ちの一括操作 (適用/取消)。 */
  let pendingBulk: { label: string; run: (ids: string[]) => Promise<void> } | null = $state(null);

  let busy = $state(false);
  let notice = $state("");

  async function run(action: () => Promise<void>): Promise<void> {
    busy = true;
    notice = "";
    try {
      await action();
    } catch (error) {
      notice = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  }

  /** 編集対象のID列 (一括時は選択全体)。AppがbulkTargetIdsへ設定する。 */
  function editIds(): string[] {
    if (item === null) {
      return [];
    }
    return bulk && bulkTargetIds.length > 0 ? [...bulkTargetIds] : [item.id];
  }

  function applyRating(rating: number): void {
    if (!canEdit || busy) {
      return;
    }
    const ids = editIds();
    if (bulk) {
      pendingBulk = {
        label: `選択 ${ids.length.toLocaleString()}件へ 評価 ★${rating} を適用`,
        run: (target) => actions.setRating(target, rating),
      };
      return;
    }
    void run(() => actions.setRating(ids, rating));
  }

  function applyCull(value: CullValue): void {
    if (!canEdit || busy) {
      return;
    }
    const ids = editIds();
    if (bulk) {
      const label = value === "picked" ? "採用" : value === "rejected" ? "除外" : "未選別";
      pendingBulk = {
        label: `選択 ${ids.length.toLocaleString()}件へ ${label} を適用`,
        run: (target) => actions.setCull(target, value),
      };
      return;
    }
    void run(() => actions.setCull(ids, value));
  }

  function applyFavorite(favorite: boolean): void {
    if (!canEdit || busy) {
      return;
    }
    const ids = editIds();
    if (bulk) {
      pendingBulk = {
        label: `選択 ${ids.length.toLocaleString()}件へ お気に入り${favorite ? "ON" : "OFF"} を適用`,
        run: (target) => actions.setFavorite(target, favorite),
      };
      return;
    }
    void run(() => actions.setFavorite(ids, favorite));
  }

  function confirmBulk(): void {
    if (pendingBulk === null) {
      return;
    }
    const pending = pendingBulk;
    pendingBulk = null;
    void run(() => pending.run(editIds()));
  }

  // ===== タグ編集 =====

  let tagInput = $state("");
  let suggestions: TagDto[] = $state([]);
  let suggestBusy = $state(false);
  let suggestTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    // タグ入力→候補絞り込み (200msデバウンス。表示は最大8件)
    void tagInput;
    if (suggestTimer !== null) {
      clearTimeout(suggestTimer);
    }
    if (!canEdit) {
      suggestions = [];
      return;
    }
    suggestTimer = setTimeout(() => {
      suggestBusy = true;
      void suggestTags(tagInput)
        .then((hits) => {
          suggestions = hits.slice(0, 8);
        })
        .catch(() => {
          suggestions = [];
        })
        .finally(() => {
          suggestBusy = false;
        });
    }, 200);
    return () => {
      if (suggestTimer !== null) {
        clearTimeout(suggestTimer);
      }
    };
  });

  function submitTags(): void {
    if (!canEdit || busy) {
      return;
    }
    // カンマ・空白区切りも受け付ける (SD運用での連続入力)
    const names = tagInput
      .split(/[,、\n]/)
      .map((n) => n.trim())
      .filter((n) => n.length > 0);
    if (names.length === 0) {
      return;
    }
    const ids = editIds();
    tagInput = "";
    if (bulk) {
      pendingBulk = {
        label: `選択 ${ids.length.toLocaleString()}件へ タグ ${names.join(", ")} を付与`,
        run: (target) => actions.addTags(target, names),
      };
      return;
    }
    void run(() => actions.addTags(ids, names));
  }

  function removeTag(tagId: string): void {
    if (!canEdit || item === null || busy) {
      return;
    }
    void run(() => actions.removeTag(item.id, tagId));
  }

  // ===== キャプション編集 (フォーカス項目のみ。一括は非対応) =====

  /** 編集中の下書き (kind→本文)。確定ボタンで保存。 */
  let captionDrafts = $state<Record<string, string>>({});
  /** 前回のdetail読み取り時の保存済み本文 (未編集判定用。非リアクティブ)。 */
  let lastSavedBodies: Record<string, string> = {};

  $effect(() => {
    // detailが変わったら下書きを同期。依存はdetailのみ (captionDraftsの読み
    // 取りはuntrack — 書き込みが自分を再トリガーする無限ループになるため)。
    // 未編集 (前回保存値のまま) の下書きのみ新保存値へ追従し、編集中の
    // 下書きは保持する (同一assetへの無関係な更新でタイプ中の本文を失わない)
    void detail;
    const saved: Record<string, string> = {};
    for (const { kind } of CAPTION_KINDS) {
      saved[kind] = detail?.captions.find((c) => c.kind === kind)?.body ?? "";
    }
    const previous = lastSavedBodies;
    lastSavedBodies = saved;
    captionDrafts = untrack(() => {
      const next: Record<string, string> = {};
      for (const { kind } of CAPTION_KINDS) {
        const draft = captionDrafts[kind] ?? "";
        next[kind] = draft === (previous[kind] ?? "") ? saved[kind] : draft;
      }
      return next;
    });
  });

  function captionOf(kind: string): {
    body: string;
    manuallyEdited: boolean;
    source: string;
  } | null {
    const found = detail?.captions.find((c) => c.kind === kind);
    if (found === undefined) {
      return null;
    }
    return {
      body: found.body,
      manuallyEdited: found.manually_edited,
      source: found.source,
    };
  }

  function saveCaption(kind: "description" | "sd_tags" | "user_note"): void {
    if (!canEdit || item === null || busy) {
      return;
    }
    const body = captionDrafts[kind] ?? "";
    void run(async () => {
      await actions.setCaption(item.id, kind, body);
    });
  }

  function captionDirty(kind: string): boolean {
    const current = captionOf(kind);
    return (captionDrafts[kind] ?? "") !== (current?.body ?? "");
  }

  // ===== WP-6: コレクション内メモ (表示中manualコレクションのフォーカス項目) =====

  let collectionMemoDraft = $state("");
  let lastCollectionMemoSaved = "";

  $effect(() => {
    // フォーカス項目・コレクションが変わったら下書きを同期 (未編集時のみ。
    // captionDraftsと同じ無限ループ回避のuntrackパターン)
    void collectionMemoContext;
    const saved = collectionMemoNote ?? "";
    void untrack(() => {
      if (collectionMemoDraft === lastCollectionMemoSaved) {
        collectionMemoDraft = saved;
      }
      lastCollectionMemoSaved = saved;
    });
  });

  function saveCollectionMemo(): void {
    if (collectionMemoContext === null || item === null || busy) {
      return;
    }
    void run(async () => {
      await onSaveCollectionMemo?.(collectionMemoDraft);
    });
  }

  function collectionMemoDirty(): boolean {
    return collectionMemoDraft !== (collectionMemoNote ?? "");
  }

  // ===== WP-9: 埋め込み画像メタデータ (表示・折りたたみ・コピー) =====

  /** 折りたたみ状態 (既定折りたたみ — 巨大workflowでパネルを止めないため
   * 展開時にのみ本文をレンダリングする)。フォーカス項目切替で全閉じる。 */
  let metaExpanded = $state<Record<string, boolean>>({});

  $effect(() => {
    // itemが変わったら折りたたみをリセット (untrackで無限ループ回避)
    void item?.id;
    void untrack(() => {
      metaExpanded = {};
    });
  });

  function metaOpen(key: string): boolean {
    return metaExpanded[key] === true;
  }

  function toggleMeta(key: string): void {
    metaExpanded[key] = !metaOpen(key);
  }

  /** コピー実行 + 完了通知 (テキストは全文 — プレビュー打ち切りと無関係)。
   * dev bridge検証用に body[data-gento-copy] へ最後のコピー結果を記録する
   * (E2Eはコピー発火と成功をX行で観察できる)。 */
  let copiedKey = $state<string | null>(null);

  async function copyMetaText(key: string, text: string): Promise<void> {
    const ok = await copyText(text);
    copiedKey = ok ? key : null;
    document.body.dataset.gentoCopy = `${key}:${ok ? "1" : "0"}:${text.length}`;
    if (ok) {
      setTimeout(() => {
        if (copiedKey === key) {
          copiedKey = null;
        }
      }, 2000);
    }
  }

  const photoMetaRows = $derived(
    assetMetadata === null ? [] : photoRows(assetMetadata.normalized),
  );
  const generationMetaRows = $derived(
    assetMetadata === null ? [] : generationRows(assetMetadata.normalized),
  );

  /** workflow JSON (コピー用全文。ComfyUIのraw_workflowが正本)。 */
  const workflowText = $derived(
    assetMetadata?.parsers.find((p) => p.rawWorkflow)?.rawWorkflow ?? null,
  );

  function rowList(rows: MetaRow[]): string {
    return rows.map((r) => r.value).join("\n");
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
        <dd>
          {#if detail !== null}
            {detail.triage === "picked" ? "採用" : detail.triage === "rejected" ? "除外" : "未選別"}
          {:else}
            {item.triage === "keep" ? "採用" : item.triage === "reject" ? "除外" : "未選別"}
          {/if}
        </dd>
      </dl>
      {#if searchEvidence !== null && searchEvidence.length > 0}
        <div class="search-evidence" data-testid="search-evidence">
          <h4>検索一致</h4>
          <ul>
            {#each searchEvidence as match (match.field)}
              <li>
                <span class="evidence-field">{EVIDENCE_FIELD_LABELS[match.field] ?? match.field}</span>
                <span class="evidence-snippet" title={match.snippet}>{match.snippet}</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if semanticScore !== null}
        <div class="semantic-score" data-testid="semantic-score">
          <h4>{semanticLabel}</h4>
          <p>{semanticLabel} {semanticPercent(semanticScore)}</p>
        </div>
      {/if}
      {#if item !== null && item.dbState === "asset"}
        <button
          class="similar-button"
          disabled={!canSimilarSearch}
          title={canSimilarSearch
            ? "この画像に意味的に近い画像を検索します"
            : "自然言語検索 (埋め込み+索引) が利用可能になると実行できます"}
          onclick={() => onSimilarSearch?.()}
          data-testid="similar-search-button"
        >
          類似画像を検索
        </button>
      {/if}
    {/if}
  </section>

  {#if item !== null && hasPhotoSection(assetMetadata)}
    <!-- WP-9: 写真メタデータ (EXIF系。読み取り専用) -->
    <section class="info-section" data-meta-section="photo">
      <h3>写真メタデータ</h3>
      <dl class="info-list">
        {#each photoMetaRows as row (row.label)}
          <dt>{row.label}</dt>
          <dd title={row.title ?? row.value}>{row.value}</dd>
        {/each}
      </dl>
      {#if photoMetaRows.some((r) => r.label === "GPS")}
        <p class="meta-note" title="GPSの外部AI送信可否は登録ルートごとの設定 (WP-10で接続)">
          GPSのAI送信設定は登録ルート単位です (現在は表示のみ)
        </p>
      {/if}
    </section>
  {/if}

  {#if item !== null && hasGenerationSection(assetMetadata)}
    <!-- WP-9: 生成メタデータ (SD/Comfy/NAI。プロンプトとworkflowは折りたたみ) -->
    <section class="info-section" data-meta-section="generation">
      <h3>生成メタデータ</h3>
      {#if generationMetaRows.length > 0}
        <dl class="info-list">
          {#each generationMetaRows as row (row.label)}
            <dt>{row.label}</dt>
            <dd title={row.title ?? row.value}>{row.value}</dd>
          {/each}
        </dl>
      {/if}

      {#if assetMetadata?.normalized.positivePrompt}
        {@const text = assetMetadata.normalized.positivePrompt}
        <div class="meta-fold" data-meta-fold="positive-prompt">
          <button class="fold-toggle" onclick={() => toggleMeta("positive")} aria-expanded={metaOpen("positive")}>
            Positive prompt ({text.length.toLocaleString()}文字) {metaOpen("positive") ? "▾" : "▸"}
          </button>
          {#if metaOpen("positive")}
            <pre class="meta-pre">{preview(text).text}{preview(text).truncated ? "…" : ""}</pre>
            <div class="fold-controls">
              <button onclick={() => void copyMetaText("positive", text)}>
                {copiedKey === "positive" ? "コピーしました" : "コピー"}
              </button>
            </div>
          {/if}
        </div>
      {/if}

      {#if assetMetadata?.normalized.negativePrompt}
        {@const text = assetMetadata.normalized.negativePrompt}
        <div class="meta-fold" data-meta-fold="negative-prompt">
          <button class="fold-toggle" onclick={() => toggleMeta("negative")} aria-expanded={metaOpen("negative")}>
            Negative prompt ({text.length.toLocaleString()}文字) {metaOpen("negative") ? "▾" : "▸"}
          </button>
          {#if metaOpen("negative")}
            <pre class="meta-pre">{preview(text).text}{preview(text).truncated ? "…" : ""}</pre>
            <div class="fold-controls">
              <button onclick={() => void copyMetaText("negative", text)}>
                {copiedKey === "negative" ? "コピーしました" : "コピー"}
              </button>
            </div>
          {/if}
        </div>
      {/if}

      {#if workflowText !== null}
        <div class="meta-fold" data-meta-fold="workflow">
          <button class="fold-toggle" onclick={() => toggleMeta("workflow")} aria-expanded={metaOpen("workflow")}>
            ComfyUI workflow JSON ({workflowText.length.toLocaleString()}文字) {metaOpen("workflow") ? "▾" : "▸"}
          </button>
          {#if metaOpen("workflow")}
            {@const p = preview(workflowText)}
            <pre class="meta-pre">{p.text}{p.truncated ? "…" : ""}</pre>
            <p class="meta-note" data-meta="workflow-note">
              {p.truncated ? `プレビューは先頭${p.text.length.toLocaleString()}文字 (全体${p.totalLength.toLocaleString()}文字)` : "全文表示中"}
            </p>
            <div class="fold-controls">
              <button data-meta="workflow-copy" onclick={() => void copyMetaText("workflow", workflowText)}>
                {copiedKey === "workflow" ? "コピーしました" : "JSONをコピー"}
              </button>
            </div>
          {/if}
        </div>
      {/if}
    </section>
  {/if}

  {#if item !== null && hasRawSection(assetMetadata)}
    <!-- WP-9: Rawメタデータ (未知形式も破棄せず表示・コピー) -->
    <section class="info-section" data-meta-section="raw">
      <h3>Rawメタデータ</h3>
      {#each assetMetadata?.parsers ?? [] as parser (parser.parserName)}
        {@const slots = rawSlots(parser)}
        {#if slots.length > 0}
          <div class="meta-fold" data-meta-fold="raw-{parser.parserName}">
            <button class="fold-toggle" onclick={() => toggleMeta(`raw-${parser.parserName}`)} aria-expanded={metaOpen(`raw-${parser.parserName}`)}>
              {parserLabel(parser.parserName)}@{parser.parserVersion} ({slots.length}項目) {metaOpen(`raw-${parser.parserName}`) ? "▾" : "▸"}
            </button>
            {#if metaOpen(`raw-${parser.parserName}`)}
              {#each slots as slot (slot.label)}
                {@const p = preview(slot.value)}
                <p class="meta-raw-label">{slot.label}</p>
                <pre class="meta-pre">{p.text}{p.truncated ? "…" : ""}</pre>
              {/each}
              <div class="fold-controls">
                <button onclick={() => void copyMetaText(`raw-${parser.parserName}`, rowList(slots))}>
                  {copiedKey === `raw-${parser.parserName}` ? "コピーしました" : "コピー"}
                </button>
              </div>
            {/if}
          </div>
        {/if}
      {/each}
    </section>
  {/if}

  <section class="info-section">
    <h3>評価・選別・タグ・キャプション</h3>
    {#if item === null}
      <p class="placeholder-note">画像を選択すると編集できます</p>
    {:else if !canEdit}
      <p class="placeholder-note">{lockReason ?? "この項目は編集できません"}</p>
    {:else}
      {#if bulk}
        <p class="bulk-note">選択 {selectionCount.toLocaleString()}件へ一括適用します (適用前に確認します)</p>
      {/if}

      {#if pendingBulk !== null}
        <div class="bulk-confirm" role="alertdialog" aria-label="一括適用の確認">
          <p>{pendingBulk.label}</p>
          <div class="session-controls">
            <button class="primary" onclick={confirmBulk} disabled={busy}>適用</button>
            <button onclick={() => (pendingBulk = null)}>取消</button>
          </div>
        </div>
      {/if}

      <!-- 評価 -->
      <div class="meta-row" data-meta="rating">
        <span class="meta-label">評価</span>
        <span class="star-row" role="radiogroup" aria-label="星評価">
          {#each [1, 2, 3, 4, 5] as star (star)}
            <button
              class="star"
              class:filled={(detail?.rating ?? item.rating) >= star}
              onclick={() => applyRating((detail?.rating ?? item.rating) === star ? 0 : star)}
              disabled={busy}
              title={star === 0 ? "評価解除" : `★${star}`}
              aria-label={`評価 ${star}`}
            >★</button>
          {/each}
          <span class="meta-value">{(detail?.rating ?? item.rating) || "—"}</span>
        </span>
      </div>

      <!-- 選別状態 -->
      <div class="meta-row" data-meta="cull">
        <span class="meta-label">選別</span>
        <span class="cull-row">
          <button
            class:cull-active={(detail?.triage ?? "unselected") === "picked"}
            onclick={() => applyCull("picked")}
            disabled={busy}
          >採用 (P)</button>
          <button
            class:cull-active={(detail?.triage ?? "unselected") === "rejected"}
            onclick={() => applyCull("rejected")}
            disabled={busy}
          >除外 (X)</button>
          <button
            class:cull-active={(detail?.triage ?? "unselected") === "unselected"}
            onclick={() => applyCull("unselected")}
            disabled={busy}
          >未選別</button>
        </span>
      </div>

      <!-- お気に入り -->
      <div class="meta-row" data-meta="favorite">
        <span class="meta-label">お気に入り</span>
        <button
          class="fav"
          class:fav-on={detail?.favorite ?? item.favorite}
          onclick={() => applyFavorite(!(detail?.favorite ?? item.favorite))}
          disabled={busy}
        >{detail?.favorite ?? item.favorite ? "♥ お気に入り済み" : "♡ 登録"}</button>
      </div>

      <!-- タグ -->
      <div class="meta-row meta-col" data-meta="tags">
        <span class="meta-label">タグ</span>
        <div class="tag-list">
          {#each detail?.tags ?? [] as tag (tag.id)}
            <span class="tag-chip" title="使用数: {tag.usage_count.toLocaleString()} · クリックで解除">
              {tag.name}
              {#if tag.source === "ai"}<span class="tag-ai" title="AI生成 (WP-10)">AI</span>{/if}
              {#if tag.manually_edited}<span class="tag-manual" title="手動編集済み (AI再生成で保護)">✎</span>{/if}
              <button
                class="tag-remove"
                onclick={() => removeTag(tag.id)}
                disabled={busy}
                aria-label="{tag.name}を解除"
              >×</button>
            </span>
          {:else}
            <span class="placeholder-note">タグなし</span>
          {/each}
        </div>
        <div class="tag-input-row">
          <input
            type="text"
            placeholder="タグ名 (カンマ区切り可)"
            value={tagInput}
            oninput={(event) => (tagInput = event.currentTarget.value)}
            onkeydown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                submitTags();
              }
            }}
            disabled={busy}
            list="gento-tag-suggestions"
          />
          <button onclick={submitTags} disabled={busy || tagInput.trim().length === 0}>追加</button>
        </div>
        {#if suggestBusy}
          <p class="placeholder-note">候補を検索中…</p>
        {:else if suggestions.length > 0 && tagInput.trim().length > 0}
          <div class="tag-suggest" data-meta="tag-suggest">
            {#each suggestions as tag (tag.id)}
              <button
                class="tag-suggest-item"
                onclick={() => {
                  tagInput = tag.name;
                  submitTags();
                }}
                disabled={busy}
              >{tag.name} <span class="tag-usage">{tag.usage_count.toLocaleString()}</span></button>
            {/each}
          </div>
        {/if}
      </div>

      <!-- キャプション3種 (フォーカス項目のみ) -->
      {#if !bulk}
        {#each CAPTION_KINDS as { kind, label, hint } (kind)}
          {@const current = captionOf(kind)}
          <div class="meta-row meta-col" data-meta="caption-{kind}">
            <span class="meta-label">
              {label}
              <span class="meta-hint">{hint}</span>
              {#if current?.source === "ai"}<span class="tag-ai" title="AI生成 (WP-10)">AI</span>{/if}
              {#if current?.manuallyEdited}
                <span class="tag-manual" title="手動編集済み (AI再生成で上書きされません)">✎手動編集済み</span>
              {/if}
            </span>
            <textarea
              rows={kind === "sd_tags" ? 2 : 3}
              placeholder="{label}を入力"
              bind:value={captionDrafts[kind]}
              disabled={busy}
            ></textarea>
            <div class="caption-controls">
              <button
                onclick={() => saveCaption(kind)}
                disabled={busy || !captionDirty(kind)}
              >保存</button>
              {#if captionDirty(kind)}
                <button
                  onclick={() => {
                    captionDrafts[kind] = current?.body ?? "";
                  }}
                  disabled={busy}
                >元に戻す</button>
              {/if}
            </div>
          </div>
        {/each}
      {:else}
        <p class="placeholder-note">キャプション編集は単一選択時のみ (フォーカス項目へ適用されます)</p>
      {/if}

      <!-- Undo (メタデータ操作のみ。ファイル操作はWP-7) -->
      <div class="meta-row" data-meta="undo">
        <button onclick={() => actions.undo()} disabled={busy || undoLabel === null} title="Ctrl+Z">
          {undoLabel === null ? "元に戻す (履歴なし)" : `元に戻す: ${undoLabel}`}
        </button>
      </div>

      <!-- WP-10: AI生成投入 + 実行履歴 -->
      {#if onEnqueueAi !== undefined}
        <div class="meta-row meta-col" data-meta="ai">
          <div class="ai-controls">
            <button
              onclick={() => onEnqueueAi?.()}
              disabled={busy}
              title="選択項目のタグ・キャプションをAIで生成 (未処理のみ。実行はバックグラウンド)"
            >
              AI: タグ・キャプション生成
            </button>
            <span class="meta-hint">手動編集済みの情報は上書きされません</span>
          </div>
          {#if aiRuns.length > 0}
            <details class="ai-runs" data-testid="ai-runs">
              <summary>AI実行履歴 ({aiRuns.length.toLocaleString()}件)</summary>
              <ul>
                {#each aiRuns.slice(0, 10) as run (run.id)}
                  <li>
                    <span class="ai-run-status" data-status={run.status}>
                      {run.status === "succeeded" ? "成功" : run.status === "failed" ? "失敗" : "実行中"}
                    </span>
                    <span class="ai-run-meta">pv{run.promptVersion} · {run.startedAt}</span>
                    {#if run.error}<span class="ai-run-error" title={run.error}>{run.error}</span>{/if}
                  </li>
                {/each}
              </ul>
            </details>
          {/if}
        </div>
      {/if}
    {/if}
    {#if metaBusy}
      <p class="placeholder-note">読み込み中…</p>
    {/if}
    {#if notice}
      <p class="info-error">{notice}</p>
    {/if}
    {#if metaError}
      <p class="info-error">{metaError}</p>
    {/if}
  </section>

  {#if collectionMemoContext !== null}
    <section class="info-section">
      <h3>コレクション内メモ</h3>
      <p class="placeholder-note">「{collectionMemoContext}」内でのみ表示されるメモです</p>
      <div class="caption-edit" data-meta="collection-memo">
        <textarea
          rows="3"
          placeholder="このコレクションでのメモ (任意)"
          value={collectionMemoDraft}
          oninput={(event) => (collectionMemoDraft = event.currentTarget.value)}
          disabled={busy}
        ></textarea>
        <div class="caption-controls">
          <button class="primary" onclick={saveCollectionMemo} disabled={busy || !collectionMemoDirty()}>
            保存
          </button>
        </div>
      </div>
    </section>
  {/if}

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
        <p class="session-note">ビューアで 1-5=評価 / X=除外 / P=採用 (高速選別モード)</p>
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
</div>

<datalist id="gento-tag-suggestions">
  {#each suggestions as tag (tag.id)}
    <option value={tag.name}></option>
  {/each}
</datalist>

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

  .search-evidence {
    margin-top: 0.5rem;
  }

  /* WP-12: 意味スコア・類似画像検索 */
  .semantic-score {
    margin-top: 0.5rem;
  }

  .semantic-score p {
    margin: 0.25rem 0 0;
    font-variant-numeric: tabular-nums;
  }

  .similar-button {
    margin-top: 0.6rem;
    width: 100%;
  }

  /* WP-9: 埋め込みメタデータの折りたたみブロック */
  .meta-fold {
    margin-top: 0.4rem;
  }

  .fold-toggle {
    background: none;
    border: none;
    color: light-dark(#0969da, #58a6ff);
    cursor: pointer;
    font-size: 0.8rem;
    padding: 0.1rem 0;
    text-align: left;
  }

  .meta-pre {
    background: light-dark(#f6f8fa, #161b22);
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 4px;
    font-size: 0.72rem;
    margin: 0.25rem 0;
    max-height: 16rem;
    overflow: auto;
    padding: 0.4rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .fold-controls {
    display: flex;
    gap: 0.4rem;
  }

  .meta-note {
    color: light-dark(#57606a, #8b949e);
    font-size: 0.72rem;
    margin: 0.2rem 0;
  }

  .meta-raw-label {
    color: light-dark(#57606a, #8b949e);
    font-size: 0.72rem;
    margin: 0.3rem 0 0;
  }

  .search-evidence h4 {
    margin: 0 0 0.25rem;
    font-size: 0.75rem;
  }

  .search-evidence ul {
    margin: 0;
    padding-left: 1rem;
    font-size: 0.75rem;
  }

  .search-evidence li {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
  }

  .evidence-field {
    flex: none;
    font-weight: 600;
  }

  .evidence-snippet {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 12rem;
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
    overflow-wrap: anywhere;
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

  button.primary {
    border-color: light-dark(#1a7f37, #238636);
    color: light-dark(#1a7f37, #3fb950);
    font-weight: bold;
  }

  .bulk-note {
    margin: 0 0 0.3rem;
    font-size: 0.75rem;
    color: light-dark(#9a6700, #d29922);
  }

  .bulk-confirm {
    border: 1px solid light-dark(#9a6700, #d29922);
    border-radius: 0.4rem;
    padding: 0.4rem;
    margin: 0.3rem 0;
    background: light-dark(#fff8c5, #25170a);
  }

  .bulk-confirm p {
    margin: 0 0 0.3rem;
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }

  .meta-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin: 0.35rem 0;
    flex-wrap: wrap;
  }

  .meta-col {
    flex-direction: column;
    align-items: stretch;
  }

  .meta-label {
    color: light-dark(#57606a, #8b949e);
    font-size: 0.78rem;
    min-width: 5.5rem;
  }

  .meta-col .meta-label {
    min-width: 0;
  }

  .meta-hint {
    font-size: 0.7rem;
    color: light-dark(#8b949e, #6e7681);
    margin-left: 0.3rem;
  }

  .meta-value {
    font-size: 0.78rem;
    color: light-dark(#57606a, #8b949e);
  }

  .star-row {
    display: inline-flex;
    align-items: center;
    gap: 0.1rem;
  }

  .star {
    border: none;
    background: transparent;
    padding: 0 0.05rem;
    font-size: 1.05rem;
    line-height: 1;
    color: light-dark(#d0d7de, #30363d);
    cursor: pointer;
  }

  .star.filled {
    color: light-dark(#d29922, #e3b341);
  }

  .cull-row {
    display: inline-flex;
    gap: 0.3rem;
    flex-wrap: wrap;
  }

  .cull-active {
    border-color: light-dark(#0969da, #58a6ff);
    font-weight: bold;
  }

  .fav {
    font-size: 0.8rem;
  }

  .fav-on {
    color: light-dark(#cf222e, #f85149);
    border-color: light-dark(#cf222e, #f85149);
  }

  .tag-list {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }

  .tag-chip {
    display: inline-flex;
    align-items: center;
    gap: 0.2rem;
    border: 1px solid light-dark(#d0d7de, #30363d);
    border-radius: 999px;
    padding: 0.05rem 0.45rem;
    font-size: 0.78rem;
    background: light-dark(#eaeef2, #21262d);
  }

  .tag-manual {
    color: light-dark(#9a6700, #d29922);
    font-size: 0.7rem;
  }

  /* WP-10: AI由来バッジ */
  .tag-ai {
    color: light-dark(#0969da, #58a6ff);
    border: 1px solid light-dark(#0969da, #58a6ff);
    border-radius: 0.25rem;
    font-size: 0.62rem;
    line-height: 1;
    padding: 0.05rem 0.2rem;
    font-weight: 600;
  }

  .ai-controls {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
  }

  .ai-runs summary {
    cursor: pointer;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
  }

  .ai-runs ul {
    list-style: none;
    margin: 0.25rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    font-size: 0.72rem;
  }

  .ai-runs li {
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
    flex-wrap: wrap;
  }

  .ai-run-status {
    font-weight: 600;
  }

  .ai-run-status[data-status="succeeded"] {
    color: light-dark(#1a7f37, #3fb950);
  }

  .ai-run-status[data-status="failed"] {
    color: light-dark(#cf222e, #f85149);
  }

  .ai-run-meta {
    color: light-dark(#8b949e, #6e7681);
  }

  .ai-run-error {
    flex-basis: 100%;
    color: light-dark(#8b949e, #6e7681);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag-remove {
    border: none;
    background: transparent;
    color: light-dark(#57606a, #8b949e);
    padding: 0;
    font-size: 0.85rem;
    line-height: 1;
    cursor: pointer;
  }

  .tag-remove:hover {
    color: light-dark(#cf222e, #f85149);
  }

  .tag-input-row {
    display: flex;
    gap: 0.3rem;
    margin-top: 0.25rem;
  }

  .tag-input-row input {
    flex: 1;
    min-width: 0;
    font-size: 0.8rem;
  }

  .tag-suggest {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    margin-top: 0.25rem;
  }

  .tag-suggest-item {
    font-size: 0.75rem;
    display: inline-flex;
    gap: 0.3rem;
    align-items: baseline;
  }

  .tag-usage {
    color: light-dark(#8b949e, #6e7681);
    font-size: 0.68rem;
  }

  .meta-col textarea {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 0.8rem;
    resize: vertical;
    min-height: 2.2rem;
  }

  .caption-controls {
    display: flex;
    gap: 0.3rem;
    margin-top: 0.2rem;
  }
</style>
