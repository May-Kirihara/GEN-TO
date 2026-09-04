<script lang="ts">
  /**
    * テスト専用harness (FolderTree.test.tsからのみ使用・製品コード不使用)。
    * Svelte 5のmountに直渡ししたpropsは外部から書き替えても反映されない
    * ため、$state経由でrootsを差し替える導線をexportする。
    */
  import FolderTree from "./FolderTree.svelte";
  import type { FolderTreeStore } from "../fsview";
  import type { LibraryRoot } from "../library";

  let {
    tree,
    initialRoots,
    currentFolder = null,
    libraryMode = false,
    onUnregisterRoot,
    onRescan,
  }: {
    tree: FolderTreeStore;
    initialRoots: LibraryRoot[];
    currentFolder?: string | null;
    libraryMode?: boolean;
    onUnregisterRoot?: (rootId: string) => void;
    onRescan?: () => void;
  } = $props();

  // テストharnessでは初期rootsのスナップショットから開始し、以降は
  // setRoots()でのみ差し替える (初期値参照の意図的な捕捉)
  // svelte-ignore state_referenced_locally
  let roots = $state(initialRoots);

  export function setRoots(next: LibraryRoot[]): void {
    roots = next;
  }
</script>

<FolderTree
  {tree}
  {roots}
  {currentFolder}
  {libraryMode}
  onSelect={() => {}}
  onDrop={undefined}
  onRegisterRoot={() => {}}
  onUnregisterRoot={(rootId) => onUnregisterRoot?.(rootId)}
  onBackupRoot={() => {}}
  onRescan={() => onRescan?.()}
/>
