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
    execute_file_op, execute_mkdir, plan_file_op, undo_candidate_of, verify_undo, ConflictInfo,
    FileOpFile, FileOpKind, FileOpOutcome, FileOpPlan, FileOpProgress, FileOpsFs, FileRootRef,
    FileStat, FsOpError, FsOpErrorKind, ResolutionSpec, StepStatus, UndoOutcome, TMP_FILE_PREFIX,
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

/// ファイル操作サービス (状態はAppStateではなくモジュールglobal的に
/// 保持せず、AppStateへmanageする)。
pub struct FileOpsService {
    db: Arc<gento_db::Db>,
    fs: Arc<dyn FileOpsFs>,
    host: Arc<dyn FileOpsHost>,
    cancel: Arc<AtomicBool>,
    running: RunningGuard,
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

    /// dry-run (計画)。
    pub fn plan(
        &self,
        kind: FileOpKind,
        paths: &[PathBuf],
        dest_dir: Option<&Path>,
        new_name: Option<&str>,
    ) -> Result<FileOpPlanDto, String> {
        let files = self.resolve_files(paths)?;
        let roots = self.roots()?;
        let plan = plan_file_op(
            &*self.fs,
            &self.cancel,
            kind,
            files,
            dest_dir,
            new_name,
            &roots,
        );
        Ok(FileOpPlanDto::from_plan(&plan))
    }

    /// 実行。内部で計画を再計算する (クライアント由来の計画を信頼しない —
    /// 現在のFS状態に対して解決する)。衝突解決は `resolutions`。
    pub fn execute(
        &self,
        kind: FileOpKind,
        paths: &[PathBuf],
        dest_dir: Option<&Path>,
        new_name: Option<&str>,
        resolutions: ResolutionSpec,
    ) -> Result<FileOpOutcome, String> {
        let _guard = self.lock_running()?;
        self.cancel.store(false, Ordering::SeqCst);
        let files = self.resolve_files(paths)?;
        let roots = self.roots()?;
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
        // 操作ログ確定後にのみカタログ反映 (安全原則5)
        self.host.operation_finished();
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
        if outcome.fully_undone {
            self.host.operation_finished();
        }
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
    /// (2) `.gento-tmp-` 残留ファイルを掃除 (コピー途中の不完全実体)。
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
        for record in interrupted {
            notices.push(format!(
                "未完了の{}操作 (id={}) を中断しました。必要なら履歴から再試行できます",
                record.operation_type, record.id
            ));
        }
        // .gento-tmp- 掃除
        if let Ok(roots) = self.roots() {
            for root in roots {
                let removed = sweep_tmp_files(&root.root_path);
                if removed > 0 {
                    notices.push(format!(
                        "コピー中断のテンポラリを {} 件掃除しました ({})",
                        removed,
                        root.root_path.display()
                    ));
                }
            }
        }
        notices
    }
}

/// ディレクトリ再帰で `.gento-tmp-` 接頭辞のファイルを掃除する。
/// 走査対象は接頭辞を持つものだけ (シンボリックリンクは辿らない)。
fn sweep_tmp_files(root: &Path) -> u64 {
    let mut removed = 0u64;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            removed += sweep_tmp_files(&entry.path());
        } else if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(TMP_FILE_PREFIX))
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
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

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FsOpError> {
        std::fs::rename(from, to).map_err(|e| {
            let mut err = io_error(from, &e);
            // フォールバック契約: 跨界はメッセージで識別可能にする
            if format!("{e:?}").contains("CrossesDevices") {
                err.message = format!(
                    "{} -> {}: CrossesDevices: {e}",
                    from.display(),
                    to.display()
                );
            } else {
                err.message = format!("{} -> {}: {e}", from.display(), to.display());
            }
            err
        })
    }

    fn copy_file(&self, from: &Path, to: &Path, cancel: &AtomicBool) -> Result<(), FsOpError> {
        // 安全原則4: テンポラリ名で書き込み→fsync→最終名へrename。
        // テンポラリは最終名と同じディレクトリ (renameの原子性)。
        let file_name = to
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = to.parent().unwrap_or_else(|| Path::new(".")).join(format!(
            "{TMP_FILE_PREFIX}{}-{file_name}",
            Uuid::now_v7().simple()
        ));
        let result = (|| -> Result<(), FsOpError> {
            let mut src = std::fs::File::open(from).map_err(|e| io_error(from, &e))?;
            let mut dst = std::fs::File::create(&tmp).map_err(|e| io_error(&tmp, &e))?;
            let mut buf = vec![0u8; 512 * 1024];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(FsOpError {
                        kind: FsOpErrorKind::Cancelled,
                        message: "コピーをキャンセルしました".to_string(),
                    });
                }
                let read = src.read(&mut buf).map_err(|e| io_error(from, &e))?;
                if read == 0 {
                    break;
                }
                dst.write_all(&buf[..read])
                    .map_err(|e| io_error(&tmp, &e))?;
            }
            // 元のmtimeを保存 (コピーは内容・属性の継承。トークン比較の
            // 安定化にも寄与 — size+mtime一致で衝突UIの判断が正確になる)
            if let Ok(mtime) = src.metadata().and_then(|m| m.modified()) {
                let _ = dst.set_modified(mtime);
            }
            dst.sync_all().map_err(|e| io_error(&tmp, &e))?;
            Ok(())
        })();
        match result {
            Ok(()) => std::fs::rename(&tmp, to).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                io_error(to, &e)
            }),
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                Err(err)
            }
        }
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsOpError> {
        std::fs::remove_file(path).map_err(|e| io_error(path, &e))
    }

    fn trash(&self, path: &Path) -> Result<(), FsOpError> {
        gento_platform::trash_to_bin(path).map_err(|msg| FsOpError::new(FsOpErrorKind::Io, msg))
    }

    fn restore_trash(&self, original: &Path) -> Result<PathBuf, FsOpError> {
        gento_platform::restore_from_trash(original).map_err(|err| {
            let (kind, message) = match &err {
                gento_platform::RestoreError::NotFound(msg) => {
                    (FsOpErrorKind::NotFound, msg.clone())
                }
                gento_platform::RestoreError::DestinationExists(_) => {
                    (FsOpErrorKind::Precondition, err.to_string())
                }
                gento_platform::RestoreError::Io(msg) => (FsOpErrorKind::Io, msg.clone()),
            };
            FsOpError::new(kind, message)
        })
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
    fn from_plan(plan: &FileOpPlan) -> Self {
        Self {
            kind: plan.kind.as_str().to_string(),
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

/// Undo実行の応答 (拒否時の理由を含む)。
#[derive(Debug, Clone, Serialize)]
pub struct UndoOutcomeDto {
    pub op_id: String,
    pub fully_undone: bool,
    pub errors: Vec<String>,
    pub refusals: Vec<String>,
}

impl UndoOutcomeDto {
    fn from_outcome(o: &UndoOutcome) -> Self {
        Self {
            op_id: o.op_id.to_string(),
            fully_undone: o.fully_undone,
            errors: o
                .steps
                .iter()
                .filter_map(|s| s.error.as_ref().map(|e| e.message.clone()))
                .collect(),
            refusals: o.refusals.iter().map(|r| r.describe()).collect(),
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

/// 実行要求 (計画と同じ入力 + 解決方針)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileOpExecuteArgs {
    pub kind: String,
    pub paths: Vec<String>,
    pub dest_dir: Option<String>,
    pub new_name: Option<String>,
    pub resolutions: Option<ResolutionSpecDto>,
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
        UndoAction,
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
        fn rename(&self, from: &Path, to: &Path) -> Result<(), FsOpError> {
            std::fs::rename(from, to).map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))
        }
        fn copy_file(&self, from: &Path, to: &Path, _c: &AtomicBool) -> Result<(), FsOpError> {
            std::fs::copy(from, to)
                .map(|_| ())
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))
        }
        fn remove_file(&self, path: &Path) -> Result<(), FsOpError> {
            std::fs::remove_file(path).map_err(|e| FsOpError::not_found(format!("{e}")))
        }
        fn trash(&self, path: &Path) -> Result<(), FsOpError> {
            let dest = self
                .trash_dir
                .lock()
                .unwrap()
                .join(path.file_name().unwrap_or_default());
            std::fs::rename(path, &dest)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))?;
            self.trashed
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), dest);
            Ok(())
        }
        fn restore_trash(&self, original: &Path) -> Result<PathBuf, FsOpError> {
            let trashed = self
                .trashed
                .lock()
                .unwrap()
                .get(original)
                .cloned()
                .ok_or_else(|| FsOpError::not_found("no trash entry (stub)"))?;
            std::fs::rename(&trashed, original)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{e}")))?;
            self.trashed.lock().unwrap().remove(original);
            Ok(original.to_path_buf())
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
    fn recovery_finalizes_executing_ops_and_sweeps_tmp() {
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
        // .gento-tmp- 残留 (コピー中断)
        dir.write(".gento-tmp-xxx-a.png", b"partial");
        dir.write("keep.png", b"keep");

        let notices = service.recover_at_startup();
        assert_eq!(notices.len(), 2, "{notices:?}");
        // executing → failed
        let ops = db
            .with_reader(|repos| Ok(repos.list_executing_operations().unwrap()))
            .unwrap();
        assert!(ops.is_empty());
        let history = service.history(10).unwrap();
        assert_eq!(history[0].status, "failed");
        // tmp掃除・正規ファイルは無傷
        assert!(!dir.0.join(".gento-tmp-xxx-a.png").exists());
        assert!(dir.0.join("keep.png").exists());
        let _ = root;
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
    fn real_file_ops_copy_preserves_mtime_and_uses_tmp_prefix() {
        let dir = TestDir::new("real-copy");
        let src = dir.write("r.png", b"real-copy-content");
        let dst = dir.0.join("out.png");
        let original_mtime = std::fs::metadata(&src).unwrap().modified().unwrap();
        RealFileOps
            .copy_file(&src, &dst, &AtomicBool::new(false))
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
            assert!(!name.starts_with(TMP_FILE_PREFIX), "tmp残留: {name}");
        }
    }

    #[test]
    fn real_file_ops_cancel_mid_copy_removes_tmp() {
        let dir = TestDir::new("real-cancel");
        let src = dir.write("big.png", &[7u8; 4 * 1024 * 1024]);
        let dst = dir.0.join("cancelled.png");
        let cancel = AtomicBool::new(true); // 最初のチャンク読み出し前に中断
        let err = RealFileOps.copy_file(&src, &dst, &cancel).unwrap_err();
        assert!(matches!(err.kind, FsOpErrorKind::Cancelled), "{err}");
        assert!(!dst.exists());
        for entry in std::fs::read_dir(&dir.0).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.starts_with(TMP_FILE_PREFIX),
                "キャンセルでtmp掃除: {name}"
            );
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
    fn real_file_ops_trash_round_trip() {
        // XDG_DATA_HOMEを一意に向けて実ゴミ箱と復帰を検証 (環境ロック直列化)
        let _env = TRASH_ENV_LOCK.lock().unwrap();
        let dir = TestDir::new("real-trash");
        let file = dir.write("t.png", b"trash-roundtrip");
        std::env::set_var("XDG_DATA_HOME", dir.0.join("xdg/data"));
        RealFileOps.trash(&file).unwrap();
        assert!(!file.exists());
        let restored = RealFileOps.restore_trash(&file).unwrap();
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
                undo: Some(UndoAction::MoveBack {
                    from: PathBuf::from("/r/new.png"),
                    to: PathBuf::from("/r/old.png"),
                    expect: TokenDto {
                        os_file_id: None,
                        byte_size: 0,
                        mtime_ns: 0,
                    },
                }),
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
}
