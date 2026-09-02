/**
 * devtest画面読み出しオーバーレイの可視性 (DEVUX-1)。
 *
 * 画面右上の固定pre (`G tot=...`) がdevビルドでヘッダー右上に常時重なり
 * 製品UIの視認を妨げていたため、読み出しは**既定で非表示**とする。
 * 可視性はdev bridgeのコマンドモード (ctrl+shift+F12 → m) の
 * readoutToggleアクションでトグルし、選択をlocalStorageへ永続化する
 * (再起動後も維持)。キーはgento:接頭辞のためresetStorage (ctrl+F1)
 * で消去され既定OFFへ戻る — 計測ドライバの初期状態決定論化を維持する。
 *
 * バックエンドログへのreportCompact送出 (1秒周期の `devtest:` 行) は
 * 表示とは独立に常時継続する (E2Eドライバの正本はログ側)。
 */

export const DEVTEST_READOUT_KEY = "gento:devtest-readout";

export type KeyValueStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

/** `"1"`のときのみ可視。未設定・破損値は既定の非表示へ落とす。 */
export function parseDevReadoutVisible(raw: string | null): boolean {
  return raw === "1";
}

/** 現在値を反転して永続化し、次の可視性状態を返す。 */
export function toggleDevReadout(storage: KeyValueStorage): boolean {
  const next = !parseDevReadoutVisible(storage.getItem(DEVTEST_READOUT_KEY));
  storage.setItem(DEVTEST_READOUT_KEY, next ? "1" : "0");
  return next;
}
