# UI/UX改善5件（メニューバー再構成ほか）— PM計画

日付: 2026-09-04 / PM: PM agent / 実装: implementer 1件WP（UI全体が相互依存のため分割しない）

## 目的
利用者から5件のUI/UX改善要求。MVP機能は完了しており、本件は操作面の質改善。

## 要求と設計方針

### U-1: メニューバー作成・設定類の収納
- 現状: headerに スキャン/左ペイン/右ペイン/AI/⚙設定/開発パネル が並び、バックアップ・復元は左ペインLibraryPanel内、エクスポートはtoolbar内に分散。「ごちゃごちゃ」
- 方針: ** conventionalなメニューバー**（ファイル/表示/ツール/ヘルプ）をheader直下に追加。headerは状態表示（health/走査indicators/notices）+ メニューバーのみに簡素化
  - ファイル: スキャン開始・中止 / SD学習用エクスポート… / 終了
  - 表示: 左ペイン・右ペイン切替 / サムネイルサイズ（現toolbarのスライダーは残置也可——メニューからはプリセット数段階）
  - ツール: AI パネル / バックアップ（ルート選択…・全DB…） / 復元… / 開発パネル（devBridgeActive時のみ）
  - ヘルプ: このアプリについて（設定dialogのAboutタブを開く）
- **既存のショートカット・data-testidは維持**（E2E/devtest依存: settings-button等。移動するがtestidは残す）
- アクセシビリティ: menubar/menu role、キーボード操作（Alt+M等の開閉・矢印移動・ESC）は focus-trap.ts パターンを流用

### U-2: コレクションがルート登録と重なり編集不能
- 現状: `.side-pane.left` が FolderTree+CollectionsPane+LibraryPanel の3段縦積み（aside は overflow:hidden）。コレクション作成/スマート条件ビルダーが展開されるとスペースを食い、LibraryPanel（ルート登録）と視覚的・入力面的に衝突
- 方針: **左ペインをタブ化**（フォルダ / コレクション / ライブラリ）。各タブ独立スクロールで重なりを構造的に排除。最終選択タブは layout-prefs に永続化
- LibraryPanel内のバックアップ/復元entryは U-1 のメニューバーへ移動するため、ライブラリタブはルート一覧+登録/解除/再スキャン+per-rootバックアップに専念

### U-3: スクロール速度を設定可能に
- 現状: グリッドはネイティブスクロール。行高さが大きいため1ホイール刻みの体感移動量が小さく「遅い」
- 方針: サムネイルグリッドのスクロールコンテナに wheel handler（passive:false）を追加し、`deltaY × 係数` で scrollBy。係数は設定dialog（表示/操作性系タブ）で 0.5–5.0（0.1刻み）調整・**既定1.0（ネイティブ挙動=ハンドラ実質無効）**。永続化は app_settings（Rust設定・WP-15機構）に「ui.scrollSpeed」namespace——設定dialogに既存の保存経路を使う。係数1.0ではpreventDefaultしない（ネイティブ完全互換）
- 適用範囲: サムネイルグリッド（本体）。ビューア/比較のズームホイールは既存のまま変更しない

### U-4: ファイラーのフォルダ右クリックでルート登録
- 現状: FolderTree に context menu 無し。ルート登録はLibraryPanelのフォームのみ
- 方針: ContextMenu.svelte を再利用し FolderTree のフォルダ节点に context menu: 「このフォルダを登録ルートに追加」（未登録時）/「登録を解除」（登録ルート时）/「このルートをバックアップ」（登録時）/「再スキャン」（登録時）。登録は既存の library.addRoot 経路（重複登録は既存の抑止に従う）
- 併せてグリッド上のフォルダ表示は現状なし（ファイルのみ）のため対象はFolderTree节点のみ

### U-5: パンくずリストへのパス直接入力
- 現状: breadcrumb はクリック移動のみ
- 方針: breadcrumb 右端に「パスを入力」ボタン（📌/✏アイコン+aria-label）。押下で input に切替→ペースト/入力→Enter で移動。移動は既存 selectFolder/fsview 経路に正規化した絶対パスを渡す（末尾スラッシュ・引用符・前後空白はtrim、`~` は展開しない——絶対パスのみ受け付け、相対はエラー通知）。存在しない/アクセス不可は既存のエラー通知経路
- ESCで入力キャンセル。フォルダ以外（ファイルパス等）はエラー

## 非目標
- テーマ/ダークモード切替、アイコン全面差し替え、多言語化
- ビューア/比較のスクロール挙動変更
- 右ペイン（InfoPanel）再構成

## 受け入れ条件
1. メニューバー経由で スキャン/AI/設定/バックアップ/復元/エクスポート/ペイン切替/About が開ける。headerのボタン類は撤去（状態表示は残る）
2. 左ペインタブ切替でフォルダ/コレクション/ライブラリが独立スクロールし、コレクション編集（作成・スマート条件・rename）があらゆるウィンドウ高で衝突なく可能
3. スクロール速度係数が設定dialogで変更・永続化され、再起動後も適用。既定1.0で挙動が現行と同一
4. FolderTree 右クリックで未登録フォルダを登録でき、登録済みなら解除/バックアップ/再スキャンが出る
5. パンくずのパス入力で絶対パス指定のフォルダへ移動できる。無効パスは通知されクラッシュしない
6. 既存E2E（uxlib1 18/18・wp4 8/8）・vitest全緑・cargo基準（965）維持・svelte-check 0 error

## テスト戦略
- vitest: MenuBar（開閉・項目・キーボード）/ 左ペインタブ状態 / scroll-speed 設定読み書きと係数計算 / breadcrumbパス正規化（純粋関数化: normalizePastedPath）/ FolderTreeコンテキストメニュー項目の状態分岐
- red→green: 本WPは新規UIが主のため、正規化関数と設定永続化はtest-first可能な部分のみred実証
- E2E: 既存2件再実行（回帰）。加えてdevtestブリッジ拡張: タブ切替行・メニュー開閉行・パス入力移動行（最低限）——ただし実装コストと時間を見て「既存E2E全緑+手動確認スクリーンショット」で代替可（その場合は手動確認をhandoffに正確記録）

## 実装計画（単一WP）
1. MenuBar.svelte 新規 + App.svelte header再構成（状態表示残置・ボタン撤去・testid維持）
2. 左ペインタブ化（LeftPaneTabs・layout-prefs拡張・LibraryPanel整理）
3. scroll-speed: app_settings namespace + 設定dialog UI + ThumbnailGrid wheel handler
4. FolderTree context menu（ContextMenu再利用・library store経路）
5. breadcrumb入力モード（normalizePastedPath純粋関数+ボタン/入力UI）
6. vitest追加・既存test修正・E2E再実行・手動確認（Xephyr・スクリーンショット）

## ドキュメント/ハンドオフ影響
- README（操作説明: メニューバー・右クリック登録・パス入力）更新
- handoff `2026-09-04-uiux-1.md` 新規
- a11y-scope.md にメニューバーのキーボード操作追記

## リスク
- E2E driverのUI参照（ボタン撤去で壊れる可能性）→ testid維持で緩和。壊れた場合はdriver側の参照更新を同WP内で実施（E2Eは検証資産であり仕様ではないため、UI変更に追従させるのは正当）
- メニューバーのkeyboard操作性（a11y回帰）→ focus-trapパターン流用+手動確認
- wheel handler追加によるスクロール性能劣化 → passive:falseは係数≠1.0時のみ登録する設計で回避

## 仮定台帳
| 仮定 | 根拠 | 反証条件 | 爆発半径 |
|---|---|---|---|
| スクロール遅さの体感原因は行高さ対ホイール刻み | 行~200px・X11標準delta | 設定後も「遅い」報告 | 係数範囲拡張のみで対応可 |
| メニューバー形式よりtoolbar整理が求められる可能性 | 利用者は「收纳」のみ指定 | 初回確認で不満 | UI差し替え（小） |
| fsview selectFolder は絶対パス正規化を受け入れる | browse.ts既存経路 | 実装時の型/検証不一致 | breadcrumb側で正規化強化 |
| app_settings にui系設定を置くのは既存慣行 | WP-15 settings.rs | 実装でnamespace衝突 | localStorage退避（小） |

## 改訂（Codex plan review反映 — /tmp/codex-uiux-plan.md）

### R-A: メューインベントリ（全操作の移動先・残置を明示）
| 現在の操作 | 移動先/残置 | 備考 |
|---|---|---|
| スキャン開始/走査キャンセル（一時カタログ） | ファイル>「一時走査を実行/中止」 | LibraryPanelの再スキャンと**名前を区別** |
| ライブラリ再スキャン/停止（LibraryPanel） | ライブラリタブに残置 | 登録ルート系はタブ内維持 |
| LibraryPanel: 再取得/ライブラリを表示/ルート有効無効/per-root操作 | ライブラリタブに残置 | バックアップ/復元entryのみメニューへ |
| SDエクスポート（toolbar） | toolbar残置 + ファイル>「SD学習用エクスポート…」は**表示中コレクション優先・非コレクション時は選択資産・空なら無効** | 文脈を壊さない |
| AI / ⚙設定 / 開発パネル | ツール>「AIパネル…」/「設定…」/「開発パネル」 | 設定dialogは**初期タブ指定参数**追加（Aboutはヘルプ>「このアプリについて」からAboutタブ直開） |
| ルートバックアップ/全DBバックアップ/復元（LibraryPanel） | ツール>バックアップ系3項目 | per-rootバックアップはライブラリタブにも残置 |
| 終了 | ファイル>「終了」 | Tauri window close経路（現存window API確認） |
| 左/右ペイン切替 | 表示 | headerボタン撤去 |
| 新規フォルダ/並び替え/コレクション操作/ビューアモード/検索/サムネサイズ | toolbar・contextual残置 | 高頻度・文脈操作はメニュー化しない |
| health/走査indicators/source badge/notices | header残置（状態表示） | 狭幅時のwrap/省略を指定 |

### R-B: タブは**全マウント維持**（非アクティブはアクセシブルに隠蔽）でドラフト保存（createMode/スマート条件/rename/root-path入力）。隠れパネルはfocus不可、タブ切替でフォーカス移動。最終タブはlayout-prefs
### R-C: スクロール速度は**layout-prefs**（localStorage・デバイス固有性）に格納。wheel仕様: deltaMode別正規化（pixel/line=行高さ換算/page）、ctrlKey=ズーム素通り、deltaX無視、係数1.0は**リスナ未登録**（設定変更で同一callback/capture identityで除去再登録）、Svelte 5は**action/lifecycleで`{passive:false}`**登録、端ではチェイニング阻害しない。設定はThumbnailGridへ反映（即時・設定dialog→App伝播）
### R-D: ルート右クリックは**3状態**（exact root / descendant / unrelated）で項目分岐。メューは**クリック時点のpath/rootIDの不変スナップショット**から構築（選択状態に依存しない・roots更新でstaleになったら閉じる）。descendantには「登録解除」を出さない。unregisteredでもaddRootの親子包含拒否は既存エラー通知に従う。ツリー行はstopPropagationで単一ノードのみ開く。ContextMenu自体のfocus/矢印操作は本WPでは最小（ESC・クリック外閉じ既存+項目focus初期化）に留め、フルmenubar keyboard modelはMenuBar側に実装
### R-E: パンくず入力の正規化は保守的: 対になった引用符のみ除去・前後空白trim・末尾スラッシュ正規化（UNC/ドライブルートは保存）・改行/NUL拒否・**通常パスのpercent-decode禁止**（file:// URIは非対応と通知）・`..`やsymlinkは**backendに正規化委譲**（UIで字句判断しない）。存在/種別/権限/大文字小文字は既存のopen-folder結果が正本。移動成功はlibrary/collection表示モードを**通常のツリー選択と同一遷移**で脱する
### R-F: テスト戦略（E2E必須・スクリーンショットは補助）: ①vitest: メニュー項目の有効無効分岐・タブドラフト生存・wheelアルゴリズム（deltaMode/ctrl/端・係数）・正規化関数（POSIX/drive/UNC/引用符/空白/改行/fileパス）・3状態ルートメニュー構築（純粋化） ②X11 E2E（devtestブリッジ拡張）: 各メニューから実操作（一時走査/設定open/About直開/バックアップ/エクスポート無効条件）・タブ切替ドラフト生存・右クリック登録（unregistered→登録確認）・パンくずパス移動（成功/不存在） ③既存driverのselector棚卸し→text/座標依存も含め意図的更新（testid維持だけでは不十分）

## ゲート
- normal扱い: Codex plan review（本計画）→ 実装 → PM検証+E2E → Codex diff review → QA review（user-facing）

## 結果（2026-09-04完了）
- 実装 `e10661e` → Codex差分r1 reject（4 blocker: wheel境界チェイニング・line正規化のextent依存・UNC root・stale rootId）→ FG-UIUX-1 `cac5add` → r2 reject（destroy再登録リーク・ctrlKey未伝播）→ FG-UIUX-2 `09f2a87` → **r3 approve-with-notes**。QA **pass-with-notes**（要求5件すべて実機検証・vitest 622・E2E uiux1 46/46+全driver再実行緑・wp4は既知フレーク）
- PM裁定記録: `core:window:allow-close`は`core:window:default`28権限に含まれない（tauri 2.11.5 autogenerated referenceで正本確認）→capabilities追加は必須・レビューの冗長指摘は誤り
- **QA発見の申し送り（次イテレーション必須・既存backend）**: `/`登録が無確認で可能+走査中解除でscanner wedge（再起動で回復）。右クリック登録でトリガー容易化。対策案: 巨大パス登録の確認dialog+解除時スキャンキャンセル+wedge検出
- その他QA指摘: 受け入れ条件3の文言改訂（toolbar配置を正に）・rename auto-select・タブラベル省略・wp4 driver専用DISPLAY化

## UIUX-2（QA Major修正・2026-09-04完了）
- `c8ac4b0`→r1 reject（3 blocker: テストが本番にないcancel観測をモデル化/`..`字句bypass/watchdog spin+RAII無し）→`ac2ed0b`（FG-UIUX-3: **walk列挙自体のcancel中断をgento-core/imageに実装**・PlatformFs実体10k件テスト・字句解決先行・Disconnected区別+Drop RAII+ScanningGuard・E2E動的集計50/50・RegistrationConfirmGate）→r2 reject（gate二重送信でpromise孤児化）→`63c9c61`（FG-UIUX-4: busy即false拒否+フォーム無効化）→r3 reject（busy検査が分類後=確認不要pathがbypass）→`a14ed8b`（FG-UIUX-5: busy検査を分類前へ・混合path回帰テスト）→**r4 approve**。
- **QA-UIUX-2: pass-with-notes・出荷可（条件なし）**: Major両症状の実機再現不能（`scan cancelled`ログ・再スキャン実行可・`/`右クリック→dialog→Escapeでroots不変）・軽微3件実機確認・数値独立一致（645/973/50-50）。
- **予算超過の申告**: 本ゲートは微小修正4ラウンド（通常上限3）。r3の指摘は「直列化の適用範囲漏れ」という別クラスタの機械的修正（レビュー指定の2行入れ替え+テスト1件）のため実施した——判断は自己postmortemに記録
- 残: LibraryPanel Enter確定テスト1件（QA Minor）・N2 dialog経由の実機操作確認・N4多ルート巻き込みの実機確認（次回QA）・リリース時はHEADから再build（stale artifact注意）
