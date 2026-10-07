/**
 * 初回起動ガイドの純粋計算 (WP-18 E3)。
 *
 * 登録ルート0件のときだけ3ステップ (登録ルート / AI設定 / モデル配置) を
 * 導線として返す。ウィザードではなく空状態の充実として表示するため、
 * 表示条件 (グリッド空・検索/コレクション表示でない) はApp側で判定する。
 */

export type FirstRunModelInfo = {
  /** モデルパッケージの配置先 (embedding_status.modelsDir。取得失敗時null)。 */
  modelsDir: string | null;
  /** 検証済みパッケージ数 (取得失敗時null)。 */
  verifiedCount: number | null;
};

export type FirstRunStep = {
  key: "roots" | "ai" | "model";
  title: string;
  body: string;
  /** 状態表示 (テキスト。色のみに依存しない)。 */
  status: string;
  done: boolean | null;
};

/**
 * 初回起動 (登録ルート0件) の導線ステップを導出する。
 * 登録ルートが1件でもあればnull (導線不要)。
 */
export function deriveFirstRunGuide(input: {
  rootsCount: number;
  aiProfilesCount: number | null;
  model: FirstRunModelInfo;
}): FirstRunStep[] | null {
  if (input.rootsCount > 0) {
    return null;
  }
  const aiStatus =
    input.aiProfilesCount === null
      ? "状態不明"
      : input.aiProfilesCount > 0
        ? `登録済み (${input.aiProfilesCount.toLocaleString()}件)`
        : "未設定";
  const modelStatus =
    input.model.verifiedCount === null
      ? "状態不明"
      : input.model.verifiedCount > 0
        ? `配置済み (${input.model.verifiedCount.toLocaleString()}件)`
        : "未配置";
  const modelsDirText =
    input.model.modelsDir ?? "既定のデータディレクトリ内 models/ (--models-dir でも指定可)";
  return [
    {
      key: "roots",
      title: "登録ルートを追加する",
      body: "左ペイン下部の「登録ルート」に画像フォルダのパスを入力して追加してください。登録すると初回スキャン・差分更新・フォルダ監視が有効になり、ライブラリとして永続管理されます。",
      status: "未実施",
      done: false,
    },
    {
      key: "ai",
      title: "AI設定 (任意)",
      body: "メニューバー「ツール>AIパネル…」からOpenAI互換endpoint (llama.cpp等) のプロファイルを登録できます。AIを設定しなくてもファイラー機能 (閲覧・選別・タグ・検索・コレクション・ファイル操作) はすべて使用できます。",
      status: aiStatus,
      done: input.aiProfilesCount === null ? null : input.aiProfilesCount > 0,
    },
    {
      key: "model",
      title: "意味検索モデルの配置 (任意)",
      body: `自然言語検索にはローカル埋め込みモデル (ruri-v3) のパッケージが必要です。配置先: ${modelsDirText}。入手・配置手順は README「初回起動ガイド」および docs/embedding-runtime.md を参照してください。`,
      status: modelStatus,
      done: input.model.verifiedCount === null ? null : input.model.verifiedCount > 0,
    },
  ];
}
