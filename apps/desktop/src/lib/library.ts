/**
 * 登録ルート (ライブラリ) のフロントエンドストア (WP-2)。
 *
 * - command呼び出し・イベント購読を注入 (LibraryIpc) してテスト容易にする
 * - ルート一覧・追加 (親子重複警告付き) ・解除・有効切替を担う
 * - ライブラリスキャンの開始/停止/状態取得
 * - カタログ表示ソース切替 (temp=一時走査 / library=DB由来)
 * - 型契約は docs/dev/api-contract.md §17
 */

/** Tauri IPC境界の注入用アダプター (本物は@tauri-apps/api、試験はモック)。 */
export type LibraryIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(
    event: string,
    handler: (event: { payload: T }) => void,
  ) => Promise<() => void>;
};

/** 登録ルート1件のDTO (`library_roots_list` / `library_roots_add`)。 */
export type LibraryRoot = {
  /** 10進文字列 (AssetIdの文字列方針に合わせる)。 */
  id: string;
  uuid: string;
  path: string;
  enabled: boolean;
  watch_enabled: boolean;
  /** パスが現在存在するか (アンマウント等で不在表示に使う)。 */
  available: boolean;
};

/** `library_roots_add` の応答。 */
export type AddRootResponse = {
  root: LibraryRoot;
  /** 親子関係にある既存ルート (二重登録警告。登録自体は実施済み)。 */
  overlapping: LibraryRoot[];
};

/** `scan_status` の応答。 */
export type LibraryScanStatus = {
  scanning: boolean;
  phase: "scanning" | "hashing" | null;
  found: number;
  updated: number;
  relocated: number;
  missing: number;
  hashed: number;
  pending_hash: number;
  errors: number;
  /** true = watcher未確立の有効ルートがある (inotify上限等の劣化運用中)。 */
  watch_degraded: boolean;
};

/** カタログ表示ソース (temp=一時走査 / library=登録ルートDB)。 */
export type CatalogSource = "temp" | "library";

export const EMPTY_SCAN_STATUS: LibraryScanStatus = {
  scanning: false,
  phase: null,
  found: 0,
  updated: 0,
  relocated: 0,
  missing: 0,
  hashed: 0,
  pending_hash: 0,
  errors: 0,
  watch_degraded: false,
};

function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/** 登録ルートUI状態ストア。 */
export class LibraryStore {
  roots: LibraryRoot[] = [];
  status: LibraryScanStatus = EMPTY_SCAN_STATUS;
  /** 追加時の親子重複警告 (表示後クリア可能)。 */
  overlapWarning: string | null = null;
  lastError: string | null = null;
  lastNotice: string | null = null;
  /** 現在のカタログ表示ソース (set_catalog_source 成功時に更新)。 */
  source: CatalogSource | null = null;

  private readonly unlisteners: Array<() => void> = [];
  private readonly changeListeners = new Set<() => void>();
  private statusTimer: ReturnType<typeof setInterval> | null = null;

  constructor(private readonly ipc: LibraryIpc) {}

  /** 状態変更通知の購読。解除関数を返す。 */
  onChange(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  private notify(): void {
    for (const listener of this.changeListeners) {
      listener();
    }
  }

  /** ルート取得・イベント購読・状態ポーリング開始。dispose関数を返す。 */
  async init(): Promise<() => void> {
    await this.refreshRoots();
    await this.refreshStatus();
    const unlistenRoots = await this.ipc.listen("roots:changed", () => {
      void this.refreshRoots();
    });
    this.unlisteners.push(unlistenRoots);
    // スキャン中は状態をポーリング (進捗詳細は scan:progress にも流れるが、
    // ストア単体ではポーリングが最も単純で確実)。
    this.statusTimer = setInterval(() => {
      void this.refreshStatus();
    }, 1000);
    return () => this.dispose();
  }

  dispose(): void {
    if (this.statusTimer !== null) {
      clearInterval(this.statusTimer);
      this.statusTimer = null;
    }
    for (const unlisten of this.unlisteners.splice(0)) {
      unlisten();
    }
  }

  async refreshRoots(): Promise<boolean> {
    try {
      this.roots = await this.ipc.invoke<LibraryRoot[]>("library_roots_list");
      this.lastError = null;
      this.notify();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async refreshStatus(): Promise<boolean> {
    try {
      this.status = await this.ipc.invoke<LibraryScanStatus>("scan_status");
      return true;
    } catch {
      // ライブラリ機能なし (DB未開) では静かに空状態を保つ
      this.status = EMPTY_SCAN_STATUS;
      return false;
    }
  }

  /** ルートを追加。親子重複があれば警告メッセージを設定する。 */
  async addRoot(path: string): Promise<boolean> {
    this.overlapWarning = null;
    this.lastNotice = null;
    try {
      const response = await this.ipc.invoke<AddRootResponse>("library_roots_add", {
        path,
      });
      this.lastError = null;
      this.lastNotice = `登録しました: ${response.root.path}`;
      if (response.overlapping.length > 0) {
        const paths = response.overlapping.map((root) => root.path).join(", ");
        this.overlapWarning = `親子関係の既存ルートがあります (二重インデックス注意): ${paths}`;
      }
      await this.refreshRoots();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async removeRoot(rootId: string): Promise<boolean> {
    try {
      await this.ipc.invoke("library_roots_remove", { rootId });
      this.lastError = null;
      this.lastNotice = "解除しました";
      await this.refreshRoots();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async setEnabled(rootId: string, enabled: boolean): Promise<boolean> {
    try {
      await this.ipc.invoke("library_roots_set_enabled", { rootId, enabled });
      this.lastError = null;
      // サーバ側の応答後に一覧を引き直して反映
      await this.refreshRoots();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  /** ライブラリ全体の再スキャンを開始。 */
  async startScan(): Promise<boolean> {
    try {
      await this.ipc.invoke("scan_start");
      this.lastError = null;
      await this.refreshStatus();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  async stopScan(): Promise<boolean> {
    try {
      await this.ipc.invoke("scan_stop");
      this.lastError = null;
      await this.refreshStatus();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }

  /** カタログ表示ソースを切り替える (成功時のみ source を更新)。 */
  async useSource(source: CatalogSource): Promise<boolean> {
    try {
      await this.ipc.invoke("set_catalog_source", { source });
      this.source = source;
      this.lastError = null;
      this.notify();
      return true;
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
      return false;
    }
  }
}
