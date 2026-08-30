/**
 * フォルダツリーのフロントエンドストア (MVP WP-4)。
 *
 * - `fs_list_dirs` commandで任意パスの直下ディレクトリを遅延ロード
 *   (子は展開時取得。展開済みノードはキャッシュ)。
 * - 起点はルート要求 (ホーム + /)。隠しディレクトリはbackendが除外済み。
 * - 読み取り不能・不在ノードは error 文言を持たせ、UIが状態表示する。
 * - Svelte 5の `$state` はクラスを深くプロキシしないため、onChange通知で
 *   ローカル$state変数へ同期する (既存ストアの規約)。
 */

export type FsIpc = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
};

/** `fs_list_dirs` の応答DTO (api-contract §19)。 */
export type FsDirList = {
  path: string;
  parent: string | null;
  entries: Array<{ name: string; path: string }>;
  error: string | null;
};

type NodeState = {
  name: string;
  /** 子の列挙結果 (null = 未取得)。 */
  children: string[] | null;
  error: string | null;
  loading: boolean;
};

/** 描画用の平坦行。 */
export type TreeRow = {
  path: string;
  name: string;
  depth: number;
  expanded: boolean;
  loading: boolean;
  error: string | null;
  /** 子を1つ以上持つか分かっているか (展開矢印の表示)。 */
  hasChildren: boolean | null;
};

export function toMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}

/**
 * パスが登録ルート (またはその配下) か。ツリーの登録バッジ・
 * 閲覧ソース切替の判定に使う純粋関数。ルートパスは正規化済み
 * (backendがcanonicalize) を想定する。
 */
export function registeredStateFor(
  path: string,
  roots: ReadonlyArray<{ path: string }>,
): "root" | "inside" | null {
  for (const root of roots) {
    if (path === root.path) {
      return "root";
    }
    if (root.path === "/" ? path.startsWith("/") : path.startsWith(root.path + "/")) {
      return "inside";
    }
  }
  return null;
}

export class FolderTreeStore {
  /** 起点ノード (ホーム・/)。 */
  tops: string[] = [];
  private readonly nodes = new Map<string, NodeState>();
  private readonly expanded = new Set<string>();
  selected: string | null = null;
  lastError: string | null = null;
  version = 0;

  private readonly changeListeners = new Set<() => void>();
  private disposed = false;

  constructor(private readonly ipc: FsIpc) {}

  onChange(listener: () => void): () => void {
    this.changeListeners.add(listener);
    return () => {
      this.changeListeners.delete(listener);
    };
  }

  private notify(): void {
    this.version++;
    for (const listener of this.changeListeners) {
      listener();
    }
  }

  private nodeOf(path: string, name?: string): NodeState {
    let node = this.nodes.get(path);
    if (node === undefined) {
      node = { name: name ?? path, children: null, error: null, loading: false };
      this.nodes.set(path, node);
    } else if (name !== undefined) {
      node.name = name;
    }
    return node;
  }

  /** 起点ノードを読み込む (アプリ起動時・リロード時)。 */
  async init(): Promise<void> {
    await this.reloadTop();
  }

  async reloadTop(): Promise<void> {
    try {
      const list = await this.ipc.invoke<FsDirList>("fs_list_dirs", {});
      for (const entry of list.entries) {
        this.nodeOf(entry.path, entry.name);
      }
      this.tops = list.entries.map((entry) => entry.path);
      this.lastError = null;
      this.notify();
    } catch (error) {
      this.lastError = toMessage(error);
      this.notify();
    }
  }

  isExpanded(path: string): boolean {
    return this.expanded.has(path);
  }

  expandedPaths(): string[] {
    return [...this.expanded];
  }

  /** 展開/折りたたみ。展開時は子を (未取得なら) 取得する。 */
  async toggle(path: string): Promise<void> {
    if (this.expanded.has(path)) {
      this.expanded.delete(path);
      this.notify();
      return;
    }
    this.expanded.add(path);
    this.notify();
    await this.ensureChildren(path);
  }

  /** 強制展開 (前回状態の復元など)。ensure=true で即座に子も取得。 */
  async expandTo(path: string, ensure = true): Promise<void> {
    this.expanded.add(path);
    this.notify();
    if (ensure) {
      await this.ensureChildren(path);
    }
  }

  /** 子を未取得なら取得する。取得済み・取得中は何もしない。 */
  async ensureChildren(path: string): Promise<void> {
    const node = this.nodeOf(path);
    if (node.children !== null || node.loading || this.disposed) {
      return;
    }
    node.loading = true;
    this.notify();
    try {
      const list = await this.ipc.invoke<FsDirList>("fs_list_dirs", { path });
      if (this.disposed) {
        return;
      }
      for (const entry of list.entries) {
        this.nodeOf(entry.path, entry.name);
      }
      node.children = list.entries.map((entry) => entry.path);
      node.error = list.error;
      node.loading = false;
      this.lastError = null;
      this.notify();
    } catch (error) {
      if (this.disposed) {
        return;
      }
      node.loading = false;
      node.children = [];
      node.error = toMessage(error);
      this.notify();
    }
  }

  select(path: string | null): void {
    this.selected = path;
    this.notify();
  }

  childrenOf(path: string): string[] | null {
    return this.nodes.get(path)?.children ?? null;
  }

  errorOf(path: string): string | null {
    return this.nodes.get(path)?.error ?? null;
  }

  /** 展開状態を平坦な行列表現へ (描画用)。 */
  visibleRows(): TreeRow[] {
    const rows: TreeRow[] = [];
    const walk = (paths: readonly string[], depth: number): void => {
      for (const path of paths) {
        const node = this.nodes.get(path);
        if (node === undefined) {
          continue;
        }
        const expanded = this.expanded.has(path);
        rows.push({
          path,
          name: node.name,
          depth,
          expanded,
          loading: node.loading,
          error: node.error,
          hasChildren: node.children === null ? null : node.children.length > 0,
        });
        if (expanded && node.children !== null) {
          walk(node.children, depth + 1);
        }
      }
    };
    walk(this.tops, 0);
    return rows;
  }

  dispose(): void {
    this.disposed = true;
    this.changeListeners.clear();
  }
}
