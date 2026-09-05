<script lang="ts">
  /**
    * 設定ダイアログ (MVP WP-15 / WP-18拡張)。
    *
    * タブ: 一般 / 表示 / キャッシュ / AI / 検索 / バックアップ / 診断 /
    * このアプリについて。
    * - 設定の正本は backend `app_settings`。破損値は既定値へフォールバック
    *   された形で表示され、corruptKeysがあれば注意を示す。
    * - キャッシュ: 使用量と上限の表示・上限変更 (即時LRU prune)・
    *   「今すぐ削除」は2段階確認 (clear_thumbnail_cache)。
    * - AI: プロファイルのendpoint分類とルート毎の外部送信設定の可視化
    *   (読み取り専用 — 編集はAIパネル)。
    * - 診断: 5領域スナップショット (更新ボタン) と診断bundle生成
    *   (秘密・実パス除外はbackendが担保)。
    * - 表示タブは既存の表示系pref (folder-prefs) の現状値の表示のみ。
    *   ただしスクロール速度 (layout-prefs) はUIUX-3でこのタブから編集可能
    *   (即時反映+永続化。ツールバーから移設 — cacheタブの上限変更と同じ
    *   「local保存の実用値はここで変えられる」例外)。
    * - このアプリについて (WP-18 E2): バージョン・ビルド情報 (about_info)、
    *   ライセンス情報、更新履歴 (bundle resourcesのCHANGELOG.md)。
    */
  import { invoke } from "@tauri-apps/api/core";
  import type { LibraryRoot } from "../library";
  import type { AiProfile } from "../ai";
  import type { BackupSettingsDto } from "../backup";
  import {
    classifyEndpoint,
    endpointClassLabel,
    formatBytes,
    mibToBytes,
    normalizeSettings,
    type DiagnosticsDto,
    type SettingsDto,
  } from "../settings";
  import {
    CHANGELOG_TEXT,
    buildProfileLabel,
    normalizeAboutInfo,
    type AboutInfo,
  } from "../about";
  import { focusTrap } from "./focus-trap";

  let {
    roots,
    profiles,
    displayInfo,
    initialTab = "general",
    onClose,
    onScrollSpeedInput,
    onScrollSpeedChange,
    onScrollSpeedReset,
  }: {
    roots: LibraryRoot[];
    profiles: AiProfile[];
    displayInfo: {
      lastFolder: string | null;
      expandedCount: number;
      sortLabel: string | null;
      scrollSpeed: number;
    };
    /** UIUX-1: 初期タブ (ヘルプ>「このアプリについて」からのAbout直開等)。 */
    initialTab?: Tab;
    onClose: () => void;
    /** UIUX-3: 速度スライダー入力の即時反映 (ドラッグ中。Appが状態を保持)。 */
    onScrollSpeedInput?: (value: number) => void;
    /** UIUX-3: 速度スライダー確定 (change) の永続化。 */
    onScrollSpeedChange?: (value: number) => void;
    /** UIUX-3: 「既定に戻す」(明示的に既定2.0を書き込む)。 */
    onScrollSpeedReset?: () => void;
  } = $props();

  type Tab = "general" | "display" | "cache" | "ai" | "search" | "backup" | "diagnostics" | "about";

  /** 不正なタブidは無視して既定 (一般) へ。初期タブは開いた時点で固定。 */
  function coerceInitialTab(value: string | undefined): Tab {
    switch (value) {
      case "display":
      case "cache":
      case "ai":
      case "search":
      case "backup":
      case "diagnostics":
      case "about":
        return value;
      default:
        return "general";
    }
  }

  // svelte-ignore state_referenced_locally
  let tab = $state<Tab>(coerceInitialTab(initialTab));
  let settings = $state<SettingsDto>(normalizeSettings(null));
  let settingsError = $state<string | null>(null);
  let about = $state<AboutInfo | null>(null);
  let aboutError = $state<string | null>(null);
  let logLevelInput = $state("info");
  let cacheBusy = $state(false);
  let cacheNotice = $state<string | null>(null);
  let logNotice = $state<string | null>(null);
  let confirmClear = $state(false);
  let thumbsMiBInput = $state("");
  let backup = $state<BackupSettingsDto | null>(null);
  let backupAutoInput = $state("off");
  let backupRetentionInput = $state("3");
  let backupNotice = $state<string | null>(null);
  let diagnostics = $state<DiagnosticsDto | null>(null);
  let diagnosticsBusy = $state(false);
  let bundleDest = $state("");
  let bundleResult = $state<string | null>(null);
  let bundleBusy = $state(false);
  let error = $state<string | null>(null);

  const LOG_LEVELS = ["error", "warn", "info", "debug"] as const;

  function applySettings(dto: SettingsDto): void {
    settings = dto;
    logLevelInput = dto.general.logLevel;
    if (thumbsMiBInput === "") {
      thumbsMiBInput = String(Math.round(dto.cache.thumbsMaxBytes / (1024 * 1024)));
    }
  }

  $effect(() => {
    void (async () => {
      try {
        applySettings(normalizeSettings(await invoke("settings_get")));
      } catch (err) {
        // 設定読み取り失敗時も既定値で表示 (受け入れ: 安全な既定値で起動)
        applySettings(normalizeSettings(null));
        settingsError = err instanceof Error ? err.message : String(err);
      }
      try {
        backup = await invoke<BackupSettingsDto>("backup_settings_get");
        backupAutoInput = backup.autoFullDb;
        backupRetentionInput = String(backup.retentionGenerations);
      } catch {
        backup = null;
      }
      try {
        bundleDest = await invoke<string>("backup_default_dir");
      } catch {
        bundleDest = "";
      }
      try {
        about = normalizeAboutInfo(await invoke("about_info"));
        aboutError = about === null ? "about_infoの応答形状が不正です" : null;
      } catch (err) {
        about = null;
        aboutError = err instanceof Error ? err.message : String(err);
      }
    })();
  });

  async function saveLogLevel(): Promise<void> {
    error = null;
    settingsError = null;
    try {
      applySettings(normalizeSettings(await invoke("settings_set_log_level", { level: logLevelInput })));
      logNotice = `ログレベルを ${logLevelInput} へ変更しました`;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function saveThumbsMax(): Promise<void> {
    error = null;
    const mib = Number.parseInt(thumbsMiBInput, 10);
    if (!Number.isFinite(mib) || mib <= 0) {
      error = "サムネイルキャッシュ上限はMiB単位の正の整数で入力してください";
      return;
    }
    try {
      applySettings(normalizeSettings(await invoke("settings_set_thumbs_max_bytes", { bytes: mibToBytes(mib) })));
      cacheNotice = `サムネイルキャッシュ上限を ${formatBytes(mibToBytes(mib))} へ変更しました`;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function clearCache(): Promise<void> {
    cacheBusy = true;
    error = null;
    cacheNotice = null;
    try {
      const outcome = await invoke<{ removedFiles: number; removedBytes: number }>(
        "clear_thumbnail_cache",
      );
      cacheNotice = `キャッシュを削除しました (${outcome.removedFiles.toLocaleString()}件 / ${formatBytes(outcome.removedBytes)})。サムネイルは表示要求で再生成されます`;
      confirmClear = false;
      applySettings(normalizeSettings(await invoke("settings_get")));
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      cacheBusy = false;
    }
  }

  async function saveBackup(): Promise<void> {
    error = null;
    backupNotice = null;
    const retention = Number.parseInt(backupRetentionInput, 10);
    if (!Number.isFinite(retention) || retention < 1 || retention > 100) {
      error = "バックアップ世代数は1〜100で指定してください";
      return;
    }
    try {
      backup = await invoke<BackupSettingsDto>("backup_settings_set", {
        autoFullDb: backupAutoInput,
        retentionGenerations: retention,
      });
      backupNotice = "バックアップ設定を保存しました";
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function refreshDiagnostics(): Promise<void> {
    diagnosticsBusy = true;
    error = null;
    try {
      diagnostics = await invoke<DiagnosticsDto>("diagnostics_snapshot");
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      diagnosticsBusy = false;
    }
  }

  async function createBundle(): Promise<void> {
    if (bundleDest.trim() === "") {
      error = "診断bundleの出力先ディレクトリを入力してください";
      return;
    }
    bundleBusy = true;
    error = null;
    bundleResult = null;
    try {
      const outcome = await invoke<{ path: string }>("diagnostics_bundle_create", {
        destDir: bundleDest.trim(),
      });
      bundleResult = outcome.path;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      bundleBusy = false;
    }
  }

  function switchTab(next: Tab): void {
    tab = next;
    if (next === "diagnostics" && diagnostics === null) {
      void refreshDiagnostics();
    }
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }

  function handleBackdropPointerDown(event: PointerEvent): void {
    if (event.target === event.currentTarget) {
      onClose();
    }
  }

  /** ジョブ種別の表示ラベル。 */
  function jobTypeLabelOf(jobType: string): string {
    if (jobType === "ai_caption_tags") return "AIキャプション・タグ";
    if (jobType === "ai_reparse") return "AI再解析";
    if (jobType === "embedding") return "埋め込み";
    return jobType;
  }

  function fmtDuration(seconds: number): string {
    if (seconds < 60) return `${seconds}秒`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}分`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)}時間`;
    return `${Math.floor(seconds / 86400)}日`;
  }

  const cacheUsagePercent = $derived(
    settings.cache.maxBytes > 0
      ? Math.min(100, (settings.cache.usedBytes / settings.cache.maxBytes) * 100)
      : 0,
  );
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="picker-backdrop" onpointerdown={handleBackdropPointerDown} role="presentation">
  <div class="picker settings-picker" role="dialog" aria-modal="true" aria-label="設定" use:focusTrap>
    <h2>設定</h2>
    <div class="tabs" role="tablist">
      {#each [["general", "一般"], ["display", "表示"], ["cache", "キャッシュ"], ["ai", "AI"], ["search", "検索"], ["backup", "バックアップ"], ["diagnostics", "診断"], ["about", "このアプリについて"]] as [id, label] (id)}
        <button
          class:active={tab === id}
          onclick={() => switchTab(id as Tab)}
          role="tab"
          aria-selected={tab === id}
        >
          {label}
        </button>
      {/each}
    </div>

    {#if settings.corruptKeys.length > 0}
      <p class="picker-error" role="alert" data-testid="settings-corrupt">
        一部の設定値が壊れていたため既定値で動作しています: {settings.corruptKeys.join(", ")}
      </p>
    {/if}
    {#if settingsError}
      <p class="picker-error" role="alert" data-testid="settings-load-error">{settingsError}</p>
    {/if}
    {#if error}
      <p class="picker-error" role="alert" data-testid="settings-error">{error}</p>
    {/if}

    {#if tab === "general"}
      <section data-testid="settings-general">
        <fieldset class="field">
          <legend>ログレベル (再起動後も保持。起動時は GENTO_LOG 環境変数が優先)</legend>
          <div class="radio-row" data-testid="settings-log-level">
            {#each LOG_LEVELS as level (level)}
              <label class="radio-label">
                <input
                  type="radio"
                  name="settings-log-level"
                  bind:group={logLevelInput}
                  value={level}
                  data-testid="settings-log-level-{level}"
                />
                {level}
              </label>
            {/each}
          </div>
        </fieldset>
        <div class="picker-actions">
          <button class="primary" onclick={() => void saveLogLevel()} data-testid="settings-log-level-save">
            保存
          </button>
        </div>
        {#if logNotice}
          <p class="picker-sub" data-testid="log-notice">{logNotice}</p>
        {/if}
      </section>
    {:else if tab === "display"}
      <section data-testid="settings-display">
        <p class="picker-sub">
          スクロール速度はここで変更できます (即時反映・永続化)。他の表示設定は
          現在の値の確認のみです (変更は各画面から)。
        </p>
        <!-- UIUX-3: スクロール速度の編集は表示タブ規約「確認のみ」の例外。
             根拠: 既存キャッシュタブが上限変更 (即時適用) を受け入れている
             前例にならい、local保存の表示系prefを直接編集する導線を1箇所に集約
             (ツールバーからの移設)。dialogはstore所有権を持たずcallbackで
             Appへ編集を委譲する。 -->
        <div class="field">
          <label for="scroll-speed-slider">
            スクロール速度 (ホイール1目盛りのスクロール量): {displayInfo.scrollSpeed.toFixed(1)}倍
          </label>
          <input
            id="scroll-speed-slider"
            type="range"
            min="0.5"
            max="5"
            step="0.1"
            value={displayInfo.scrollSpeed}
            data-testid="scroll-speed-slider"
            aria-label="スクロール速度"
            oninput={(event) => onScrollSpeedInput?.(Number(event.currentTarget.value))}
            onchange={(event) => onScrollSpeedChange?.(Number(event.currentTarget.value))}
          />
          <span class="picker-sub" data-testid="scroll-speed-value">
            {displayInfo.scrollSpeed.toFixed(1)}倍 (1.0=標準)
          </span>
          <div class="picker-actions">
            <button onclick={() => onScrollSpeedReset?.()} data-testid="scroll-speed-reset">
              既定に戻す (2.0)
            </button>
          </div>
        </div>
        <dl class="kv">
          <dt>前回のフォルダ</dt>
          <dd class="mono">{displayInfo.lastFolder ?? "(未設定)"}</dd>
          <dt>ツリー展開数</dt>
          <dd>{displayInfo.expandedCount.toLocaleString()}</dd>
          <dt>現在のソート</dt>
          <dd>{displayInfo.sortLabel ?? "(既定)"}</dd>
        </dl>
      </section>
    {:else if tab === "cache"}
      <section data-testid="settings-cache">
        <dl class="kv">
          <dt>サムネイルキャッシュ使用量</dt>
          <dd data-testid="cache-usage">
            {formatBytes(settings.cache.usedBytes)} / {formatBytes(settings.cache.maxBytes)}
          </dd>
        </dl>
        <div class="usage-bar" role="progressbar" aria-valuenow={Math.round(cacheUsagePercent)}>
          <div class="usage-fill" style="width:{cacheUsagePercent}%"></div>
        </div>
        <label class="field">
          上限 (MiB。変更は即時反映、縮小時は古いものから削除されます)
          <input
            type="number"
            min="64"
            bind:value={thumbsMiBInput}
            data-testid="cache-max-input"
          />
        </label>
        <div class="picker-actions">
          <button onclick={() => void saveThumbsMax()} data-testid="cache-max-save">適用</button>
        </div>
        {#if cacheNotice}
          <p class="picker-sub" data-testid="cache-notice">{cacheNotice}</p>
        {/if}
        <hr />
        {#if confirmClear}
          <p class="picker-sub" data-testid="cache-clear-confirm">
            キャッシュ内のサムネイルを全て削除します (元画像は変更しません。
            サムネイルは表示要求で自動再生成されます)。実行しますか?
          </p>
          <div class="picker-actions">
            <button onclick={() => (confirmClear = false)}>キャンセル</button>
            <button class="danger" disabled={cacheBusy} onclick={() => void clearCache()} data-testid="cache-clear-execute">
              {cacheBusy ? "削除中…" : "削除を実行"}
            </button>
          </div>
        {:else}
          <div class="picker-actions">
            <button onclick={() => (confirmClear = true)} data-testid="cache-clear-button">
              今すぐ削除
            </button>
          </div>
        {/if}
      </section>
    {:else if tab === "ai"}
      <section data-testid="settings-ai">
        <p class="picker-sub">
          外部送信の判断材料として、プロファイルの接続先分類とルート毎の設定を示します。
          変更はAIパネルから行ってください。初期状態では自動送信は全て無効です。
        </p>
        <h3>AIプロファイル</h3>
        {#if profiles.length === 0}
          <p class="picker-sub">プロファイルは登録されていません</p>
        {:else}
          <ul class="plain-list">
            {#each profiles as profile (profile.id)}
              <li>
                <span class="mono">{profile.baseUrl}</span>
                <span
                  class="endpoint-badge"
                  class:local={classifyEndpoint(profile.baseUrl) === "local"}
                  class:external={classifyEndpoint(profile.baseUrl) === "external"}
                  data-testid="ai-endpoint-class"
                >
                  {endpointClassLabel(classifyEndpoint(profile.baseUrl))}
                </span>
                {#if profile.hasApiKey}<span class="endpoint-badge">APIキー登録済</span>{/if}
              </li>
            {/each}
          </ul>
        {/if}
        <h3>ルート毎の自動処理</h3>
        {#if roots.length === 0}
          <p class="picker-sub">登録ルートはありません</p>
        {:else}
          <table class="matrix" data-testid="ai-roots-matrix">
            <thead>
              <tr><th>ルート</th><th>AI自動処理</th><th>EXIF送信</th><th>GPS送信</th></tr>
            </thead>
            <tbody>
              {#each roots as root (root.id)}
                <tr>
                  <td class="mono">{root.path}</td>
                  <td>{root.ai_enabled ? "オン" : "オフ"}</td>
                  <td>{root.ai_enabled && root.send_exif ? "オン" : "オフ"}</td>
                  <td>{root.ai_enabled && root.send_gps ? "オン" : "オフ"}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    {:else if tab === "search"}
      <section data-testid="settings-search">
        <p class="picker-sub">検索索引の現在状態 (読み取り専用)。</p>
        {#if diagnostics === null}
          <p class="picker-sub">
            <button onclick={() => void refreshDiagnostics()} data-testid="search-load">
              状態を取得
            </button>
          </p>
        {:else}
          <dl class="kv">
            <dt>FTS文書数</dt>
            <dd>
              {diagnostics.fts.actualDocs.toLocaleString()} / 期待 {diagnostics.fts.expectedDocs.toLocaleString()}
              {diagnostics.fts.consistent ? "(整合)" : "(要再構築)"}
            </dd>
            <dt>FTS最終再構築</dt>
            <dd>{diagnostics.fts.lastRebuildAt ?? "(未実施)"}</dd>
            <dt>USearch索引</dt>
            <dd>
              {diagnostics.usearch.indexedCount.toLocaleString()} / 正本 {diagnostics.usearch.sqliteCount.toLocaleString()}
              {diagnostics.usearch.model ? `(${diagnostics.usearch.model})` : ""}
            </dd>
          </dl>
        {/if}
      </section>
    {:else if tab === "backup"}
      <section data-testid="settings-backup">
        {#if backup === null}
          <p class="picker-sub">バックアップ設定を取得できません (DB未接続の可能性)</p>
        {:else}
          <fieldset class="field">
            <legend>フルDB自動バックアップ</legend>
            <div class="radio-row" data-testid="backup-auto">
              {#each [["off", "無効"], ["daily", "毎日"], ["weekly", "毎週"]] as [value, label] (value)}
                <label class="radio-label">
                  <input
                    type="radio"
                    name="backup-auto"
                    bind:group={backupAutoInput}
                    {value}
                    data-testid="backup-auto-{value}"
                  />
                  {label}
                </label>
              {/each}
            </div>
          </fieldset>
          <label class="field">
            世代保持数 (1〜100)
            <input
              type="number"
              min="1"
              max="100"
              bind:value={backupRetentionInput}
              data-testid="backup-retention"
            />
          </label>
          <div class="picker-actions">
            <button class="primary" onclick={() => void saveBackup()} data-testid="backup-save">
              保存
            </button>
          </div>
          {#if backupNotice}
            <p class="picker-sub" data-testid="backup-notice">{backupNotice}</p>
          {/if}
        {/if}
      </section>
    {:else if tab === "diagnostics"}
      <section data-testid="settings-diagnostics">
        <div class="picker-actions diag-actions">
          <button onclick={() => void refreshDiagnostics()} disabled={diagnosticsBusy} data-testid="diagnostics-refresh">
            {diagnosticsBusy ? "更新中…" : "更新"}
          </button>
        </div>
        {#if diagnostics === null}
          <p class="picker-sub">スナップショットを取得していません</p>
        {:else}
          <h3>DB</h3>
          <dl class="kv" data-testid="diag-db">
            <dt>接続</dt>
            <dd>{diagnostics.db.dbOpen ? "接続済" : "未接続"}</dd>
            <dt>整合性検査 (quick_check)</dt>
            <dd>{diagnostics.db.integrityCheck}</dd>
            <dt>schemaバージョン</dt>
            <dd>{diagnostics.db.schemaVersion}</dd>
            <dt>サイズ</dt>
            <dd>DB {formatBytes(diagnostics.db.dbBytes)} / WAL {formatBytes(diagnostics.db.walBytes)}</dd>
            <dt>設定破損検知</dt>
            <dd>{diagnostics.db.settingsCorruptCount.toLocaleString()}件</dd>
          </dl>
          <h3>FTS</h3>
          <dl class="kv" data-testid="diag-fts">
            <dt>文書数</dt>
            <dd>{diagnostics.fts.actualDocs.toLocaleString()} / 期待 {diagnostics.fts.expectedDocs.toLocaleString()}</dd>
            <dt>整合</dt>
            <dd>{diagnostics.fts.consistent ? "整合" : `不整合 (欠落${diagnostics.fts.missingDocs}・孤立${diagnostics.fts.orphanDocs}・内容不一致${diagnostics.fts.staleContents})`}</dd>
            <dt>最終再構築</dt>
            <dd>{diagnostics.fts.lastRebuildAt ?? "(未実施)"}</dd>
          </dl>
          <h3>USearch</h3>
          <dl class="kv" data-testid="diag-usearch">
            <dt>モデル</dt>
            <dd>{diagnostics.usearch.model || "(未選択)"}</dd>
            <dt>索引 / 正本</dt>
            <dd>
              {diagnostics.usearch.indexedCount.toLocaleString()} / {diagnostics.usearch.sqliteCount.toLocaleString()}
              {diagnostics.usearch.stale ? "(再構築が必要)" : "(一致)"}
            </dd>
            <dt>次元</dt>
            <dd>{diagnostics.usearch.dimensions ?? "-"}</dd>
            <dt>状態</dt>
            <dd>
              {diagnostics.usearch.ready ? "検索可能" : diagnostics.usearch.building ? "構築中" : "未構築"}
              {#if diagnostics.usearch.lastError}· {diagnostics.usearch.lastError}{/if}
            </dd>
          </dl>
          <h3>モデル</h3>
          <dl class="kv" data-testid="diag-models">
            <dt>埋め込み</dt>
            <dd>{diagnostics.models.enabled ? "有効" : "無効"}</dd>
            <dt>ワーカー</dt>
            <dd>
              {diagnostics.models.workerPhase}
              {#if diagnostics.models.workerRestarts > 0}
                (再起動{diagnostics.models.workerRestarts}回)
              {/if}
            </dd>
            {#each diagnostics.models.packages as pkg (pkg.modelId)}
              <dt>{pkg.modelId}</dt>
              <dd>
                {pkg.state} · {pkg.embeddingDim}次元 · {pkg.license}
              </dd>
            {/each}
          </dl>
          <h3>ジョブ</h3>
          <dl class="kv" data-testid="diag-jobs">
            {#each diagnostics.jobs.byType as row (row.jobType + row.status)}
              <dt>{jobTypeLabelOf(row.jobType)} ({row.status})</dt>
              <dd>{row.count.toLocaleString()}件</dd>
            {:else}
              <dt>待機中</dt>
              <dd>なし</dd>
            {/each}
            {#if diagnostics.jobs.oldestPendingAgeSeconds !== null}
              <dt>最古の待機経過</dt>
              <dd>{fmtDuration(diagnostics.jobs.oldestPendingAgeSeconds)}</dd>
            {/if}
          </dl>
        {/if}
        <hr />
        <h3>診断bundle</h3>
        <p class="picker-sub">
          設定・診断・直近ログを1つのJSONに書き出します。APIキー・GPS座標・
          実パスは除外されます (共有前に内容を確認してください)。
        </p>
        <label class="field">
          出力先ディレクトリ
          <input bind:value={bundleDest} data-testid="bundle-dest" />
        </label>
        <div class="picker-actions">
          <button
            class="primary"
            disabled={bundleBusy}
            onclick={() => void createBundle()}
            data-testid="bundle-create"
          >
            {bundleBusy ? "生成中…" : "bundleを生成"}
          </button>
        </div>
        {#if bundleResult}
          <p class="picker-sub" data-testid="bundle-result">
            生成しました: <span class="mono">{bundleResult}</span>
          </p>
        {/if}
      </section>
    {:else if tab === "about"}
      <section data-testid="settings-about">
        <h3>バージョン・ビルド情報</h3>
        {#if about !== null}
          <dl class="kv" data-testid="about-info">
            <dt>アプリバージョン</dt>
            <dd>{about.version}</dd>
            <dt>APIバージョン</dt>
            <dd>{about.apiVersion}</dd>
            <dt>ビルド</dt>
            <dd>{buildProfileLabel(about.profile, about.devtoolsBuild)}</dd>
            <dt>commit</dt>
            <dd class="mono">{about.commitShort} <span class="mono-full">({about.commit})</span></dd>
          </dl>
        {:else}
          <p class="picker-error" role="alert" data-testid="about-error">
            バージョン情報を取得できませんでした: {aboutError ?? "不明なエラー"}
          </p>
        {/if}
        <hr />
        <h3>ライセンス</h3>
        <dl class="kv">
          <dt>アプリ本体</dt>
          <dd>未設定 (MVP時点でプロジェクトライセンスは未選択)</dd>
          <dt>意味検索モデル (ruri-v3)</dt>
          <dd>Apache-2.0 (配布元: cl-nagoya/ruri-v3。ONNX変換版の表明は licenses/MODEL-LICENSES.md)</dd>
          <dt>サードパーティライセンス</dt>
          <dd>
            同梱の licenses/THIRD-PARTY-NOTICES.md、licenses/MODEL-LICENSES.md
            (インストール先のリソースディレクトリ) を参照
          </dd>
        </dl>
        <hr />
        <h3>更新履歴</h3>
        <pre class="changelog">{CHANGELOG_TEXT}</pre>
      </section>
    {/if}

    <div class="picker-actions">
      <button onclick={onClose}>閉じる</button>
    </div>
  </div>
</div>

<style>
  .picker-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 60;
  }

  .settings-picker {
    width: min(44rem, 92vw);
    max-height: 86vh;
    overflow-y: auto;
  }

  .picker {
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

  .picker h2 {
    font-size: 1rem;
    margin: 0;
  }

  .picker h3 {
    font-size: 0.82rem;
    margin: 0.4rem 0 0.1rem;
  }

  .picker-sub {
    margin: 0;
    font-size: 0.75rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .picker-error {
    margin: 0;
    font-size: 0.78rem;
    color: light-dark(#cf222e, #f85149);
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: monospace;
    font-size: 0.72rem;
  }

  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    border-bottom: 1px solid light-dark(#d0d7de, #30363d);
    padding-bottom: 0.4rem;
  }

  .tabs button {
    border: 1px solid transparent;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 0.78rem;
    padding: 0.2rem 0.55rem;
    border-radius: 0.375rem;
    cursor: pointer;
  }

  .tabs button.active {
    background: light-dark(#ddf4ff, #121d2f);
    border-color: light-dark(#0969da, #1f6feb);
    font-weight: 600;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.78rem;
  }

  .field input {
    font: inherit;
    font-size: 0.8rem;
  }

  fieldset.field {
    border: none;
    padding: 0;
    margin: 0;
  }

  fieldset.field legend {
    padding: 0;
    font-size: 0.78rem;
  }

  .radio-row {
    display: flex;
    gap: 0.9rem;
    flex-wrap: wrap;
  }

  .radio-label {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    font-size: 0.78rem;
  }

  .picker-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }

  .picker-actions button {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.25rem 0.7rem;
    border-radius: 0.375rem;
    border: 1px solid light-dark(#d0d7de, #444c56);
    background: light-dark(#f6f8fa, #21262d);
    color: inherit;
    cursor: pointer;
  }

  .picker-actions button.primary {
    background: light-dark(#0969da, #1f6feb);
    color: #ffffff;
    border: none;
  }

  .picker-actions button.danger {
    background: light-dark(#cf222e, #da3633);
    color: #ffffff;
    border: none;
  }

  .picker-actions button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .kv {
    display: grid;
    grid-template-columns: minmax(8rem, 14rem) 1fr;
    gap: 0.15rem 0.8rem;
    margin: 0;
    font-size: 0.78rem;
  }

  .kv dt {
    color: light-dark(#57606a, #8b949e);
  }

  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
  }

  .usage-bar {
    height: 0.5rem;
    border-radius: 0.25rem;
    background: light-dark(#eaeef2, #21262d);
    overflow: hidden;
  }

  .usage-fill {
    height: 100%;
    background: light-dark(#0969da, #1f6feb);
  }

  .plain-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.78rem;
  }

  .endpoint-badge {
    display: inline-block;
    margin-left: 0.4rem;
    padding: 0 0.4rem;
    border-radius: 0.6rem;
    font-size: 0.7rem;
    background: light-dark(#eaeef2, #21262d);
    color: light-dark(#57606a, #8b949e);
  }

  .endpoint-badge.local {
    background: light-dark(#dafbe1, #12261e);
    color: light-dark(#1a7f37, #3fb950);
  }

  .endpoint-badge.external {
    background: light-dark(#fff1e5, #2d1d10);
    color: light-dark(#bc4c00, #d29922);
  }

  .matrix {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.75rem;
  }

  .matrix th,
  .matrix td {
    text-align: left;
    padding: 0.15rem 0.4rem;
    border-bottom: 1px solid light-dark(#eaeef2, #21262d);
    overflow-wrap: anywhere;
  }

  .matrix th {
    color: light-dark(#57606a, #8b949e);
    font-weight: 600;
  }

  hr {
    border: none;
    border-top: 1px solid light-dark(#eaeef2, #30363d);
    margin: 0.5rem 0;
  }

  .mono-full {
    font-size: 0.66rem;
    color: light-dark(#57606a, #8b949e);
    overflow-wrap: anywhere;
  }

  .changelog {
    margin: 0;
    padding: 0.5rem 0.6rem;
    border: 1px solid light-dark(#eaeef2, #30363d);
    border-radius: 0.4rem;
    background: light-dark(#f6f8fa, #21262d);
    font-family: monospace;
    font-size: 0.68rem;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 14rem;
    overflow-y: auto;
  }
</style>
