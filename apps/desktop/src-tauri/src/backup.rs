//! バックアップと復元のcommand群 (MVP WP-13)。
//!
//! - コア実装 (`.gento`形式・マッチング・適用) は gento-db `backup`。
//! - この層は (1) 確認トークンゲート (WP-7 fileops.rsと同一の
//!   intent-bound単発トークン方式)、(2) IPC DTO変換 (camelCase)、
//!   (3) フルDB自動バックアップの背景ループを担う。
//! - 復元は常に「plan (dry-run) → 確認 → execute」の順。executeは
//!   意図ダイジェスト (gento-db `restore_intent_digest`) がトークン発行時と
//!   一致する場合のみトークンを消費し、**消費済みトークンに束縛された
//!   確定plan**を gento-dbへ渡す — 適用は単一writerトランザクション内で
//!   現状からplanを再計算し、ダイジェストが再び一致する場合のみ行われる
//!   (確認→適用間のDB変化は `RestoreError::PlanChanged` で拒否)。
//! - 埋め込み・サムネイル・FTS・USearchはバックアップ対象外 (派生データ。
//!   復元後に再構築可能 — 統合テストで直接保証)。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gento_db::{
    build_restore_plan, create_portable_backup, execute_restore, full_db_backup,
    open_portable_backup, restore_intent_digest, run_auto_backup_if_due, AutoBackupFrequency,
    BackupError, RestoreError, RestorePlan, SETTING_BACKUP_AUTO_FULL_DB, SETTING_BACKUP_RETENTION,
};
use serde::Serialize;
use tauri::State;

use crate::catalog::AppState;
use crate::fileops::ConfirmTokenStore;

/// 自動バックアップのチェック間隔 (起動時即チェック → 1時間毎)。
const AUTO_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// バックアップサービス (AppStateへmanage)。
pub struct BackupService {
    db: Arc<gento_db::Db>,
    /// フルDBバックアップの既定出力先 (db-backups/)。
    default_full_dir: PathBuf,
    confirm_tokens: ConfirmTokenStore,
    /// 復元実行の排他 (同時多重実行の禁止)。
    running: Mutex<Option<()>>,
    stop: Arc<AtomicBool>,
}

impl BackupService {
    pub fn new(db: Arc<gento_db::Db>, default_full_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            db,
            default_full_dir,
            confirm_tokens: ConfirmTokenStore::default(),
            running: Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// フルDBバックアップの既定ディレクトリ。
    pub fn default_full_dir(&self) -> &Path {
        &self.default_full_dir
    }

    /// ポータブルバックアップ作成 ( blocking — UIスレッド外から)。
    pub fn create_portable(
        &self,
        root_id: i64,
        dest_parent: Option<&Path>,
    ) -> Result<PortableCreatedDto, String> {
        let dest = dest_parent.unwrap_or(self.default_full_dir());
        let path = create_portable_backup(&self.db, root_id, dest).map_err(backup_err)?;
        let info_path = std::fs::read_to_string(path.join("backup-info.json"))
            .map_err(|e| format!("バックアップ検証に失敗しました: {e}"))?;
        let info: serde_json::Value = serde_json::from_str(&info_path)
            .map_err(|e| format!("バックアップ検証に失敗しました: {e}"))?;
        Ok(PortableCreatedDto {
            path: path.display().to_string(),
            asset_count: info["asset_count"].as_u64().unwrap_or(0),
            file_count: info["file_count"].as_u64().unwrap_or(0),
            collection_count: info["collection_count"].as_u64().unwrap_or(0),
        })
    }

    /// フルDBバックアップ作成。
    pub fn full_backup(&self, dest_dir: Option<&Path>) -> Result<FullDbBackupDto, String> {
        let outcome = full_db_backup(&self.db, dest_dir).map_err(backup_err)?;
        Ok(FullDbBackupDto {
            path: outcome.path.display().to_string(),
            kept_generations: outcome.kept_generations as u32,
        })
    }

    /// 復元plan (dry-run) + 確認トークン発行。
    pub fn restore_plan(
        &self,
        backup_dir: &Path,
        target_root_id: Option<i64>,
    ) -> Result<RestorePlanDto, String> {
        let backup = open_portable_backup(backup_dir).map_err(backup_err)?;
        let plan = build_restore_plan(&self.db, &backup, target_root_id).map_err(backup_err)?;
        let digest = restore_intent_digest(&backup, &plan);
        let token = self.confirm_tokens.issue(digest);
        Ok(RestorePlanDto::from_plan(&plan, token))
    }

    /// 復元実行 (確認トークン必須。トークンに束縛された確定planを適用)。
    ///
    /// シーケンス (FG-WP13-1 B2 — 確認と適用のTOCTOU閉塞):
    /// 1. 実行時点でplanを再計算し、意図ダイジェストがトークン発行時と
    ///    一致することを確認して**トークンを消費する** (単発)。
    /// 2. 消費済みトークンが保証する確定planを gento-dbへ渡す。適用は
    ///    単一writerトランザクション内で現状からplanを再計算し、
    ///    ダイジェスト一致を再検証してから行われる — 手順1と適用の間に
    ///    DBが変化していれば `RestoreError::PlanChanged` で中止される。
    pub fn restore_execute(
        &self,
        backup_dir: &Path,
        target_root_id: Option<i64>,
        confirm_token: &str,
    ) -> Result<RestoreResultDto, String> {
        let _guard = self.lock_running()?;
        let backup = open_portable_backup(backup_dir).map_err(backup_err)?;
        // (1) 実行時点のplanと意図ダイジェストでトークンを検証・消費する
        //     (WP-7 FG-WP7-2と同じ構造強制 — 有効なトークンを別のバック
        //     アップや変更後のDB状態への流用を排除)。
        let plan = build_restore_plan(&self.db, &backup, target_root_id).map_err(backup_err)?;
        let digest = restore_intent_digest(&backup, &plan);
        if !self.confirm_tokens.consume(confirm_token, &digest) {
            return Err(
                "復元の実行には確認が必要です (計画をやり直して確認ダイアログから実行してください)"
                    .to_string(),
            );
        }
        // (2) 確定planの適用 (gento-dbが適用トランザクション内で再検証)。
        let result = execute_restore(&self.db, &backup, &plan, &digest).map_err(restore_err)?;
        Ok(RestoreResultDto::from_result(&result))
    }

    /// 設定の読み取り。
    pub fn settings(&self) -> Result<BackupSettingsDto, String> {
        let frequency = gento_db::auto_frequency(&self.db);
        let retention = self
            .db
            .with_reader(|repos| repos.setting_get(SETTING_BACKUP_RETENTION))
            .map_err(|e| e.to_string())?
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(gento_db::DEFAULT_BACKUP_RETENTION as u32);
        Ok(BackupSettingsDto {
            auto_full_db: frequency.as_str().to_string(),
            retention_generations: retention,
        })
    }

    /// 設定の更新 (頻度 off/daily/weekly・世代数)。
    pub fn set_settings(
        &self,
        auto_full_db: &str,
        retention_generations: Option<u32>,
    ) -> Result<BackupSettingsDto, String> {
        let frequency = AutoBackupFrequency::parse(auto_full_db);
        if frequency.as_str() != auto_full_db {
            return Err(format!(
                "不明なバックアップ頻度です: {auto_full_db} (off/daily/weekly)"
            ));
        }
        self.db
            .with_writer(|repos| {
                repos.setting_set(SETTING_BACKUP_AUTO_FULL_DB, frequency.as_str())?;
                if let Some(n) = retention_generations {
                    if !(1..=100).contains(&n) {
                        return Err(gento_core::repo::RepoError::Constraint(
                            "世代数は1〜100で指定してください".to_string(),
                        ));
                    }
                    repos.setting_set(SETTING_BACKUP_RETENTION, &n.to_string())?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        self.settings()
    }

    /// 実行排他の取得。
    fn lock_running(&self) -> Result<MutexGuard<'_, Option<()>>, String> {
        let guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Err("別の復元が実行中です".to_string());
        }
        Ok(guard)
    }

    /// 自動バックアップの背景ループを開始する (起動時即チェック → 定期)。
    pub fn start_auto_loop(self: &Arc<Self>) {
        let service = Arc::clone(self);
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name("backup-auto".into())
            .spawn(move || {
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match run_auto_backup_if_due(&service.db, &service.default_full_dir) {
                        Ok(Some(outcome)) => tracing::info!(
                            path = %outcome.path.display(),
                            "auto full db backup created"
                        ),
                        Ok(None) => {}
                        Err(err) => tracing::warn!(error = %err, "auto full db backup failed"),
                    }
                    // sleepを分割してshutdown応答性を保つ
                    let mut waited = 0u64;
                    while waited < AUTO_CHECK_INTERVAL.as_secs() {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        waited += 10;
                    }
                }
            })
            .ok();
    }

    /// 停止 (RunEvent::Exitから)。
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn backup_err(e: BackupError) -> String {
    match &e {
        BackupError::Incomplete { dir } => {
            format!("バックアップ {dir} は不完全です (作成が完了していません)。利用できません")
        }
        BackupError::DigestMismatch { dir, detail } => {
            format!("バックアップ {dir} の内容検証に失敗しました ({detail})")
        }
        BackupError::UnsupportedFormat { found, .. } => format!(
            "バックアップ形式 version {found} はこのバージョンが対応していません (新しいGEN-TOが必要です)"
        ),
        BackupError::NoTargetRoot { .. } => {
            "復元先のルートを解決できません。先にルートを登録するか、復元先を選択してください".to_string()
        }
        BackupError::DestinationExists(path) => {
            format!("バックアップ先が既に存在します: {}", path.display())
        }
        other => format!("バックアップ/復元に失敗しました: {other}"),
    }
}

/// 復元実行エラーのUI表示 (PlanChangedは再確認要求として伝える)。
fn restore_err(e: RestoreError) -> String {
    match e {
        RestoreError::PlanChanged { .. } => {
            "確認の後にライブラリの状態が変化しました。計画をやり直してから実行してください"
                .to_string()
        }
        RestoreError::Backup(inner) => backup_err(inner),
    }
}

/// 復元意図の正準ダイジェストは gento-db `restore_intent_digest` が正本
/// (トークン発行・消費・適用tx内検証の3時点で同一式である必要があるため)。
/// 対象は backup集約digest / root uuid / 対象root id / 全entryの
/// (uuid, match種別, action, 解決済みasset id, content_hash, file rel_path) /
/// summary件数 / コレクション (uuid, will_create, 項目解決結果)。

// ===== DTO (camelCase — api-contract §47) =====

#[derive(Debug, Clone, Serialize)]
pub struct PortableCreatedDto {
    pub path: String,
    pub asset_count: u64,
    pub file_count: u64,
    pub collection_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FullDbBackupDto {
    pub path: String,
    pub kept_generations: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlanDto {
    pub root_uuid: String,
    pub root_display_path: String,
    pub created_at: String,
    pub target_root_id: String,
    pub target_root_path: String,
    pub entries: Vec<RestoreEntryDto>,
    pub collections: Vec<RestoreCollectionDto>,
    pub summary: RestoreSummaryDto,
    /// 実行に必須の単発確認トークン (5分失効・1回限り・意図束ね)。
    pub confirm_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreEntryDto {
    pub asset_uuid: String,
    pub content_hash: String,
    pub display_name: String,
    pub current_asset_id: Option<String>,
    /// "uuid" | "hash" | "path_add" | "conflict" | "missing" | "ambiguous"
    pub matched: String,
    /// "same" | "update" | "add" | "skip"
    pub action: String,
    pub reason: String,
    pub files: Vec<RestoreFileDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFileDto {
    pub backup_rel_path: String,
    pub current_rel_path: Option<String>,
    pub on_disk: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreCollectionDto {
    pub uuid: String,
    pub name: String,
    pub kind: String,
    pub will_create: bool,
    pub items_total: u64,
    pub items_resolvable: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSummaryDto {
    pub same: u64,
    pub update: u64,
    pub add: u64,
    pub conflict: u64,
    pub missing: u64,
    pub ambiguous: u64,
}

impl RestorePlanDto {
    fn from_plan(plan: &RestorePlan, confirm_token: String) -> Self {
        Self {
            root_uuid: plan.root_uuid.clone(),
            root_display_path: plan.root_display_path.clone(),
            created_at: plan.created_at.clone(),
            target_root_id: plan.target_root_id.to_string(),
            target_root_path: plan.target_root_path.clone(),
            confirm_token,
            entries: plan
                .entries
                .iter()
                .map(|e| RestoreEntryDto {
                    asset_uuid: e.asset_uuid.clone(),
                    content_hash: e.content_hash.clone(),
                    display_name: e.display_name.clone(),
                    current_asset_id: e.current_asset_id.map(|id| id.to_string()),
                    matched: e.matched.as_str().to_string(),
                    action: e.action.as_str().to_string(),
                    reason: e.reason.clone(),
                    files: e
                        .files
                        .iter()
                        .map(|f| RestoreFileDto {
                            backup_rel_path: f.backup_rel_path.clone(),
                            current_rel_path: f.current_rel_path.clone(),
                            on_disk: f.on_disk,
                        })
                        .collect(),
                })
                .collect(),
            collections: plan
                .collections
                .iter()
                .map(|c| RestoreCollectionDto {
                    uuid: c.uuid.clone(),
                    name: c.name.clone(),
                    kind: c.kind.clone(),
                    will_create: c.will_create,
                    items_total: c.items_total,
                    items_resolvable: c.items_resolvable,
                })
                .collect(),
            summary: RestoreSummaryDto {
                same: plan.summary.same,
                update: plan.summary.update,
                add: plan.summary.add,
                conflict: plan.summary.conflict,
                missing: plan.summary.missing,
                ambiguous: plan.summary.ambiguous,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResultDto {
    pub applied: Vec<RestoreAppliedDto>,
    pub skipped: Vec<RestoreSkippedDto>,
    pub collections_created: u64,
    pub collections_merged: u64,
    pub collection_items_added: u64,
    pub collection_items_unresolved: u64,
    pub fts_rebuilt: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreAppliedDto {
    pub asset_uuid: String,
    pub action: String,
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSkippedDto {
    pub asset_uuid: String,
    pub matched: String,
    pub reason: String,
}

impl RestoreResultDto {
    fn from_result(r: &gento_db::RestoreResult) -> Self {
        Self {
            applied: r
                .applied
                .iter()
                .map(|a| RestoreAppliedDto {
                    asset_uuid: a.asset_uuid.clone(),
                    action: a.action.as_str().to_string(),
                    changed: a.changed.clone(),
                })
                .collect(),
            skipped: r
                .skipped
                .iter()
                .map(|s| RestoreSkippedDto {
                    asset_uuid: s.asset_uuid.clone(),
                    matched: s.matched.as_str().to_string(),
                    reason: s.reason.clone(),
                })
                .collect(),
            collections_created: r.collections_created,
            collections_merged: r.collections_merged,
            collection_items_added: r.collection_items_added,
            collection_items_unresolved: r.collection_items_unresolved,
            fts_rebuilt: r.fts_rebuilt,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSettingsDto {
    /// "off" | "daily" | "weekly"
    pub auto_full_db: String,
    pub retention_generations: u32,
}

// ===== Tauri commands (async + spawn_blocking。DTOはcamelCase) =====

fn service_of(state: &State<'_, AppState>) -> Result<Arc<BackupService>, String> {
    state
        .backup
        .clone()
        .ok_or_else(|| "バックアップはDBが無効なため利用できません".to_string())
}

fn parse_root_id(value: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("ルートIDが不正です: {value}"))
}

/// ルート単位のポータブルバックアップを作成。
#[tauri::command]
pub async fn backup_portable_create(
    state: State<'_, AppState>,
    root_id: String,
    dest_dir: Option<String>,
) -> Result<PortableCreatedDto, String> {
    let service = service_of(&state)?;
    let root_id = parse_root_id(&root_id)?;
    let dest = dest_dir.map(PathBuf::from);
    tauri::async_runtime::spawn_blocking(move || service.create_portable(root_id, dest.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// フルDBバックアップを作成 (VACUUM INTO・世代保持)。
#[tauri::command]
pub async fn backup_full_db(
    state: State<'_, AppState>,
    dest_dir: Option<String>,
) -> Result<FullDbBackupDto, String> {
    let service = service_of(&state)?;
    let dest = dest_dir.map(PathBuf::from);
    tauri::async_runtime::spawn_blocking(move || service.full_backup(dest.as_deref()))
        .await
        .map_err(|e| e.to_string())?
}

/// フルDBバックアップの既定ディレクトリ (UIの保存先初期値)。
#[tauri::command]
pub fn backup_default_dir(state: State<'_, AppState>) -> Result<String, String> {
    let service = service_of(&state)?;
    Ok(service.default_full_dir().display().to_string())
}

/// 復元plan (dry-run)。差分・衝突・行方不明・曖昧をレポートのみ返す。
#[tauri::command]
pub async fn backup_restore_plan(
    state: State<'_, AppState>,
    backup_dir: String,
    target_root_id: Option<String>,
) -> Result<RestorePlanDto, String> {
    let service = service_of(&state)?;
    let target = target_root_id.map(|v| parse_root_id(&v)).transpose()?;
    let dir = PathBuf::from(backup_dir);
    tauri::async_runtime::spawn_blocking(move || service.restore_plan(&dir, target))
        .await
        .map_err(|e| e.to_string())?
}

/// 復元を実行 (確認トークン必須)。
#[tauri::command]
pub async fn backup_restore_execute(
    state: State<'_, AppState>,
    backup_dir: String,
    target_root_id: Option<String>,
    confirm_token: String,
) -> Result<RestoreResultDto, String> {
    let service = service_of(&state)?;
    let target = target_root_id.map(|v| parse_root_id(&v)).transpose()?;
    let dir = PathBuf::from(backup_dir);
    tauri::async_runtime::spawn_blocking(move || {
        service.restore_execute(&dir, target, &confirm_token)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// バックアップ設定の読み取り。
#[tauri::command]
pub fn backup_settings_get(state: State<'_, AppState>) -> Result<BackupSettingsDto, String> {
    let service = service_of(&state)?;
    service.settings()
}

/// バックアップ設定の更新。
#[tauri::command]
pub fn backup_settings_set(
    state: State<'_, AppState>,
    auto_full_db: String,
    retention_generations: Option<u32>,
) -> Result<BackupSettingsDto, String> {
    let service = service_of(&state)?;
    service.set_settings(&auto_full_db, retention_generations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, RepoError, ScanState,
    };

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-backup-{tag}-{}-{}",
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
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn hash_of(bytes: &[u8]) -> String {
        format!("blake3:{}", blake3::hash(bytes).to_hex())
    }

    fn seed(db: &gento_db::Db, root_dir: &Path, rel: &str, seed: u8) -> i64 {
        let bytes = vec![seed; 32];
        let p = root_dir.join(rel);
        std::fs::write(&p, &bytes).unwrap();
        let root = db.with_writer(|repos| repos.add_root(root_dir)).unwrap();
        let file = db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from(rel),
                    display_name: rel.to_string(),
                    byte_size: 32,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (asset, _) = db
            .with_writer(|repos| {
                repos.create_or_get_asset(NewAsset {
                    content_hash: hash_of(&bytes),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 32,
                    mime_type: None,
                    width: None,
                    height: None,
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })
            })
            .unwrap();
        db.with_writer(|repos| repos.link_file(file.id, asset.id))
            .unwrap();
        root.id
    }

    #[test]
    fn plan_execute_flow_with_confirm_token() {
        let tmp = TestDir::new("flow");
        let db = Arc::new(gento_db::Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let root_dir = tmp.path().join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        let root_id = seed(&db, &root_dir, "a.png", 1);

        let service = BackupService::new(Arc::clone(&db), tmp.path().join("db-backups"));
        // 作成 (rating=0の状態で)
        let created = service
            .create_portable(root_id, Some(&tmp.path().join("exports")))
            .unwrap();
        assert!(created.path.ends_with(".gento"), "{}", created.path);
        assert_eq!(created.asset_count, 1);

        // バックアップ後に現DB側で評価を変更 → planはUpdateを報告
        db.with_writer(|repos| {
            let asset = repos
                .find_by_hash(&hash_of(&[1u8; 32]))?
                .ok_or(RepoError::NotFound)?;
            repos.set_rating(asset.id, 5)
        })
        .unwrap();

        // plan → token
        let plan = service
            .restore_plan(Path::new(&created.path), None)
            .unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert!(!plan.confirm_token.is_empty());
        assert_eq!(
            plan.summary.update, 1,
            "rating 5→0 (バックアップ時点へ戻す)"
        );

        // 誤トークンでは実行拒否
        let err = service
            .restore_execute(Path::new(&created.path), None, "wrong-token")
            .unwrap_err();
        assert!(err.contains("確認が必要です"), "{err}");
        // トークンは消費済みではない (誤使用で焼却されるのは正トークンのみ...

        // 実際は誤token提示では正トークンは消費されない — consume("wrong")は
        // storeから"wrong"をremoveしにいくだけ。正トークンで再実行可能。
        let result = service
            .restore_execute(Path::new(&created.path), None, &plan.confirm_token)
            .unwrap();
        assert_eq!(result.applied.len(), 1);

        // トークン再利用は拒否 (単発)
        let err = service
            .restore_execute(Path::new(&created.path), None, &plan.confirm_token)
            .unwrap_err();
        assert!(err.contains("確認が必要です"), "{err}");
    }

    #[test]
    fn token_is_bound_to_backup_identity() {
        // 異なるバックアップへの流用拒否 (意図ダイジェスト不一致)。
        let tmp = TestDir::new("bind");
        let db = Arc::new(gento_db::Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let root_a = tmp.path().join("a");
        let root_b = tmp.path().join("b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let id_a = seed(&db, &root_a, "a.png", 1);
        let id_b = seed(&db, &root_b, "b.png", 2);

        let service = BackupService::new(Arc::clone(&db), tmp.path().join("db-backups"));
        let exports = tmp.path().join("exports");
        let backup_a = service.create_portable(id_a, Some(&exports)).unwrap();
        let backup_b = service.create_portable(id_b, Some(&exports)).unwrap();

        // 新規DBでplan(b)のトークンを発行 → execute(a)には使えない
        let fresh = Arc::new(gento_db::Db::open(tmp.path().join("fresh.sqlite3")).unwrap());
        let fresh_root_dir = tmp.path().join("fresh-root");
        std::fs::create_dir_all(&fresh_root_dir).unwrap();
        // ルートを登録 (復元先)。中身は空。
        fresh
            .with_writer(|repos| repos.add_root(&fresh_root_dir))
            .unwrap();
        let fresh_service = BackupService::new(Arc::clone(&fresh), tmp.path().join("bk"));
        let plan_b = fresh_service
            .restore_plan(Path::new(&backup_b.path), None)
            .unwrap();
        let err = fresh_service
            .restore_execute(Path::new(&backup_a.path), None, &plan_b.confirm_token)
            .unwrap_err();
        assert!(
            err.contains("確認が必要です"),
            "別バックアップへの流用拒否: {err}"
        );
        // 正しい組み合わせなら実行できる
        let plan_a = fresh_service
            .restore_plan(Path::new(&backup_a.path), None)
            .unwrap();
        fresh_service
            .restore_execute(Path::new(&backup_a.path), None, &plan_a.confirm_token)
            .unwrap();
    }

    #[test]
    fn settings_round_trip_and_validation() {
        let tmp = TestDir::new("settings");
        let db = Arc::new(gento_db::Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let service = BackupService::new(Arc::clone(&db), tmp.path().join("bk"));
        let initial = service.settings().unwrap();
        assert_eq!(initial.auto_full_db, "off");
        assert_eq!(initial.retention_generations, 3);

        let updated = service.set_settings("weekly", Some(7)).unwrap();
        assert_eq!(updated.auto_full_db, "weekly");
        assert_eq!(updated.retention_generations, 7);

        let err = service.set_settings("sometimes", None).unwrap_err();
        assert!(err.contains("不明なバックアップ頻度"), "{err}");
        let err = service.set_settings("daily", Some(0)).unwrap_err();
        assert!(err.contains("1〜100"), "{err}");
    }

    #[test]
    fn full_backup_via_service_uses_default_dir() {
        let tmp = TestDir::new("full");
        let db = Arc::new(gento_db::Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let default_dir = tmp.path().join("db-backups");
        let service = BackupService::new(Arc::clone(&db), default_dir.clone());
        let out = service.full_backup(None).unwrap();
        assert!(Path::new(&out.path).starts_with(&default_dir));
        assert!(out.kept_generations >= 1);
        assert_eq!(
            service.default_full_dir().display().to_string(),
            default_dir.display().to_string()
        );
    }

    #[test]
    fn restore_execute_rejects_token_after_db_changes_between_plan_and_execute() {
        // トークン発行 (確認) と実行の間にDBが変化したら実行拒否 (TOCTOU閉塞)。
        let tmp = TestDir::new("toctou");
        let db = Arc::new(gento_db::Db::open(tmp.path().join("db.sqlite3")).unwrap());
        let root_dir = tmp.path().join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        let root_id = seed(&db, &root_dir, "a.png", 1);

        let service = BackupService::new(Arc::clone(&db), tmp.path().join("db-backups"));
        let created = service
            .create_portable(root_id, Some(&tmp.path().join("exports")))
            .unwrap();

        // 復元先は別DB (空ルート + ディスクに同一内容ファイル → PathAdd想定)
        let fresh = Arc::new(gento_db::Db::open(tmp.path().join("fresh.sqlite3")).unwrap());
        let fresh_root_dir = tmp.path().join("fresh-root");
        std::fs::create_dir_all(&fresh_root_dir).unwrap();
        std::fs::write(fresh_root_dir.join("a.png"), vec![1u8; 32]).unwrap();
        fresh
            .with_writer(|repos| repos.add_root(&fresh_root_dir))
            .unwrap();
        let fresh_service = BackupService::new(Arc::clone(&fresh), tmp.path().join("bk"));
        let plan = fresh_service
            .restore_plan(Path::new(&created.path), None)
            .unwrap();
        assert_eq!(plan.summary.add, 1, "空DB → PathAdd");

        // 確認後に現DBを変化させる (同内容をDBへ登録 → hash再関連付けへ変化)
        let hash = hash_of(&[1u8; 32]);
        fresh
            .with_writer(|repos| {
                let roots = repos.list_roots()?;
                let file = repos.upsert_file(FileUpsert {
                    root_id: roots[0].id,
                    rel_path: PathBuf::from("a.png"),
                    display_name: "a.png".to_string(),
                    byte_size: 32,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })?;
                let (asset, _) = repos.create_or_get_asset(NewAsset {
                    content_hash: hash,
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 32,
                    mime_type: None,
                    width: None,
                    height: None,
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })?;
                repos.link_file(file.id, asset.id)?;
                Ok(())
            })
            .unwrap();

        // 旧トークンでは実行拒否 (意図digest不一致 — トークンは焼却)
        let err = fresh_service
            .restore_execute(Path::new(&created.path), None, &plan.confirm_token)
            .unwrap_err();
        assert!(err.contains("確認が必要です"), "{err}");
        // 適用されていない
        let assets: i64 = fresh
            .with_reader(|repos| {
                repos
                    .conn()
                    .query_row("SELECT count(*) FROM assets", [], |r| r.get(0))
                    .map_err(|e| RepoError::Db(e.to_string()))
            })
            .unwrap();
        assert_eq!(assets, 1, "現DB側の登録のみ (復元適用なし)");
    }

    /// 復元 → サムネイル再生成 (cache missでgen) + USearch再構築
    /// (rebuild後similar検索) の統合受け入れテスト。
    ///
    /// §21「サムネイル、FTS、USearchをバックアップなしで再構築できる」の
    /// 直接カバー — 派生データ (サムネイルキャッシュ・埋め込み索引) は
    /// 復元後のDB + ディスク実体から再構築できることを実サービス経路で検証。
    #[test]
    fn restore_then_thumbnail_and_vector_index_rebuild_acceptance() {
        use crate::thumbnails::{
            AssetSnapshot, ThumbEvents, ThumbFailedItem, ThumbReadyItem, ThumbnailService,
        };
        use crate::vectorindex::VectorIndexService;
        use gento_core::{AssetId, Priority, SizeClass};
        use std::sync::Mutex as StdMutex;

        let tmp = TestDir::new("rebuild");
        // --- 元DB: 実PNG 2件 (内容が異なる) を含むルート ---
        let write_real_png = |dir: &Path, name: &str, tone: u8| -> (PathBuf, String) {
            let img =
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(600, 400, |x, y| {
                    image::Rgba([
                        (x as u8 % 56).wrapping_add(tone),
                        (y as u8 % 56).wrapping_add(tone),
                        tone,
                        255,
                    ])
                }));
            let p = dir.join(name);
            img.save(&p).unwrap();
            let bytes = std::fs::read(&p).unwrap();
            (p, hash_of(&bytes))
        };
        let src_root = tmp.path().join("photos");
        std::fs::create_dir_all(&src_root).unwrap();
        let src_db = Arc::new(gento_db::Db::open(tmp.path().join("src.sqlite3")).unwrap());
        let (a_path, hash_a) = write_real_png(&src_root, "a.png", 10);
        let (b_path, hash_b) = write_real_png(&src_root, "b.png", 200);
        let seed_png = |db: &gento_db::Db, root_id: i64, path: &Path, hash: &str| {
            let meta = std::fs::metadata(path).unwrap();
            let file = db
                .with_writer(|repos| {
                    repos.upsert_file(FileUpsert {
                        root_id,
                        rel_path: PathBuf::from(
                            path.file_name().unwrap().to_string_lossy().into_owned(),
                        ),
                        display_name: path.file_name().unwrap().to_string_lossy().into_owned(),
                        byte_size: meta.len(),
                        mtime_ms: 0,
                        mtime_ns: 0,
                        scan_state: ScanState::Present,
                        os_file_id: None,
                        detach_asset: false,
                    })
                })
                .unwrap();
            let (asset, _) = db
                .with_writer(|repos| {
                    repos.create_or_get_asset(NewAsset {
                        content_hash: hash.to_string(),
                        hash_algorithm: "blake3".to_string(),
                        byte_size: meta.len(),
                        mime_type: Some("image/png".to_string()),
                        width: Some(600),
                        height: Some(400),
                        has_alpha: None,
                        is_animated: None,
                        frame_count: None,
                    })
                })
                .unwrap();
            db.with_writer(|repos| repos.link_file(file.id, asset.id))
                .unwrap();
        };
        let root_id = src_db
            .with_writer(|repos| repos.add_root(&src_root))
            .unwrap()
            .id;
        seed_png(&src_db, root_id, &a_path, &hash_a);
        seed_png(&src_db, root_id, &b_path, &hash_b);

        let service = BackupService::new(Arc::clone(&src_db), tmp.path().join("bk"));
        let created = service
            .create_portable(root_id, Some(&tmp.path().join("exports")))
            .unwrap();
        assert_eq!(created.asset_count, 2);

        // --- 復元先: 新規DBに空ルート (ディスク実体は同じ場所) ---
        let cur_db = Arc::new(gento_db::Db::open(tmp.path().join("cur.sqlite3")).unwrap());
        cur_db
            .with_writer(|repos| repos.add_root(&src_root))
            .unwrap();
        let cur_service = BackupService::new(Arc::clone(&cur_db), tmp.path().join("bk2"));
        let plan = cur_service
            .restore_plan(Path::new(&created.path), None)
            .unwrap();
        assert_eq!(plan.summary.add, 2, "空DB → PathAdd×2");
        let result = cur_service
            .restore_execute(Path::new(&created.path), None, &plan.confirm_token)
            .unwrap();
        assert_eq!(result.applied.len(), 2);
        assert_eq!(result.fts_rebuilt, 2, "FTSは復元経路で再構築済み");

        let (asset_a, asset_b) = cur_db
            .with_reader(|repos| {
                Ok((
                    repos.find_by_hash(&hash_a)?.expect("a復元済み").id,
                    repos.find_by_hash(&hash_b)?.expect("b復元済み").id,
                ))
            })
            .unwrap();
        let _ = asset_b;

        // --- (1) サムネイル: 復元後のcache missで生成され、2回目はヒット ---
        #[derive(Clone, Default)]
        struct Recorder(Arc<StdMutex<Vec<ThumbReadyItem>>>);
        impl ThumbEvents for Recorder {
            fn ready(&self, items: &[ThumbReadyItem]) {
                self.0.lock().unwrap().extend(items.iter().cloned());
            }
            fn failed(&self, items: &[ThumbFailedItem]) {
                panic!("サムネイル生成が失敗した: {items:?}");
            }
        }
        let recorder = Recorder::default();
        let thumb =
            ThumbnailService::new(tmp.path().join("thumb-cache"), Box::new(recorder.clone()))
                .unwrap();
        let snapshot_of = |id: i64, path: &Path| -> AssetSnapshot {
            let md = std::fs::metadata(path).unwrap();
            AssetSnapshot {
                id: AssetId::new(id as u64),
                source_path: path.to_path_buf(),
                byte_size: md.len(),
                mtime_ms: md
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
                readable: true,
                content_type: "image/png".to_string(),
            }
        };
        let first = thumb.request(
            &[snapshot_of(asset_a, &a_path), snapshot_of(asset_b, &b_path)],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(first.cache_hits, 0, "復元直後はキャッシュ無し (miss)");
        assert_eq!(first.queued, 2, "miss → 生成ジョブ投入");
        // 生成完了を待つ (実デコード+リサイズ)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while recorder.0.lock().unwrap().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "サムネイル生成タイムアウト"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let second = thumb.request(
            &[snapshot_of(asset_a, &a_path)],
            SizeClass::S256,
            Priority::Background,
        );
        assert_eq!(second.cache_hits, 1, "生成物がキャッシュへ");
        assert_eq!(second.queued, 0);
        thumb.shutdown();

        // --- (2) USearch: 埋め込みは復元対象外 → 再生成 (正本投入) →
        //     索引を破棄して全再構築 → similar検索で件数・一致検証 ---
        let identity = crate::vectorindex::identity_of_model("ruri-v3-70m").unwrap();
        assert_eq!(identity.dimensions, 384);
        // 決定論的な直交ベクトル (a=第1成分, b=第2成分) — top-1が予測可能
        let mut vec_a = vec![0f32; 384];
        vec_a[0] = 1.0;
        let mut vec_b = vec![0f32; 384];
        vec_b[1] = 1.0;
        cur_db
            .with_writer(|repos| {
                repos.embedding_upsert(
                    asset_a,
                    "ruri-v3-70m",
                    &identity.model_revision,
                    384,
                    "f32",
                    "hash-a",
                    &vec_a,
                )?;
                repos.embedding_upsert(
                    asset_b,
                    "ruri-v3-70m",
                    &identity.model_revision,
                    384,
                    "f32",
                    "hash-b",
                    &vec_b,
                )
            })
            .unwrap();

        let notices: StdMutex<Vec<String>> = StdMutex::new(Vec::new());
        let vis = VectorIndexService::start(
            Arc::clone(&cur_db),
            tmp.path().join("data"),
            Arc::new(move |event: &str, payload: &str| {
                notices.lock().unwrap().push(format!("{event}: {payload}"));
            }),
        );
        // 索引ファイル無し → 全再構築 (SQLite正本から)
        vis.rebuild_now();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !vis.is_ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "索引再構築タイムアウト"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = vis.status();
        assert_eq!(status.indexed_count, 2, "SQLite正本の2件を再構築");
        assert_eq!(status.sqlite_count, 2);
        assert!(status.ready);

        // similar検索: vec_aに最も近いのはasset_a (cosine)
        let neighbors = vis.search(&vec_a, 2, None).expect("search可能");
        assert_eq!(neighbors.len(), 2, "件数一致");
        assert_eq!(neighbors[0].key, asset_a, "top-1一致 (aのベクトルで検索)");
        assert!(neighbors[0].similarity > neighbors[1].similarity);
        let neighbors_b = vis.search(&vec_b, 1, None).expect("search可能");
        assert_eq!(neighbors_b[0].key, asset_b, "bのベクトルのtop-1はb");

        // 再構築冪等性: 明示的に破棄してもう一度再構築しても同一結果
        vis.rebuild_now();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // rebuild_nowはスロットを外す → is_readyが一度falseになるのを待ってから完了待ち
        while !vis.is_ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "再構築2回目タイムアウト"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let again = vis.search(&vec_a, 2, None).expect("search可能");
        assert_eq!(again[0].key, asset_a, "再構築後も同一結果");

        vis.shutdown();
        drop(vis);
    }
}
