//! 安全なファイル操作のcommand群 (MVP WP-7)。
//!
//! - ドメインロジック (計画・実行・Undo前提検証) は gento-core `file_ops`。
//! - この層は以下を担う:
//!   (1) 実FS実装 ([`RealFileOps`]: std fs + BLAKE3 (gento-image) + ゴミ箱
//!   (gento-platform))、(2) IPC DTO変換、(3) watcher抑制・カタログ反映の
//!   接着、(4) 異常終了回復。
//! - 実行は常に「計画 (dry-run) → 確認 → 実行 → ログ確定 → 通知」の順
//!   (安全原則5)。commandは [`gento_core::file_ops::execute_file_op`] の
//!   戻り (file_operations確定後) を応答する。
//! - 進捗は `file_ops:progress` イベント (ファイル単位)。
//! - 同時実行は1操作に制限 ([`FileOpsService::execute`] のロック)。
//! - キャンセルは協調 ([`FileOpsService::cancel`] → AtomicBool)。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gento_core::file_ops::{
    execute_file_op, execute_mkdir, owned_tmp_paths_of, plan_file_op, undo_candidate_of,
    verify_undo, ConflictInfo, ConflictResolution, FileOpFile, FileOpKind, FileOpOutcome,
    FileOpPlan, FileOpProgress, FileOpsFs, FileOpsRepo, FileRootRef, FileStat, FsOpError,
    FsOpErrorKind, ResolutionSpec, StepStatus, UndoOutcome,
};
use gento_core::repo::{AssetRepo, FileRepo, LibraryRootRepo, RepoError};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

use crate::catalog::AppState;

/// 1操作あたりの実行排他 (同時多重実行の禁止)。
type RunningGuard = Mutex<Option<i64>>;

/// ファイル操作のUI通知境界 (実AppHandle / テスト: 記録)。
pub trait FileOpsHost: Send + Sync {
    /// `file_ops:progress` イベント。
    fn progress(&self, payload: &FileOpProgressDto);
    /// 操作完了 (操作ログ確定後のみ呼ばれる): カタログ再反映。
    fn operation_finished(&self);
    /// FG-WP7: 操作完了後のwatcher和解 (抑制中に破棄した外部変更イベント
    /// パスのscoped差分スキャン発火)。既定は何もしない (テストHost向け)。
    fn reconcile_after_operation(&self) {}
    /// watcher抑制の登録/解除。
    fn suppress(&self, paths: &[PathBuf]);
    fn unsuppress(&self, paths: &[PathBuf]);
}

/// AppHandle経由の実装。
pub struct TauriFileOpsHost {
    pub app: AppHandle,
}

impl FileOpsHost for TauriFileOpsHost {
    fn progress(&self, payload: &FileOpProgressDto) {
        let _ = self.app.emit("file_ops:progress", payload);
    }

    fn operation_finished(&self) {
        let Some(state) = self.app.try_state::<AppState>() else {
            return;
        };
        if let Some(controller) = &state.library {
            let snapshot = controller.build_snapshot().ok();
            if let Some(records) = snapshot {
                crate::catalog::apply_library_snapshot(&self.app, records);
            }
        }
    }

    fn reconcile_after_operation(&self) {
        let Some(state) = self.app.try_state::<AppState>() else {
            return;
        };
        if let Some(controller) = &state.library {
            controller.reconcile_suppressed();
        }
    }

    fn suppress(&self, paths: &[PathBuf]) {
        let Some(state) = self.app.try_state::<AppState>() else {
            return;
        };
        if let Some(controller) = &state.library {
            controller.suppress_paths(paths);
        }
    }

    fn unsuppress(&self, paths: &[PathBuf]) {
        let Some(state) = self.app.try_state::<AppState>() else {
            return;
        };
        if let Some(controller) = &state.library {
            controller.unsuppress_paths(paths);
        }
    }
}

/// 恒久削除・上書きのbackend確認ゲート用トークン (FG-WP7)。
///
/// - `file_ops_plan` が破壊的操作を含む計画に対して**単発用**の乱数nonceを
///   発行する (メモリ保持・5分失効・1回限り)。
/// - `file_ops_execute` は `kind: delete_forever` または上書き解決の実行に
///   **トークン一致を必須**化する (不一致/失効/再利用はErr)。
///   UIのチェックボックスだけに依存しない構造強制 (レビュー指摘対応)。
/// - FG-WP7-2 (操作バインド): トークンは発行時の**破壊的意図の正準
///   ダイジェスト**と対で保持される。executeは実際の要求から同じ方式で
///   ダイジェストを再計算し、一致 + トークン一致 + 未使用 + 未失効の
///   全てを要求する — 有効なトークンを**別の**破壊的要求へ流用することを
///   構造的に排除する。
#[derive(Debug)]
struct ConfirmTokenEntry {
    expires_at: std::time::Instant,
    /// plan時に確定した破壊的意図の正準ダイジェスト (`blake3:…`)。
    intent_digest: String,
}

/// トークンの失効までの時間。
const CONFIRM_TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// 確認トークンストア (メモリ。プロセス再起動で失効 = 再確認要求)。
/// WP-13のバックアップ復元 (backup.rs) も同一パターンで使用するため
/// crate内共有。
#[derive(Default)]
pub(crate) struct ConfirmTokenStore {
    tokens: Mutex<HashMap<String, ConfirmTokenEntry>>,
}

impl ConfirmTokenStore {
    /// 期限切れを刈り取りつつ新しいトークンを発行する (破壊的意図の
    /// ダイジェストに束ねる)。
    pub(crate) fn issue(&self, intent_digest: String) -> String {
        let token = Uuid::now_v7().to_string();
        // v7は時刻順序付きの乱数 (122bit randomness) — 予測困難性は
        // 秘密性ではなく「持っていること」の証明に十分。
        let mut guard = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
        let now = std::time::Instant::now();
        guard.retain(|_, entry| entry.expires_at > now);
        guard.insert(
            token.clone(),
            ConfirmTokenEntry {
                expires_at: now + CONFIRM_TOKEN_TTL,
                intent_digest,
            },
        );
        token
    }

    /// トークンを検証して**消費する** (1回限り)。要求の正準ダイジェストが
    /// 発行時と一致し、かつ未失効である場合のみtrue。いかなる使用試行も
    /// トークンを破棄する (不一致・失効・再利用の全てで焼却 = 再試行不可)。
    pub(crate) fn consume(&self, token: &str, intent_digest: &str) -> bool {
        let mut guard = self.tokens.lock().unwrap_or_else(|p| p.into_inner());
        match guard.remove(token) {
            Some(entry) => {
                entry.expires_at > std::time::Instant::now() && entry.intent_digest == intent_digest
            }
            None => false,
        }
    }
}

/// 破壊的意図の正準直列化 (FG-WP7-2)。フィールド順は固定:
/// kind / 対象file id (昇順) / 宛先 / 新名 / 上書き対象候補id (昇順)。
///
/// 各フィールドは「UTF-8バイト長:内容」の長さ前置で連結する — 区切り文字
/// を含むパス・名前での直列化衝突 (異なる要求→同一バイト列) を構造的に
/// 排除する。id列は10進数のカンマ結合のため区切りは曖昧を生まない。
fn canonical_intent_input(
    kind: FileOpKind,
    file_ids: &[i64],
    dest_dir: Option<&Path>,
    new_name: Option<&str>,
    conflict_file_ids: &[i64],
) -> String {
    let mut normalized_ids = file_ids.to_vec();
    normalized_ids.sort_unstable();
    let mut conflict_ids = conflict_file_ids.to_vec();
    conflict_ids.sort_unstable();
    let ids_csv = normalized_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let conflicts_csv = conflict_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let dest = dest_dir.map(|p| p.display().to_string());
    let name = new_name.map(str::to_string);
    // Noneと空文字を区別するため、Optionは "0:null" / "1:<内容>" のタグ付き。
    let parts: [String; 5] = [
        kind.as_str().to_string(),
        ids_csv,
        match dest {
            Some(d) => format!("1:{d}"),
            None => "0:null".to_string(),
        },
        match name {
            Some(n) => format!("1:{n}"),
            None => "0:null".to_string(),
        },
        conflicts_csv,
    ];
    let mut out = String::new();
    for part in parts {
        out.push_str(&format!("{}:{}", part.len(), part));
    }
    out
}

/// 破壊的意図の正準ダイジェスト (`blake3:<hex>`)。plan時とexecute時で
/// 同一入力から同一値が得られることが契約 (決定性は
/// `destructive_intent_digest_is_deterministic_and_unambiguous` で固定)。
fn destructive_intent_digest(
    kind: FileOpKind,
    file_ids: &[i64],
    dest_dir: Option<&Path>,
    new_name: Option<&str>,
    conflict_file_ids: &[i64],
) -> String {
    let canonical = canonical_intent_input(kind, file_ids, dest_dir, new_name, conflict_file_ids);
    format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex())
}

/// plan/executeの各時点で観測された内容から破壊的意図ダイジェストを
/// 組み立てる。`plan` の衝突ステップ (= 上書き対象候補) は再計算時点の
/// FS状態から導かれるため、plan→execute間の衝突集合変化はダイジェスト
/// 不一致として拒否される。
fn intent_digest_of(
    kind: FileOpKind,
    file_ids: &[i64],
    dest_dir: Option<&Path>,
    new_name: Option<&str>,
    plan: &FileOpPlan,
) -> String {
    let conflict_ids: Vec<i64> = plan
        .steps
        .iter()
        .filter(|s| s.conflict.is_some())
        .map(|s| s.file.file_id)
        .collect();
    destructive_intent_digest(kind, file_ids, dest_dir, new_name, &conflict_ids)
}

/// ファイル操作サービス (状態はAppStateではなくモジュールglobal的に
/// 保持せず、AppStateへmanageする)。
pub struct FileOpsService {
    db: Arc<gento_db::Db>,
    fs: Arc<dyn FileOpsFs>,
    host: Arc<dyn FileOpsHost>,
    cancel: Arc<AtomicBool>,
    running: RunningGuard,
    confirm_tokens: ConfirmTokenStore,
}

impl FileOpsService {
    pub fn new(db: Arc<gento_db::Db>, host: Arc<dyn FileOpsHost>) -> Arc<Self> {
        Self::with_fs(db, host, Arc::new(RealFileOps))
    }

    /// テスト用: FileOpsFsを注入する。
    pub fn with_fs(
        db: Arc<gento_db::Db>,
        host: Arc<dyn FileOpsHost>,
        fs: Arc<dyn FileOpsFs>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            fs,
            host,
            cancel: Arc::new(AtomicBool::new(false)),
            running: Mutex::new(None),
            confirm_tokens: ConfirmTokenStore::default(),
        })
    }

    fn roots(&self) -> Result<Vec<FileRootRef>, String> {
        self.db
            .with_reader(|repos| repos.list_roots())
            .map_err(|e| e.to_string())
            .map(|roots| {
                roots
                    .into_iter()
                    .map(|root| FileRootRef {
                        root_id: root.id,
                        root_path: root.path,
                    })
                    .collect()
            })
    }

    /// 操作対象パス列からDBのfiles行を解決する (カタログ項目はパスを持つ
    /// ため、file行idの代わりにパスを受ける。登録ルート内の一意キー)。
    fn resolve_files(&self, paths: &[PathBuf]) -> Result<Vec<FileOpFile>, String> {
        let roots = self.roots()?;
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let path = Path::new(path);
            let root_ref = roots
                .iter()
                .find(|root| path.starts_with(&root.root_path))
                .ok_or_else(|| {
                    format!(
                        "{} は登録ルート内にありません (ファイル操作の対象は登録済み画像のみです)",
                        path.display()
                    )
                })?;
            let rel = path
                .strip_prefix(&root_ref.root_path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .to_path_buf();
            let (record, content_hash) = self
                .db
                .with_reader(|repos| {
                    let record = repos
                        .find_by_path(root_ref.root_id, &rel)?
                        .ok_or(RepoError::NotFound)?;
                    let hash = match record.asset_id {
                        Some(asset_id) => repos.get_asset(asset_id)?.map(|a| a.content_hash),
                        None => None,
                    };
                    Ok((record, hash))
                })
                .map_err(|_| {
                    format!(
                        "{} がライブラリに見つかりません (再スキャンをお試しください)",
                        path.display()
                    )
                })?;
            out.push(FileOpFile {
                file_id: record.id,
                root_id: record.root_id,
                rel_path: record.rel_path.clone(),
                abs_path: root_ref.root_path.join(&record.rel_path),
                display_name: record.display_name.clone(),
                content_hash,
                asset_id: record.asset_id,
            });
        }
        Ok(out)
    }

    /// dry-run (計画)。`delete_forever` または上書き可能性のある衝突を
    /// 含む場合は、確認ゲート用の単発トークンを発行して応答へ含める
    /// (executeで一致が必須。UIは 計画→確認ダイアログ→トークン付き実行)。
    pub fn plan(
        &self,
        kind: FileOpKind,
        paths: &[PathBuf],
        dest_dir: Option<&Path>,
        new_name: Option<&str>,
    ) -> Result<FileOpPlanDto, String> {
        let files = self.resolve_files(paths)?;
        let roots = self.roots()?;
        // 破壊的意図ダイジェストの対象id (計画前の要求解決結果から)。
        let file_ids: Vec<i64> = files.iter().map(|f| f.file_id).collect();
        let plan = plan_file_op(
            &*self.fs,
            &self.cancel,
            kind,
            files,
            dest_dir,
            new_name,
            &roots,
        );
        let confirm_token = if kind == FileOpKind::DeleteForever
            || plan.steps.iter().any(|s| s.conflict.is_some())
        {
            // トークンは計画時の破壊的意図 (対象・宛先・上書き候補) の
            // 正準ダイジェストに束ねて発行する (FG-WP7-2)。
            let digest = intent_digest_of(kind, &file_ids, dest_dir, new_name, &plan);
            Some(self.confirm_tokens.issue(digest))
        } else {
            None
        };
        Ok(FileOpPlanDto::from_plan(&plan, confirm_token))
    }

    /// 実行。内部で計画を再計算する (クライアント由来の計画を信頼しない —
    /// 現在のFS状態に対して解決する)。衝突解決は `resolutions`。
    ///
    /// 確認ゲート (FG-WP7): `kind: delete_forever` または実行時計画で
    /// 上書き解決となるステップを含む場合、`confirm_token` が plan発行の
    /// 有効な単発トークンと一致しなければErr (不一致・失効・再利用もErr)。
    /// トークンは消費される (1回限り)。
    pub fn execute(
        &self,
        kind: FileOpKind,
        paths: &[PathBuf],
        dest_dir: Option<&Path>,
        new_name: Option<&str>,
        resolutions: ResolutionSpec,
        confirm_token: Option<&str>,
    ) -> Result<FileOpOutcome, String> {
        let _guard = self.lock_running()?;
        self.cancel.store(false, Ordering::SeqCst);
        let files = self.resolve_files(paths)?;
        let roots = self.roots()?;
        // 破壊的意図ダイジェストの対象id (再計画の解決結果から)。
        let file_ids: Vec<i64> = files.iter().map(|f| f.file_id).collect();
        let plan = plan_file_op(
            &*self.fs,
            &self.cancel,
            kind,
            files,
            dest_dir,
            new_name,
            &roots,
        );
        if !plan.plan_errors.is_empty() {
            return Err(plan
                .plan_errors
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join(" / "));
        }
        // 確認ゲート: 破壊的操作 (delete_forever / overwrite解決) には
        // planで発行した単発トークンの一致を必須とする。トークンは plan時の
        // 破壊的意図の正準ダイジェストに束ねられているため、対象・宛先・
        // 衝突集合 (上書き対象候補) がplanと異なる要求では一致しない
        // (FG-WP7-2: トークンの別要求への流用排除)。
        let needs_confirm = kind == FileOpKind::DeleteForever
            || plan.steps.iter().any(|s| {
                s.conflict.is_some()
                    && resolutions.of(s.file.file_id) == Some(ConflictResolution::Overwrite)
            });
        if needs_confirm {
            let digest = intent_digest_of(kind, &file_ids, dest_dir, new_name, &plan);
            match confirm_token {
                Some(token) if self.confirm_tokens.consume(token, &digest) => {}
                _ => {
                    return Err(
                        "破壊的操作の実行には確認が必要です (操作をやり直して確認ダイアログから実行してください)"
                            .to_string(),
                    );
                }
            }
        }
        // watcher抑制: 操作対象パス (元・先) のイベントを二重反映させない。
        let suppress_paths: Vec<PathBuf> = plan
            .steps
            .iter()
            .flat_map(|s| [s.src.clone(), s.dst.clone()])
            .collect();
        self.host.suppress(&suppress_paths);
        let host = Arc::clone(&self.host);
        let outcome = execute_file_op(
            &*self.db,
            &*self.fs,
            &plan,
            &resolutions,
            &roots,
            &self.cancel,
            &mut |progress: FileOpProgress| {
                host.progress(&FileOpProgressDto::from_progress(&progress));
            },
        )
        .map_err(|e| format!("操作の記録に失敗しました: {e}"));
        self.host.unsuppress(&suppress_paths);
        let outcome = outcome?;
        // 操作ログ確定後にのみカタログ反映 (安全原則5) + 抑制中に破棄した
        // 外部変更イベントの和解スキャン (FG-WP7)。
        self.host.operation_finished();
        self.host.reconcile_after_operation();
        Ok(outcome)
    }

    /// 新規フォルダ作成。
    pub fn mkdir(&self, parent: &Path, name: &str) -> Result<FileOpOutcome, String> {
        let _guard = self.lock_running()?;
        self.cancel.store(false, Ordering::SeqCst);
        let outcome = execute_mkdir(&*self.db, &*self.fs, parent, name)
            .map_err(|e| format!("操作の記録に失敗しました: {e}"))?;
        if outcome.status != gento_core::file_ops::FileOpStatus::Failed {
            self.host.operation_finished();
        }
        Ok(outcome)
    }

    /// Undo前提検証 (実行はしない)。
    pub fn undo_check(&self, op_id: i64) -> Result<UndoCheckDto, String> {
        let record = self
            .db
            .with_reader(|repos| {
                use gento_core::file_ops::FileOpsRepo;
                repos.get_operation(op_id)
            })
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("操作 (id={op_id}) が見つかりません"))?;
        let candidate = undo_candidate_of(&record);
        match verify_undo(&candidate, &*self.fs) {
            Ok(()) => Ok(UndoCheckDto {
                op_id: op_id.to_string(),
                ok: true,
                kind: candidate.kind.as_str().to_string(),
                refusals: Vec::new(),
            }),
            Err(refusals) => Ok(UndoCheckDto {
                op_id: op_id.to_string(),
                ok: false,
                kind: candidate.kind.as_str().to_string(),
                refusals: refusals.iter().map(|r| r.describe()).collect(),
            }),
        }
    }

    /// Undo実行 (前提検証→実行の2段の後半)。
    pub fn undo(&self, op_id: i64) -> Result<UndoOutcome, String> {
        let _guard = self.lock_running()?;
        self.cancel.store(false, Ordering::SeqCst);
        let record = self
            .db
            .with_reader(|repos| {
                use gento_core::file_ops::FileOpsRepo;
                repos.get_operation(op_id)
            })
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("操作 (id={op_id}) が見つかりません"))?;
        let candidate = undo_candidate_of(&record);
        let roots = self.roots()?;
        // 逆操作の対象パスを抑制 (undoもwatcher二重反映の対象)
        let paths: Vec<PathBuf> = candidate
            .actions
            .iter()
            .flat_map(|action| match action {
                gento_core::file_ops::UndoAction::MoveBack { from, to, .. } => {
                    [from.clone(), to.clone()]
                }
                gento_core::file_ops::UndoAction::RestoreFromTrash { original, .. } => {
                    [original.clone(), original.clone()]
                }
                gento_core::file_ops::UndoAction::DeleteCopy { path, .. } => {
                    [path.clone(), path.clone()]
                }
            })
            .collect();
        self.host.suppress(&paths);
        let outcome = gento_core::file_ops::execute_undo(
            &*self.db,
            &*self.fs,
            &candidate,
            &roots,
            &self.cancel,
        )
        .map_err(|e| format!("Undoの記録に失敗しました: {e}"));
        self.host.unsuppress(&paths);
        let outcome = outcome?;
        // undoも操作: 完了時のカタログ反映 + 抑制中破棄イベントの和解。
        if outcome.fully_undone {
            self.host.operation_finished();
        }
        self.host.reconcile_after_operation();
        Ok(outcome)
    }

    /// 操作履歴 (新着順)。
    pub fn history(&self, limit: u32) -> Result<Vec<FileOpHistoryDto>, String> {
        self.db
            .with_reader(|repos| {
                use gento_core::file_ops::FileOpsRepo;
                repos.list_recent_operations(limit)
            })
            .map_err(|e| e.to_string())
            .map(|records| records.iter().map(FileOpHistoryDto::from_record).collect())
    }

    /// 実行中操作のキャンセル (協調)。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// 実行排他の取得。二重実行はエラー (UIで進行中ダイアログを表示中のため)。
    fn lock_running(&self) -> Result<MutexGuard<'_, Option<i64>>, String> {
        let guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Err("別のファイル操作が実行中です".to_string());
        }
        Ok(guard)
    }

    /// 起動時の異常終了回復:
    /// (1) `executing` 残留操作を `failed` で確定 (FSが正本のため、再スキャンで
    ///     DBは最終状態へ収束する。「安全な再開」= 失敗項目のUI再試行)。
    /// (2) GEN-TO所有の一時ファイル (厳形式 `.{名前}.gento-tmp-{op_uuid}-{seq}`)
    ///     を掃除 (コピー途中の不完全実体)。削除は以下に限定する (FG-WP7:
    ///     外部作成の類似名ファイルを誤削除しない):
    ///     (a) file_operationsのrequest/result JSONに記録された所有tmpパス
    ///     (b) 厳密正規表現一致 (uuid形式検証含む) **かつ登録ルート配下**
    ///     それ以外の `.gento-tmp-` 類似ファイルは**残置**しログで通知する。
    /// 戻り値は検出した未完了操作の説明 (UIの案内表示用)。
    pub fn recover_at_startup(&self) -> Vec<String> {
        let mut notices = Vec::new();
        let interrupted = self
            .db
            .with_writer(|repos| {
                use gento_core::file_ops::{FileOpStatus, FileOpsRepo};
                let executing: Vec<gento_core::file_ops::FileOpRecord> =
                    repos.list_executing_operations()?;
                for record in &executing {
                    let note = "{\"interrupted\": true, \"recovery\": \"failed-at-startup\"}";
                    repos.update_operation(record.id, FileOpStatus::Failed, Some(note), false)?;
                }
                Ok(executing)
            })
            .unwrap_or_default();
        for record in &interrupted {
            notices.push(format!(
                "未完了の{}操作 (id={}) を中断しました。必要なら履歴から再試行できます",
                record.operation_type, record.id
            ));
        }
        // 所有tmp掃除 (所有権検証付き)
        if let Ok(roots) = self.roots() {
            let owned = self.collect_owned_tmp_paths(&interrupted);
            for root in roots {
                let (removed, skipped) = sweep_tmp_files(&root.root_path, &owned);
                if removed > 0 {
                    notices.push(format!(
                        "コピー中断のテンポラリを {} 件掃除しました ({})",
                        removed,
                        root.root_path.display()
                    ));
                }
                for name in skipped {
                    tracing::warn!(
                        "所有権を確認できない .gento-tmp- 類似ファイルを残置しました: {}",
                        root.root_path.join(&name).display()
                    );
                }
            }
        }
        notices
    }

    /// GEN-TOが所有する一時ファイルパスの集合。
    /// (a) 完了操作のresult JSONに記録された `steps[].tmp`
    /// (b) executing残留行から再構築 (request JSONの `files[].to` + 行uuid
    ///     + step index — tmp命名は決定論的)。
    fn collect_owned_tmp_paths(
        &self,
        executing: &[gento_core::file_ops::FileOpRecord],
    ) -> std::collections::HashSet<PathBuf> {
        let mut owned = std::collections::HashSet::new();
        let records = self
            .db
            .with_reader(|repos| repos.list_recent_operations(u32::MAX))
            .unwrap_or_default();
        for record in records.iter().chain(executing) {
            owned.extend(owned_tmp_paths_of(record));
        }
        owned
    }
}

/// ディレクトリ再帰でGEN-TO所有の一時ファイルのみを掃除する
/// (シンボリックリンクは辿らない)。戻り値: (削除件数, 残置した類似名)。
///
/// 削除基準 (いずれか):
/// - `owned` (request/result JSONに記録された所有パス) に一致
/// - 厳密形式 `.{名前}.gento-tmp-{32桁hex}-{6桁seq}` (uuid形式検証含む)
///   ※呼び出し側が登録ルート配下に対して呼ぶため「登録ルート配下」は保証済み
///
/// 旧形式 (`.gento-tmp-`接頭辞・uuid無し) や不正な形式のファイルは
/// **削除せず**名前を返す (呼び出し側がログ通知する)。
fn sweep_tmp_files(root: &Path, owned: &std::collections::HashSet<PathBuf>) -> (u64, Vec<String>) {
    let mut removed = 0u64;
    let mut skipped = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (0, skipped);
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let (sub_removed, sub_skipped) = sweep_tmp_files(&entry.path(), owned);
            removed += sub_removed;
            skipped.extend(sub_skipped);
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let is_gento_tmp_like = name.contains(".gento-tmp-");
        if !is_gento_tmp_like {
            continue;
        }
        let deletable =
            owned.contains(&entry.path()) || gento_core::file_ops::is_owned_tmp_name(&name);
        if deletable && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            continue;
        }
        if !deletable {
            skipped.push(name);
        }
    }
    (removed, skipped)
}

// ===== 実FS実装 =====

/// std fs + BLAKE3 (gento-image) + ゴミ箱 (gento-platform) の実FS実装。
pub struct RealFileOps;

/// io::ErrorをFsOpErrorへ分類する。
fn io_error(path: &Path, err: &std::io::Error) -> FsOpError {
    use std::io::ErrorKind;
    let kind = match err.kind() {
        ErrorKind::NotFound => FsOpErrorKind::NotFound,
        ErrorKind::PermissionDenied => FsOpErrorKind::PermissionDenied,
        ErrorKind::StorageFull => FsOpErrorKind::DiskFull,
        ErrorKind::NotADirectory => FsOpErrorKind::NotADirectory,
        ErrorKind::AlreadyExists => FsOpErrorKind::AlreadyExists,
        // 安定版stdのErrorKindにCrossesDevicesがあるかは版依存のため、
        // メッセージ照合で捕捉する (executeのフォールバック契約)。
        other => {
            if format!("{err:?}").contains("CrossesDevices") {
                FsOpErrorKind::Io
            } else {
                let _ = other;
                FsOpErrorKind::Io
            }
        }
    };
    let message = if format!("{err:?}").contains("CrossesDevices") {
        // フォールバック契約: メッセージに CrossesDevices を含める
        format!("{}: CrossesDevices: {err}", path.display())
    } else {
        format!("{}: {err}", path.display())
    };
    FsOpError { kind, message }
}

fn mtime_ns_of(md: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let secs: u64 = md.mtime().try_into().unwrap_or(u64::MAX);
        let nanos: u32 = md.mtime_nsec().try_into().unwrap_or(0);
        secs.saturating_mul(1_000_000_000)
            .saturating_add(nanos as u64)
    }
    #[cfg(not(unix))]
    {
        md.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

impl FileOpsFs for RealFileOps {
    fn stat(&self, path: &Path) -> Result<FileStat, FsOpError> {
        let md = std::fs::symlink_metadata(path).map_err(|e| io_error(path, &e))?;
        let mtime_ns = mtime_ns_of(&md);
        Ok(FileStat {
            byte_size: md.len(),
            mtime_ms: mtime_ns / 1_000_000,
            mtime_ns,
            os_file_id: gento_platform::os_file_id_of(&md),
        })
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn is_dir(&self, path: &Path) -> bool {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    }

    fn is_symlink(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }

    fn mkdir(&self, path: &Path) -> Result<(), FsOpError> {
        std::fs::create_dir(path).map_err(|e| io_error(path, &e))
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<(), FsOpError> {
        gento_platform::rename_noreplace(from, to).map_err(|e| match e {
            gento_platform::RenameNoReplaceError::DestinationExists => FsOpError::new(
                FsOpErrorKind::AlreadyExists,
                format!("{} が既に存在するため上書きしませんでした", to.display()),
            ),
            gento_platform::RenameNoReplaceError::Io(msg) => FsOpError::new(FsOpErrorKind::Io, msg),
        })
    }

    fn copy_file(
        &self,
        from: &Path,
        to: &Path,
        tmp: &Path,
        cancel: &AtomicBool,
    ) -> Result<(), FsOpError> {
        // 安全原則4: テンポラリ (厳密形式。呼び出し側が命名・所有権記録)
        // へ書き込み→fsync→NOREPLACEで最終名へ確定。
        // テンポラリは最終名と同じディレクトリ (renameの原子性)。
        let result = (|| -> Result<(), FsOpError> {
            let mut src = std::fs::File::open(from).map_err(|e| io_error(from, &e))?;
            let mut dst = std::fs::File::create(tmp).map_err(|e| io_error(tmp, &e))?;
            let mut buf = vec![0u8; 512 * 1024];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    let _ = std::fs::remove_file(tmp);
                    return Err(FsOpError {
                        kind: FsOpErrorKind::Cancelled,
                        message: "コピーをキャンセルしました".to_string(),
                    });
                }
                let read = src.read(&mut buf).map_err(|e| io_error(from, &e))?;
                if read == 0 {
                    break;
                }
                dst.write_all(&buf[..read]).map_err(|e| io_error(tmp, &e))?;
            }
            // 元のmtimeを保存 (コピーは内容・属性の継承。トークン比較の
            // 安定化にも寄与 — size+mtime一致で衝突UIの判断が正確になる)
            if let Ok(mtime) = src.metadata().and_then(|m| m.modified()) {
                let _ = dst.set_modified(mtime);
            }
            dst.sync_all().map_err(|e| io_error(tmp, &e))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                // 最終配置はNOREPLACE (上書きTOCTOU排除)。宛先が出現して
                // いた場合は失敗し、テンポラリを掃除する。
                self.rename_noreplace(tmp, to).inspect_err(|_| {
                    let _ = std::fs::remove_file(tmp);
                })
            }
            Err(err) => {
                let _ = std::fs::remove_file(tmp);
                Err(err)
            }
        }
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsOpError> {
        std::fs::remove_file(path).map_err(|e| io_error(path, &e))
    }

    fn trash(&self, path: &Path) -> Result<gento_core::file_ops::TrashEntry, FsOpError> {
        gento_platform::trash_to_bin(path).map_err(|msg| FsOpError::new(FsOpErrorKind::Io, msg))
    }

    fn restore_trash(
        &self,
        entry: &gento_core::file_ops::TrashEntry,
        original: &Path,
        expect_byte_size: u64,
    ) -> Result<PathBuf, FsOpError> {
        gento_platform::restore_from_trash(entry, original, expect_byte_size).map_err(|err| {
            let (kind, message) = match &err {
                gento_platform::RestoreError::NotFound(msg) => {
                    (FsOpErrorKind::NotFound, msg.clone())
                }
                gento_platform::RestoreError::DestinationExists(_) => {
                    (FsOpErrorKind::Precondition, err.to_string())
                }
                gento_platform::RestoreError::SizeMismatch { .. } => {
                    (FsOpErrorKind::Precondition, err.to_string())
                }
                gento_platform::RestoreError::Io(msg) => (FsOpErrorKind::Io, msg.clone()),
            };
            FsOpError::new(kind, message)
        })
    }

    fn trash_entry_size(&self, entry: &gento_core::file_ops::TrashEntry) -> Option<u64> {
        gento_platform::trash_entry_byte_size(entry)
    }

    fn free_bytes(&self, dir: &Path) -> Result<u64, FsOpError> {
        #[cfg(unix)]
        {
            let c_path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, e.to_string()))?;
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
            if rc != 0 {
                return Err(FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("{}: statvfs failed", dir.display()),
                ));
            }
            Ok((stat.f_bavail as u64).saturating_mul(stat.f_bsize as u64))
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
            Ok(u64::MAX)
        }
    }

    fn hash_file(&self, path: &Path, cancel: &AtomicBool) -> Result<String, FsOpError> {
        let token = gento_image::hash::stat_token(path)
            .ok_or_else(|| FsOpError::not_found(format!("{}: statに失敗", path.display())))?;
        gento_image::hash::hash_and_probe_file(path, &token, cancel)
            .map(|hashed| hashed.content.content_hash)
            .map_err(|e| FsOpError::new(FsOpErrorKind::Io, e.to_string()))
    }
}

// ===== DTO =====

/// 進捗イベント (`file_ops:progress`)。
#[derive(Debug, Clone, Serialize)]
pub struct FileOpProgressDto {
    pub op_id: String,
    pub index: usize,
    pub total: usize,
    pub from: String,
    pub to: Option<String>,
    pub done: bool,
}

impl FileOpProgressDto {
    fn from_progress(p: &FileOpProgress) -> Self {
        Self {
            op_id: p.op_id.to_string(),
            index: p.index,
            total: p.total,
            from: p.from.clone(),
            to: p.to.clone(),
            done: p.done,
        }
    }
}

/// 計画 (dry-run) の応答。
#[derive(Debug, Clone, Serialize)]
pub struct FileOpPlanDto {
    pub kind: String,
    pub steps: Vec<PlanStepDto>,
    pub plan_errors: Vec<String>,
    pub dest_root_id: Option<String>,
    pub disk_shortfall_bytes: Option<u64>,
    /// 破壊的操作 (delete_forever / 上書き解決の可能性) を含む場合に
    /// 発行される単発確認トークン (5分失効・1回限り)。execute時に必須。
    pub confirm_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStepDto {
    pub file_id: String,
    pub src: String,
    pub dst: String,
    pub display_name: String,
    pub conflict: Option<ConflictDto>,
    pub plan_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictDto {
    pub dst: String,
    pub dst_byte_size: u64,
    pub dst_mtime_ms: u64,
    pub hash_matches: Option<bool>,
    pub keep_both_name: String,
}

impl FileOpPlanDto {
    fn from_plan(plan: &FileOpPlan, confirm_token: Option<String>) -> Self {
        Self {
            kind: plan.kind.as_str().to_string(),
            confirm_token,
            steps: plan
                .steps
                .iter()
                .map(|s| PlanStepDto {
                    file_id: s.file.file_id.to_string(),
                    src: s.src.display().to_string(),
                    dst: s.dst.display().to_string(),
                    display_name: s.file.display_name.clone(),
                    conflict: s.conflict.as_ref().map(ConflictDto::from_conflict),
                    plan_error: s.plan_error.as_ref().map(|e| e.message.clone()),
                })
                .collect(),
            plan_errors: plan.plan_errors.iter().map(|e| e.message.clone()).collect(),
            dest_root_id: plan.dest_root_id.map(|id| id.to_string()),
            disk_shortfall_bytes: plan.disk_shortfall_bytes,
        }
    }
}

impl ConflictDto {
    fn from_conflict(c: &ConflictInfo) -> Self {
        Self {
            dst: c.dst.display().to_string(),
            dst_byte_size: c.dst_byte_size,
            dst_mtime_ms: c.dst_mtime_ms,
            hash_matches: c.hash_matches,
            keep_both_name: c.keep_both_name.clone(),
        }
    }
}

/// 実行結果 (部分成功レポート含む)。
#[derive(Debug, Clone, Serialize)]
pub struct FileOpOutcomeDto {
    pub op_id: String,
    pub kind: String,
    pub status: String,
    pub undoable: bool,
    pub steps: Vec<StepResultDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepResultDto {
    pub file_id: String,
    pub from: String,
    pub to: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
}

impl FileOpOutcomeDto {
    pub fn from_outcome(o: &FileOpOutcome) -> Self {
        Self {
            op_id: o.op_id.to_string(),
            kind: o.kind.as_str().to_string(),
            status: o.status.as_str().to_string(),
            undoable: o.undoable,
            steps: o
                .steps
                .iter()
                .map(|s| StepResultDto {
                    file_id: s.file_id.to_string(),
                    from: s.from.display().to_string(),
                    to: s.to.as_ref().map(|p| p.display().to_string()),
                    status: step_status_str(s.status).to_string(),
                    resolution: s.resolution.map(|r| r.as_str().to_string()),
                    error: s.error.as_ref().map(|e| e.message.clone()),
                    warning: s.warning.clone(),
                })
                .collect(),
        }
    }
}

fn step_status_str(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Done => "done",
        StepStatus::Skipped => "skipped",
        StepStatus::Failed => "failed",
        StepStatus::Cancelled => "cancelled",
    }
}

/// Undo前提検証の応答。
#[derive(Debug, Clone, Serialize)]
pub struct UndoCheckDto {
    pub op_id: String,
    pub ok: bool,
    pub kind: String,
    /// 拒否理由 (人が読める形式)
    pub refusals: Vec<String>,
}

/// 履歴項目。
#[derive(Debug, Clone, Serialize)]
pub struct FileOpHistoryDto {
    pub op_id: String,
    pub kind: String,
    pub status: String,
    pub undoable: bool,
    pub created_at: String,
    pub undone_at: Option<String>,
    pub request_json: String,
    pub result_json: Option<String>,
}

impl FileOpHistoryDto {
    fn from_record(r: &gento_core::file_ops::FileOpRecord) -> Self {
        Self {
            op_id: r.id.to_string(),
            kind: r.operation_type.as_str().to_string(),
            status: r.status.as_str().to_string(),
            undoable: r.undoable,
            created_at: r.created_at.clone(),
            undone_at: r.undone_at.clone(),
            request_json: r.request_json.clone(),
            result_json: r.result_json.clone(),
        }
    }
}

/// Undo実行の応答 (拒否時の理由・補償ロールバック結果を含む)。
#[derive(Debug, Clone, Serialize)]
pub struct UndoOutcomeDto {
    pub op_id: String,
    pub fully_undone: bool,
    /// "undone" | "partial_undone" | "refused"。
    pub status: String,
    pub errors: Vec<String>,
    pub refusals: Vec<String>,
    /// 補償ロールバックにも失敗した項目 (手動確認が必要)。
    pub rollback_errors: Vec<String>,
}

impl UndoOutcomeDto {
    fn from_outcome(o: &UndoOutcome) -> Self {
        let status = if o.fully_undone {
            "undone"
        } else if o.refusals.is_empty() {
            "partial_undone"
        } else {
            "refused"
        };
        Self {
            op_id: o.op_id.to_string(),
            fully_undone: o.fully_undone,
            status: status.to_string(),
            errors: o
                .steps
                .iter()
                .filter_map(|s| s.error.as_ref().map(|e| e.message.clone()))
                .collect(),
            refusals: o.refusals.iter().map(|r| r.describe()).collect(),
            rollback_errors: o.rollback_errors.clone(),
        }
    }
}

// ===== command引数DTO =====

/// 衝突解決の指定。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResolutionSpecDto {
    /// 全体方針 (「以降全部」)。
    pub global: Option<String>,
    /// ファイル個別 (file_id文字列 → 方針)。
    pub per_file: Option<HashMap<String, String>>,
}

impl ResolutionSpecDto {
    fn parse(&self) -> Result<ResolutionSpec, String> {
        let parse_one = |s: &str| match s {
            "overwrite" => Ok(gento_core::file_ops::ConflictResolution::Overwrite),
            "skip" => Ok(gento_core::file_ops::ConflictResolution::Skip),
            "keep_both" => Ok(gento_core::file_ops::ConflictResolution::KeepBoth),
            other => Err(format!("不明な衝突解決: {other}")),
        };
        let global = match &self.global {
            Some(s) => Some(parse_one(s)?),
            None => None,
        };
        let mut per_file = HashMap::new();
        for (path, resolution) in self.per_file.as_ref().unwrap_or(&HashMap::new()) {
            let id: i64 = path
                .parse()
                .map_err(|_| format!("ファイルID (パス) が不正: {path}"))?;
            per_file.insert(id, parse_one(resolution)?);
        }
        Ok(ResolutionSpec { global, per_file })
    }
}

/// 実行要求 (計画と同じ入力 + 解決方針 + 確認トークン)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileOpExecuteArgs {
    pub kind: String,
    pub paths: Vec<String>,
    pub dest_dir: Option<String>,
    pub new_name: Option<String>,
    pub resolutions: Option<ResolutionSpecDto>,
    /// 破壊的操作 (delete_forever / overwrite) に必須の単発確認トークン
    /// (plan応答に含まれる)。
    pub confirm_token: Option<String>,
}

fn parse_kind(s: &str) -> Result<FileOpKind, String> {
    use std::str::FromStr;
    FileOpKind::from_str(s).map_err(|_| format!("不明な操作種別: {s}"))
}

fn parse_paths(values: &[String]) -> Result<Vec<PathBuf>, String> {
    if values.is_empty() {
        return Err("対象ファイルが空です".to_string());
    }
    Ok(values.iter().map(PathBuf::from).collect())
}

fn service_of(state: &State<'_, AppState>) -> Result<Arc<FileOpsService>, String> {
    state
        .file_ops
        .clone()
        .ok_or_else(|| "ファイル操作はDBが無効なため利用できません".to_string())
}

// ===== Tauri commands =====

/// dry-run: 衝突検出 (同名・ハッシュ一致表示用情報) と実行計画を返す。
#[tauri::command]
pub fn file_ops_plan(
    state: State<'_, AppState>,
    kind: String,
    paths: Vec<String>,
    dest_dir: Option<String>,
    new_name: Option<String>,
) -> Result<FileOpPlanDto, String> {
    let service = service_of(&state)?;
    service.plan(
        parse_kind(&kind)?,
        &parse_paths(&paths)?,
        dest_dir.as_deref().map(Path::new),
        new_name.as_deref(),
    )
}

/// 計画を実行する (進捗は `file_ops:progress` イベント)。
/// async command: ブロッキングFS操作をUIスレッド外で実行する。
#[tauri::command]
pub async fn file_ops_execute(
    state: State<'_, AppState>,
    args: FileOpExecuteArgs,
) -> Result<FileOpOutcomeDto, String> {
    let service = service_of(&state)?;
    let resolutions = match &args.resolutions {
        Some(dto) => dto.parse()?,
        None => ResolutionSpec::default(),
    };
    let outcome = service.execute(
        parse_kind(&args.kind)?,
        &parse_paths(&args.paths)?,
        args.dest_dir.as_deref().map(Path::new),
        args.new_name.as_deref(),
        resolutions,
        args.confirm_token.as_deref(),
    )?;
    Ok(FileOpOutcomeDto::from_outcome(&outcome))
}

/// Undo前提検証 (拒否時は理由を返す。実行はしない)。
#[tauri::command]
pub fn file_ops_undo_check(
    state: State<'_, AppState>,
    op_id: String,
) -> Result<UndoCheckDto, String> {
    let service = service_of(&state)?;
    let id: i64 = op_id
        .parse()
        .map_err(|_| format!("操作IDが不正: {op_id}"))?;
    service.undo_check(id)
}

/// Undoを実行する (前提検証→確認→実行の2段の後半)。
#[tauri::command]
pub async fn file_ops_undo(
    state: State<'_, AppState>,
    op_id: String,
) -> Result<UndoOutcomeDto, String> {
    let service = service_of(&state)?;
    let id: i64 = op_id
        .parse()
        .map_err(|_| format!("操作IDが不正: {op_id}"))?;
    let outcome = service.undo(id)?;
    Ok(UndoOutcomeDto::from_outcome(&outcome))
}

/// 操作履歴 (新着順)。
#[tauri::command]
pub fn file_ops_history(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<FileOpHistoryDto>, String> {
    let service = service_of(&state)?;
    service.history(limit.unwrap_or(50).clamp(1, 200))
}

/// 実行中操作のキャンセル (協調キャンセル)。
#[tauri::command]
pub fn file_ops_cancel(state: State<'_, AppState>) -> Result<(), String> {
    let service = service_of(&state)?;
    service.cancel();
    Ok(())
}

/// 新規フォルダ作成。
#[tauri::command]
pub async fn folder_create(
    state: State<'_, AppState>,
    parent: String,
    name: String,
) -> Result<FileOpOutcomeDto, String> {
    let service = service_of(&state)?;
    let outcome = service.mkdir(Path::new(&parent), &name)?;
    Ok(FileOpOutcomeDto::from_outcome(&outcome))
}

/// 起動時回復の通知 (dev bridge・E2E検証用: 検出された未完了操作の説明)。
#[tauri::command]
pub fn file_ops_recovery_report(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let service = service_of(&state)?;
    Ok(service.recover_at_startup())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::file_ops::{
        ConflictResolution, FileOpStatus, FileOpsRepo, FsOpErrorKind, StepResult, TokenDto,
        TrashEntry, UndoAction,
    };
    use gento_core::repo::{FileUpsert, ScanState};
    use std::sync::Mutex as StdMutex;

    /// XDG_DATA_HOMEを書き換えるテストの直列化 (プロセス広域環境変数)。
    static TRASH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-fileops-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            if let Some(parent) = Path::new(rel).parent() {
                std::fs::create_dir_all(self.0.join(parent)).unwrap();
            }
            let p = self.0.join(rel);
            std::fs::write(&p, bytes).unwrap();
            p
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(&p).unwrap();
            p
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 記録ホスト (抑制呼び出し・進捗・完了を記録)。
    struct RecordingHost {
        suppressed: StdMutex<Vec<PathBuf>>,
        unsuppressed: StdMutex<Vec<PathBuf>>,
        finished: StdMutex<u32>,
        progress: StdMutex<Vec<FileOpProgressDto>>,
    }

    impl RecordingHost {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                suppressed: StdMutex::new(Vec::new()),
                unsuppressed: StdMutex::new(Vec::new()),
                finished: StdMutex::new(0),
                progress: StdMutex::new(Vec::new()),
            })
        }
    }

    impl FileOpsHost for RecordingHost {
        fn progress(&self, payload: &FileOpProgressDto) {
            self.progress.lock().unwrap().push(payload.clone());
        }
        fn operation_finished(&self) {
            *self.finished.lock().unwrap() += 1;
        }
        fn suppress(&self, paths: &[PathBuf]) {
            self.suppressed.lock().unwrap().extend_from_slice(paths);
        }
        fn unsuppress(&self, paths: &[PathBuf]) {
            self.unsuppressed.lock().unwrap().extend_from_slice(paths);
        }
    }

    /// 実DB + 注入FS (trashはディレクトリ退避) のサービス。
    fn service_with_db(
        tag: &str,
    ) -> (
        TestDir,
        Arc<gento_db::Db>,
        Arc<FileOpsService>,
        Arc<RecordingHost>,
        PathBuf,
    ) {
        let dir = TestDir::new(tag);
        let trash = dir.mkdir(".trash");
        let db = Arc::new(gento_db::Db::open(dir.0.join("db.sqlite3")).unwrap());
        let host = RecordingHost::new();
        let fs: Arc<dyn FileOpsFs> = Arc::new(StubFs {
            trash_dir: StdMutex::new(trash.clone()),
            trashed: StdMutex::new(HashMap::new()),
        });
        let service = FileOpsService::with_fs(Arc::clone(&db), host.clone(), fs);
        (dir, db, service, host, trash)
    }

    /// テスト用FS (std + trash退避)。
    struct StubFs {
        trash_dir: StdMutex<PathBuf>,
        trashed: StdMutex<HashMap<PathBuf, PathBuf>>,
    }

    impl FileOpsFs for StubFs {
        fn stat(&self, path: &Path) -> Result<FileStat, FsOpError> {
            let md = std::fs::symlink_metadata(path)
                .map_err(|e| FsOpError::not_found(format!("{}: {e}", path.display())))?;
            #[cfg(unix)]
            let ns = {
                use std::os::unix::fs::MetadataExt;
                (md.mtime() as u64)
                    .saturating_mul(1_000_000_000)
                    .saturating_add(md.mtime_nsec() as u64)
            };
            #[cfg(not(unix))]
            let ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            Ok(FileStat {
                byte_size: md.len(),
                mtime_ms: ns / 1_000_000,
                mtime_ns: ns,
                os_file_id: gento_platform::os_file_id_of(&md),
            })
        }
        fn exists(&self, path: &Path) -> bool {
            std::fs::symlink_metadata(path).is_ok()
        }
        fn is_dir(&self, path: &Path) -> bool {
            std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
        }
        fn is_symlink(&self, path: &Path) -> bool {
            std::fs::symlink_metadata(path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        }
        fn mkdir(&self, path: &Path) -> Result<(), FsOpError> {
            std::fs::create_dir(path).map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))
        }
        fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<(), FsOpError> {
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(FsOpError::new(
                    FsOpErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            std::fs::rename(from, to).map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))
        }
        fn copy_file(
            &self,
            from: &Path,
            to: &Path,
            tmp: &Path,
            _c: &AtomicBool,
        ) -> Result<(), FsOpError> {
            std::fs::copy(from, tmp)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))?;
            self.rename_noreplace(tmp, to)
        }
        fn remove_file(&self, path: &Path) -> Result<(), FsOpError> {
            std::fs::remove_file(path).map_err(|e| FsOpError::not_found(format!("{e}")))
        }
        fn trash(&self, path: &Path) -> Result<TrashEntry, FsOpError> {
            let trash_dir = self.trash_dir.lock().unwrap().clone();
            let base = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut file_name = base.clone();
            {
                let mut seq = 2;
                while trash_dir.join(&file_name).exists() {
                    file_name = format!("{base}.{seq}");
                    seq += 1;
                }
            }
            let dest = trash_dir.join(&file_name);
            std::fs::rename(path, &dest)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))?;
            self.trashed
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), dest);
            Ok(TrashEntry {
                trash_dir,
                file_name,
            })
        }
        fn restore_trash(
            &self,
            entry: &TrashEntry,
            original: &Path,
            expect_byte_size: u64,
        ) -> Result<PathBuf, FsOpError> {
            let trashed = entry.trash_dir.join(&entry.file_name);
            let size = std::fs::metadata(&trashed)
                .map_err(|e| FsOpError::not_found(format!("{e}")))?
                .len();
            if size != expect_byte_size {
                return Err(FsOpError::precondition(format!(
                    "サイズ不一致 (期待 {expect_byte_size}, 実際 {size})"
                )));
            }
            if std::fs::symlink_metadata(original).is_ok() {
                return Err(FsOpError::precondition(format!(
                    "復帰先が存在: {}",
                    original.display()
                )));
            }
            std::fs::rename(&trashed, original)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))?;
            self.trashed.lock().unwrap().remove(original);
            Ok(original.to_path_buf())
        }
        fn trash_entry_size(&self, entry: &TrashEntry) -> Option<u64> {
            std::fs::metadata(entry.trash_dir.join(&entry.file_name))
                .ok()
                .map(|m| m.len())
        }
        fn free_bytes(&self, _dir: &Path) -> Result<u64, FsOpError> {
            Ok(u64::MAX)
        }
        fn hash_file(&self, path: &Path, _c: &AtomicBool) -> Result<String, FsOpError> {
            let bytes = std::fs::read(path).map_err(|e| FsOpError::not_found(format!("{e}")))?;
            // 内容ベース (等価判定に使うため長さだけでは不十分)
            let mut hash: u64 = 0xcbf29ce484222325;
            for b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            Ok(format!("stubhash:{hash:016x}"))
        }
    }

    fn add_root_file(
        db: &gento_db::Db,
        root_id: i64,
        rel: &str,
        bytes: &[u8],
    ) -> gento_core::repo::FileRecord {
        let path = PathBuf::from(rel);
        db.with_writer(|repos| {
            repos.upsert_file(FileUpsert {
                root_id,
                rel_path: path.clone(),
                display_name: Path::new(rel)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                byte_size: bytes.len() as u64,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
        })
        .unwrap()
    }

    fn root_id_of(db: &gento_db::Db) -> i64 {
        db.with_reader(|repos| Ok(repos.list_roots().unwrap()[0].id))
            .unwrap()
    }

    #[test]
    fn service_copy_flow_with_real_db_and_suppression() {
        let (dir, db, service, host, _trash) = service_with_db("svc-copy");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        let src = dir.write("a.png", b"svc");
        add_root_file(&db, root.id, "a.png", b"svc");
        // 実ファイルのstatを行に反映 (トークン解決を実態化)
        let stat = service_stat(&dir.0.join("a.png"));
        db.with_writer(|repos| {
            repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("a.png"),
                display_name: "a.png".to_string(),
                byte_size: stat.byte_size,
                mtime_ms: stat.mtime_ms,
                mtime_ns: stat.mtime_ns,
                scan_state: ScanState::Present,
                os_file_id: stat.os_file_id,
                detach_asset: false,
            })
        })
        .unwrap();
        let dest = dir.mkdir("dest");

        // plan → 衝突なし
        let plan = service
            .plan(
                FileOpKind::Copy,
                std::slice::from_ref(&src),
                Some(&dest),
                None,
            )
            .unwrap();
        assert!(plan.plan_errors.is_empty());
        assert!(plan.steps[0].conflict.is_none());

        let outcome = service
            .execute(
                FileOpKind::Copy,
                std::slice::from_ref(&src),
                Some(&dest),
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(dest.join("a.png").exists());
        assert!(src.exists());
        // watcher抑制が実行の前後で呼ばれた (元・先パス)
        let suppressed = host.suppressed.lock().unwrap().clone();
        assert!(
            suppressed.contains(&dir.0.join("a.png")),
            "src抑制: {suppressed:?}"
        );
        assert!(
            suppressed.contains(&dest.join("a.png")),
            "dst抑制: {suppressed:?}"
        );
        assert_eq!(host.unsuppressed.lock().unwrap().len(), 2);
        // 操作完了通知 (操作ログ確定後)
        assert_eq!(*host.finished.lock().unwrap(), 1);
        // 進捗イベント: 1ファイル × 開始/完了
        assert_eq!(host.progress.lock().unwrap().len(), 2);
        // DB: 2行 (元 + コピー)
        let count = db
            .with_reader(|repos| {
                let (page, total) = repos.list_files_paged(root.id, 0, 100).unwrap();
                let _ = page;
                Ok(total)
            })
            .unwrap();
        assert_eq!(count, 2);
        let _ = root_id_of(&db);
    }

    fn service_stat(path: &Path) -> FileStat {
        real_file_fs_stat(path)
    }

    /// StubFs::hash_fileと同一規則の内容ハッシュ。
    fn stub_hash_of(bytes: &[u8]) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("stubhash:{hash:016x}")
    }

    /// テスト内でlstat相当を得る (RealFileOpsのstatを流用)。
    fn real_file_fs_stat(path: &Path) -> FileStat {
        RealFileOps.stat(path).unwrap()
    }

    #[test]
    fn service_undo_flow_with_check_phase() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-undo");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("u.png", b"undo");
        add_root_file(&db, root.id, "u.png", b"undo");
        let dest = dir.mkdir("dest");

        let outcome = service
            .execute(
                FileOpKind::Move,
                &[dir.0.join("u.png")],
                Some(&dest),
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(dest.join("u.png").exists());

        // undo_check: 前提OK
        let check = service.undo_check(outcome.op_id).unwrap();
        assert!(check.ok, "{:?}", check.refusals);
        // undo実行
        let undo = service.undo(outcome.op_id).unwrap();
        assert!(undo.fully_undone);
        assert!(dir.0.join("u.png").exists());
        // 操作行はundone
        let op = db
            .with_reader(|repos| Ok(repos.get_operation(outcome.op_id).unwrap()))
            .unwrap();
        let op = op.expect("操作行が存在");
        assert_eq!(op.status, FileOpStatus::Undone);
        // 再undoは拒否
        let check2 = service.undo_check(outcome.op_id).unwrap();
        assert!(!check2.ok);
        assert!(
            check2.refusals.iter().any(|r| r.contains("Undo")),
            "{:?}",
            check2.refusals
        );
    }

    #[test]
    fn service_undo_check_reports_external_modification() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-extmod");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("e.png", b"before");
        add_root_file(&db, root.id, "e.png", b"before");
        let dest = dir.mkdir("dest");
        let outcome = service
            .execute(
                FileOpKind::Move,
                &[dir.0.join("e.png")],
                Some(&dest),
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap();
        // 外部変更
        std::fs::write(dest.join("e.png"), b"externally changed").unwrap();
        let check = service.undo_check(outcome.op_id).unwrap();
        assert!(!check.ok);
        assert!(
            check.refusals.iter().any(|r| r.contains("外部で変更")),
            "{:?}",
            check.refusals
        );
        // undo実行も拒否 (ファイルは無傷)
        let undo = service.undo(outcome.op_id).unwrap();
        assert!(!undo.fully_undone);
        assert_eq!(
            std::fs::read(dest.join("e.png")).unwrap(),
            b"externally changed"
        );
    }

    #[test]
    fn recovery_finalizes_executing_ops_and_sweeps_only_owned_tmp() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-recovery");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        // executing残留行 (プロセス死亡のシミュレーション)
        db.with_writer(|repos| {
            repos.insert_operation(gento_core::file_ops::NewFileOp {
                operation_type: FileOpKind::Copy,
                status: FileOpStatus::Executing,
                undoable: false,
                request_json: "{}".to_string(),
            })
        })
        .unwrap();
        // 外部作成の .gento-tmp- 風ファイル (uuid形式なし) — 残置される
        dir.write(".gento-tmp-xxx-a.png", b"external lookalike");
        // GEN-TO所有の厳密形式tmp (uuid検証OK) — 掃除される
        dir.write(
            ".a.png.gento-tmp-0123456789abcdef0123456789abcdef-000001",
            b"ours",
        );
        dir.write("keep.png", b"keep");

        let _notices = service.recover_at_startup();
        // executing → failed
        let ops = db
            .with_reader(|repos| Ok(repos.list_executing_operations().unwrap()))
            .unwrap();
        assert!(ops.is_empty());
        let history = service.history(10).unwrap();
        assert_eq!(history[0].status, "failed");
        // 所有tmpは掃除・外部類似名は残置・正規ファイルは無傷
        assert!(
            !dir.0
                .join(".a.png.gento-tmp-0123456789abcdef0123456789abcdef-000001")
                .exists(),
            "所有tmpは掃除される"
        );
        assert!(
            dir.0.join(".gento-tmp-xxx-a.png").exists(),
            "外部作成の類似名は削除しない (ログ通知のみ)"
        );
        assert!(dir.0.join("keep.png").exists());
        let _ = root;
    }

    /// 完了操作のresult JSONに記録された所有tmpパスも掃除される
    /// (記録経由の所有権確認)。
    #[test]
    fn recovery_sweeps_tmp_recorded_in_result_json() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-recovery-owned");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        let _ = root;
        // コピーを実行して記録されたtmp…は完了時に残らないため、意図的に
        // result JSONにtmpパスを記録した行を仕込み、そのパスのファイルを置く
        let recorded_tmp = dir
            .0
            .join(".x.png.gento-tmp-0123456789abcdef0123456789abcdef-000002");
        std::fs::write(&recorded_tmp, b"crashed leftover").unwrap();
        let outcome_json = format!(
            r#"{{"op_id":1,"kind":"copy","status":"done","undoable":false,"steps":[{{"file_id":1,"from":"/r/x.png","to":"/r/x.png","status":"done","resolution":null,"error":null,"warning":null,"tmp":"{}","undo":[]}}]}}"#,
            recorded_tmp.display().to_string().replace('\\', "\\\\")
        );
        db.with_writer(|repos| {
            let record = repos
                .insert_operation(gento_core::file_ops::NewFileOp {
                    operation_type: FileOpKind::Copy,
                    status: FileOpStatus::Done,
                    undoable: false,
                    request_json: "{}".to_string(),
                })
                .unwrap();
            repos.update_operation(record.id, FileOpStatus::Done, Some(&outcome_json), false)
        })
        .unwrap();
        service.recover_at_startup();
        assert!(!recorded_tmp.exists(), "記録された所有tmpは掃除される");
    }

    #[test]
    fn service_rejects_concurrent_execution() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-lock");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("lock.png", b"l");
        add_root_file(&db, root.id, "lock.png", b"l");
        let dest = dir.mkdir("dest");
        // runningロックを手動で占有 (実行中をシミュレート)
        {
            let mut guard = service.running.lock().unwrap();
            *guard = Some(999);
        }
        let err = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("lock.png")],
                Some(&dest),
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap_err();
        assert!(err.contains("実行中"), "{err}");
    }

    #[test]
    fn service_conflict_flow_through_dto_resolution() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-conflict");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("c.png", b"new-content");
        let record = add_root_file(&db, root.id, "c.png", b"new-content");
        // Assetを生成してリンク (content_hash = stubhash:<len>)
        let asset = db
            .with_writer(|repos| {
                repos.create_or_get_asset(gento_core::repo::NewAsset {
                    content_hash: stub_hash_of(b"new-content").to_string(),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 11,
                    mime_type: None,
                    width: None,
                    height: None,
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })
            })
            .unwrap()
            .0;
        db.with_writer(|repos| gento_core::repo::AssetRepo::link_file(repos, record.id, asset.id))
            .unwrap();
        let dest = dir.mkdir("dest");
        std::fs::write(dest.join("c.png"), b"old-content").unwrap();

        let plan = service
            .plan(FileOpKind::Copy, &[dir.0.join("c.png")], Some(&dest), None)
            .unwrap();
        let conflict = plan.steps[0].conflict.as_ref().expect("衝突検出");
        assert_eq!(
            conflict.hash_matches,
            Some(false),
            "内容が異なるためhash不一致"
        );
        assert_eq!(conflict.keep_both_name, "c (2).png");

        // keep_bothで実行
        let outcome = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("c.png")],
                Some(&dest),
                None,
                ResolutionSpec {
                    global: Some(ConflictResolution::KeepBoth),
                    per_file: HashMap::new(),
                },
                None,
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert_eq!(
            std::fs::read(dest.join("c.png")).unwrap(),
            b"old-content",
            "既存は無傷"
        );
        assert_eq!(
            std::fs::read(dest.join("c (2).png")).unwrap(),
            b"new-content"
        );
    }

    #[test]
    fn mkdir_command_flow() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-mkdir");
        let _ = db;
        let outcome = service.mkdir(&dir.0, "新規フォルダ").unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(dir.0.join("新規フォルダ").is_dir());
        let dup = service.mkdir(&dir.0, "新規フォルダ").unwrap();
        assert_eq!(dup.status, FileOpStatus::Failed);
    }

    #[test]
    fn history_lists_recent_with_dto_fields() {
        let (dir, db, service, _host, _trash) = service_with_db("svc-history");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("h.png", b"h");
        add_root_file(&db, root.id, "h.png", b"h");
        service
            .execute(
                FileOpKind::Trash,
                &[dir.0.join("h.png")],
                None,
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap();
        let history = service.history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].kind, "trash");
        assert_eq!(history[0].status, "done");
        assert!(history[0].undoable);
    }

    // ===== RealFileFsの実FS挙動 =====

    #[test]
    fn real_file_ops_copy_preserves_mtime_and_uses_strict_tmp_name() {
        let dir = TestDir::new("real-copy");
        let src = dir.write("r.png", b"real-copy-content");
        let dst = dir.0.join("out.png");
        let original_mtime = std::fs::metadata(&src).unwrap().modified().unwrap();
        let tmp =
            gento_core::file_ops::tmp_path_for(&dst, "01234567-89ab-cdef-0123-456789abcdef", 0);
        RealFileOps
            .copy_file(&src, &dst, &tmp, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"real-copy-content");
        assert_eq!(
            std::fs::metadata(&dst).unwrap().modified().unwrap(),
            original_mtime,
            "コピーはmtimeを保存する"
        );
        // テンポラリは残らない
        for entry in std::fs::read_dir(&dir.0).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.contains(".gento-tmp-"), "tmp残留: {name}");
        }
    }

    /// 最終配置はNOREPLACE: 宛先が既に存在する場合は上書きせず失敗。
    #[test]
    fn real_file_ops_copy_refuses_existing_destination() {
        let dir = TestDir::new("real-copy-exists");
        let src = dir.write("r.png", b"new");
        let dst = dir.0.join("out.png");
        std::fs::write(&dst, b"existing").unwrap();
        let tmp =
            gento_core::file_ops::tmp_path_for(&dst, "01234567-89ab-cdef-0123-456789abcdef", 0);
        let err = RealFileOps
            .copy_file(&src, &dst, &tmp, &AtomicBool::new(false))
            .unwrap_err();
        assert!(matches!(err.kind, FsOpErrorKind::AlreadyExists), "{err}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"existing", "上書きされない");
        assert!(!tmp.exists(), "テンポラリは掃除される");
    }

    #[test]
    fn real_file_ops_cancel_mid_copy_removes_tmp() {
        let dir = TestDir::new("real-cancel");
        let src = dir.write("big.png", &[7u8; 4 * 1024 * 1024]);
        let dst = dir.0.join("cancelled.png");
        let cancel = AtomicBool::new(true); // 最初のチャンク読み出し前に中断
        let tmp =
            gento_core::file_ops::tmp_path_for(&dst, "01234567-89ab-cdef-0123-456789abcdef", 0);
        let err = RealFileOps
            .copy_file(&src, &dst, &tmp, &cancel)
            .unwrap_err();
        assert!(matches!(err.kind, FsOpErrorKind::Cancelled), "{err}");
        assert!(!dst.exists());
        for entry in std::fs::read_dir(&dir.0).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.contains(".gento-tmp-"), "キャンセルでtmp掃除: {name}");
        }
    }

    #[test]
    fn real_file_ops_hash_matches_blake3_format() {
        let dir = TestDir::new("real-hash");
        let src = dir.write("x.png", b"hash-me");
        let hash = RealFileOps
            .hash_file(&src, &AtomicBool::new(false))
            .unwrap();
        assert!(hash.starts_with("blake3:"), "{hash}");
        assert_eq!(hash.len(), "blake3:".len() + 64);
    }

    #[test]
    fn real_file_ops_stat_uses_lstat_semantics() {
        let dir = TestDir::new("real-stat");
        let file = dir.write("f.png", b"12345");
        let stat = RealFileOps.stat(&file).unwrap();
        assert_eq!(stat.byte_size, 5);
        assert!(stat.os_file_id.is_some() || !cfg!(unix));
        #[cfg(unix)]
        {
            let link = dir.0.join("link.png");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            let link_stat = RealFileOps.stat(&link).unwrap();
            assert_ne!(
                link_stat.byte_size, stat.byte_size,
                "lstat相当: リンク自身のsize"
            );
            assert!(RealFileOps.is_symlink(&link));
        }
        let missing = RealFileOps.stat(&dir.0.join("missing.png")).unwrap_err();
        assert!(matches!(missing.kind, FsOpErrorKind::NotFound));
    }

    #[test]
    fn real_file_ops_free_bytes_returns_positive() {
        let dir = TestDir::new("real-free");
        let free = RealFileOps.free_bytes(&dir.0).unwrap();
        assert!(free > 0, "statvfs: {free}");
    }

    #[test]
    fn real_file_ops_trash_round_trip_with_exact_entry() {
        // XDG_DATA_HOMEを一意に向けて実ゴミ箱と復帰を検証 (環境ロック直列化)
        let _env = TRASH_ENV_LOCK.lock().unwrap();
        let dir = TestDir::new("real-trash");
        let file = dir.write("t.png", b"trash-roundtrip");
        std::env::set_var("XDG_DATA_HOME", dir.0.join("xdg/data"));
        let entry = RealFileOps.trash(&file).unwrap();
        assert!(!file.exists());
        assert_eq!(RealFileOps.trash_entry_size(&entry), Some(15));
        let restored = RealFileOps.restore_trash(&entry, &file, 15).unwrap();
        assert_eq!(restored, file);
        assert_eq!(std::fs::read(&file).unwrap(), b"trash-roundtrip");
    }

    #[test]
    fn real_file_ops_trash_missing_is_error_without_fallback() {
        let _env = TRASH_ENV_LOCK.lock().unwrap();
        let dir = TestDir::new("real-trashfail");
        std::env::set_var("XDG_DATA_HOME", dir.0.join("xdg/data"));
        let ghost = dir.0.join("ghost.png");
        let err = RealFileOps.trash(&ghost).unwrap_err();
        assert!(err.message.contains("not found"), "{err}");
        // 通常削除へのフォールバック無し: 対象は存在しないままであること自体が証明
        assert!(!ghost.exists());
    }

    #[test]
    fn dto_resolution_spec_parses_and_rejects_unknown() {
        let dto = ResolutionSpecDto {
            global: Some("keep_both".to_string()),
            per_file: Some(HashMap::from([("7".to_string(), "overwrite".to_string())])),
        };
        let spec = dto.parse().unwrap();
        assert_eq!(
            spec.global,
            Some(gento_core::file_ops::ConflictResolution::KeepBoth)
        );
        assert_eq!(spec.per_file.get(&7), Some(&ConflictResolution::Overwrite));
        let bad = ResolutionSpecDto {
            global: Some("explode".to_string()),
            per_file: None,
        };
        assert!(bad.parse().is_err());
    }

    #[test]
    fn execute_args_parse_paths_and_kind() {
        assert!(parse_paths(&["/a/b.png".to_string()]).is_ok());
        assert!(parse_paths(&[]).is_err(), "空は拒否");
        assert_eq!(
            parse_kind("delete_forever").unwrap(),
            FileOpKind::DeleteForever
        );
        assert!(parse_kind("rm").is_err());
    }

    #[test]
    fn undo_candidate_from_result_json_round_trip() {
        // result_jsonからUndoActionが復元できる (再起動後のundo候補)
        let outcome = FileOpOutcome {
            op_id: 1,
            kind: FileOpKind::Rename,
            status: FileOpStatus::Done,
            undoable: true,
            steps: vec![StepResult {
                file_id: 5,
                from: PathBuf::from("/r/old.png"),
                to: Some(PathBuf::from("/r/new.png")),
                status: StepStatus::Done,
                resolution: None,
                error: None,
                warning: None,
                tmp: None,
                undo: vec![UndoAction::MoveBack {
                    from: PathBuf::from("/r/new.png"),
                    to: PathBuf::from("/r/old.png"),
                    expect: TokenDto {
                        os_file_id: None,
                        byte_size: 0,
                        mtime_ns: 0,
                    },
                }],
            }],
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let record = gento_core::file_ops::FileOpRecord {
            id: 1,
            uuid: "u".to_string(),
            operation_type: FileOpKind::Rename,
            status: FileOpStatus::Done,
            undoable: true,
            request_json: "{}".to_string(),
            result_json: Some(json),
            created_at: "t".to_string(),
            undone_at: None,
        };
        let candidate = undo_candidate_of(&record);
        assert_eq!(candidate.actions.len(), 1);
        assert!(matches!(candidate.actions[0], UndoAction::MoveBack { .. }));
    }

    // ===== FG-WP7: 確認トークンゲート (恒久削除・上書きのbackend強制) =====

    /// delete_foreverはトークン無しでは実行拒否。plan→トークン→実行のみ通る。
    #[test]
    fn confirm_token_gate_delete_forever() {
        let (dir, db, service, _host, _trash) = service_with_db("token-df");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("f.png", b"forever");
        add_root_file(&db, root.id, "f.png", b"forever");

        // トークン無し → 拒否
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                None,
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "{err}");
        assert!(dir.0.join("f.png").exists(), "ファイルは無傷");

        // 不正トークン → 拒否
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some("00000000-0000-0000-0000-000000000000"),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "{err}");

        // plan → トークン発行 → 実行成功 → ファイル消滅
        let plan = service
            .plan(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
            )
            .unwrap();
        let token = plan.confirm_token.expect("delete_foreverはトークン発行");
        let outcome = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token),
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!dir.0.join("f.png").exists());

        // 再利用 → 拒否 (1回限り)
        dir.write("g.png", b"forever2");
        add_root_file(&db, root.id, "g.png", b"forever2");
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("g.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "再利用拒否: {err}");
        assert!(dir.0.join("g.png").exists(), "g.pngは無傷");
    }

    /// 失効したトークンは拒否される (5分TTL)。
    #[test]
    fn confirm_token_expires_after_ttl() {
        let (dir, db, service, _host, _trash) = service_with_db("token-exp");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        let _ = root;
        dir.write("f.png", b"x");
        add_root_file(&db, root.id, "f.png", b"x");
        let plan = service
            .plan(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
            )
            .unwrap();
        let token = plan.confirm_token.unwrap();
        // 失効を強制的に再現 (期限を過去へ)
        {
            let mut guard = service.confirm_tokens.tokens.lock().unwrap();
            if let Some(entry) = guard.get_mut(&token) {
                entry.expires_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
            }
        }
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "{err}");
        assert!(dir.0.join("f.png").exists());
    }

    /// 上書き解決にもトークンが必須。keep_both/skipはトークン不要。
    #[test]
    fn confirm_token_gate_overwrite_resolution() {
        let (dir, db, service, _host, _trash) = service_with_db("token-ow");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("c.png", b"new-content");
        add_root_file(&db, root.id, "c.png", b"new-content");
        let dest = dir.mkdir("dest");
        std::fs::write(dest.join("c.png"), b"old-content").unwrap();
        let overwrite = ResolutionSpec {
            global: Some(ConflictResolution::Overwrite),
            per_file: HashMap::new(),
        };

        // トークン無しの上書き → 拒否 (旧ファイル無傷)
        let err = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("c.png")],
                Some(&dest),
                None,
                overwrite.clone(),
                None,
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "{err}");
        assert_eq!(
            std::fs::read(dest.join("c.png")).unwrap(),
            b"old-content",
            "旧ファイルは無傷"
        );

        // plan (衝突あり) → トークン → 上書き実行
        let plan = service
            .plan(FileOpKind::Copy, &[dir.0.join("c.png")], Some(&dest), None)
            .unwrap();
        assert!(plan.steps[0].conflict.is_some());
        let token = plan.confirm_token.expect("衝突ありplanはトークン発行");
        let outcome = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("c.png")],
                Some(&dest),
                None,
                overwrite,
                Some(&token),
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert_eq!(std::fs::read(dest.join("c.png")).unwrap(), b"new-content");

        // keep_bothはトークン無しで実行可能 (破壊的でない)
        dir.write("k.png", b"kb");
        add_root_file(&db, root.id, "k.png", b"kb");
        std::fs::write(dest.join("k.png"), b"existing").unwrap();
        let outcome = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("k.png")],
                Some(&dest),
                None,
                ResolutionSpec {
                    global: Some(ConflictResolution::KeepBoth),
                    per_file: HashMap::new(),
                },
                None,
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(dest.join("k (2).png").exists());
    }

    /// 衝突の無いplanはトークンを発行しない (通常操作のUXを損なわない)。
    #[test]
    fn confirm_token_not_issued_for_non_destructive_plan() {
        let (dir, db, service, _host, _trash) = service_with_db("token-none");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("n.png", b"n");
        add_root_file(&db, root.id, "n.png", b"n");
        let dest = dir.mkdir("dest");
        let plan = service
            .plan(FileOpKind::Copy, &[dir.0.join("n.png")], Some(&dest), None)
            .unwrap();
        assert!(plan.confirm_token.is_none(), "衝突なしcopyはトークン不要");
        // trashもトークン不要
        let plan2 = service
            .plan(FileOpKind::Trash, &[dir.0.join("n.png")], None, None)
            .unwrap();
        assert!(plan2.confirm_token.is_none());
    }

    // ===== FG-WP7-2: 確認トークンの操作バインド (正準ダイジェスト) =====

    /// トークンは発行元の要求に束ねられている: トークンAで要求B (対象違い)
    /// は拒否され、不一致使用でトークンは失効する。再計画の新トークンでの
    /// 正しい要求は成功 + 消費される。
    #[test]
    fn confirm_token_binds_to_planned_targets() {
        let (dir, db, service, _host, _trash) = service_with_db("token-bind");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("f.png", b"target-f");
        add_root_file(&db, root.id, "f.png", b"target-f");
        dir.write("g.png", b"target-g");
        add_root_file(&db, root.id, "g.png", b"target-g");

        // f.pngの計画で発行したトークン
        let plan = service
            .plan(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
            )
            .unwrap();
        let token = plan.confirm_token.expect("delete_foreverはトークン発行");

        // トークンAで要求B (対象違い) → 拒否。g.pngは無傷。
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("g.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "対象違い拒否: {err}");
        assert_eq!(std::fs::read(dir.0.join("g.png")).unwrap(), b"target-g");

        // 不一致使用でトークンは失効済み — 元の正しい要求でも拒否
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "失効トークン拒否: {err}");
        assert!(dir.0.join("f.png").exists());

        // 再計画 → 新トークン → 正しい要求は成功 + 消費
        let plan2 = service
            .plan(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
            )
            .unwrap();
        let token2 = plan2.confirm_token.expect("再発行");
        assert_ne!(token, token2);
        let outcome = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("f.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token2),
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!dir.0.join("f.png").exists());

        // 消費済みトークンの再利用 → 拒否 (g.pngがまだあるため要求として有効)
        let err = service
            .execute(
                FileOpKind::DeleteForever,
                &[dir.0.join("g.png")],
                None,
                None,
                ResolutionSpec::default(),
                Some(&token2),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "再利用拒否: {err}");
        assert!(dir.0.join("g.png").exists(), "g.pngは無傷");
    }

    /// 上書き要求のトークンは宛先に束ねられている: 別宛先の上書きには
    /// 使えない (不正流用で第三者のファイルを破壊させない)。
    #[test]
    fn confirm_token_binds_destination_of_overwrite() {
        let (dir, db, service, _host, _trash) = service_with_db("token-dest");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("c.png", b"new-content");
        add_root_file(&db, root.id, "c.png", b"new-content");
        let dest1 = dir.mkdir("dest1");
        let dest2 = dir.mkdir("dest2");
        std::fs::write(dest1.join("c.png"), b"old-1").unwrap();
        std::fs::write(dest2.join("c.png"), b"old-2").unwrap();
        let overwrite = ResolutionSpec {
            global: Some(ConflictResolution::Overwrite),
            per_file: HashMap::new(),
        };

        let plan = service
            .plan(FileOpKind::Copy, &[dir.0.join("c.png")], Some(&dest1), None)
            .unwrap();
        let token = plan.confirm_token.expect("衝突ありplanはトークン発行");

        // dest1へ向けたトークンでdest2の上書き → 拒否。old-2は無傷。
        let err = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("c.png")],
                Some(&dest2),
                None,
                overwrite.clone(),
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "宛先違い拒否: {err}");
        assert_eq!(std::fs::read(dest2.join("c.png")).unwrap(), b"old-2");
        assert_eq!(
            std::fs::read(dest1.join("c.png")).unwrap(),
            b"old-1",
            "本来の宛先も未実行"
        );

        // 再計画のトークンで正しい要求 → 成功
        let plan2 = service
            .plan(FileOpKind::Copy, &[dir.0.join("c.png")], Some(&dest1), None)
            .unwrap();
        let token2 = plan2.confirm_token.expect("再発行");
        let outcome = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("c.png")],
                Some(&dest1),
                None,
                overwrite,
                Some(&token2),
            )
            .unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert_eq!(std::fs::read(dest1.join("c.png")).unwrap(), b"new-content");
    }

    /// plan→execute間に外部が新たな衝突を作った場合 (衝突集合の変化)、
    /// トークンのダイジェストは不一致となり拒否される — 確認ダイアログに
    /// 一度も表示されなかったファイルの上書きを防ぐ。
    #[test]
    fn confirm_token_rejects_when_conflict_set_changed_after_plan() {
        let (dir, db, service, _host, _trash) = service_with_db("token-race");
        let root = db.with_writer(|repos| repos.add_root(&dir.0)).unwrap();
        dir.write("a1.png", b"one");
        add_root_file(&db, root.id, "a1.png", b"one");
        dir.write("a2.png", b"two");
        add_root_file(&db, root.id, "a2.png", b"two");
        let dest = dir.mkdir("dest");
        // 計画時点では a1 のみ衝突
        std::fs::write(dest.join("a1.png"), b"old-1").unwrap();
        let overwrite = ResolutionSpec {
            global: Some(ConflictResolution::Overwrite),
            per_file: HashMap::new(),
        };
        let plan = service
            .plan(
                FileOpKind::Copy,
                &[dir.0.join("a1.png"), dir.0.join("a2.png")],
                Some(&dest),
                None,
            )
            .unwrap();
        assert!(plan.steps[0].conflict.is_some());
        assert!(plan.steps[1].conflict.is_none());
        let token = plan.confirm_token.expect("衝突ありplanはトークン発行");

        // plan後に外部が a2 の宛先へファイルを作成 (race)
        std::fs::write(dest.join("a2.png"), b"sneaked").unwrap();

        let err = service
            .execute(
                FileOpKind::Copy,
                &[dir.0.join("a1.png"), dir.0.join("a2.png")],
                Some(&dest),
                None,
                overwrite,
                Some(&token),
            )
            .unwrap_err();
        assert!(err.contains("確認が必要"), "衝突集合変化は拒否: {err}");
        // 確認されていなかった a2 宛先は上書きされない。a1 も未実行。
        assert_eq!(std::fs::read(dest.join("a2.png")).unwrap(), b"sneaked");
        assert_eq!(std::fs::read(dest.join("a1.png")).unwrap(), b"old-1");
        assert!(dir.0.join("a1.png").exists() && dir.0.join("a2.png").exists());
    }

    /// 正準ダイジェストの決定性 (フィールド順・区切り固定・順序正規化) と
    /// 非曖昧性 (区切り文字を含む値の直列化衝突がないこと) の固定。
    #[test]
    fn destructive_intent_digest_is_deterministic_and_unambiguous() {
        let digest = |ids: &[i64], dest: &str, name: &str, conflicts: &[i64]| {
            destructive_intent_digest(
                FileOpKind::Copy,
                ids,
                Some(Path::new(dest)),
                Some(name),
                conflicts,
            )
        };
        // 形式: blake3:<64hex> (hash_fileと同じ表示規約)
        let d = digest(&[1, 2], "/dst", "n.png", &[2]);
        assert!(d.starts_with("blake3:"), "{d}");
        assert_eq!(d.len(), "blake3:".len() + 64);

        // 決定的: 同一入力は常に同一
        assert_eq!(digest(&[1, 2], "/dst", "n.png", &[2]), d);
        // 対象idは順序非依存 (正規化)
        assert_eq!(digest(&[2, 1], "/dst", "n.png", &[2]), d);
        // 衝突候補も順序非依存 (同一集合の並べ替えは一致)
        assert_eq!(
            digest(&[1, 2], "/dst", "n.png", &[1, 2]),
            digest(&[1, 2], "/dst", "n.png", &[2, 1])
        );

        // どのフィールドが変わっても別ダイジェスト
        assert_ne!(digest(&[1, 3], "/dst", "n.png", &[2]), d, "対象id違い");
        assert_ne!(digest(&[1, 2], "/other", "n.png", &[2]), d, "宛先違い");
        assert_ne!(digest(&[1, 2], "/dst", "m.png", &[2]), d, "新名違い");
        assert_ne!(
            digest(&[1, 2], "/dst", "n.png", &[1, 2]),
            d,
            "上書き対象違い"
        );
        assert_ne!(
            destructive_intent_digest(
                FileOpKind::Move,
                &[1, 2],
                Some(Path::new("/dst")),
                Some("n.png"),
                &[2],
            ),
            d,
            "kind違い"
        );
        // None と空文字の混同なし (宛先なしdelete_forever等)
        assert_ne!(
            destructive_intent_digest(FileOpKind::Copy, &[1, 2], None, None, &[2],),
            digest(&[1, 2], "", "", &[2]),
            "Noneと空文字は区別"
        );

        // 非曖昧性: 区切り文字を含む値の組み替えで同一直列化にならない
        // (長さ前置のため「a|b」+「c」と「a」+「b|c」は別物)
        assert_ne!(
            digest(&[1], "/a|b", "c", &[]),
            digest(&[1], "/a", "b|c", &[]),
            "フィールド境界の曖昧一致なし"
        );
    }
}
