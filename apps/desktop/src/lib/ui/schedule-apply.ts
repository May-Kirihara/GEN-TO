/**
 * WP-UX-FILTER: trailingデバウンスで「後で1回だけ適用」をスケジュールする
 * 最小スケジューラ (純粋ヘルパー — App.svelteはハーネス不在のためロジックを
 * 切り出してvitestで検証する)。
 *
 * - schedule(fn): 保留中の実行を破棄してからfnを遅延後に1回だけ再スケジュール
 *   (latest-wins)。フィルタ連続設定・チップ連続削除が最終状態の1回の実行に
 *   収束する。
 * - cancel(): 保留中の実行を取り消す (コンポーネント破棄時の握り潰し)。
 * - タイマーは注入可能 (既定はglobalThis) — テストでは疑似タイマーで検証。
 *
 * 呼び出し元の設計意図: 検索フィルタは即時適用・検索モード
 * (キーワード/ハイブリッド/意味) はUIUX-7設計どおり明示実行 — 本クラスは
 * フィルタ変更経路 (applySearchFilters) のみで使う。
 */

/** タイマー取得フック (テスト注入用)。handle型は環境 (DOM/Node) 非依存。 */
export type ApplyTimerHooks = {
  setTimeout: (handler: () => void, ms: number) => unknown;
  clearTimeout: (handle: unknown) => void;
};

const defaultTimerHooks: ApplyTimerHooks = {
  setTimeout: (handler, ms) => globalThis.setTimeout(handler, ms),
  clearTimeout: (handle) =>
    globalThis.clearTimeout(handle as ReturnType<typeof globalThis.setTimeout>),
};

/** 遅延後に1回だけ実行する適用をスケジュールする (再スケジュールは最新勝ち)。 */
export class ScheduleApply {
  private handle: unknown = null;

  constructor(
    private readonly delayMs: number,
    private readonly timers: ApplyTimerHooks = defaultTimerHooks,
  ) {}

  /** fnをdelayMs後に1回実行するよう再スケジュール (保留中があれば先に破棄)。 */
  schedule(fn: () => void): void {
    this.cancel();
    this.handle = this.timers.setTimeout(() => {
      // 実行前に解放 — fnが例外を投げても保留状態には戻さない
      this.handle = null;
      fn();
    }, this.delayMs);
  }

  /** 保留中の実行を取り消す (実行済み・未スケジュールなら何もしない)。 */
  cancel(): void {
    if (this.handle !== null) {
      this.timers.clearTimeout(this.handle);
      this.handle = null;
    }
  }

  /** 実行待ちのタスクがあるか。 */
  isPending(): boolean {
    return this.handle !== null;
  }
}
