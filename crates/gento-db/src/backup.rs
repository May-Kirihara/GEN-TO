//! バックアップと復元 (MVP WP-13)。
//!
//! # 2種類のバックアップ
//!
//! 1. **フルDBバックアップ** (`full_db_backup`): `VACUUM INTO` でDB全体を
//!    単一SQLiteファイルへ書き出す (migration前backupと同一のWAL安全手法。
//!    integrity_check検証付き)。出力先は既定 `db-backups/`
//!    ([`crate::BACKUP_DIR_NAME`])、世代保持 (`backup.retention_generations`)。
//!    自動実行 (off/daily/weekly) は `app_settings` の `backup.auto_full_db`。
//! 2. **`.gento` ポータブルバックアップ** (`create_portable_backup`):
//!    登録ルート単位の**ディレクトリ形式** (単一アーカイブは却下 — 圧縮
//!    依存を増やさずJSONLの部分検査・ストリーム書き出しが容易なため)。
//!
//! # `.gento` ディレクトリ構成
//!
//! - `backup-info.json`: 後述schema。**最後に書く** (completeフラグの意味)。
//! - `metadata.jsonl`: 1行1レコード (`{"type":"asset",...}` /
//!   `{"type":"file",...}`)。rel_pathは百分号エスケープ正準形
//!   ([`crate::percent_encode_bytes`])。
//! - `collections.json`: 通常+スマートコレクション (query_json含む) + items。
//!
//! 作成は一時ディレクトリ `.gento-tmp-{uuid}` へ全部書いてから
//! `backup-info.json` を書き、ディレクトリ名を
//! `{root名}-{YYYYMMDDTHHMMSSmmmZ}.gento` へ原子的リネームする。
//! 復元側は `complete:true` + digest一致を検証し、不完全なものは拒否する
//! (**不完全バックアップを正常完了として扱わない**)。
//!
//! # 復元は2フェーズ (実行はアプリ層の確認トークンと併用)
//!
//! - `build_restore_plan`: dry-run。UUID完全一致→content_hash一致→
//!   rel_path一致の優先でマッチングし、差分・衝突・行方不明・
//!   同一ハッシュ複数件を**レポートのみ**返す。
//! - `execute_restore`: planを再計算して適用する。曖昧マッチと行方不明は
//!   **スキップして結果に報告する** (推測して適用しない)。
//!
//! 埋め込みベクトル・サムネイル・FTS・USearch索引はバックアップ対象外
//! (派生データ)。復元後に再構築できることをテストで保証する
//! (FTS: `rebuild_docs_for_assets` 経路、埋め込み: 再生成扱いで固定)。
//!
//! # backup-info.json schema (format_version 1)
//!
//! ```json
//! {
//!   "format_version": 1,
//!   "api_version": 1,
//!   "app_version": "0.x.y",
//!   "created_at": "2026-09-02T00:00:00.000Z",
//!   "root_uuid": "…",
//!   "root_display_path": "/data/photos",
//!   "asset_count": 3,
//!   "file_count": 4,
//!   "collection_count": 2,
//!   "file_digests": [
//!     {"name": "collections.json", "blake3": "…64hex…"},
//!     {"name": "metadata.jsonl",  "blake3": "…64hex…"}
//!   ],
//!   "digest": "blake3:…64hex…",
//!   "complete": true
//! }
//! ```
//!
//! `digest` は `"{name}:{blake3hex}\n"` 行を名前順に連結したもののBLAKE3。
//! `backup-info.json` 自体はdigest対象外 (自分のdigestを含むため)。

use std::io::Read;
use std::path::{Path, PathBuf};

use gento_core::repo::{LibraryRootRepo, RepoError};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ids::{new_uuid_v7, now_utc_compact, now_utc_rfc3339};
use crate::paths::{decode_path, percent_decode_to_bytes, percent_encode_bytes};
use crate::repos::SqliteRepos;
use crate::Db;

/// app_settings: フルDB自動バックアップ頻度 (`off`|`daily`|`weekly`)。
pub const SETTING_BACKUP_AUTO_FULL_DB: &str = "backup.auto_full_db";
/// app_settings: フルDBバックアップ世代保持数 (十進数文字列)。
pub const SETTING_BACKUP_RETENTION: &str = "backup.retention_generations";
/// app_settings: 自動バックアップ前回実行時刻 (内部。epoch ms十進数)。
pub(crate) const SETTING_BACKUP_LAST_AUTO: &str = "backup.last_auto_run";

/// `.gento`バックアップのformat_version (本binaryが書く値)。
pub const BACKUP_FORMAT_VERSION: u32 = 1;
pub const BACKUP_INFO_FILE: &str = "backup-info.json";
pub const METADATA_JSONL_FILE: &str = "metadata.jsonl";
pub const COLLECTIONS_JSON_FILE: &str = "collections.json";
/// 一時ディレクトリ接頭辞 (作成中の印。復元側は拒否する)。
pub const TMP_DIR_PREFIX: &str = ".gento-tmp-";
/// フルDBバックアップファイル名の接頭辞 (migration用 `library-pre-` と区別)。
const FULL_BACKUP_PREFIX: &str = "library-full-";
const FULL_BACKUP_SUFFIX: &str = ".sqlite3";
/// 世代保持の既定値 (設定が無い/不正な場合)。
pub const DEFAULT_BACKUP_RETENTION: usize = 3;

/// バックアップ・復元の失敗。
#[derive(Debug)]
pub enum BackupError {
    /// IO・ファイルシステムの失敗 (文脈込み)。
    Io(String),
    /// DB読み書きの失敗。
    Db(String),
    /// バックアップ形式の不正 (tmp残骸・必須ファイル欠落・JSON破損)。
    Format(String),
    /// `complete:true` でない (書き込み中途)。
    Incomplete { dir: String },
    /// digest不一致 (内容が書き出し後に変質)。
    DigestMismatch { dir: String, detail: String },
    /// 読み取り側が対応するより新しいformat_version。
    UnsupportedFormat { found: u32, supported: u32 },
    /// 対象ルートが存在しない。
    RootNotFound(i64),
    /// 復元先ルートを解決できない (登録も選択もされていない)。
    NoTargetRoot { backup_root_uuid: String },
    /// 書き込み先に同名のバックアップが既に存在する。
    DestinationExists(PathBuf),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Io(msg) => write!(f, "backup io error: {msg}"),
            BackupError::Db(msg) => write!(f, "backup db error: {msg}"),
            BackupError::Format(msg) => write!(f, "invalid backup format: {msg}"),
            BackupError::Incomplete { dir } => write!(
                f,
                "backup {dir} is incomplete (missing complete flag); refusing to use it"
            ),
            BackupError::DigestMismatch { dir, detail } => {
                write!(f, "backup {dir} digest mismatch: {detail}")
            }
            BackupError::UnsupportedFormat { found, supported } => write!(
                f,
                "backup format version {found} is newer than supported ({supported}); \
                 a newer GEN-TO binary is required"
            ),
            BackupError::RootNotFound(id) => write!(f, "library root {id} not found"),
            BackupError::NoTargetRoot { backup_root_uuid } => write!(
                f,
                "restore target root could not be resolved \
                 (backup root uuid {backup_root_uuid}; register or select a root)"
            ),
            BackupError::DestinationExists(path) => {
                write!(f, "backup destination already exists: {}", path.display())
            }
        }
    }
}

impl std::error::Error for BackupError {}

impl From<RepoError> for BackupError {
    fn from(e: RepoError) -> Self {
        BackupError::Db(e.to_string())
    }
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> Self {
        BackupError::Io(e.to_string())
    }
}

/// フルDB自動バックアップの頻度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoBackupFrequency {
    Off,
    Daily,
    Weekly,
}

impl AutoBackupFrequency {
    pub const fn as_str(self) -> &'static str {
        match self {
            AutoBackupFrequency::Off => "off",
            AutoBackupFrequency::Daily => "daily",
            AutoBackupFrequency::Weekly => "weekly",
        }
    }

    /// 期間 (ms)。Offは実行しないため0。
    const fn interval_ms(self) -> u64 {
        match self {
            AutoBackupFrequency::Off => 0,
            AutoBackupFrequency::Daily => 24 * 60 * 60 * 1000,
            AutoBackupFrequency::Weekly => 7 * 24 * 60 * 60 * 1000,
        }
    }

    /// 設定値の解析。不正値はOff (安全側: 自動書き込みを始めない)。
    pub fn parse(s: &str) -> Self {
        match s {
            "daily" => AutoBackupFrequency::Daily,
            "weekly" => AutoBackupFrequency::Weekly,
            _ => AutoBackupFrequency::Off,
        }
    }
}

/// フルDBバックアップの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullDbBackupOutcome {
    pub path: PathBuf,
    /// 回収後に残った世代数 (今回作成分を含む)。
    pub kept_generations: usize,
}

// ===== ポータブルバックアップのレコード型 =====

/// asset単位の保守メタデータ (metadata.jsonlのasset行)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupAssetEntry {
    pub uuid: String,
    pub content_hash: String,
    pub hash_algorithm: String,
    pub byte_size: u64,
    pub mime_type: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_alpha: Option<bool>,
    pub is_animated: Option<bool>,
    pub frame_count: Option<u32>,
    pub rating: u8,
    pub triage: String,
    pub favorite: bool,
    /// 現行タグ付与 (正規化名 + 由来。手動編集フラグ保持)。
    pub tags: Vec<BackupTagEntry>,
    /// 現行キャプション (kindごと。履歴は対象外 — 現行から再生成できる)。
    pub captions: Vec<BackupCaptionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupTagEntry {
    pub display_name: String,
    pub normalized_name: String,
    pub source: String,
    pub manually_edited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupCaptionEntry {
    pub kind: String,
    pub body: String,
    pub source: String,
    pub language: Option<String>,
    pub manually_edited: bool,
}

/// file単位の所在情報 (metadata.jsonlのfile行)。
/// `rel_path` は百分号エスケープ正準形 (非ASCII・不正バイトも保持)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupFileEntry {
    pub uuid: String,
    pub asset_uuid: String,
    pub rel_path: String,
    pub display_name: String,
    pub extension: Option<String>,
    pub byte_size: u64,
    pub mtime_ms: u64,
    pub mtime_ns: Option<u64>,
    /// (dev, ino)。Linux以外や未取得はNone。
    pub os_file_id: Option<(u64, u64)>,
    pub scan_state: String,
}

/// コレクション (collections.json)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupCollectionEntry {
    pub uuid: String,
    pub name: String,
    pub kind: String,
    /// smartのみ (保存済み正規JSONをそのまま)。
    pub query_json: Option<String>,
    pub items: Vec<BackupCollectionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupCollectionItem {
    pub asset_uuid: String,
    pub position: u32,
    pub note: Option<String>,
}

/// metadata.jsonlの1行 (タグ付き)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MetadataRow {
    Asset(BackupAssetEntry),
    File(BackupFileEntry),
}

/// digest対象ファイルの情報。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupFileDigest {
    pub name: String,
    /// ファイル内容のBLAKE3 hex (64文字。接頭辞なし)。
    pub blake3: String,
}

/// backup-info.json。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupInfo {
    pub format_version: u32,
    pub api_version: u32,
    pub app_version: String,
    pub created_at: String,
    pub root_uuid: String,
    pub root_display_path: String,
    pub asset_count: u64,
    pub file_count: u64,
    pub collection_count: u64,
    pub file_digests: Vec<BackupFileDigest>,
    /// 全digest行の集約 (下のモジュールdoc参照)。
    pub digest: String,
    pub complete: bool,
}

/// 開いた (検証済み) ポータブルバックアップ。
#[derive(Debug, Clone)]
pub struct PortableBackup {
    pub dir: PathBuf,
    pub info: BackupInfo,
    pub assets: Vec<BackupAssetEntry>,
    pub files: Vec<BackupFileEntry>,
    pub collections: Vec<BackupCollectionEntry>,
}

// ===== 復元plan =====

/// マッチング結果の分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMatch {
    /// asset UUIDが完全一致 (同一内容)。
    Uuid,
    /// content_hashで再関連付け (パス移動後の復元)。
    Hash,
    /// DBに無いがディスク上の同一パスに内容一致 (新規追加)。
    PathAdd,
    /// 同一UUIDに別内容、またはバックアップ経路に別内容が存在。
    Conflict,
    /// バックアップにのみ存在し、現DBにもディスクにも無い (または不明)。
    Missing,
    /// 同一ハッシュが現DBで複数箇所に存在 (対応づけ不能)。
    Ambiguous,
}

impl RestoreMatch {
    pub const fn as_str(self) -> &'static str {
        match self {
            RestoreMatch::Uuid => "uuid",
            RestoreMatch::Hash => "hash",
            RestoreMatch::PathAdd => "path_add",
            RestoreMatch::Conflict => "conflict",
            RestoreMatch::Missing => "missing",
            RestoreMatch::Ambiguous => "ambiguous",
        }
    }
}

/// 実行されるaction。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    /// メタデータ完全一致 (何もしない)。
    Same,
    /// メタデータを更新する。
    Update,
    /// asset + file行を新規作成する。
    Add,
    /// 適用しない (レポートのみ)。
    Skip,
}

impl RestoreAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            RestoreAction::Same => "same",
            RestoreAction::Update => "update",
            RestoreAction::Add => "add",
            RestoreAction::Skip => "skip",
        }
    }
}

/// ファイル所在ごとの報告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFileReport {
    /// バックアップ側の相対パス (正準形のまま)。
    pub backup_rel_path: String,
    /// 現DBでの相対パス (パス一致またはhash再関連付けで判明した場合)。
    pub current_rel_path: Option<String>,
    /// 対象ルート配下に実体があるか。
    pub on_disk: bool,
}

/// asset単位のplan項目。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreEntry {
    pub asset_uuid: String,
    pub content_hash: String,
    pub display_name: String,
    /// 現DBのasset行id (Uuid/Hash一致時)。
    pub current_asset_id: Option<i64>,
    pub matched: RestoreMatch,
    pub action: RestoreAction,
    /// 人が読む理由 (日本語)。
    pub reason: String,
    pub files: Vec<RestoreFileReport>,
}

/// コレクション復元のplan項目。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreCollectionReport {
    pub uuid: String,
    pub name: String,
    pub kind: String,
    /// 新規作成か (false = 既存uuidへ項目マージ)。
    pub will_create: bool,
    pub items_total: u64,
    /// 復元対象のassetに解決できる項目数。
    pub items_resolvable: u64,
}

/// plan全体の集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreSummary {
    pub same: u64,
    pub update: u64,
    pub add: u64,
    pub conflict: u64,
    pub missing: u64,
    pub ambiguous: u64,
}

/// 復元plan (dry-run結果)。
#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub root_uuid: String,
    pub root_display_path: String,
    pub created_at: String,
    pub target_root_id: i64,
    pub target_root_path: String,
    pub entries: Vec<RestoreEntry>,
    pub collections: Vec<RestoreCollectionReport>,
    pub summary: RestoreSummary,
}

// ===== 復元実行結果 =====

/// 適用したassetの結果。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreApplied {
    pub asset_uuid: String,
    pub action: RestoreAction,
    /// 変更したフィールド (rating/triage/favorite/tags/captions/created)。
    pub changed: Vec<String>,
}

/// スキップ報告。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreSkipped {
    pub asset_uuid: String,
    pub matched: RestoreMatch,
    pub reason: String,
}

/// 復元実行結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreResult {
    pub applied: Vec<RestoreApplied>,
    pub skipped: Vec<RestoreSkipped>,
    pub collections_created: u64,
    pub collections_merged: u64,
    pub collection_items_added: u64,
    /// 対象assetが解決できなかったコレクション項目。
    pub collection_items_unresolved: u64,
    /// FTS文書を再構築したasset数。
    pub fts_rebuilt: u64,
}

// ===== 実装 (公開API) =====

/// with_reader/with_writerクロージャはRepoErrorを返すための変換。
fn repo_err(e: BackupError) -> RepoError {
    RepoError::Db(e.to_string())
}

/// バックアップ実行用の短命接続。
///
/// `VACUUM INTO` は対象DBへは書き込まないが、reader接続の
/// `PRAGMA query_only` に拒否されるため、専用に開く
/// (writer接続を長時間専有しない — DB並行性方針)。
fn open_backup_conn(db: &Db) -> Result<Connection, BackupError> {
    let conn = Connection::open(db.path())
        .map_err(|e| BackupError::Io(format!("open {}: {e}", db.path().display())))?;
    conn.busy_timeout(std::time::Duration::from_millis(
        crate::DEFAULT_BUSY_TIMEOUT_MS,
    ))
    .map_err(|e| BackupError::Io(format!("set busy_timeout: {e}")))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| BackupError::Io(format!("set foreign_keys: {e}")))?;
    Ok(conn)
}

/// 保持世代数設定の読み取り (無設定・不正は既定値)。
fn retention_setting(db: &Db) -> Option<usize> {
    db.with_reader(|repos| repos.setting_get(SETTING_BACKUP_RETENTION))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1)
}

/// フルDBバックアップを作成する (VACUUM INTO + integrity_check + 世代回収)。
///
/// `dest_dir` が `None` なら既定 (`db-backups/`)。保持世代数は
/// `backup.retention_generations` 設定 (不正なら既定3)。
pub fn full_db_backup(
    db: &Db,
    dest_dir: Option<&Path>,
) -> Result<FullDbBackupOutcome, BackupError> {
    let dest_dir = dest_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::default_backup_dir(db.path()));
    let keep = retention_setting(db)
        .unwrap_or(DEFAULT_BACKUP_RETENTION)
        .max(1);
    std::fs::create_dir_all(&dest_dir)?;

    // 同一ミリ秒の連続作成に備えて衝突時は連番suffix (VACUUM INTOは
    // 既存ファイルへの書き込みを拒否する)。
    let ts = now_utc_compact();
    let mut dest = dest_dir.join(format!("{FULL_BACKUP_PREFIX}{ts}{FULL_BACKUP_SUFFIX}"));
    let mut suffix = 1u32;
    while dest.exists() {
        dest = dest_dir.join(format!(
            "{FULL_BACKUP_PREFIX}{ts}-{suffix}{FULL_BACKUP_SUFFIX}"
        ));
        suffix += 1;
    }
    let dest_str = dest.to_string_lossy().into_owned();
    let conn = open_backup_conn(db)?;
    conn.execute("VACUUM INTO ?1", [dest_str.as_str()])
        .map_err(|e| BackupError::Db(format!("VACUUM INTO {dest_str}: {e}")))?;
    drop(conn);

    // 検証: 開いてintegrity_check (READ_ONLYにしない — FTS5整合性検査が
    // 内部で書き込みを試みるため。migrate.rsと同じ理由)。
    let check = Connection::open(&dest)
        .map_err(|e| BackupError::Io(format!("verify {}: {e}", dest.display())))?;
    let result: String = check
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| BackupError::Io(format!("integrity check {}: {e}", dest.display())))?;
    drop(check);
    if result != "ok" {
        // 不正backupは世代計算を汚さないよう削除する
        let _ = std::fs::remove_file(&dest);
        return Err(BackupError::Db(format!(
            "backup integrity check failed: {result}"
        )));
    }

    rotate_full_backups(&dest_dir, keep)?;
    let kept_generations = list_full_backups(&dest_dir).len();
    Ok(FullDbBackupOutcome {
        path: dest,
        kept_generations,
    })
}

fn list_full_backups(dir: &Path) -> Vec<PathBuf> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|iter| {
            iter.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| {
                            n.starts_with(FULL_BACKUP_PREFIX) && n.ends_with(FULL_BACKUP_SUFFIX)
                        })
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    backups.sort();
    backups
}

/// `library-full-*.sqlite3` を新しい方から `keep` 世代残す。
/// ファイル名はタイムスタンプ先行の固定幅のためソート = 時系列
/// (migration用 `library-pre-*` は接頭辞が違い対象外)。
fn rotate_full_backups(dir: &Path, keep: usize) -> Result<(), BackupError> {
    let mut backups = list_full_backups(dir);
    while backups.len() > keep {
        let oldest = backups.remove(0);
        std::fs::remove_file(&oldest)?;
    }
    Ok(())
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 自動バックアップ頻度の設定読み取り (無設定・不正値はOff)。
pub fn auto_frequency(db: &Db) -> AutoBackupFrequency {
    let raw = db
        .with_reader(|repos| repos.setting_get(SETTING_BACKUP_AUTO_FULL_DB))
        .ok()
        .flatten()
        .unwrap_or_else(|| AutoBackupFrequency::Off.as_str().to_string());
    AutoBackupFrequency::parse(&raw)
}

/// 期限が来ていればフルDBバックアップを実行する。
/// 戻り値は実行した場合のみSome。実行時に前回時刻を更新する。
pub fn run_auto_backup_if_due(
    db: &Db,
    default_dir: &Path,
) -> Result<Option<FullDbBackupOutcome>, BackupError> {
    let frequency = auto_frequency(db);
    if frequency == AutoBackupFrequency::Off {
        return Ok(None);
    }
    let now = now_epoch_ms();
    let last = db
        .with_reader(|repos| repos.setting_get(SETTING_BACKUP_LAST_AUTO))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(last) = last {
        if now.saturating_sub(last) < frequency.interval_ms() {
            return Ok(None);
        }
    }
    let outcome = full_db_backup(db, Some(default_dir))?;
    db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_LAST_AUTO, &now.to_string()))
        .map_err(|e| BackupError::Db(e.to_string()))?;
    Ok(Some(outcome))
}

/// パスの生バイト (backup正準表現への入力)。
fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

/// ディレクトリ名に使えるルート名 (最終パス成分。区切り・制御文字・
/// 先頭ドットを除去。空なら "root")。
fn sanitize_root_name(display_path: &str) -> String {
    let last = Path::new(display_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| display_path.to_string());
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || c == '/' || c == '\\' || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_start_matches(['.', '_']);
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.to_string()
    }
}

/// バックアップ対象の一括読み出し (単一readerトランザクション相当)。
struct BackupSource {
    root_uuid: String,
    root_display_path: String,
    assets: Vec<BackupAssetEntry>,
    files: Vec<BackupFileEntry>,
    collections: Vec<BackupCollectionEntry>,
}

fn collect_backup_source(
    repos: &SqliteRepos<'_>,
    root_id: i64,
) -> Result<BackupSource, BackupError> {
    let conn = repos.conn();
    let root = repos
        .get_root(root_id)?
        .ok_or(BackupError::RootNotFound(root_id))?;

    // files (root配下全行。走査状態も保存)
    let mut files = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT uuid, asset_id, rel_path, display_name, extension, byte_size, \
                 mtime_ms, mtime_ns, os_file_id, scan_state \
                 FROM files WHERE root_id = ?1 ORDER BY id",
            )
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let mapped = stmt
            .query_map([root_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let rows = mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| BackupError::Db(e.to_string()))?;
        for (
            uuid,
            asset_id,
            rel,
            display_name,
            extension,
            byte_size,
            mtime_ms,
            mtime_ns,
            os_file_id,
            scan_state,
        ) in rows
        {
            let asset_uuid = match asset_id {
                Some(id) => asset_uuid_of(conn, id)?,
                None => continue, // Asset未リンクの走査中Fileは対象外
            };
            let os_file_id = os_file_id
                .as_deref()
                .and_then(gento_core::repo::OsFileId::from_bytes)
                .map(|id| (id.dev, id.ino));
            files.push(BackupFileEntry {
                uuid,
                asset_uuid,
                rel_path: percent_encode_bytes(&rel),
                display_name,
                extension,
                byte_size: byte_size.max(0) as u64,
                mtime_ms: mtime_ms.max(0) as u64,
                mtime_ns: mtime_ns.and_then(|v| u64::try_from(v).ok()),
                os_file_id,
                scan_state,
            });
        }
    }

    // assets (ファイルから参照されるもの = 保守対象)
    let asset_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT asset_id FROM files \
                 WHERE root_id = ?1 AND asset_id IS NOT NULL ORDER BY asset_id",
            )
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let mapped = stmt
            .query_map([root_id], |row| row.get::<_, i64>(0))
            .map_err(|e| BackupError::Db(e.to_string()))?;
        mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| BackupError::Db(e.to_string()))?
    };
    let mut assets = Vec::with_capacity(asset_ids.len());
    for id in asset_ids {
        assets.push(read_backup_asset(conn, id)?);
    }

    // collections (全コレクション。manual項目はasset uuid参照)
    let mut collections = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, uuid, name, kind, query_json FROM collections ORDER BY id")
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let rows: Vec<(i64, String, String, String, Option<String>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| BackupError::Db(e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| BackupError::Db(e.to_string()))?;
        for (id, uuid, name, kind, query_json) in rows {
            let mut items = Vec::new();
            let mut item_stmt = conn
                .prepare(
                    "SELECT asset_id, position, note FROM collection_items \
                     WHERE collection_id = ?1 ORDER BY position",
                )
                .map_err(|e| BackupError::Db(e.to_string()))?;
            let item_rows: Vec<(i64, i64, Option<String>)> = item_stmt
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|e| BackupError::Db(e.to_string()))?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| BackupError::Db(e.to_string()))?;
            for (asset_id, position, note) in item_rows {
                items.push(BackupCollectionItem {
                    asset_uuid: asset_uuid_of(conn, asset_id)?,
                    position: position.max(0) as u32,
                    note,
                });
            }
            collections.push(BackupCollectionEntry {
                uuid,
                name,
                kind,
                query_json,
                items,
            });
        }
    }

    Ok(BackupSource {
        root_uuid: root.uuid,
        root_display_path: root.display_path,
        assets,
        files,
        collections,
    })
}

fn asset_uuid_of(conn: &Connection, asset_id: i64) -> Result<String, BackupError> {
    conn.query_row("SELECT uuid FROM assets WHERE id = ?1", [asset_id], |r| {
        r.get::<_, String>(0)
    })
    .map_err(|e| BackupError::Db(format!("asset {asset_id} uuid: {e}")))
}

/// read_backup_assetのSELECT行 (複雑型のためalias)。
type AssetRow = (
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    String,
    i64,
);

/// asset行を保守メタデータ付きで読む。
fn read_backup_asset(conn: &Connection, asset_id: i64) -> Result<BackupAssetEntry, BackupError> {
    let row: AssetRow = conn
        .query_row(
            "SELECT uuid, content_hash, hash_algorithm, byte_size, mime_type, width, height, \
             has_alpha, is_animated, frame_count, rating, triage, favorite \
             FROM assets WHERE id = ?1",
            [asset_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .map_err(|e| BackupError::Db(format!("asset {asset_id}: {e}")))?;
    let (
        uuid,
        content_hash,
        hash_algorithm,
        byte_size,
        mime_type,
        width,
        height,
        has_alpha,
        is_animated,
        frame_count,
        rating,
        triage,
        favorite,
    ) = row;

    let mut tags = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT t.display_name, t.normalized_name, at.source, at.manually_edited \
                 FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
                 WHERE at.asset_id = ?1 ORDER BY t.normalized_name",
            )
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let rows: Vec<(String, String, String, i64)> = stmt
            .query_map([asset_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|e| BackupError::Db(e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| BackupError::Db(e.to_string()))?;
        for (display_name, normalized_name, source, manually_edited) in rows {
            tags.push(BackupTagEntry {
                display_name,
                normalized_name,
                source,
                manually_edited: manually_edited != 0,
            });
        }
    }
    let mut captions = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT kind, body, source, language, manually_edited FROM captions \
                 WHERE asset_id = ?1 AND current = 1 ORDER BY kind",
            )
            .map_err(|e| BackupError::Db(e.to_string()))?;
        let rows: Vec<(String, String, String, Option<String>, i64)> = stmt
            .query_map([asset_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| BackupError::Db(e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| BackupError::Db(e.to_string()))?;
        for (kind, body, source, language, manually_edited) in rows {
            captions.push(BackupCaptionEntry {
                kind,
                body,
                source,
                language,
                manually_edited: manually_edited != 0,
            });
        }
    }

    Ok(BackupAssetEntry {
        uuid,
        content_hash,
        hash_algorithm,
        byte_size: byte_size.max(0) as u64,
        mime_type,
        width: width.and_then(|v| u32::try_from(v).ok()),
        height: height.and_then(|v| u32::try_from(v).ok()),
        has_alpha: has_alpha.map(|v| v != 0),
        is_animated: is_animated.map(|v| v != 0),
        frame_count: frame_count.and_then(|v| u32::try_from(v).ok()),
        rating: rating.clamp(0, 5) as u8,
        triage,
        favorite: favorite != 0,
        tags,
        captions,
    })
}

/// バックアップディレクトリへ内容を書き出す (backup-info.jsonは最後)。
fn write_backup_dir(dir: &Path, source: &BackupSource) -> Result<BackupInfo, BackupError> {
    // metadata.jsonl (asset行→file行の順。ストリーム書き出し)
    {
        let mut out = String::new();
        for asset in &source.assets {
            let line = serde_json::to_string(&MetadataRow::Asset(asset.clone()))
                .map_err(|e| BackupError::Format(e.to_string()))?;
            out.push_str(&line);
            out.push('\n');
        }
        for file in &source.files {
            let line = serde_json::to_string(&MetadataRow::File(file.clone()))
                .map_err(|e| BackupError::Format(e.to_string()))?;
            out.push_str(&line);
            out.push('\n');
        }
        std::fs::write(dir.join(METADATA_JSONL_FILE), out)?;
    }
    // collections.json
    {
        let json = serde_json::json!({ "collections": source.collections });
        std::fs::write(dir.join(COLLECTIONS_JSON_FILE), json.to_string())?;
    }
    // digest (名前順。backup-info.json自体は対象外)
    let mut names = [COLLECTIONS_JSON_FILE, METADATA_JSONL_FILE];
    names.sort_unstable();
    let mut digests = Vec::with_capacity(names.len());
    let mut aggregate = String::new();
    for name in names {
        let bytes = std::fs::read(dir.join(name))?;
        let hex = blake3::hash(&bytes).to_hex().to_string();
        aggregate.push_str(&format!("{name}:{hex}\n"));
        digests.push(BackupFileDigest {
            name: name.to_string(),
            blake3: hex,
        });
    }
    // backup-info.json は最後に書く (complete:true = 全ファイル書き完了の印)
    let info = BackupInfo {
        format_version: BACKUP_FORMAT_VERSION,
        api_version: gento_core::GENTO_API_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: now_utc_rfc3339(),
        root_uuid: source.root_uuid.clone(),
        root_display_path: source.root_display_path.clone(),
        asset_count: source.assets.len() as u64,
        file_count: source.files.len() as u64,
        collection_count: source.collections.len() as u64,
        file_digests: digests,
        digest: format!("blake3:{}", blake3::hash(aggregate.as_bytes()).to_hex()),
        complete: true,
    };
    std::fs::write(
        dir.join(BACKUP_INFO_FILE),
        serde_json::to_string_pretty(&info).map_err(|e| BackupError::Format(e.to_string()))?,
    )?;
    Ok(info)
}

/// ポータブルバックアップを作成し、最終ディレクトリパスを返す。
pub fn create_portable_backup(
    db: &Db,
    root_id: i64,
    dest_parent: &Path,
) -> Result<PathBuf, BackupError> {
    // RootNotFoundは構造化したまま返す (クロージャ変換で文字列化しない)
    let root_exists = db.with_reader(|repos| repos.get_root(root_id).map(|r| r.is_some()))?;
    if !root_exists {
        return Err(BackupError::RootNotFound(root_id));
    }
    let source = db.with_reader(|repos| collect_backup_source(repos, root_id).map_err(repo_err))?;
    std::fs::create_dir_all(dest_parent)?;
    let tmp_dir = dest_parent.join(format!("{TMP_DIR_PREFIX}{}", new_uuid_v7()));
    std::fs::create_dir_all(&tmp_dir)?;
    if let Err(e) = write_backup_dir(&tmp_dir, &source) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    // 原子的リネーム (同一ファイルシステム上)。最終名が既に存在する場合は
    // 上書きせず連番suffixへ (無断上書き禁止)。
    let ts = now_utc_compact();
    let base = sanitize_root_name(&source.root_display_path);
    let mut final_name = format!("{base}-{ts}.gento");
    let mut suffix = 1u32;
    while dest_parent.join(&final_name).exists() {
        final_name = format!("{base}-{ts}-{suffix}.gento");
        suffix += 1;
    }
    let final_path = dest_parent.join(&final_name);
    std::fs::rename(&tmp_dir, &final_path)?;
    Ok(final_path)
}

/// ディスク上のファイル内容ハッシュ (`blake3:<hex>`。gento-imageと同一形式)。
fn hash_file_blake3(path: &Path) -> Result<String, BackupError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| BackupError::Io(format!("open {}: {e}", path.display())))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| BackupError::Io(format!("read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// パスの在不在3分類 (Path::existsは権限エラーでもfalseになるため使わない)。
enum DiskStatus {
    File,
    Absent,
    Unknown,
}

fn disk_status(path: &Path) -> DiskStatus {
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.is_file() => DiskStatus::File,
        Ok(_) => DiskStatus::Absent, // ディレクトリ等は画像実体ではない
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiskStatus::Absent,
        Err(_) => DiskStatus::Unknown,
    }
}

/// migrate_to_currentの引き渡し集合 (複雑型のためalias)。
type MigratedBackup = (
    Vec<BackupAssetEntry>,
    Vec<BackupFileEntry>,
    Vec<BackupCollectionEntry>,
);

/// format_versionに応じた読み取り変換 (将来欄。現状v1のみ)。
fn migrate_to_current(
    format_version: u32,
    assets: Vec<BackupAssetEntry>,
    files: Vec<BackupFileEntry>,
    collections: Vec<BackupCollectionEntry>,
) -> Result<MigratedBackup, BackupError> {
    match format_version {
        1 => Ok((assets, files, collections)),
        other => Err(BackupError::UnsupportedFormat {
            found: other,
            supported: BACKUP_FORMAT_VERSION,
        }),
    }
}

/// ポータブルバックアップを開いて検証する (complete + digest + 形式)。
pub fn open_portable_backup(dir: &Path) -> Result<PortableBackup, BackupError> {
    let dir_display = dir.display().to_string();
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with(TMP_DIR_PREFIX) {
        return Err(BackupError::Format(format!(
            "{dir_display} looks like an in-progress temporary backup ({TMP_DIR_PREFIX}...); \
             incomplete backups are never treated as valid"
        )));
    }
    let info_path = dir.join(BACKUP_INFO_FILE);
    if !info_path.is_file() {
        return Err(BackupError::Format(format!(
            "{dir_display} has no {BACKUP_INFO_FILE}"
        )));
    }
    let info: BackupInfo = serde_json::from_str(
        &std::fs::read_to_string(&info_path)
            .map_err(|e| BackupError::Io(format!("read {}: {e}", info_path.display())))?,
    )
    .map_err(|e| BackupError::Format(format!("parse {BACKUP_INFO_FILE}: {e}")))?;
    if info.format_version > BACKUP_FORMAT_VERSION {
        return Err(BackupError::UnsupportedFormat {
            found: info.format_version,
            supported: BACKUP_FORMAT_VERSION,
        });
    }
    if !info.complete {
        return Err(BackupError::Incomplete { dir: dir_display });
    }
    // digest検証: 全対象ファイルの内容BLAKE3 + 集約
    let mut sorted = info.file_digests.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut aggregate = String::new();
    let known = [COLLECTIONS_JSON_FILE, METADATA_JSONL_FILE];
    for fd in &sorted {
        if !known.contains(&fd.name.as_str()) {
            return Err(BackupError::DigestMismatch {
                dir: dir_display.clone(),
                detail: format!("unexpected file in digest list: {}", fd.name),
            });
        }
        let path = dir.join(&fd.name);
        let bytes = std::fs::read(&path).map_err(|e| BackupError::DigestMismatch {
            dir: dir_display.clone(),
            detail: format!("cannot read {}: {e}", path.display()),
        })?;
        let hex = blake3::hash(&bytes).to_hex().to_string();
        if hex != fd.blake3 {
            return Err(BackupError::DigestMismatch {
                dir: dir_display.clone(),
                detail: format!("{} content digest mismatch", fd.name),
            });
        }
        aggregate.push_str(&format!("{}:{}\n", fd.name, hex));
    }
    let digest = format!("blake3:{}", blake3::hash(aggregate.as_bytes()).to_hex());
    if digest != info.digest {
        return Err(BackupError::DigestMismatch {
            dir: dir_display.clone(),
            detail: "aggregate digest mismatch".to_string(),
        });
    }

    // metadata.jsonl解析
    let mut assets = Vec::new();
    let mut files = Vec::new();
    let jsonl = std::fs::read_to_string(dir.join(METADATA_JSONL_FILE))
        .map_err(|e| BackupError::Io(format!("read {METADATA_JSONL_FILE}: {e}")))?;
    for (index, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: MetadataRow = serde_json::from_str(line).map_err(|e| {
            BackupError::Format(format!("{METADATA_JSONL_FILE} line {}: {e}", index + 1))
        })?;
        match row {
            MetadataRow::Asset(a) => assets.push(a),
            MetadataRow::File(f) => files.push(f),
        }
    }
    // collections.json解析
    let collections: Vec<BackupCollectionEntry> = {
        #[derive(Deserialize)]
        struct CollectionsFile {
            collections: Vec<BackupCollectionEntry>,
        }
        let parsed: CollectionsFile = serde_json::from_str(
            &std::fs::read_to_string(dir.join(COLLECTIONS_JSON_FILE))
                .map_err(|e| BackupError::Io(format!("read {COLLECTIONS_JSON_FILE}: {e}")))?,
        )
        .map_err(|e| BackupError::Format(format!("parse {COLLECTIONS_JSON_FILE}: {e}")))?;
        parsed.collections
    };

    let (assets, files, collections) =
        migrate_to_current(info.format_version, assets, files, collections)?;
    Ok(PortableBackup {
        dir: dir.to_path_buf(),
        info,
        assets,
        files,
        collections,
    })
}

/// 現DBの照合用スナップショット (plan構築ごとに1回読む)。
struct CurrentState {
    /// asset uuid → (id, content_hash)
    by_uuid: std::collections::HashMap<String, (i64, String)>,
    /// content_hash → id (UNIQUE)
    by_hash: std::collections::HashMap<String, i64>,
    /// asset id → (rating, triage, favorite)
    asset_flags: std::collections::HashMap<i64, (u8, String, bool)>,
    /// asset id → presentなfile行数
    present_locations: std::collections::HashMap<i64, u64>,
    /// 対象ルート内 rel_path正準形 → asset_id
    files_in_root: std::collections::HashMap<String, i64>,
    /// asset id → 代表presentパス (正準形)
    representative_path: std::collections::HashMap<i64, String>,
}

fn read_current_state(conn: &Connection, root_id: i64) -> Result<CurrentState, BackupError> {
    let db_err = |e: rusqlite::Error| BackupError::Db(e.to_string());
    let mut by_uuid = std::collections::HashMap::new();
    let mut by_hash = std::collections::HashMap::new();
    let mut asset_flags = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, uuid, content_hash, rating, triage, favorite FROM assets")
            .map_err(db_err)?;
        let rows: Vec<(i64, String, String, i64, String, i64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db_err)?;
        for (id, uuid, hash, rating, triage, favorite) in rows {
            by_uuid.insert(uuid, (id, hash.clone()));
            by_hash.insert(hash, id);
            asset_flags.insert(id, (rating.clamp(0, 5) as u8, triage, favorite != 0));
        }
    }
    let mut present_locations = std::collections::HashMap::new();
    let mut representative_path = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT asset_id, rel_path FROM files \
                 WHERE scan_state = 'present' AND asset_id IS NOT NULL ORDER BY id",
            )
            .map_err(db_err)?;
        let rows: Vec<(i64, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db_err)?;
        for (asset_id, rel) in rows {
            *present_locations.entry(asset_id).or_insert(0) += 1;
            representative_path
                .entry(asset_id)
                .or_insert_with(|| percent_encode_bytes(&rel));
        }
    }
    let mut files_in_root = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT rel_path, asset_id FROM files WHERE root_id = ?1 AND asset_id IS NOT NULL",
            )
            .map_err(db_err)?;
        let rows: Vec<(Vec<u8>, i64)> = stmt
            .query_map([root_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db_err)?;
        for (rel, asset_id) in rows {
            files_in_root.insert(percent_encode_bytes(&rel), asset_id);
        }
    }
    Ok(CurrentState {
        by_uuid,
        by_hash,
        asset_flags,
        present_locations,
        files_in_root,
        representative_path,
    })
}

/// 現DB側のassetメタデータとbackup側が一致するか (Same/Update判定)。
fn metadata_matches(
    conn: &Connection,
    current_asset_id: i64,
    backup: &BackupAssetEntry,
    state: &CurrentState,
) -> Result<bool, BackupError> {
    let db_err = |e: rusqlite::Error| BackupError::Db(e.to_string());
    let Some((rating, triage, favorite)) = state.asset_flags.get(&current_asset_id) else {
        return Ok(false);
    };
    if *rating != backup.rating || *triage != backup.triage || *favorite != backup.favorite {
        return Ok(false);
    }
    // タグ: 正規化名集合の比較 (現DB側が上位集合でも「更新」で併合するため
    // 一致判定は集合等価でなく backup ⊆ current かつ current ⊆ backup)。
    let current_tags: std::collections::HashSet<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT t.normalized_name FROM asset_tags at \
                 JOIN tags t ON t.id = at.tag_id WHERE at.asset_id = ?1",
            )
            .map_err(db_err)?;
        let mapped = stmt
            .query_map([current_asset_id], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        mapped
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()
            .map_err(db_err)?
    };
    let backup_tags: std::collections::HashSet<String> = backup
        .tags
        .iter()
        .map(|t| t.normalized_name.clone())
        .collect();
    if current_tags != backup_tags {
        return Ok(false);
    }
    // 現行キャプション: kind→body の比較 (backup側のkindのみ)
    for caption in &backup.captions {
        let current_body: Option<String> = conn
            .query_row(
                "SELECT body FROM captions WHERE asset_id = ?1 AND kind = ?2 AND current = 1",
                rusqlite::params![current_asset_id, caption.kind],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if current_body.as_deref() != Some(caption.body.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 復元plan (dry-run) を構築する。
///
/// `target_root_id` が `None` の場合: バックアップのroot uuidと一致する
/// ルートがあればそれを使い、無ければルートがちょうど1つならそれを使う。
/// いずれも不可なら [`BackupError::NoTargetRoot`]。
pub fn build_restore_plan(
    db: &Db,
    backup: &PortableBackup,
    target_root_id: Option<i64>,
) -> Result<RestorePlan, BackupError> {
    // ルート解決
    let roots = db.with_reader(|repos| repos.list_roots())?;
    let target = match target_root_id {
        Some(id) => roots
            .iter()
            .find(|r| r.id == id)
            .ok_or(BackupError::RootNotFound(id))?,
        None => roots
            .iter()
            .find(|r| r.uuid == backup.info.root_uuid)
            .or_else(|| (roots.len() == 1).then(|| &roots[0]))
            .ok_or(BackupError::NoTargetRoot {
                backup_root_uuid: backup.info.root_uuid.clone(),
            })?,
    };

    // asset uuid → file列 (表示用)
    let mut files_of: std::collections::HashMap<&str, Vec<&BackupFileEntry>> =
        std::collections::HashMap::new();
    for f in &backup.files {
        files_of.entry(f.asset_uuid.as_str()).or_default().push(f);
    }

    let entries = db.with_reader(|repos| {
        plan_entries(repos.conn(), backup, target, &files_of).map_err(repo_err)
    })?;

    let mut summary = RestoreSummary::default();
    for e in &entries {
        match e.action {
            RestoreAction::Same => summary.same += 1,
            RestoreAction::Update => summary.update += 1,
            RestoreAction::Add => summary.add += 1,
            RestoreAction::Skip => match e.matched {
                RestoreMatch::Conflict => summary.conflict += 1,
                RestoreMatch::Missing => summary.missing += 1,
                RestoreMatch::Ambiguous => summary.ambiguous += 1,
                _ => {}
            },
        }
    }

    // コレクション: uuid既存か、項目が解決できるか
    let current_collection_uuids: std::collections::HashSet<String> = db
        .with_reader(|repos| {
            let mut stmt = repos
                .conn()
                .prepare("SELECT uuid FROM collections")
                .map_err(|e| RepoError::Db(e.to_string()))?;
            let mapped = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| RepoError::Db(e.to_string()))?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::Db(e.to_string()))
        })
        .map_err(BackupError::from)?
        .into_iter()
        .collect();
    let resolvable: std::collections::HashSet<&str> = entries
        .iter()
        .filter(|e| e.action != RestoreAction::Skip)
        .map(|e| e.asset_uuid.as_str())
        .collect();
    let collections = backup
        .collections
        .iter()
        .map(|c| RestoreCollectionReport {
            uuid: c.uuid.clone(),
            name: c.name.clone(),
            kind: c.kind.clone(),
            will_create: !current_collection_uuids.contains(&c.uuid),
            items_total: c.items.len() as u64,
            items_resolvable: c
                .items
                .iter()
                .filter(|i| resolvable.contains(i.asset_uuid.as_str()))
                .count() as u64,
        })
        .collect();

    Ok(RestorePlan {
        root_uuid: backup.info.root_uuid.clone(),
        root_display_path: backup.info.root_display_path.clone(),
        created_at: backup.info.created_at.clone(),
        target_root_id: target.id,
        target_root_path: target.display_path.clone(),
        entries,
        collections,
        summary,
    })
}

/// plan構築の本体 (reader接続上でマッチング)。
fn plan_entries(
    conn: &Connection,
    backup: &PortableBackup,
    target: &gento_core::repo::LibraryRootRecord,
    files_of: &std::collections::HashMap<&str, Vec<&BackupFileEntry>>,
) -> Result<Vec<RestoreEntry>, BackupError> {
    let state = read_current_state(conn, target.id)?;
    let mut entries = Vec::with_capacity(backup.assets.len());
    for asset in &backup.assets {
        let backup_files = files_of
            .get(asset.uuid.as_str())
            .cloned()
            .unwrap_or_default();
        let display_name = backup_files
            .first()
            .map(|f| f.display_name.clone())
            .unwrap_or_else(|| asset.content_hash.clone());

        // ファイル所在レポート (バックアップ側パスの現状)
        let mut file_reports: Vec<RestoreFileReport> = Vec::with_capacity(backup_files.len());
        for f in &backup_files {
            let on_disk = matches!(
                disk_status(&target.path.join(decode_rel(&f.rel_path)?)),
                DiskStatus::File
            );
            file_reports.push(RestoreFileReport {
                backup_rel_path: f.rel_path.clone(),
                current_rel_path: None,
                on_disk,
            });
        }

        let entry = if let Some((cur_id, cur_hash)) = state.by_uuid.get(&asset.uuid) {
            if *cur_hash == asset.content_hash {
                let matched = RestoreMatch::Uuid;
                let same = metadata_matches(conn, *cur_id, asset, &state)?;
                finish_matched(
                    *cur_id,
                    asset,
                    backup_files,
                    matched,
                    same,
                    &state,
                    file_reports,
                    &display_name,
                )?
            } else {
                RestoreEntry {
                    asset_uuid: asset.uuid.clone(),
                    content_hash: asset.content_hash.clone(),
                    display_name,
                    current_asset_id: Some(*cur_id),
                    matched: RestoreMatch::Conflict,
                    action: RestoreAction::Skip,
                    reason: "同一UUIDの現DBアセットは別の内容です (バックアップ作成後に
                        内容が差し替えられています)"
                        .to_string(),
                    files: file_reports,
                }
            }
        } else if let Some(cur_id) = state.by_hash.get(&asset.content_hash) {
            let locations = state.present_locations.get(cur_id).copied().unwrap_or(0);
            if locations >= 2 {
                RestoreEntry {
                    asset_uuid: asset.uuid.clone(),
                    content_hash: asset.content_hash.clone(),
                    display_name,
                    current_asset_id: Some(*cur_id),
                    matched: RestoreMatch::Ambiguous,
                    action: RestoreAction::Skip,
                    reason: format!(
                        "同一内容が現DBで {locations} 箇所に存在するため対応づけを確定できません \
                         (推測して適用しません)"
                    ),
                    files: file_reports,
                }
            } else {
                let same = metadata_matches(conn, *cur_id, asset, &state)?;
                finish_matched(
                    *cur_id,
                    asset,
                    backup_files,
                    RestoreMatch::Hash,
                    same,
                    &state,
                    file_reports,
                    &display_name,
                )?
            }
        } else {
            // DBに無い: パス・ディスク確認
            let mut add_candidates: Vec<String> = Vec::new();
            let mut conflicts: Vec<String> = Vec::new();
            let mut missing: Vec<String> = Vec::new();
            for (i, f) in backup_files.iter().enumerate() {
                let rel = decode_rel(&f.rel_path)?;
                let abs = target.path.join(&rel);
                if state.files_in_root.contains_key(&f.rel_path) {
                    // この分岐に到達した時点でuuid/hash照合は失敗しているため、
                    // 同パスの現DB行は必ず別内容 (hash一致ならby_hashで
                    // マッチ済みのほうへ分岐している)。
                    conflicts.push(format!("{} に現DBの別内容アセットが存在します", f.rel_path));
                    file_reports[i].current_rel_path = Some(f.rel_path.clone());
                } else {
                    match disk_status(&abs) {
                        DiskStatus::File => {
                            if hash_file_blake3(&abs)? == asset.content_hash {
                                add_candidates.push(f.rel_path.clone());
                            } else {
                                conflicts.push(format!(
                                    "{} のディスク実体はバックアップと別内容です",
                                    f.rel_path
                                ));
                            }
                        }
                        DiskStatus::Absent => {
                            missing.push(format!("{} が見つかりません", f.rel_path));
                        }
                        DiskStatus::Unknown => {
                            missing.push(format!("{} の状態を確認できません", f.rel_path));
                        }
                    }
                }
            }
            if !add_candidates.is_empty() {
                RestoreEntry {
                    asset_uuid: asset.uuid.clone(),
                    content_hash: asset.content_hash.clone(),
                    display_name,
                    current_asset_id: None,
                    matched: RestoreMatch::PathAdd,
                    action: RestoreAction::Add,
                    reason: format!(
                        "現DBに無く、ディスクの同一内容ファイルから新規登録します ({})",
                        add_candidates.join(", ")
                    ),
                    files: file_reports,
                }
            } else if !conflicts.is_empty() {
                RestoreEntry {
                    asset_uuid: asset.uuid.clone(),
                    content_hash: asset.content_hash.clone(),
                    display_name,
                    current_asset_id: None,
                    matched: RestoreMatch::Conflict,
                    action: RestoreAction::Skip,
                    reason: conflicts.join(" / "),
                    files: file_reports,
                }
            } else {
                RestoreEntry {
                    asset_uuid: asset.uuid.clone(),
                    content_hash: asset.content_hash.clone(),
                    display_name,
                    current_asset_id: None,
                    matched: RestoreMatch::Missing,
                    action: RestoreAction::Skip,
                    reason: if missing.is_empty() {
                        "バックアップにのみ存在し、現DBにもディスクにも見つかりません".to_string()
                    } else {
                        missing.join(" / ")
                    },
                    files: file_reports,
                }
            }
        };
        entries.push(entry);
    }
    Ok(entries)
}

/// Uuid/Hash一致エントリの組み立て (Same/Update判定と所在報告)。
#[allow(clippy::too_many_arguments)]
fn finish_matched(
    current_asset_id: i64,
    asset: &BackupAssetEntry,
    _backup_files: Vec<&BackupFileEntry>,
    matched: RestoreMatch,
    same: bool,
    state: &CurrentState,
    mut file_reports: Vec<RestoreFileReport>,
    display_name: &str,
) -> Result<RestoreEntry, BackupError> {
    let current_rel = state
        .representative_path
        .get(&current_asset_id)
        .cloned()
        .map(|p| {
            percent_decode_to_bytes(&p)
                .map(|bytes| percent_encode_bytes(&bytes))
                .unwrap_or(p)
        });
    for report in &mut file_reports {
        report.current_rel_path = current_rel.clone();
    }
    let action = if same {
        RestoreAction::Same
    } else {
        RestoreAction::Update
    };
    let reason = match matched {
        RestoreMatch::Uuid => {
            if same {
                "UUID完全一致・メタデータ同一 (変更なし)".to_string()
            } else {
                "UUID完全一致・メタデータをバックアップ時点へ更新します".to_string()
            }
        }
        RestoreMatch::Hash => {
            let path_note = current_rel
                .as_deref()
                .map(|p| format!(" (現DBでは {p} として管理)"))
                .unwrap_or_default();
            if same {
                format!("内容ハッシュで再関連付け済み・メタデータ同一{path_note}")
            } else {
                format!("内容ハッシュで再関連付けしてメタデータを更新します{path_note}")
            }
        }
        other => unreachable!("finish_matched called with {other:?}"),
    };
    let _ = asset;
    Ok(RestoreEntry {
        asset_uuid: asset.uuid.clone(),
        content_hash: asset.content_hash.clone(),
        display_name: display_name.to_string(),
        current_asset_id: Some(current_asset_id),
        matched,
        action,
        reason,
        files: file_reports,
    })
}

/// 正準形rel_path → OSパス。
fn decode_rel(canonical: &str) -> Result<std::path::PathBuf, BackupError> {
    let bytes = percent_decode_to_bytes(canonical).ok_or_else(|| {
        BackupError::Format(format!("invalid percent-encoding in rel_path: {canonical}"))
    })?;
    Ok(decode_path(&bytes))
}

/// 復元を実行する (planを内部で再計算して適用。1トランザクション)。
///
/// 確認トークンの検証はアプリ層 (tauri backup.rs) の責務 — この関数は
/// 渡されたバックアップを機械的に適用する。Uuid/Hash一致→メタデータ更新、
/// PathAdd→新規作成、Conflict/Missing/Ambiguous→スキップして報告。
pub fn execute_restore(
    db: &Db,
    backup: &PortableBackup,
    target_root_id: Option<i64>,
) -> Result<RestoreResult, BackupError> {
    let plan = build_restore_plan(db, backup, target_root_id)?;
    let target = db
        .with_reader(|repos| repos.get_root(plan.target_root_id))?
        .ok_or(BackupError::RootNotFound(plan.target_root_id))?;
    let files_of: std::collections::HashMap<&str, Vec<&BackupFileEntry>> = {
        let mut map: std::collections::HashMap<&str, Vec<&BackupFileEntry>> =
            std::collections::HashMap::new();
        for f in &backup.files {
            map.entry(f.asset_uuid.as_str()).or_default().push(f);
        }
        map
    };
    let backup_assets: std::collections::HashMap<&str, &BackupAssetEntry> =
        backup.assets.iter().map(|a| (a.uuid.as_str(), a)).collect();
    let backup_collections: std::collections::HashMap<&str, &BackupCollectionEntry> = backup
        .collections
        .iter()
        .map(|c| (c.uuid.as_str(), c))
        .collect();

    db.with_writer(move |repos| {
        let conn = repos.conn();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let result = apply_restore_tx(
            &tx,
            &plan,
            &target,
            &files_of,
            &backup_assets,
            &backup_collections,
        );
        match result {
            Ok(result) => {
                tx.commit().map_err(|e| RepoError::Db(e.to_string()))?;
                Ok(result)
            }
            Err(e) => {
                let _ = tx.rollback();
                Err(RepoError::Db(e.to_string()))
            }
        }
    })
    .map_err(BackupError::from)
}

/// 適用の本体 (トランザクション内)。
fn apply_restore_tx(
    tx: &Connection,
    plan: &RestorePlan,
    target: &gento_core::repo::LibraryRootRecord,
    files_of: &std::collections::HashMap<&str, Vec<&BackupFileEntry>>,
    backup_assets: &std::collections::HashMap<&str, &BackupAssetEntry>,
    backup_collections: &std::collections::HashMap<&str, &BackupCollectionEntry>,
) -> Result<RestoreResult, String> {
    let now = now_utc_rfc3339();
    let mut result = RestoreResult::default();
    let mut touched: Vec<i64> = Vec::new();
    // asset uuid → 復元後の現DB asset id (コレクション項目解決用)
    let mut resolved_ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let sql_err = |e: rusqlite::Error| format!("sqlite error: {e}");

    for entry in &plan.entries {
        let Some(asset) = backup_assets.get(entry.asset_uuid.as_str()) else {
            continue;
        };
        match entry.action {
            RestoreAction::Skip => {
                result.skipped.push(RestoreSkipped {
                    asset_uuid: entry.asset_uuid.clone(),
                    matched: entry.matched,
                    reason: entry.reason.clone(),
                });
                continue;
            }
            RestoreAction::Same => {
                if let Some(id) = entry.current_asset_id {
                    resolved_ids.insert(entry.asset_uuid.clone(), id);
                }
                result.applied.push(RestoreApplied {
                    asset_uuid: entry.asset_uuid.clone(),
                    action: RestoreAction::Same,
                    changed: Vec::new(),
                });
                continue;
            }
            RestoreAction::Update => {}
            RestoreAction::Add => {}
        }

        match entry.action {
            RestoreAction::Update => {
                let Some(cur_id) = entry.current_asset_id else {
                    return Err(format!(
                        "update entry without current id: {}",
                        entry.asset_uuid
                    ));
                };
                let mut changed = Vec::new();
                // 評価・選別・お気に入り
                tx.execute(
                    "UPDATE assets SET rating = ?2, triage = ?3, favorite = ?4, updated_at = ?5 \
                     WHERE id = ?1",
                    rusqlite::params![
                        cur_id,
                        asset.rating,
                        asset.triage,
                        asset.favorite as i64,
                        now
                    ],
                )
                .map_err(sql_err)?;
                changed.push("rating".to_string());
                changed.push("triage".to_string());
                changed.push("favorite".to_string());
                apply_tags_and_captions(tx, cur_id, asset, &now, &mut changed)
                    .map_err(|e| format!("tags/captions for {}: {e}", entry.asset_uuid))?;
                touched.push(cur_id);
                resolved_ids.insert(entry.asset_uuid.clone(), cur_id);
                result.applied.push(RestoreApplied {
                    asset_uuid: entry.asset_uuid.clone(),
                    action: RestoreAction::Update,
                    changed,
                });
            }
            RestoreAction::Add => {
                // asset作成 (UUIDはバックアップ保存。万一衝突時は新規UUID)
                let uuid = if uuid_free(tx, "assets", &asset.uuid) {
                    asset.uuid.clone()
                } else {
                    new_uuid_v7()
                };
                tx.execute(
                    "INSERT INTO assets (uuid, content_hash, hash_algorithm, hash_state, \
                     byte_size, mime_type, width, height, has_alpha, is_animated, frame_count, \
                     rating, triage, favorite, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, 'done', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                    rusqlite::params![
                        uuid,
                        asset.content_hash,
                        asset.hash_algorithm,
                        asset.byte_size as i64,
                        asset.mime_type,
                        asset.width,
                        asset.height,
                        asset.has_alpha.map(|b| b as i64),
                        asset.is_animated.map(|b| b as i64),
                        asset.frame_count,
                        asset.rating,
                        asset.triage,
                        asset.favorite as i64,
                        now,
                    ],
                )
                .map_err(sql_err)?;
                let asset_id = tx.last_insert_rowid();
                // 検証済みon-diskファイルのfile行作成 (plan時点でhash確認済みでも
                // 実行時に再検証する — 確定したものだけ登録)
                let mut created_files = 0u64;
                for f in files_of
                    .get(entry.asset_uuid.as_str())
                    .cloned()
                    .unwrap_or_default()
                {
                    let rel = decode_rel(&f.rel_path).map_err(|e| e.to_string())?;
                    let abs = target.path.join(&rel);
                    // 既存行 (同パス) は作らない
                    let exists: bool = tx
                        .query_row(
                            "SELECT 1 FROM files WHERE root_id = ?1 AND rel_path = ?2",
                            rusqlite::params![target.id, path_bytes(&rel)],
                            |_| Ok(true),
                        )
                        .optional()
                        .map_err(sql_err)?
                        .unwrap_or(false);
                    if exists {
                        continue;
                    }
                    if !matches!(disk_status(&abs), DiskStatus::File) {
                        continue;
                    }
                    if hash_file_blake3(&abs).map_err(|e| e.to_string())? != asset.content_hash {
                        continue;
                    }
                    let md = std::fs::metadata(&abs)
                        .map_err(|e| format!("stat {}: {e}", abs.display()))?;
                    #[cfg(unix)]
                    let (mtime_ms, mtime_ns, os_file_id) = {
                        use std::os::unix::fs::MetadataExt;
                        let mtime_ns = (md.mtime() as i64)
                            .saturating_mul(1_000_000_000)
                            .saturating_add(md.mtime_nsec() as i64);
                        let os = gento_core::repo::OsFileId::new(md.dev(), md.ino());
                        (mtime_ns / 1_000_000, mtime_ns, Some(os.to_bytes().to_vec()))
                    };
                    #[cfg(not(unix))]
                    let (mtime_ms, mtime_ns, os_file_id) = {
                        let ns = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_nanos() as i64)
                            .unwrap_or(0);
                        (ns / 1_000_000, ns, None)
                    };
                    let file_uuid = if uuid_free(tx, "files", &f.uuid) {
                        f.uuid.clone()
                    } else {
                        new_uuid_v7()
                    };
                    tx.execute(
                        "INSERT INTO files (uuid, root_id, asset_id, rel_path, path_encoding, \
                         display_name, extension, byte_size, mtime_ms, mtime_ns, os_file_id, \
                         scan_state, last_seen_at, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'present', ?12, ?12, ?12)",
                        rusqlite::params![
                            file_uuid,
                            target.id,
                            asset_id,
                            path_bytes(&rel),
                            path_encoding_of(&rel),
                            f.display_name,
                            f.extension,
                            md.len() as i64,
                            mtime_ms,
                            mtime_ns,
                            os_file_id,
                            now,
                        ],
                    )
                    .map_err(sql_err)?;
                    created_files += 1;
                }
                if created_files == 0 {
                    // plan時点でAddと判定されたが実行時には検証可能な実体が無い
                    // → ロールバックせずスキップ報告 (推測して登録しない)
                    tx.execute("DELETE FROM assets WHERE id = ?1", [asset_id])
                        .map_err(sql_err)?;
                    result.skipped.push(RestoreSkipped {
                        asset_uuid: entry.asset_uuid.clone(),
                        matched: RestoreMatch::Missing,
                        reason: "実行時点で内容を検証できるファイルが見つかりませんでした"
                            .to_string(),
                    });
                    continue;
                }
                let mut changed = vec!["created".to_string()];
                apply_tags_and_captions(tx, asset_id, asset, &now, &mut changed)
                    .map_err(|e| format!("tags/captions for {}: {e}", entry.asset_uuid))?;
                touched.push(asset_id);
                resolved_ids.insert(entry.asset_uuid.clone(), asset_id);
                result.applied.push(RestoreApplied {
                    asset_uuid: entry.asset_uuid.clone(),
                    action: RestoreAction::Add,
                    changed,
                });
            }
            _ => unreachable!("handled above"),
        }
    }

    // コレクション
    for report in &plan.collections {
        let Some(bc) = backup_collections.get(report.uuid.as_str()) else {
            continue;
        };
        if report.will_create {
            // smart: 保存済みquery_jsonを検証してから登録 (不正ならスキップ+未解決計上)
            if bc.kind == "smart" {
                let ok = bc
                    .query_json
                    .as_deref()
                    .and_then(|q| match gento_search::stored::parse_stored(q) {
                        Ok(_) => Some(()),
                        Err(_) => None,
                    })
                    .is_some();
                if !ok {
                    result.collection_items_unresolved += bc.items.len() as u64;
                    continue;
                }
            }
            let uuid = if uuid_free(tx, "collections", &bc.uuid) {
                bc.uuid.clone()
            } else {
                new_uuid_v7()
            };
            tx.execute(
                "INSERT INTO collections (uuid, name, kind, query_json, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![uuid, bc.name, bc.kind, bc.query_json, now],
            )
            .map_err(sql_err)?;
            let collection_id = tx.last_insert_rowid();
            result.collections_created += 1;
            let mut position = 0i64;
            for item in bc.items.iter().filter(|i| i.asset_uuid.len() == 36) {
                let Some(&asset_id) = resolved_ids.get(&item.asset_uuid) else {
                    result.collection_items_unresolved += 1;
                    continue;
                };
                tx.execute(
                    "INSERT INTO collection_items (collection_id, asset_id, position, note, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![collection_id, asset_id, position, item.note, now],
                )
                .map_err(sql_err)?;
                position += 1;
                result.collection_items_added += 1;
            }
        } else {
            // 既存uuidへ項目マージ (冪等: 既に含まれるassetはスキップ)
            let collection_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM collections WHERE uuid = ?1",
                    [&report.uuid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            let Some(collection_id) = collection_id else {
                continue;
            };
            result.collections_merged += 1;
            let mut next_position: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM collection_items \
                     WHERE collection_id = ?1",
                    [collection_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            for item in &bc.items {
                let Some(&asset_id) = resolved_ids.get(&item.asset_uuid) else {
                    result.collection_items_unresolved += 1;
                    continue;
                };
                let already: bool = tx
                    .query_row(
                        "SELECT 1 FROM collection_items WHERE collection_id = ?1 AND asset_id = ?2",
                        rusqlite::params![collection_id, asset_id],
                        |_| Ok(true),
                    )
                    .optional()
                    .map_err(sql_err)?
                    .unwrap_or(false);
                if already {
                    continue;
                }
                tx.execute(
                    "INSERT INTO collection_items (collection_id, asset_id, position, note, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![collection_id, asset_id, next_position, item.note, now],
                )
                .map_err(sql_err)?;
                next_position += 1;
                result.collection_items_added += 1;
            }
        }
    }

    // FTS: 触ったassetの文書を再構築 (派生データ。WP-8のrebuild経路)
    if !touched.is_empty() {
        let rebuilt = crate::search::rebuild_docs_for_assets(tx, &touched)
            .map_err(|e| format!("fts rebuild: {e}"))?;
        result.fts_rebuilt = rebuilt as u64;
    }
    Ok(result)
}

/// テーブルのuuid列が未使用か。
fn uuid_free(tx: &Connection, table: &str, uuid: &str) -> bool {
    let sql = format!("SELECT 1 FROM {table} WHERE uuid = ?1");
    tx.query_row(&sql, [uuid], |_| Ok(true))
        .optional()
        .ok()
        .flatten()
        .is_none()
}

/// rel_pathの格納に対応するpath_encoding値。
fn path_encoding_of(rel: &Path) -> &'static str {
    if rel.to_str().is_some() {
        "utf8"
    } else {
        "unix-bytes"
    }
}

/// タグ・キャプションの適用 (Update/Add共通)。
fn apply_tags_and_captions(
    tx: &Connection,
    asset_id: i64,
    asset: &BackupAssetEntry,
    now: &str,
    changed: &mut Vec<String>,
) -> Result<(), String> {
    let sql_err = |e: rusqlite::Error| format!("sqlite error: {e}");
    let mut tags_added = 0u64;
    for tag in &asset.tags {
        // 正規化名で既存タグを検索 → 無ければ作成 (表記はバックアップ保存)
        let tag_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM tags WHERE normalized_name = ?1",
                [&tag.normalized_name],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        let tag_id = match tag_id {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO tags (uuid, display_name, normalized_name, category, \
                     created_at, updated_at) VALUES (?1, ?2, ?3, NULL, ?4, ?4)",
                    rusqlite::params![new_uuid_v7(), tag.display_name, tag.normalized_name, now],
                )
                .map_err(sql_err)?;
                tx.last_insert_rowid()
            }
        };
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id, source, manually_edited, \
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    asset_id,
                    tag_id,
                    tag.source,
                    tag.manually_edited as i64,
                    now
                ],
            )
            .map_err(sql_err)?;
        tags_added += inserted as u64;
    }
    if tags_added > 0 {
        changed.push("tags".to_string());
    }
    let mut captions_changed = 0u64;
    for caption in &asset.captions {
        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, body FROM captions WHERE asset_id = ?1 AND kind = ?2 AND current = 1",
                rusqlite::params![asset_id, caption.kind],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_err)?;
        match current {
            Some((_id, body)) if body == caption.body => {}
            Some((id, _body)) => {
                // 現行を履歴へ降格し、バックアップ内容を新現行へ
                tx.execute(
                    "UPDATE captions SET current = 0, updated_at = ?2 WHERE id = ?1",
                    rusqlite::params![id, now],
                )
                .map_err(sql_err)?;
                tx.execute(
                    "INSERT INTO captions (uuid, asset_id, kind, body, source, language, \
                     current, manually_edited, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?8)",
                    rusqlite::params![
                        new_uuid_v7(),
                        asset_id,
                        caption.kind,
                        caption.body,
                        caption.source,
                        caption.language,
                        caption.manually_edited as i64,
                        now
                    ],
                )
                .map_err(sql_err)?;
                captions_changed += 1;
            }
            None => {
                tx.execute(
                    "INSERT INTO captions (uuid, asset_id, kind, body, source, language, \
                     current, manually_edited, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?8)",
                    rusqlite::params![
                        new_uuid_v7(),
                        asset_id,
                        caption.kind,
                        caption.body,
                        caption.source,
                        caption.language,
                        caption.manually_edited as i64,
                        now
                    ],
                )
                .map_err(sql_err)?;
                captions_changed += 1;
            }
        }
    }
    if captions_changed > 0 {
        changed.push("captions".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::collections::{CollectionRepo, SmartQuery};
    use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo};
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, OsFileId, ScanState,
        TriageState,
    };

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-backup-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// テスト用の実画像ファイル (中身 = 決定論的バイト列)。
    fn write_image(dir: &Path, rel: &str, seed: u8) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, vec![seed; 64]).unwrap();
        p
    }

    /// ファイル内容のBLAKE3 (`blake3:<hex>`。gento-imageと同一形式)。
    fn hash_of(bytes: &[u8]) -> String {
        format!("blake3:{}", blake3::hash(bytes).to_hex())
    }

    /// テストDBへ root + on-diskファイル + asset + リンクを投入。
    /// 戻り値: (root_id, content_hash)
    fn seed_library(db: &Db, root_dir: impl AsRef<Path>, rel: &str, seed: u8) -> (i64, String) {
        let root_dir = root_dir.as_ref();
        let bytes = vec![seed; 64];
        let hash = hash_of(&bytes);
        let abs = write_image(root_dir, rel, seed);
        let meta = std::fs::metadata(&abs).unwrap();
        #[cfg(unix)]
        let os_file_id = {
            use std::os::unix::fs::MetadataExt;
            Some(OsFileId::new(meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let os_file_id = None;
        let root_id = db.with_writer(|repos| repos.add_root(root_dir)).unwrap().id;
        let file = db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id,
                    rel_path: PathBuf::from(rel),
                    display_name: Path::new(rel)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    byte_size: meta.len(),
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (asset, _) = db
            .with_writer(|repos| {
                repos.create_or_get_asset(NewAsset {
                    content_hash: hash.clone(),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: meta.len(),
                    mime_type: Some("image/png".to_string()),
                    width: Some(8),
                    height: Some(8),
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })
            })
            .unwrap();
        db.with_writer(|repos| repos.link_file(file.id, asset.id))
            .unwrap();
        (root_id, hash)
    }

    fn make_asset_input(hash: &str) -> NewAsset {
        NewAsset {
            content_hash: hash.to_string(),
            hash_algorithm: "blake3".to_string(),
            byte_size: 64,
            mime_type: None,
            width: None,
            height: None,
            has_alpha: None,
            is_animated: None,
            frame_count: None,
        }
    }

    // ===== 1. フルDBバックアップ =====

    #[test]
    fn full_db_backup_creates_verified_file_and_rotates_generations() {
        let tmp = TempDir::new("fulldb");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        seed_library(&db, tmp.path().join("root1"), "a.png", 1);

        let dest = tmp.path().join("backups");
        let out1 = full_db_backup(&db, Some(&dest)).unwrap();
        assert!(out1.path.starts_with(&dest), "{}", out1.path.display());
        assert!(out1.path.to_string_lossy().contains("library-full-"));
        assert!(out1.path.extension().is_some_and(|e| e == "sqlite3"));
        // 単独で開ける完全なDB (WAL活性下でも内容を含む)
        let conn = Connection::open(&out1.path).unwrap();
        let ic: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ic, "ok");
        let roots: i64 = conn
            .query_row("SELECT count(*) FROM library_roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(roots, 1);
        drop(conn);

        // 世代回収: 既定保持3。追加で3回作ると最古が削除される。
        full_db_backup(&db, Some(&dest)).unwrap();
        full_db_backup(&db, Some(&dest)).unwrap();
        let out4 = full_db_backup(&db, Some(&dest)).unwrap();
        assert_eq!(out4.kept_generations, 3);
        let remaining = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("library-full-") && n.ends_with(".sqlite3")
            })
            .count();
        assert_eq!(remaining, 3, "keep=3 (既定)");
        assert!(!out1.path.exists() || out1.path != out4.path);
        // migration用backup (library-pre-) が混在していても壊さない
        std::fs::write(dest.join("library-pre-x.sqlite3"), b"dummy").unwrap();
        full_db_backup(&db, Some(&dest)).unwrap();
        assert!(dest.join("library-pre-x.sqlite3").exists());
    }

    #[test]
    fn full_db_backup_respects_retention_setting_and_default_dir() {
        let tmp = TempDir::new("retention");
        let db_path = tmp.path().join("db.sqlite3");
        let db = Db::open(&db_path).unwrap();
        db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_RETENTION, "5"))
            .unwrap();
        for _ in 0..7 {
            full_db_backup(&db, None).unwrap();
        }
        let default_dir = crate::default_backup_dir(&db_path);
        let count = std::fs::read_dir(&default_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("library-full-")
            })
            .count();
        assert_eq!(count, 5, "設定 backup.retention_generations=5");
    }

    #[test]
    fn auto_backup_frequency_settings_and_due_logic() {
        let tmp = TempDir::new("auto");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        // 既定 = off
        assert_eq!(auto_frequency(&db), AutoBackupFrequency::Off);
        // offでは実行しない
        let dest = tmp.path().join("auto-backups");
        assert!(run_auto_backup_if_due(&db, &dest).unwrap().is_none());
        assert!(!dest.exists(), "offならディレクトリすら作らない");

        // daily: 未実行 → 即実行
        db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_AUTO_FULL_DB, "daily"))
            .unwrap();
        let ran = run_auto_backup_if_due(&db, &dest).unwrap();
        assert!(ran.is_some(), "前回実行時刻が無ければ実行");
        // 直後は再実行しない (24時間経過前)
        let again = run_auto_backup_if_due(&db, &dest).unwrap();
        assert!(again.is_none(), "期限前は再実行しない");
        // 前回時刻を強制的に古くすると再実行
        let stale = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64)
            .saturating_sub(25 * 60 * 60 * 1000);
        db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_LAST_AUTO, &stale.to_string()))
            .unwrap();
        assert!(run_auto_backup_if_due(&db, &dest).unwrap().is_some());

        // weekly: 25時間前ではまだ実行しない
        db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_AUTO_FULL_DB, "weekly"))
            .unwrap();
        let stale_daily = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64)
            .saturating_sub(25 * 60 * 60 * 1000);
        db.with_writer(|repos| {
            repos.setting_set(SETTING_BACKUP_LAST_AUTO, &stale_daily.to_string())
        })
        .unwrap();
        assert!(
            run_auto_backup_if_due(&db, &dest).unwrap().is_none(),
            "weeklyは7日経過前は実行しない"
        );
        // 不正値はoff扱い
        db.with_writer(|repos| repos.setting_set(SETTING_BACKUP_AUTO_FULL_DB, "sometimes"))
            .unwrap();
        assert_eq!(auto_frequency(&db), AutoBackupFrequency::Off);
    }

    // ===== 2. ポータブルバックアップ作成 =====

    #[test]
    fn portable_backup_writes_jsonl_collections_and_info_last() {
        let tmp = TempDir::new("create");
        let root_dir = tmp.path().join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        let (root_id, hash_a) = seed_library(&db, &root_dir, "a.png", 1);
        // 同一assetの複製ファイル + メタデータ + コレクション
        let (file_b, asset) = db
            .with_writer(|repos| {
                let roots = repos.list_roots()?;
                let root = &roots[0];
                let file = repos.upsert_file(FileUpsert {
                    root_id: root.id,
                    rel_path: PathBuf::from("sub/b.png"),
                    display_name: "b.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 1,
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: Some(OsFileId::new(3, 11)),
                    detach_asset: false,
                })?;
                let asset = repos.find_by_hash(&hash_a)?.unwrap();
                Ok((file, asset))
            })
            .unwrap();
        let _ = write_image(&root_dir, "sub/b.png", 1);
        db.with_writer(|repos| {
            repos.link_file(file_b.id, asset.id)?;
            repos.set_rating(asset.id, 5)?;
            repos.set_cull_state(asset.id, TriageState::Picked)?;
            repos.set_favorite(asset.id, true)?;
            repos.add_tags_batch(&[asset.id], &["Sky", "  blue  "])?;
            repos.set_caption(asset.id, CaptionKind::Description, "夕空の写真")
        })
        .unwrap();
        let collection = db
            .with_writer(|repos| repos.create_manual_collection("選択"))
            .unwrap();
        db.with_writer(|repos| repos.add_collection_items(collection.id, &[asset.id]))
            .unwrap();
        let smart = db
            .with_writer(|repos| {
                repos.create_smart_collection(
                    "高評価",
                    &SmartQuery {
                        conditions: vec![gento_core::collections::SmartCondition::RatingGe {
                            value: 4,
                        }],
                    },
                )
            })
            .unwrap();

        // 別asset (行方不明テスト用: ファイルを行方不明にしておく)
        let (root_id2, hash_c) = seed_library(&db, tmp.path().join("root2"), "c.png", 2);
        let _ = root_id2;
        let _ = hash_c;

        let dest = tmp.path().join("exports");
        std::fs::create_dir_all(&dest).unwrap();
        let path = create_portable_backup(&db, root_id, &dest).unwrap();

        // ディレクトリ名: {root名}-{固定幅タイムスタンプ}.gento
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("photos-"), "{name}");
        assert!(name.ends_with(".gento"), "{name}");
        let ts = name
            .trim_start_matches("photos-")
            .trim_end_matches(".gento");
        assert_eq!(ts.len(), 19, "YYYYMMDDTHHMMSSmmmZ: {name}");
        assert!(ts.ends_with('Z'));

        // metadata.jsonl: asset行 + file行
        let jsonl = std::fs::read_to_string(path.join(METADATA_JSONL_FILE)).unwrap();
        let rows: Vec<serde_json::Value> = jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let asset_rows: Vec<_> = rows.iter().filter(|r| r["type"] == "asset").collect();
        let file_rows: Vec<_> = rows.iter().filter(|r| r["type"] == "file").collect();
        assert_eq!(asset_rows.len(), 1);
        assert_eq!(file_rows.len(), 2, "同一assetの2ファイル");
        let a = &asset_rows[0];
        assert_eq!(a["content_hash"].as_str().unwrap(), hash_a);
        assert_eq!(a["rating"], serde_json::json!(5));
        assert_eq!(a["triage"], serde_json::json!("picked"));
        assert_eq!(a["favorite"], serde_json::json!(true));
        let tags = a["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags
            .iter()
            .any(|t| t["normalized_name"] == "sky" && t["source"] == "manual"));
        assert!(tags.iter().any(|t| t["normalized_name"] == "blue"));
        let caps = a["captions"].as_array().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0]["kind"], "description");
        assert_eq!(caps[0]["body"], "夕空の写真");

        let f0 = &file_rows[0];
        assert!(
            f0["rel_path"].as_str().unwrap().is_ascii(),
            "rel_pathは百分号エスケープ正準形"
        );
        assert!(file_rows.iter().any(|f| f["rel_path"] == "sub/b.png"));
        let fb = file_rows
            .iter()
            .find(|f| f["rel_path"] == "sub/b.png")
            .unwrap();
        assert_eq!(fb["os_file_id"][0], serde_json::json!(3));
        assert_eq!(fb["os_file_id"][1], serde_json::json!(11));
        assert_eq!(fb["mtime_ns"], serde_json::json!(1000));

        // collections.json: manual items + smart query_json
        let cjson: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path.join(COLLECTIONS_JSON_FILE)).unwrap(),
        )
        .unwrap();
        let cols = cjson["collections"].as_array().unwrap();
        assert_eq!(cols.len(), 2);
        let manual = cols
            .iter()
            .find(|c| c["kind"] == "manual")
            .expect("manual exists");
        assert_eq!(manual["name"], "選択");
        assert_eq!(manual["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            manual["items"][0]["asset_uuid"],
            serde_json::json!(a["uuid"])
        );
        let smart_row = cols.iter().find(|c| c["kind"] == "smart").unwrap();
        assert!(smart_row["query_json"]
            .as_str()
            .unwrap()
            .contains("rating_ge"));
        assert_eq!(smart_row["name"], "高評価");
        let _ = smart;

        // backup-info.json: complete + digest + 件数
        let info: BackupInfo =
            serde_json::from_str(&std::fs::read_to_string(path.join(BACKUP_INFO_FILE)).unwrap())
                .unwrap();
        assert_eq!(info.format_version, BACKUP_FORMAT_VERSION);
        assert!(info.complete);
        assert!(info.digest.starts_with("blake3:"));
        assert_eq!(info.asset_count, 1);
        assert_eq!(info.file_count, 2);
        assert_eq!(info.collection_count, 2);
        assert_eq!(info.api_version, gento_core::GENTO_API_VERSION);
        assert!(!info.root_uuid.is_empty());
        assert_eq!(info.file_digests.len(), 2);

        // 一時ディレクトリは残っていない
        let leftovers: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(TMP_DIR_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "tmp掃除済み");

        // 作成直後に検証を通る
        let opened = open_portable_backup(&path).unwrap();
        assert_eq!(opened.assets.len(), 1);
        assert_eq!(opened.files.len(), 2);
        assert_eq!(opened.collections.len(), 2);
        assert_eq!(opened.info.root_uuid, info.root_uuid);

        // 同一名への再作成は上書きせず別名 (DestinationExistsにはならない)
        let path2 = create_portable_backup(&db, root_id, &dest).unwrap();
        assert_ne!(path, path2);
        assert!(path.exists(), "既存は温存");
    }

    #[test]
    fn portable_backup_rejects_unknown_root() {
        let tmp = TempDir::new("unknownroot");
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        let err = create_portable_backup(&db, 999, tmp.path()).unwrap_err();
        assert!(matches!(err, BackupError::RootNotFound(999)), "{err}");
    }

    // ===== 3. 不完全バックアップ拒否 =====

    #[test]
    fn open_rejects_incomplete_digest_mismatch_and_tmp_remains() {
        let tmp = TempDir::new("reject");
        let root_dir = tmp.path().join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        let db = Db::open(tmp.path().join("db.sqlite3")).unwrap();
        let (root_id, _) = seed_library(&db, &root_dir, "a.png", 1);
        let dest = tmp.path().join("exports");
        std::fs::create_dir_all(&dest).unwrap();
        let path = create_portable_backup(&db, root_id, &dest).unwrap();

        // (a) complete: false を拒否
        {
            let mut opened = open_portable_backup(&path).unwrap();
            opened.info.complete = false;
            let dir = tmp.path().join("incomplete.gento");
            std::fs::create_dir_all(&dir).unwrap();
            let files = [COLLECTIONS_JSON_FILE, METADATA_JSONL_FILE, BACKUP_INFO_FILE];
            for name in files {
                let src = match name {
                    x if x == METADATA_JSONL_FILE => serialize_jsonl(&opened),
                    x if x == COLLECTIONS_JSON_FILE => serialize_collections(&opened),
                    _ => serde_json::to_string_pretty(&opened.info).unwrap(),
                };
                std::fs::write(dir.join(name), src).unwrap();
            }
            let err = open_portable_backup(&dir).unwrap_err();
            assert!(matches!(err, BackupError::Incomplete { .. }), "{err}");
        }

        // (b) digest不一致 (内容改変) を拒否
        {
            let dir = tmp.path().join("tampered.gento");
            let _ = std::fs::remove_dir_all(&dir);
            dircopy(&path, &dir);
            let jsonl = dir.join(METADATA_JSONL_FILE);
            let mutated = std::fs::read_to_string(&jsonl)
                .unwrap()
                .replace("\"rating\":0", "\"rating\":5");
            std::fs::write(&jsonl, mutated).unwrap();
            let err = open_portable_backup(&dir).unwrap_err();
            assert!(matches!(err, BackupError::DigestMismatch { .. }), "{err}");
        }

        // (c) backup-info.json 欠落を拒否
        {
            let dir = tmp.path().join("noinfo.gento");
            let _ = std::fs::remove_dir_all(&dir);
            dircopy(&path, &dir);
            std::fs::remove_file(dir.join(BACKUP_INFO_FILE)).unwrap();
            let err = open_portable_backup(&dir).unwrap_err();
            assert!(matches!(err, BackupError::Format(_)), "{err}");
        }

        // (d) tmp残骸ディレクトリを拒否
        {
            let dir = tmp.path().join(format!("{TMP_DIR_PREFIX}deadbeef.gento"));
            let _ = std::fs::remove_dir_all(&dir);
            dircopy(&path, &dir);
            let err = open_portable_backup(&dir).unwrap_err();
            assert!(matches!(err, BackupError::Format(_)), "{err}");
            assert!(err.to_string().contains("tmp"));
        }

        // (e) 未来のformat_versionを拒否
        {
            let mut opened = open_portable_backup(&path).unwrap();
            opened.info.format_version = BACKUP_FORMAT_VERSION + 1;
            opened.info.complete = true;
            let dir = tmp.path().join("future.gento");
            let _ = std::fs::remove_dir_all(&dir);
            dircopy(&path, &dir);
            std::fs::write(
                dir.join(BACKUP_INFO_FILE),
                serde_json::to_string_pretty(&opened.info).unwrap(),
            )
            .unwrap();
            let err = open_portable_backup(&dir).unwrap_err();
            assert!(
                matches!(err, BackupError::UnsupportedFormat { found, .. } if found == BACKUP_FORMAT_VERSION + 1),
                "{err}"
            );
        }
    }

    fn dircopy(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap().filter_map(|e| e.ok()) {
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                dircopy(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    fn serialize_jsonl(b: &PortableBackup) -> String {
        let mut out = String::new();
        for a in &b.assets {
            out.push_str(&serde_json::to_string(&MetadataRow::Asset(a.clone())).unwrap());
            out.push('\n');
        }
        for f in &b.files {
            out.push_str(&serde_json::to_string(&MetadataRow::File(f.clone())).unwrap());
            out.push('\n');
        }
        out
    }

    fn serialize_collections(b: &PortableBackup) -> String {
        serde_json::json!({ "collections": b.collections }).to_string()
    }

    // ===== 4. 復元マッチング =====

    /// 元DBでバックアップを作り、現DBへplan/executeする土台。
    struct Fixture {
        _tmp: TempDir,
        src_db: Db,
        cur_db: Db,
        root_dir: PathBuf,
        export_dir: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let tmp = TempDir::new(tag);
            let root_dir = tmp.path().join("photos");
            std::fs::create_dir_all(&root_dir).unwrap();
            let export_dir = tmp.path().join("exports");
            std::fs::create_dir_all(&export_dir).unwrap();
            Self {
                _tmp: tmp,
                src_db: Db::open(root_dir.parent().unwrap().join("src.sqlite3")).unwrap(),
                cur_db: Db::open(root_dir.parent().unwrap().join("cur.sqlite3")).unwrap(),
                root_dir,
                export_dir,
            }
        }

        fn tmp(&self) -> PathBuf {
            self.root_dir.parent().unwrap().to_path_buf()
        }

        fn backup(&self, root_id: i64) -> PathBuf {
            create_portable_backup(&self.src_db, root_id, &self.export_dir).unwrap()
        }

        /// 現DBへルートを同一パスで登録 (復元先)。
        fn register_target_root(&self) -> i64 {
            self.cur_db
                .with_writer(|repos| repos.add_root(&self.root_dir))
                .unwrap()
                .id
        }
    }

    #[test]
    fn plan_matches_by_uuid_and_reports_same_when_identical() {
        let fx = Fixture::new("uuid");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: 同一UUIDのassetは無いが同一ルート・同一ファイル → PathAddではなく
        // ディスク内容一致でhashマッチ... 実は現DBには何も無いのでPathAddになる。
        // UUIDマッチを検証するには現DBに同uuid assetが必要 → 直接流し込み。
        let src_asset = fx
            .src_db
            .with_reader(|repos| repos.find_by_hash(&hash))
            .unwrap()
            .expect("asset");
        let cur_root = fx.register_target_root();
        let cur_file = fx
            .cur_db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id: cur_root,
                    rel_path: PathBuf::from("a.png"),
                    display_name: "a.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (cur_asset, _) = fx
            .cur_db
            .with_writer(|repos| repos.create_or_get_asset(make_asset_input(&hash)))
            .unwrap();
        // UUIDを手動で同期 (UUID一致ケースの構成)
        fx.cur_db
            .with_writer(|repos| {
                repos
                    .conn()
                    .execute(
                        "UPDATE assets SET uuid = ?1 WHERE id = ?2",
                        rusqlite::params![src_asset.uuid, cur_asset.id],
                    )
                    .map_err(|e| RepoError::Db(e.to_string()))?;
                repos.link_file(cur_file.id, cur_asset.id)?;
                Ok(())
            })
            .unwrap();

        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        assert_eq!(plan.entries.len(), 1);
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::Uuid);
        assert_eq!(e.action, RestoreAction::Same, "メタデータ同一");
        assert_eq!(plan.summary.same, 1);
        assert_eq!(plan.target_root_id, cur_root);
    }

    #[test]
    fn plan_reassociates_by_hash_after_path_move() {
        let fx = Fixture::new("move");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        fx.src_db
            .with_writer(|repos| {
                let asset = repos.find_by_hash(&hash)?.unwrap();
                repos.set_rating(asset.id, 4)
            })
            .unwrap();
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: ファイルは moved/renamed.png へ移動済み (同一内容)
        let cur_root = fx.register_target_root();
        write_image(&fx.root_dir, "moved.png", 1);
        let moved = fx
            .cur_db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id: cur_root,
                    rel_path: PathBuf::from("moved.png"),
                    display_name: "moved.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (asset, _) = fx
            .cur_db
            .with_writer(|repos| repos.create_or_get_asset(make_asset_input(&hash)))
            .unwrap();
        fx.cur_db
            .with_writer(|repos| repos.link_file(moved.id, asset.id))
            .unwrap();

        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::Hash, "hash再関連付け");
        assert_eq!(e.action, RestoreAction::Update, "rating 0→4");
        assert_eq!(
            e.files[0].current_rel_path.as_deref(),
            Some("moved.png"),
            "移動後パスを報告"
        );
        assert_eq!(plan.summary.update, 1);
    }

    #[test]
    fn plan_reports_ambiguous_when_same_hash_in_multiple_locations() {
        let fx = Fixture::new("dup");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: 同一内容が2箇所 (コピー) — UUID不一致
        let cur_root = fx.register_target_root();
        write_image(&fx.root_dir, "copy1.png", 1);
        write_image(&fx.root_dir, "copy2.png", 1);
        let (asset, _) = fx
            .cur_db
            .with_writer(|repos| repos.create_or_get_asset(make_asset_input(&hash)))
            .unwrap();
        for rel in ["copy1.png", "copy2.png"] {
            let f = fx
                .cur_db
                .with_writer(|repos| {
                    repos.upsert_file(FileUpsert {
                        root_id: cur_root,
                        rel_path: PathBuf::from(rel),
                        display_name: rel.to_string(),
                        byte_size: 64,
                        mtime_ms: 0,
                        mtime_ns: 0,
                        scan_state: ScanState::Present,
                        os_file_id: None,
                        detach_asset: false,
                    })
                })
                .unwrap();
            fx.cur_db
                .with_writer(|repos| repos.link_file(f.id, asset.id))
                .unwrap();
        }

        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::Ambiguous);
        assert_eq!(e.action, RestoreAction::Skip, "推測して適用しない");
        assert_eq!(plan.summary.ambiguous, 1);
    }

    #[test]
    fn plan_reports_conflict_when_path_holds_different_content() {
        let fx = Fixture::new("conflict");
        let (root_id, _) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: 同一パスに別内容 (seed=9)
        let cur_root = fx.register_target_root();
        write_image(&fx.root_dir, "a.png", 9);
        let other_hash = hash_of(&[9u8; 64]);
        let f = fx
            .cur_db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id: cur_root,
                    rel_path: PathBuf::from("a.png"),
                    display_name: "a.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (asset, _) = fx
            .cur_db
            .with_writer(|repos| repos.create_or_get_asset(make_asset_input(&other_hash)))
            .unwrap();
        fx.cur_db
            .with_writer(|repos| repos.link_file(f.id, asset.id))
            .unwrap();

        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::Conflict);
        assert_eq!(e.action, RestoreAction::Skip);
        assert_eq!(plan.summary.conflict, 1);
    }

    #[test]
    fn plan_reports_missing_when_backup_only_and_file_absent() {
        let fx = Fixture::new("missing");
        let (root_id, _) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: 空ルート。ディスクにもファイルが無い (削除しておく)
        std::fs::remove_file(fx.root_dir.join("a.png")).unwrap();
        fx.register_target_root();

        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::Missing);
        assert_eq!(e.action, RestoreAction::Skip);
        assert!(!e.files[0].on_disk);
        assert_eq!(plan.summary.missing, 1);
    }

    #[test]
    fn plan_path_add_when_disk_file_matches_and_db_empty() {
        let fx = Fixture::new("pathadd");
        let (root_id, _) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: 空ルートだがディスクに同一内容のファイル
        fx.register_target_root();
        let plan = build_restore_plan(&fx.cur_db, &backup, None).unwrap();
        let e = &plan.entries[0];
        assert_eq!(e.matched, RestoreMatch::PathAdd);
        assert_eq!(e.action, RestoreAction::Add);
        assert!(e.files[0].on_disk);
        assert_eq!(plan.summary.add, 1);
    }

    #[test]
    fn plan_requires_resolvable_target_root() {
        let fx = Fixture::new("noroot");
        let (root_id, _) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();

        // 現DB: ルート0個かつuuid不一致 → NoTargetRoot
        let err = build_restore_plan(&fx.cur_db, &backup, None).unwrap_err();
        assert!(matches!(err, BackupError::NoTargetRoot { .. }), "{err}");

        // ルート2個 (uuid不一致) でも一意に決まらない
        fx.register_target_root();
        fx.cur_db
            .with_writer(|repos| repos.add_root(&fx.tmp().join("other")))
            .unwrap();
        let err = build_restore_plan(&fx.cur_db, &backup, None).unwrap_err();
        assert!(matches!(err, BackupError::NoTargetRoot { .. }), "{err}");

        // 明示指定なら解決する
        let roots = fx.cur_db.with_reader(|repos| repos.list_roots()).unwrap();
        let plan = build_restore_plan(&fx.cur_db, &backup, Some(roots[0].id)).unwrap();
        assert_eq!(plan.target_root_id, roots[0].id);
    }

    // ===== 5. 復元実行 =====

    #[test]
    fn execute_restores_into_empty_db_with_all_metadata() {
        let fx = Fixture::new("empty");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let (root_id2, _) = seed_library(&fx.src_db, fx.tmp().join("root2"), "c.png", 2);
        let _ = root_id2;
        // srcにメタデータ + コレクション
        let seed_asset = fx
            .src_db
            .with_reader(|repos| repos.find_by_hash(&hash))
            .unwrap()
            .expect("asset");
        fx.src_db
            .with_writer(|repos| {
                repos.set_rating(seed_asset.id, 3)?;
                repos.add_tags_batch(&[seed_asset.id], &["sky", "blue"])?;
                repos.set_caption(seed_asset.id, CaptionKind::Description, "説明文")?;
                repos.set_caption(seed_asset.id, CaptionKind::SdTags, "tag1, tag2")?;
                Ok(())
            })
            .unwrap();
        let col = fx
            .src_db
            .with_writer(|repos| repos.create_manual_collection("復元コレクション"))
            .unwrap();
        fx.src_db
            .with_writer(|repos| repos.add_collection_items(col.id, &[seed_asset.id]))
            .unwrap();

        let backup_path = fx.backup(root_id);
        let backup = open_portable_backup(&backup_path).unwrap();
        let cur_root = fx.register_target_root();

        let result = execute_restore(&fx.cur_db, &backup, None).unwrap();
        assert_eq!(result.applied.len(), 1);
        assert_eq!(result.applied[0].action, RestoreAction::Add);
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(result.collections_created, 1);
        assert_eq!(result.collection_items_added, 1);
        assert_eq!(result.fts_rebuilt, 1, "FTS文書を再構築");

        // 現DBの内容検証
        fx.cur_db
            .with_reader(|repos| {
                let cur = repos.find_by_hash(&hash)?.expect("asset復元");
                assert_eq!(cur.rating, 3);
                assert_eq!(cur.uuid, seed_asset.uuid, "UUID保存 (次回のUUID一致用)");
                let tags = repos.list_asset_tags(cur.id)?;
                let names: Vec<String> =
                    tags.iter().map(|t| t.tag.normalized_name.clone()).collect();
                assert!(names.contains(&"sky".to_string()));
                assert!(names.contains(&"blue".to_string()));
                let cap = repos
                    .get_current_caption(cur.id, CaptionKind::Description)?
                    .expect("caption復元");
                assert_eq!(cap.body, "説明文");
                let sd = repos
                    .get_current_caption(cur.id, CaptionKind::SdTags)?
                    .expect("sd_tags復元");
                assert_eq!(sd.body, "tag1, tag2");
                let cols = repos.list_collections()?;
                assert_eq!(cols.len(), 1);
                assert_eq!(cols[0].name, "復元コレクション");
                assert_eq!(cols[0].item_count, 1);
                let (items, total) = repos.list_collection_items(cols[0].id, 0, 10)?;
                assert_eq!(total, 1);
                assert_eq!(items[0].asset_id, cur.id);
                let file = repos
                    .find_by_path(cur_root, &PathBuf::from("a.png"))?
                    .expect("file行復元");
                assert_eq!(file.asset_id, Some(cur.id));
                assert_eq!(file.scan_state, ScanState::Present);
                Ok(())
            })
            .unwrap();

        // FTS: 復元済み文書が検索に掛かる (再構築済みの証)
        let hits = fx
            .cur_db
            .with_reader(|repos| {
                let query = gento_search::parse("説明文").unwrap().query;
                crate::search::search_asset_ids(
                    repos.conn(),
                    &query,
                    &crate::search::SearchScope::for_display(),
                    0,
                    10,
                )
                .map(|(ids, _)| ids)
            })
            .unwrap();
        assert!(!hits.is_empty(), "FTS再構築後に検索可能");
        let (expected, actual) = fx
            .cur_db
            .with_reader(|repos| crate::search::fts_counts(repos.conn()))
            .unwrap();
        assert_eq!((expected, actual), (1, 1));

        // 再実行は冪等 (Sameへ収斂)
        let again = execute_restore(&fx.cur_db, &backup, None).unwrap();
        assert_eq!(again.applied.len(), 1);
        assert_eq!(again.applied[0].action, RestoreAction::Same);
        assert_eq!(again.collection_items_added, 0, "重複追加しない");
    }

    #[test]
    fn execute_updates_matched_asset_and_skips_ambiguous_and_missing() {
        let fx = Fixture::new("mixed");
        let (root_id, hash_a) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        // ambiguous用: srcに同一内容2箇所 … いやambiguousは「現DB側で複数」。
        // missing用: srcのみのファイル (現DBにもディスクにも無い)
        let (root_id_m, hash_m) = seed_library(&fx.src_db, fx.tmp().join("mroot"), "gone.png", 3);
        let _ = hash_m;

        let _backup_a_initial = open_portable_backup(&fx.backup(root_id)).unwrap();
        let backup_m = open_portable_backup(&fx.backup(root_id_m)).unwrap();

        // 現DB: a.pngは移動済み (hashマッチ・単一所在) → Update
        let cur_root = fx.register_target_root();
        write_image(&fx.root_dir, "moved.png", 1);
        let moved = fx
            .cur_db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id: cur_root,
                    rel_path: PathBuf::from("moved.png"),
                    display_name: "moved.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        let (asset, _) = fx
            .cur_db
            .with_writer(|repos| repos.create_or_get_asset(make_asset_input(&hash_a)))
            .unwrap();
        fx.cur_db
            .with_writer(|repos| repos.link_file(moved.id, asset.id))
            .unwrap();
        fx.src_db
            .with_writer(|repos| {
                let a = repos.find_by_hash(&hash_a)?.unwrap();
                repos.set_rating(a.id, 5)
            })
            .unwrap();
        let backup_a = open_portable_backup(&fx.backup(root_id)).unwrap();

        let result = execute_restore(&fx.cur_db, &backup_a, Some(cur_root)).unwrap();
        assert_eq!(result.applied.len(), 1);
        assert_eq!(result.applied[0].action, RestoreAction::Update);
        assert!(result.applied[0].changed.contains(&"rating".to_string()));
        let rating = fx
            .cur_db
            .with_reader(|repos| Ok(repos.find_by_hash(&hash_a)?.unwrap().rating))
            .unwrap();
        assert_eq!(rating, 5);

        // missing: 現DBに移す (ファイル実体は無い)
        let result_m = execute_restore(&fx.cur_db, &backup_m, Some(cur_root)).unwrap();
        assert!(result_m.applied.is_empty());
        assert_eq!(result_m.skipped.len(), 1);
        assert_eq!(result_m.skipped[0].matched, RestoreMatch::Missing);

        // ambiguous: 現DBへ同一内容を2箇所登録してから再実行
        write_image(&fx.root_dir, "copy1.png", 1);
        write_image(&fx.root_dir, "copy2.png", 1);
        for rel in ["copy1.png", "copy2.png"] {
            let f = fx
                .cur_db
                .with_writer(|repos| {
                    repos.upsert_file(FileUpsert {
                        root_id: cur_root,
                        rel_path: PathBuf::from(rel),
                        display_name: rel.to_string(),
                        byte_size: 64,
                        mtime_ms: 0,
                        mtime_ns: 0,
                        scan_state: ScanState::Present,
                        os_file_id: None,
                        detach_asset: false,
                    })
                })
                .unwrap();
            fx.cur_db
                .with_writer(|repos| repos.link_file(f.id, asset.id))
                .unwrap();
        }
        let result_dup = execute_restore(&fx.cur_db, &backup_a, Some(cur_root)).unwrap();
        assert_eq!(result_dup.skipped.len(), 1);
        assert_eq!(result_dup.skipped[0].matched, RestoreMatch::Ambiguous);
        assert_eq!(
            result_dup.applied.len(),
            0,
            "ambiguousは適用しない (既にrating 5だがSame評価自体スキップ)"
        );
    }

    #[test]
    fn execute_never_touches_current_only_data() {
        // 復元は現DBのデータを削除しない (追加・更新のみ)。
        let fx = Fixture::new("nodelete");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let backup = open_portable_backup(&fx.backup(root_id)).unwrap();

        let cur_root = fx.register_target_root();
        // 現DBのみのasset (バックアップに無い)
        write_image(&fx.root_dir, "extra.png", 7);
        fx.cur_db
            .with_writer(|repos| {
                let f = repos.upsert_file(FileUpsert {
                    root_id: cur_root,
                    rel_path: PathBuf::from("extra.png"),
                    display_name: "extra.png".to_string(),
                    byte_size: 64,
                    mtime_ms: 0,
                    mtime_ns: 0,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })?;
                let (a, _) = repos.create_or_get_asset(make_asset_input(&hash_of(&[7u8; 64])))?;
                repos.link_file(f.id, a.id)?;
                repos.set_rating(a.id, 2)
            })
            .unwrap();
        let _ = hash;

        execute_restore(&fx.cur_db, &backup, Some(cur_root)).unwrap();
        let count = fx
            .cur_db
            .with_reader(|repos| {
                Ok(repos
                    .find_by_hash(&hash_of(&[7u8; 64]))?
                    .expect("現DBのみのassetは存続")
                    .rating)
            })
            .unwrap();
        assert_eq!(count, 2, "現DB側の評価は改変しない");
    }

    #[test]
    fn execute_rebuilds_fts_and_records_counts() {
        let fx = Fixture::new("fts");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        let asset = fx
            .src_db
            .with_reader(|repos| repos.find_by_hash(&hash))
            .unwrap()
            .expect("asset");
        fx.src_db
            .with_writer(|repos| {
                repos.set_caption(asset.id, CaptionKind::Description, "青空")?;
                Ok(())
            })
            .unwrap();
        // srcのFTSを構築 (差分比較のため)
        fx.src_db
            .with_writer(|repos| {
                crate::search::rebuild_docs_for_assets(repos.conn(), &[asset.id])?;
                Ok(())
            })
            .unwrap();
        let backup = open_portable_backup(&fx.backup(root_id)).unwrap();

        fx.register_target_root();
        let result = execute_restore(&fx.cur_db, &backup, None).unwrap();
        assert_eq!(result.fts_rebuilt, 1);
        let (expected, actual) = fx
            .cur_db
            .with_reader(|repos| crate::search::fts_counts(repos.conn()))
            .unwrap();
        assert_eq!((expected, actual), (1, 1));
        let report = fx
            .cur_db
            .with_reader(|repos| crate::search::fts_integrity(repos.conn()))
            .unwrap();
        assert!(report.consistent, "{report:?}");
    }

    #[test]
    fn rel_path_round_trip_preserves_non_utf8_bytes() {
        // 非UTF-8相対パスのポータブル往復 (百分号エスケープ正準形)。
        let fx = Fixture::new("nonutf8");
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let rel = std::path::PathBuf::from(OsString::from_vec(vec![
                b'd', 0xff, b'/', b'b', b'.', b'p', b'n', b'g',
            ]));
            let abs = fx.root_dir.join(&rel);
            let parent = rel.parent().unwrap().to_path_buf();
            std::fs::create_dir_all(fx.root_dir.join(parent)).unwrap();
            std::fs::write(&abs, vec![5u8; 32]).unwrap();
            let hash = hash_of(&[5u8; 32]);
            let root_id = fx
                .src_db
                .with_writer(|repos| repos.add_root(&fx.root_dir))
                .unwrap()
                .id;
            let f = fx
                .src_db
                .with_writer(|repos| {
                    repos.upsert_file(FileUpsert {
                        root_id,
                        rel_path: rel.clone(),
                        display_name: "b.png".to_string(),
                        byte_size: 32,
                        mtime_ms: 0,
                        mtime_ns: 0,
                        scan_state: ScanState::Present,
                        os_file_id: None,
                        detach_asset: false,
                    })
                })
                .unwrap();
            let (asset, _) = fx
                .src_db
                .with_writer(|repos| {
                    repos.create_or_get_asset(NewAsset {
                        byte_size: 32,
                        ..make_asset_input(&hash)
                    })
                })
                .unwrap();
            fx.src_db
                .with_writer(|repos| repos.link_file(f.id, asset.id))
                .unwrap();

            let path = create_portable_backup(&fx.src_db, root_id, &fx.export_dir).unwrap();
            let backup = open_portable_backup(&path).unwrap();
            // JSONL内はASCII百分号エスケープ
            let jsonl = std::fs::read_to_string(path.join(METADATA_JSONL_FILE)).unwrap();
            assert!(jsonl.contains("d%FF/b.png"), "{jsonl}");
            assert_eq!(backup.files[0].rel_path, "d%FF/b.png");

            let cur_root = fx.register_target_root();
            let result = execute_restore(&fx.cur_db, &backup, Some(cur_root)).unwrap();
            assert_eq!(result.applied.len(), 1);
            // 現DBのfile行は元の非UTF-8バイト列で復元される
            fx.cur_db
                .with_reader(|repos| {
                    let file = repos
                        .find_by_path(cur_root, &rel)?
                        .expect("非UTF-8パスで復元");
                    assert_eq!(file.rel_path, rel);
                    Ok(())
                })
                .unwrap();
        }
        #[cfg(not(unix))]
        {
            let _ = fx;
        }
    }

    #[test]
    fn smart_collection_query_json_survives_round_trip() {
        let fx = Fixture::new("smart");
        let (root_id, hash) = seed_library(&fx.src_db, &fx.root_dir, "a.png", 1);
        fx.src_db
            .with_writer(|repos| {
                let asset = repos.find_by_hash(&hash)?.unwrap();
                repos.set_rating(asset.id, 5)
            })
            .unwrap();
        // WP-8形式 (AST v2) で作成
        let ast_query = gento_search::SearchQuery {
            terms: vec![gento_search::SearchTerm::Field(
                gento_search::FieldCondition {
                    key: gento_search::FieldKey::Rating,
                    op: gento_search::CmpOp::Ge,
                    value: gento_search::FieldValue::Number(4),
                },
            )],
        };
        let search_json = gento_search::stored::to_json(&ast_query);
        fx.src_db
            .with_writer(|repos| {
                use gento_core::collections::CollectionRepo;
                repos.create_smart_collection_search("ast-smart", &search_json)
            })
            .unwrap();

        let backup = open_portable_backup(&fx.backup(root_id)).unwrap();
        fx.register_target_root();
        let result = execute_restore(&fx.cur_db, &backup, None).unwrap();
        assert_eq!(result.collections_created, 1);
        let restored = fx
            .cur_db
            .with_reader(|repos| repos.list_collections())
            .unwrap();
        let smart = restored
            .iter()
            .find(|c| c.name == "ast-smart")
            .expect("smart復元");
        assert_eq!(smart.item_count, 1, "評価結果も復元後DBで機能する");
    }
}
