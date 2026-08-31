//! 安全なファイル操作エンジン (MVP WP-7)。
//!
//! ユーザーデータを初めて変更・削除する領域のため、以下の安全原則を
//! 構造で強制する (計画書§15 + WP-7レビュー対応):
//!
//! 1. **通常削除はゴミ箱**: [`FileOpKind::Trash`] が既定の削除。
//!    [`FileOpKind::DeleteForever`] は別操作 (呼び出し層の確認ゲート
//!    — 単発トークン — を通過した場合のみ実行される)。
//!    ゴミ箱移動の失敗は通常削除へフォールバックしない (エラー表示のみ)。
//! 2. **上書きは明示確認 + 原子的no-clobber配置**: 計画 (dry-run) で衝突を
//!    検出し、[`ConflictResolution::Overwrite`] が明示された場合のみ
//!    (1) 旧宛先を先にゴミ箱へ退避し (2) 新ファイルを配置する。
//!    最終配置はすべて `renameat2(RENAME_NOREPLACE)` 相当の原子操作で
//!    行うため、計画→実行間に宛先が現れても**上書きされない**
//!    (TOCTOU競合は kind=AlreadyExists の失敗になる)。
//!    解決方針未指定の衝突はスキップ (安全側)。
//!    「操作先 == 操作元」の上書きは実行拒否 (自己破壊)。
//! 3. **外部変更で前提が崩れたUndoは拒否**: Undo実行前に全ステップの
//!    前提 (対象の存在・世代トークン一致・復帰先の不在・ゴミ箱エントリの
//!    存在+サイズ一致) を検証し、1つでも崩れていれば全体を拒否し理由を
//!    返す ([`verify_undo`])。
//! 4. **コピー完了前のファイルを正規ファイルとして登録しない**:
//!    コピーは宛先ディレクトリ内のテンポラリ名 (厳密形式
//!    `.{最終名}.gento-tmp-{op_uuid}-{seq}`。[`tmp_path_for`]) で書き込み、
//!    完了後にNOREPLACEで最終名へ確定してからDB反映する。走査側
//!    (gento-image `is_image_entry`) も `.gento-tmp-` を含む名を画像扱い
//!    しないため、異常終了で残ったテンポラリも登録されない
//!    (起動時掃除は所有権検証付き — 呼び出し側 `recover_at_startup`)。
//! 5. **操作ログを確定してからUI上の成功を通知する**: 実行は
//!    (a) file_operations行を `executing` で確保 → (b) ファイル単位で
//!    FS操作+DB反映 (1ファイル=1トランザクション) → (c) 全ファイル成否を
//!    result JSONへ書き込みstatus確定、の順。呼び出し側は
//!    [`execute_file_op`] の戻り (確定後) をもって成功通知してよい。
//!
//! # Undo原子性ポリシー (逐次実行 + 補償ロールバック)
//!
//! Undoはトランザクション的原子性を持たない複数FS操作の列であるため、
//! 以下の構造で一貫性を保証する:
//!
//! - **事前検証**: [`verify_undo`] で全アクションの前提を検証してから
//!   開始する (1つでも崩れていれば何も実行しない)。検証は記録順を考慮
//!   (先頭側アクションが空けるパスを後続の「復帰先不在」判定に反映)。
//! - **逐次実行 + 実行直前再検証**: 各アクションは実行直前に自分の前提を
//!   再検証する (verifyと実行の間の外部変更にも安全側で倒れる)。
//! - **補償ロールバック**: アクションが失敗 (またはキャンセル) した場合、
//!   実行済みアクションを**逆順で補償**する (各補償も前提検証付き。
//!   補償に必要な実行時情報 — 例: DeleteCopyが生成したゴミ箱エントリ —
//!   は実行時に捕捉する)。補償結果をstatusへ正確に反映する:
//!   全成功でのみ `undone`、巻き戻し中の失敗があれば `partial_undone`
//!   +理由 ([`UndoOutcome`])。
//!
//! # 異常終了回復
//!
//! プロセス死亡で `executing` 行が残った場合、再起動時に
//! [`FileOpsRepo::list_executing_operations`] で検出できる。FSが正本のため、
//! 安全な収束 = 該当操作を中断 (`failed` 確定) + 再スキャンでDBを最終状態へ
//! 合致させる (「安全な再開」= 失敗項目のUI再試行)。
//!
//! # 依存注入
//!
//! FS操作は [`FileOpsFs`] trait、永続化は [`RepoStore`] (AllRepos) へ
//! 注入する (計画書§5「外部機能はtraitを介して注入」。テストはフェイク
//! 実装でアルゴリズムを検証)。ゴミ箱の実体 (trash crate) は
//! gento-platform / gento-tauri 側の実装が持つ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::library::{GenerationToken, RepoStore};
use crate::repo::{FileUpsert, OsFileId, RepoError, ScanState};

/// コピー進行中テンポラリのファイル名接頭辞 (旧形式。互換のため残存)。
/// 最終名へのrename後に正規ファイル化する。走査 (gento-image
/// `is_image_entry`) はこの接頭辞を画像扱いしない — 「コピー完了前の
/// ファイルを登録しない」の走査側強制。
pub const TMP_FILE_PREFIX: &str = ".gento-tmp-";

/// テンポラリ名の中間マーカー。厳密形式 `.{最終名}.gento-tmp-{op_uuid}
/// -{seq}` に含まれる (WP-7レビュー対応: 掃除の所有権検証用)。
pub const TMP_FILE_MARKER: &str = ".gento-tmp-";

/// 一時ファイルの厳密形式 `.{最終名}.gento-tmp-{op_uuid}-{seq}` を組み
/// 立てる。`op_uuid` はfile_operations行のuuid (ハイフンは除去して
/// 32桁hexへ正規化)。`seq` はステップ索引 (0開始・6桁固定)。
///
/// 一時名から操作とステップを一意に再構成できるため、起動掃除は
/// 「記録された所有パス」「厳密形式+uuid形式検証+登録ルート配下」の
/// みを削除対象にできる (外部作成の類似名ファイルは残置)。
pub fn tmp_path_for(dst: &Path, op_uuid: &str, seq: usize) -> PathBuf {
    let file_name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let uuid_simple: String = op_uuid.chars().filter(|c| *c != '-').collect();
    let name = format!(".{file_name}{TMP_FILE_MARKER}{uuid_simple}-{seq:06}");
    match dst.parent() {
        Some(p) => p.join(name),
        None => PathBuf::from(name),
    }
}

/// uuidの簡易形式検証 (ハイフン除去後32桁の小写hex)。
fn is_uuid_simple_hex(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// ファイル名がGEN-TO所有の一時ファイルの厳密形式に一致するか
/// (`.{名前}.gento-tmp-{32桁hex}-{6桁seq}`)。uuid形式検証を含むため、
/// 類似名の外部ファイルは一致しない。
pub fn is_owned_tmp_name(file_name: &str) -> bool {
    let Some(rest) = file_name.strip_prefix('.') else {
        return false;
    };
    let Some(pos) = rest.rfind(TMP_FILE_MARKER) else {
        return false;
    };
    let suffix = &rest[pos + TMP_FILE_MARKER.len()..];
    let Some((uuid_part, seq_part)) = suffix.rsplit_once('-') else {
        return false;
    };
    is_uuid_simple_hex(uuid_part)
        && seq_part.len() == 6
        && seq_part.bytes().all(|b| b.is_ascii_digit())
}

/// 一時ファイル名から所有操作のuuid (簡易形式) を取り出す。
/// 厳密形式に一致しない場合はNone。
pub fn tmp_owner_uuid_simple(file_name: &str) -> Option<String> {
    if !is_owned_tmp_name(file_name) {
        return None;
    }
    let rest = file_name.strip_prefix('.')?;
    let pos = rest.rfind(TMP_FILE_MARKER)? + TMP_FILE_MARKER.len();
    let suffix = &rest[pos..];
    let (uuid_part, _) = suffix.rsplit_once('-')?;
    Some(uuid_part.to_string())
}

/// file_operations行からGEN-TOが所有する一時ファイルパスを抽出する
/// (起動掃除の削除候補。FG-WP7)。
///
/// - 完了済み (result JSONあり): `steps[].tmp` に記録されたパス。
/// - executing残留 (result JSONなし): request JSONの `files[].to` (計画時
///   宛先) + 行uuid + step indexから決定論的に再構築。
///   (keep_both代替名・undo経路のtmpは宛先が異なるため再構築から漏れるが、
///   厳密形式 (uuid検証) の正規表現掃除が補完する。)
pub fn owned_tmp_paths_of(record: &FileOpRecord) -> Vec<PathBuf> {
    if let Some(outcome) = record
        .result_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<FileOpOutcome>(json).ok())
    {
        return outcome.steps.iter().filter_map(|s| s.tmp.clone()).collect();
    }
    let Ok(request) = serde_json::from_str::<serde_json::Value>(&record.request_json) else {
        return Vec::new();
    };
    let Some(files) = request["files"].as_array() else {
        return Vec::new();
    };
    files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            file["to"]
                .as_str()
                .map(|to| tmp_path_for(Path::new(to), &record.uuid, index))
        })
        .collect()
}

/// 操作種別 (data-model §12)。`DeleteForever` は通常削除と意図的に分離
/// (spec §5.1「完全削除は通常の削除操作と分離し、明示的な確認を必要とする」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpKind {
    Copy,
    Move,
    Rename,
    Mkdir,
    Trash,
    DeleteForever,
}

impl FileOpKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            FileOpKind::Copy => "copy",
            FileOpKind::Move => "move",
            FileOpKind::Rename => "rename",
            FileOpKind::Mkdir => "mkdir",
            FileOpKind::Trash => "trash",
            FileOpKind::DeleteForever => "delete_forever",
        }
    }
}

impl std::fmt::Display for FileOpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 不正な操作種別文字列のパース失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseFileOpKindError;

impl std::fmt::Display for ParseFileOpKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "unknown operation type: expected one of \
                     copy|move|rename|mkdir|trash|delete_forever",
        )
    }
}

impl std::error::Error for ParseFileOpKindError {}

impl std::str::FromStr for FileOpKind {
    type Err = ParseFileOpKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "copy" => Ok(FileOpKind::Copy),
            "move" => Ok(FileOpKind::Move),
            "rename" => Ok(FileOpKind::Rename),
            "mkdir" => Ok(FileOpKind::Mkdir),
            "trash" => Ok(FileOpKind::Trash),
            "delete_forever" => Ok(FileOpKind::DeleteForever),
            _ => Err(ParseFileOpKindError),
        }
    }
}

/// ゴミ箱内エントリの厳密特定情報 (undo復帰の対象特定。WP-7レビュー対応)。
///
/// `trash` 実行直後に生成エントリを特定してresult JSONへ記録し、
/// undo/restoreはこの正確なエントリを対象にする (パス再利用・複数
/// エントリ競合での誤復帰防止)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEntry {
    /// ゴミ箱ディレクトリ (`info/`・`files/` の親。
    /// 例: `~/.local/share/Trash`、`<mount>/.Trash-1000`)。
    pub trash_dir: PathBuf,
    /// `files/` 内のエントリ名 (対応trashinfoのステムと一致。
    /// 名前衝突でtrash crateが付けるsuffixも含む)。
    pub file_name: String,
}

/// 衝突の解決方針 (spec §5.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// 上書き (既存の宛先ファイルを置換する。明示確認済みであること)。
    Overwrite,
    /// スキップ (このファイルの操作を行わない)。
    Skip,
    /// 両方保持 (新ファイルを別名で置く。`name (2).ext` 形式)。
    KeepBoth,
}

impl ConflictResolution {
    pub const fn as_str(self) -> &'static str {
        match self {
            ConflictResolution::Overwrite => "overwrite",
            ConflictResolution::Skip => "skip",
            ConflictResolution::KeepBoth => "keep_both",
        }
    }
}

/// 操作の状態 (file_operations.status。migration #6のCHECK値)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpStatus {
    /// 計画済み・未実行 (dry-run結果を保持)。
    Planned,
    /// 実行中 (異常終了検出のマーカー)。
    Executing,
    /// 全ファイル成功 (スキップのみを含む)。
    Done,
    /// 一部成功・一部失敗。
    Partial,
    /// 全滅 (または中断で何も成功していない)。
    Failed,
    /// Undo済み。
    Undone,
    /// Undo実行中に一部アクションが失敗し、補償ロールバックで
    /// 巻き戻した (または補償にも失敗した)。全成功でのみ `undone`。
    PartialUndone,
}

impl FileOpStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            FileOpStatus::Planned => "planned",
            FileOpStatus::Executing => "executing",
            FileOpStatus::Done => "done",
            FileOpStatus::Partial => "partial",
            FileOpStatus::Failed => "failed",
            FileOpStatus::Undone => "undone",
            FileOpStatus::PartialUndone => "partial_undone",
        }
    }
}

/// DB格納値からの復元 (格納値は `as_str` の正本)。
impl std::str::FromStr for FileOpStatus {
    type Err = ParseFileOpStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "planned" => Ok(FileOpStatus::Planned),
            "executing" => Ok(FileOpStatus::Executing),
            "done" => Ok(FileOpStatus::Done),
            "partial" => Ok(FileOpStatus::Partial),
            "failed" => Ok(FileOpStatus::Failed),
            "undone" => Ok(FileOpStatus::Undone),
            "partial_undone" => Ok(FileOpStatus::PartialUndone),
            _ => Err(ParseFileOpStatusError),
        }
    }
}

/// 不正な操作状態文字列のパース失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseFileOpStatusError;

impl std::fmt::Display for ParseFileOpStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "unknown operation status: expected one of \
                     planned|executing|done|partial|failed|undone|partial_undone",
        )
    }
}

impl std::error::Error for ParseFileOpStatusError {}

/// FS操作エラーの分類 (権限/不在/ディスク満杯/キャンセル/不明)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsOpErrorKind {
    PermissionDenied,
    NotFound,
    DiskFull,
    Cancelled,
    /// 宛先フォルダが存在しない等。
    NotADirectory,
    AlreadyExists,
    /// 前提検証失敗 (自己上書き・シンボリックリンク等、実行してよい
    /// 前提が成立していない)。
    Precondition,
    Io,
}

/// FS操作の失敗 (分類 + メッセージ)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsOpError {
    pub kind: FsOpErrorKind,
    pub message: String,
}

impl FsOpError {
    pub fn new(kind: FsOpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(FsOpErrorKind::NotFound, message)
    }

    pub fn precondition(message: impl Into<String>) -> Self {
        Self::new(FsOpErrorKind::Precondition, message)
    }
}

impl std::fmt::Display for FsOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

/// ファイルの現在メタデータ (実行時観測)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub byte_size: u64,
    pub mtime_ms: u64,
    pub mtime_ns: u64,
    pub os_file_id: Option<OsFileId>,
}

impl FileStat {
    /// 世代トークン (WP-2と同じ組成)。
    pub fn token(&self) -> GenerationToken {
        GenerationToken {
            os_file_id: self.os_file_id,
            byte_size: self.byte_size,
            mtime_ns: self.mtime_ns,
        }
    }
}

/// ファイル操作のFS境界 (実装: gento-tauri。std fs + platform trash + BLAKE3)。
///
/// # no-clobber契約 (WP-7レビュー対応: 上書きTOCTOUの排除)
///
/// - `rename_noreplace` は `to` が既に存在する場合 kind=AlreadyExists で
///   失敗する**原子操作** (Linux: `renameat2(RENAME_NOREPLACE)`)。
///   最終配置 (copy/move/keep_both・上書き時の再配置を含む) はすべてこの
///   操作で行う — 上書き解決は先に旧宛先をゴミ箱へ退避するため、
///   置換renameは不要 (replace経路を構造的に排除)。
///   跨界 (EXDEV) の場合は [`FsOpError`] (kind=Io、メッセージに
///   `CrossesDevices` を含む) を返し、呼び出し側 (execute) がcopy+removeへ
///   フォールバックする。
/// - `copy_file` は `tmp` (最終名と同ディレクトリ・厳密形式
///   `.{名前}.gento-tmp-{uuid}-{seq}`) へ書き込み・fsyncした後、
///   `tmp → to` を **NOREPLACEで** 確定する。`to` が存在すれば
///   kind=AlreadyExists の失敗 (上書きしない)。
/// - `stat` はシンボリックリンクを辿らない (lstat相当)。
/// - `trash` は移動後のゴミ箱エントリを特定して返す (undo復帰の厳密
///   同一性検証用)。失敗時は対象を変更しない。
/// - `restore_trash` は `entry` が指す正確なエントリを `original` へ復帰
///   する。復帰前にエントリ存在検証+サイズ検証 (`expect_byte_size` =
///   トークンのsize成分) を行い、不一致は拒否 (Err)。
/// - `trash` の失敗は通常削除へフォールバックしない (エラー返すのみ)。
pub trait FileOpsFs: Send + Sync {
    /// lstat相当のメタデータ。対象不在は kind=NotFound。
    fn stat(&self, path: &Path) -> Result<FileStat, FsOpError>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn is_symlink(&self, path: &Path) -> bool;
    /// ディレクトリを1つ作成する (親は存在必須。fs::create_dir相当。
    /// 既存ならAlreadyExists)。
    fn mkdir(&self, path: &Path) -> Result<(), FsOpError>;
    /// 原子的no-clobberリネーム (同一FS。`to` 既存はAlreadyExists。
    /// 跨界は `CrossesDevices` を含むメッセージのIoエラー)。
    fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<(), FsOpError>;
    /// コピー (テンポラリ`tmp`へ書き込み→NOREPLACEで`to`へ確定。
    /// 既存の`to`は置換**しない** — AlreadyExists失敗)。
    fn copy_file(
        &self,
        from: &Path,
        to: &Path,
        tmp: &Path,
        cancel: &AtomicBool,
    ) -> Result<(), FsOpError>;
    /// 完全削除 (ゴミ箱を経由しない。DeleteForever専用)。
    fn remove_file(&self, path: &Path) -> Result<(), FsOpError>;
    /// OSゴミ箱へ移動し、生成されたゴミ箱エントリを特定して返す。
    /// 失敗時は対象を変更しない。
    fn trash(&self, path: &Path) -> Result<TrashEntry, FsOpError>;
    /// ゴミ箱エントリ `entry` を `original` へ復帰 (Undo用)。
    /// 復帰前にエントリの存在とサイズ (`expect_byte_size`) を検証し、
    /// 不一致は拒否する。
    fn restore_trash(
        &self,
        entry: &TrashEntry,
        original: &Path,
        expect_byte_size: u64,
    ) -> Result<PathBuf, FsOpError>;
    /// ゴミ箱エントリの現在サイズ (存在検証用。不在・読取不可はNone)。
    fn trash_entry_size(&self, entry: &TrashEntry) -> Option<u64>;
    /// ディレクトリの空きバイト数 (容量事前確認)。
    fn free_bytes(&self, dir: &Path) -> Result<u64, FsOpError>;
    /// ファイルの内容ハッシュ (`blake3:<hex>`)。衝突UIのハッシュ一致表示用。
    fn hash_file(&self, path: &Path, cancel: &AtomicBool) -> Result<String, FsOpError>;
}

/// 登録ルートの参照 (パス→root_id解決)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRootRef {
    pub root_id: i64,
    pub root_path: PathBuf,
}

/// 操作対象ファイル (DB解決済み。Tauri層がfile idから組み立てる)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOpFile {
    pub file_id: i64,
    pub root_id: i64,
    /// 登録ルートからの相対パス。
    pub rel_path: PathBuf,
    /// 絶対パス。
    pub abs_path: PathBuf,
    /// 表示名 (lossy)。
    pub display_name: String,
    /// 現在の内容ハッシュ (`blake3:…`。Asset未リンクはNone)。
    pub content_hash: Option<String>,
    /// Asset行id (未リンクはNone。コピーの即時Asset共有リンクに使用)。
    pub asset_id: Option<i64>,
}

/// 衝突情報 (dry-run表示用。spec §5.3: 容量・更新日時・ハッシュ一致)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// 衝突先の絶対パス。
    pub dst: PathBuf,
    pub dst_byte_size: u64,
    pub dst_mtime_ms: u64,
    /// `Some(true)` = 内容ハッシュ一致、`Some(false)` = 不一致、
    /// `None` = 未比較 (サイズ不一致・計算失敗等)。
    pub hash_matches: Option<bool>,
    /// keep_both選択時の代替名 (計算済み)。
    pub keep_both_name: String,
}

/// 計画された1ファイル分のステップ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedStep {
    pub file: FileOpFile,
    /// 操作元の絶対パス。
    pub src: PathBuf,
    /// 操作先の絶対パス (Trash/DeleteForeverはsrcと同じ)。
    pub dst: PathBuf,
    pub conflict: Option<ConflictInfo>,
    /// 計画時点で実行不能な理由 (src不在・シンボリックリンク等)。
    /// 実行するとFailedステップになる。
    pub plan_error: Option<FsOpError>,
}

/// dry-run結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOpPlan {
    pub kind: FileOpKind,
    pub steps: Vec<PlannedStep>,
    /// 宛先が属する登録ルート (宛先が全ルート外ならNone → moveはmissing化)。
    pub dest_root_id: Option<i64>,
    /// 計画全体のエラー (renameに複数ファイル等。実行不可)。
    pub plan_errors: Vec<FsOpError>,
    /// コピー総容量が必要容量に満たない場合の不足バイト数。
    pub disk_shortfall_bytes: Option<u64>,
}

/// 衝突解決の指定 (未指定の衝突はskip = 安全側)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionSpec {
    /// 全体方針 (「以降全部」相当)。
    pub global: Option<ConflictResolution>,
    /// ファイル個別 (優先)。
    pub per_file: HashMap<i64, ConflictResolution>,
}

impl ResolutionSpec {
    /// ファイル個別 (優先) → 全体方針の順で解決した方針。
    pub fn of(&self, file_id: i64) -> Option<ConflictResolution> {
        self.per_file.get(&file_id).copied().or(self.global)
    }
}

/// ステップ単位の成否。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Done,
    Skipped,
    Failed,
    Cancelled,
}

/// Undoの逆操作 (result JSONへ直列化して保存)。
///
/// 1ステップのundoは複数アクションの列 (上書きステップは「新ファイル
/// 除去+旧ファイル復帰」の複合)。実行順は記録順 — 先頭側のアクションが
/// 末尾側の前提を整える (例: 新ファイル除去が旧ファイルの復帰先を空ける)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum UndoAction {
    /// 移動・リネームを元へ戻す。`expect`は移動後 (現在地) のトークン。
    MoveBack {
        from: PathBuf,
        to: PathBuf,
        #[serde(flatten)]
        expect: TokenDto,
    },
    /// ゴミ箱から元位置へ戻す。`expect`はゴミ箱へ移す直前のトークン。
    /// `trash_entry` はtrash実行時に特定した厳密エントリ (復帰はこの
    /// エントリを対象に、存在+サイズ検証を行う)。
    RestoreFromTrash {
        original: PathBuf,
        trash_entry: TrashEntry,
        #[serde(flatten)]
        expect: TokenDto,
    },
    /// コピーのUndo = 生成物の削除 (ゴミ箱へ — 安全側)。
    DeleteCopy {
        path: PathBuf,
        #[serde(flatten)]
        expect: TokenDto,
    },
}

/// 世代トークンのJSON表現 (result JSON保存用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenDto {
    pub os_file_id: Option<[u8; 16]>,
    pub byte_size: u64,
    pub mtime_ns: u64,
}

impl From<GenerationToken> for TokenDto {
    fn from(token: GenerationToken) -> Self {
        Self {
            os_file_id: token.os_file_id.map(|id| id.to_bytes()),
            byte_size: token.byte_size,
            mtime_ns: token.mtime_ns,
        }
    }
}

impl TokenDto {
    pub fn to_token(&self) -> GenerationToken {
        GenerationToken {
            os_file_id: self
                .os_file_id
                .as_ref()
                .and_then(|bytes| OsFileId::from_bytes(bytes.as_slice())),
            byte_size: self.byte_size,
            mtime_ns: self.mtime_ns,
        }
    }
}

/// 1ステップの実行結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub file_id: i64,
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub status: StepStatus,
    /// 衝突があったステップの実際の解決。
    pub resolution: Option<ConflictResolution>,
    pub error: Option<FsOpError>,
    /// 完了したが注意喚起がいる事象 (例: 跨界移動でコピー後の元ファイル
    /// 削除に失敗 — 宛先は完成済み)。statusには影響しない。
    #[serde(default)]
    pub warning: Option<String>,
    /// このステップが使用した一時ファイルのパス (所有権記録。
    /// 起動掃除はこの記録と厳密形式検証のみを削除対象にする)。
    #[serde(default)]
    pub tmp: Option<PathBuf>,
    /// 逆操作の列 (上書きステップは複合)。実行順 = 記録順。
    #[serde(default)]
    pub undo: Vec<UndoAction>,
}

/// 操作全体の実行結果 (file_operations.result_jsonの正本)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileOpOutcome {
    pub op_id: i64,
    pub kind: FileOpKind,
    pub status: FileOpStatus,
    pub undoable: bool,
    pub steps: Vec<StepResult>,
}

impl FileOpOutcome {
    /// 部分成功レポート用: 失敗ステップのfile_id一覧。
    pub fn failed_step_ids(&self) -> Vec<i64> {
        self.steps
            .iter()
            .filter(|s| s.status == StepStatus::Failed)
            .map(|s| s.file_id)
            .collect()
    }
}

/// 進捗イベント (ファイル単位)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOpProgress {
    pub op_id: i64,
    /// 0開始のインデックス。
    pub index: usize,
    pub total: usize,
    pub from: String,
    pub to: Option<String>,
    pub done: bool,
}

// ===== file_operations repository抽象 =====

/// file_operations行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOpRecord {
    pub id: i64,
    pub uuid: String,
    pub operation_type: FileOpKind,
    pub status: FileOpStatus,
    pub undoable: bool,
    pub request_json: String,
    pub result_json: Option<String>,
    pub created_at: String,
    pub undone_at: Option<String>,
}

/// [`FileOpsRepo::insert_operation`] の入力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileOp {
    pub operation_type: FileOpKind,
    pub status: FileOpStatus,
    pub undoable: bool,
    pub request_json: String,
}

/// ファイル操作履歴のrepository (実装: gento-db)。
pub trait FileOpsRepo {
    /// 操作を1行記録する (1論理操作=1行)。
    fn insert_operation(&self, op: NewFileOp) -> Result<FileOpRecord, RepoError>;

    /// status・result JSON・undoableを更新する (存在しないidはNotFound)。
    fn update_operation(
        &self,
        id: i64,
        status: FileOpStatus,
        result_json: Option<&str>,
        undoable: bool,
    ) -> Result<(), RepoError>;

    /// 操作をundo済みへ遷移させる (status='undone' + undone_at + undoable=0)。
    fn mark_undone(&self, id: i64) -> Result<(), RepoError>;

    fn get_operation(&self, id: i64) -> Result<Option<FileOpRecord>, RepoError>;

    /// 新着順 (limit上限)。
    fn list_recent_operations(&self, limit: u32) -> Result<Vec<FileOpRecord>, RepoError>;

    /// `executing` 残留行の検出 (異常終了回復。起動時に呼ぶ)。
    fn list_executing_operations(&self) -> Result<Vec<FileOpRecord>, RepoError>;
}

// ===== 計画 (dry-run) =====

/// リネーム名の検証: 単一のファイル名であること (パス区切り・親参照なし、
/// 空・`.`・`..`・テンポラリ接頭辞不可)。
pub fn validate_new_name(name: &str) -> Result<(), FsOpError> {
    if name.is_empty() {
        return Err(FsOpError::precondition("新しい名前が空です"));
    }
    if name == "." || name == ".." {
        return Err(FsOpError::precondition(
            "'.' や '..' は名前として使えません",
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(FsOpError::precondition(
            "名前にパス区切り ('/' 等) を含められません",
        ));
    }
    if name.starts_with(TMP_FILE_PREFIX) {
        return Err(FsOpError::precondition(
            "'.gento-tmp-' で始まる名前は予約されています",
        ));
    }
    Ok(())
}

/// keep_both時の代替名: `name (2).ext` から順に空きを探す。
fn next_available_name(fs: &dyn FileOpsFs, dst: &Path) -> String {
    let file_name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), Some(ext.to_string())),
        _ => (file_name.clone(), None),
    };
    for n in 2..u32::MAX {
        let candidate = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        if !fs.exists(&dst.with_file_name(&candidate)) {
            return candidate;
        }
    }
    match &ext {
        Some(ext) => format!("{stem} (overflow).{ext}"),
        None => format!("{stem} (overflow)"),
    }
}

/// 衝突情報を組み立てる。ハッシュ一致は「サイズ一致かつ元の内容ハッシュが
/// 既知」のときのみ計算する (不一致サイズの全ファイルハッシュ計算を避ける)。
fn conflict_info(
    fs: &dyn FileOpsFs,
    dst: &Path,
    src_hash: Option<&str>,
    cancel: &AtomicBool,
) -> ConflictInfo {
    let stat = fs.stat(dst).ok();
    let byte_size = stat.as_ref().map(|s| s.byte_size).unwrap_or(0);
    let mtime_ms = stat.as_ref().map(|s| s.mtime_ms).unwrap_or(0);
    let keep_both_name = next_available_name(fs, dst);
    let hash_matches = match (src_hash, stat.as_ref()) {
        (Some(src_hash), Some(stat)) if stat.byte_size > 0 => fs
            .hash_file(dst, cancel)
            .ok()
            .map(|dst_hash| dst_hash == src_hash),
        _ => None,
    };
    ConflictInfo {
        dst: dst.to_path_buf(),
        dst_byte_size: byte_size,
        dst_mtime_ms: mtime_ms,
        hash_matches,
        keep_both_name,
    }
}

/// 操作を計画する (dry-run)。FSは現時点の観測のみに依存する。
///
/// - `dest_dir`: Copy/Moveの宛先ディレクトリ (絶対パス)。
/// - `new_name`: Renameの新しい名前。
/// - `roots`: 登録ルート一覧 (宛先root解決・DB反映判定に使用)。
pub fn plan_file_op(
    fs: &dyn FileOpsFs,
    cancel: &AtomicBool,
    kind: FileOpKind,
    files: Vec<FileOpFile>,
    dest_dir: Option<&Path>,
    new_name: Option<&str>,
    roots: &[FileRootRef],
) -> FileOpPlan {
    let mut plan_errors: Vec<FsOpError> = Vec::new();
    let dest_root_id = dest_dir.and_then(|dir| root_id_of(roots, dir));

    match kind {
        FileOpKind::Copy | FileOpKind::Move => {
            let Some(dir) = dest_dir else {
                plan_errors.push(FsOpError::precondition("宛先ディレクトリが必要です"));
                return empty_plan(kind, plan_errors);
            };
            if !fs.is_dir(dir) {
                plan_errors.push(FsOpError::new(
                    FsOpErrorKind::NotADirectory,
                    format!("{} はディレクトリではありません", dir.display()),
                ));
            }
        }
        FileOpKind::Rename => {
            if files.len() != 1 {
                plan_errors.push(FsOpError::precondition(
                    "リネームは1ファイルずつ行ってください",
                ));
            }
            match new_name {
                Some(name) => {
                    if let Err(err) = validate_new_name(name) {
                        plan_errors.push(err);
                    }
                }
                None => plan_errors.push(FsOpError::precondition("新しい名前が必要です")),
            }
        }
        FileOpKind::Mkdir | FileOpKind::Trash | FileOpKind::DeleteForever => {}
    }
    if !plan_errors.is_empty() {
        return empty_plan(kind, plan_errors);
    }

    let mut steps = Vec::with_capacity(files.len());
    let mut total_bytes = 0u64;
    for file in files {
        let src = file.abs_path.clone();
        let dst = match kind {
            FileOpKind::Copy | FileOpKind::Move => {
                dest_dir.expect("検証済み").join(&file.display_name)
            }
            FileOpKind::Rename => src
                .parent()
                .map(|parent| parent.join(new_name.expect("検証済み")))
                .unwrap_or_else(|| src.clone()),
            _ => src.clone(),
        };

        let mut plan_error = None;
        if fs.is_symlink(&src) {
            plan_error = Some(FsOpError::precondition(format!(
                "{} はシンボリックリンクです (操作対象外)",
                src.display()
            )));
        } else if !fs.exists(&src) {
            plan_error = Some(FsOpError::not_found(format!(
                "{} が見つかりません (外部で移動・削除された可能性)",
                src.display()
            )));
        }

        let conflict = if plan_error.is_none()
            && matches!(
                kind,
                FileOpKind::Copy | FileOpKind::Move | FileOpKind::Rename
            )
            && fs.exists(&dst)
        {
            Some(conflict_info(
                fs,
                &dst,
                file.content_hash.as_deref(),
                cancel,
            ))
        } else {
            None
        };
        if kind == FileOpKind::Copy {
            if let Ok(stat) = fs.stat(&src) {
                total_bytes += stat.byte_size;
            }
        }
        steps.push(PlannedStep {
            file,
            src,
            dst,
            conflict,
            plan_error,
        });
    }

    // 容量事前確認 (コピーのみ。不足は警告として返し実行阻止はしない —
    // 実行時にも同エラーが起こるため二重防護)
    let disk_shortfall_bytes = if kind == FileOpKind::Copy {
        dest_dir.and_then(|dir| {
            fs.free_bytes(dir)
                .ok()
                .filter(|free| *free < total_bytes)
                .map(|free| total_bytes - free)
        })
    } else {
        None
    };

    FileOpPlan {
        kind,
        steps,
        dest_root_id,
        plan_errors,
        disk_shortfall_bytes,
    }
}

fn empty_plan(kind: FileOpKind, plan_errors: Vec<FsOpError>) -> FileOpPlan {
    FileOpPlan {
        kind,
        steps: Vec::new(),
        dest_root_id: None,
        plan_errors,
        disk_shortfall_bytes: None,
    }
}

/// `path` を含む登録ルートのid。
fn root_id_of(roots: &[FileRootRef], path: &Path) -> Option<i64> {
    roots
        .iter()
        .find(|root| path.starts_with(&root.root_path))
        .map(|root| root.root_id)
}

// ===== 実行 =====

/// 計画を実行する。部分成功 (ファイル単位のtry) を集計し、file_operationsへ
/// 1論理操作として記録する。戻り値は操作ログ確定後の結果 (呼び出し側は
/// これをもってUIの成功通知をしてよい — 安全原則5)。
pub fn execute_file_op(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    plan: &FileOpPlan,
    resolutions: &ResolutionSpec,
    roots: &[FileRootRef],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(FileOpProgress),
) -> Result<FileOpOutcome, RepoError> {
    let request_json = serde_json::to_string(&request_json_of(plan, resolutions))
        .map_err(|e| RepoError::Db(format!("serialize request: {e}")))?;
    let record = {
        let request_json = request_json.clone();
        write_ret(store, move |repos: &dyn crate::library::AllRepos| {
            repos.insert_operation(NewFileOp {
                operation_type: plan.kind,
                status: FileOpStatus::Executing,
                undoable: false,
                request_json,
            })
        })?
    };
    let op_id = record.id;
    let op_uuid = record.uuid.clone();

    let mut results: Vec<StepResult> = Vec::with_capacity(plan.steps.len());
    for (index, step) in plan.steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            results.push(StepResult {
                file_id: step.file.file_id,
                from: step.src.clone(),
                to: None,
                status: StepStatus::Cancelled,
                resolution: None,
                error: Some(FsOpError::new(
                    FsOpErrorKind::Cancelled,
                    "キャンセルされました",
                )),
                warning: None,
                tmp: None,
                undo: Vec::new(),
            });
            continue;
        }
        progress(FileOpProgress {
            op_id,
            index,
            total: plan.steps.len(),
            from: step.src.display().to_string(),
            to: Some(step.dst.display().to_string()),
            done: false,
        });
        let result = execute_step(
            store,
            fs,
            step,
            plan.kind,
            resolutions,
            roots,
            cancel,
            &op_uuid,
            index,
        );
        results.push(result);
        progress(FileOpProgress {
            op_id,
            index,
            total: plan.steps.len(),
            from: step.src.display().to_string(),
            to: Some(step.dst.display().to_string()),
            done: true,
        });
    }

    let (status, undoable) = summarize(&results, plan.kind);
    let outcome = FileOpOutcome {
        op_id,
        kind: plan.kind,
        status,
        undoable,
        steps: results,
    };
    let result_json = serde_json::to_string(&outcome)
        .map_err(|e| RepoError::Db(format!("serialize result: {e}")))?;
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        repos.update_operation(op_id, status, Some(&result_json), undoable)
    })?;
    Ok(outcome)
}

/// request JSONの実体。`dest_dir`/`new_name`/各ステップの計画時`to`は
/// 異常終了回復の一時ファイル再構築 (所有tmpパスの復元) に使用する。
fn request_json_of(plan: &FileOpPlan, resolutions: &ResolutionSpec) -> serde_json::Value {
    serde_json::json!({
        "kind": plan.kind.as_str(),
        "files": plan.steps.iter().map(|s| serde_json::json!({
            "file_id": s.file.file_id,
            "from": s.src.display().to_string(),
            "to": s.dst.display().to_string(),
        })).collect::<Vec<_>>(),
        "dest_dir": plan.steps.first()
            .and_then(|s| s.dst.parent())
            .map(|p| p.display().to_string()),
        "new_name": plan.steps.first()
            .and_then(|s| s.dst.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        "dest_root_id": plan.dest_root_id,
        "global_resolution": resolutions.global.map(|r| r.as_str()),
    })
}

/// 全ステップ結果から操作全体のstatus・undoableを導く。
fn summarize(results: &[StepResult], kind: FileOpKind) -> (FileOpStatus, bool) {
    let done = results
        .iter()
        .filter(|r| r.status == StepStatus::Done)
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, StepStatus::Failed | StepStatus::Cancelled))
        .count();
    let undo_actions = results.iter().filter(|r| !r.undo.is_empty()).count();
    let status = if failed == 0 {
        FileOpStatus::Done
    } else if done > 0 {
        FileOpStatus::Partial
    } else {
        FileOpStatus::Failed
    };
    // Mkdir (対象ファイル無し) とDeleteForever (不可逆) はUndo対象外。
    // 部分成功でも実行済みステップの逆操作は可能 (前提検証で守られる)。
    let undoable =
        !matches!(kind, FileOpKind::Mkdir | FileOpKind::DeleteForever) && undo_actions > 0;
    (status, undoable)
}

/// 1ステップの実行: FS操作 → DB反映 (1tx)。
///
/// 上書き解決の実行モデル (旧ファイル保護): (1) 旧宛先を先にゴミ箱へ
/// 退避し (トークン+エントリを記録) → (2) 新ファイルをNOREPLACEで配置。
/// 配置失敗時は退避した旧ファイルを復帰させてステップを不成立にする。
/// undo記録は「新ファイルの除去 + 旧ファイルの復帰」の複合。
#[allow(clippy::too_many_arguments)]
fn execute_step(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    step: &PlannedStep,
    kind: FileOpKind,
    resolutions: &ResolutionSpec,
    roots: &[FileRootRef],
    cancel: &AtomicBool,
    op_uuid: &str,
    seq: usize,
) -> StepResult {
    let mut result = StepResult {
        file_id: step.file.file_id,
        from: step.src.clone(),
        to: Some(step.dst.clone()),
        status: StepStatus::Failed,
        resolution: None,
        error: None,
        warning: None,
        tmp: None,
        undo: Vec::new(),
    };
    if let Some(err) = &step.plan_error {
        result.error = Some(err.clone());
        return result;
    }

    // 衝突の解決 (未指定はskip = 安全側)
    let resolution = if step.conflict.is_some() {
        let chosen = resolutions
            .of(step.file.file_id)
            .unwrap_or(ConflictResolution::Skip);
        result.resolution = Some(chosen);
        Some(chosen)
    } else {
        None
    };
    if resolution == Some(ConflictResolution::Skip) {
        result.status = StepStatus::Skipped;
        result.to = None;
        return result;
    }

    // 実効宛先 (keep_bothは代替名)
    let effective_dst = match (step.conflict.as_ref(), resolution) {
        (Some(conflict), Some(ConflictResolution::KeepBoth)) => step
            .dst
            .parent()
            .map(|p| p.join(&conflict.keep_both_name))
            .unwrap_or_else(|| step.dst.clone()),
        _ => step.dst.clone(),
    };

    // 自己上書き拒否: 操作先が操作元そのもの (上書きは破壊に等しいため)。
    // Trash/DeleteForever/Mkdirはdst==srcが正常形のため対象外。
    if matches!(
        kind,
        FileOpKind::Copy | FileOpKind::Move | FileOpKind::Rename
    ) && effective_dst == step.src
    {
        result.error = Some(FsOpError::precondition(
            "操作先が元ファイル自身のため拒否しました",
        ));
        return result;
    }

    // 実行直前の存在確認 (計画→実行間の外部削除)
    if !fs.exists(&step.src) {
        result.error = Some(FsOpError::not_found(format!(
            "{} が見つかりません (実行直前に消失しました)",
            step.src.display()
        )));
        return result;
    }

    let overwrite = resolution == Some(ConflictResolution::Overwrite);
    let places_file = matches!(
        kind,
        FileOpKind::Copy | FileOpKind::Move | FileOpKind::Rename
    );

    // no-clobber前置検査: 上書き解決以外で宛先が現存する場合、配置を
    // 拒否して再計画へ戻す (NOREPLACE確定でも同じ結果になるが、早期の
    // 明確なエラーとして提供。TOCTOU競合の最終防御はNOREPLACE自身)。
    if places_file && !overwrite && fs.exists(&effective_dst) {
        result.error = Some(FsOpError::new(
            FsOpErrorKind::AlreadyExists,
            format!(
                "{} が計画後に作成されたため実行しませんでした (最新の状態で再計画してください)",
                effective_dst.display()
            ),
        ));
        return result;
    }

    // 上書き: 旧宛先を先にゴミ箱へ退避する (旧ファイル保護)。
    // 退避に失敗した場合、旧ファイルは無傷のまま残るため上書き中止。
    let mut evacuated: Option<(TrashEntry, FileStat)> = None;
    if overwrite {
        let old_stat = match fs.stat(&effective_dst) {
            Ok(stat) => stat,
            Err(_) => {
                result.error = Some(FsOpError::not_found(format!(
                    "{} (上書き対象) が実行直前に消失しました (再計画してください)",
                    effective_dst.display()
                )));
                return result;
            }
        };
        match fs.trash(&effective_dst) {
            Ok(entry) => evacuated = Some((entry, old_stat)),
            Err(err) => {
                result.error = Some(FsOpError::new(
                    err.kind.clone(),
                    format!(
                        "上書き前の旧ファイル退避に失敗したため中止しました (旧ファイルは無傷): {err}"
                    ),
                ));
                return result;
            }
        }
    }

    // 一時ファイル (コピーと跨界移動フォールバックで使用。所有権記録)。
    let tmp = if matches!(kind, FileOpKind::Copy | FileOpKind::Move) {
        let t = tmp_path_for(&effective_dst, op_uuid, seq);
        result.tmp = Some(t.clone());
        t
    } else {
        PathBuf::new()
    };

    /// 配置結果 (moveの跨界フォールバックでsrc削除失敗があった場合の
    /// 警告を運ぶ — 宛先完成済みのため成功扱い)。
    enum Placement {
        Placed,
        /// ゴミ箱へ移した (エントリと移動直前のトークン)。
        Trashed(TrashEntry, Option<FileStat>),
        Removed,
        DirCreated,
    }
    let placement: Result<Placement, FsOpError> = match kind {
        FileOpKind::Copy => fs
            .copy_file(&step.src, &effective_dst, &tmp, cancel)
            .map(|_| Placement::Placed),
        FileOpKind::Move | FileOpKind::Rename => {
            match move_file(fs, &step.src, &effective_dst, &tmp, cancel) {
                Ok(MoveOutcome::Completed) => Ok(Placement::Placed),
                Ok(MoveOutcome::CompletedWithWarning(warning)) => {
                    result.warning = Some(warning);
                    Ok(Placement::Placed)
                }
                Err(err) => Err(err),
            }
        }
        FileOpKind::Trash => {
            // トークンはゴミ箱へ移す直前の観測 (復帰検証用)
            let before = fs.stat(&step.src).ok();
            fs.trash(&step.src)
                .map(|entry| Placement::Trashed(entry, before))
        }
        FileOpKind::DeleteForever => fs.remove_file(&step.src).map(|_| Placement::Removed),
        FileOpKind::Mkdir => fs.mkdir(&effective_dst).map(|_| Placement::DirCreated),
    };

    match placement {
        Ok(Placement::Placed) => {
            let observed = fs.stat(&effective_dst).ok();
            result.status = StepStatus::Done;
            result.to = Some(effective_dst.clone());
            result.undo = undo_actions_of(kind, step, &effective_dst, observed.as_ref(), evacuated);
            if let Err(err) = reflect_db(store, step, kind, &effective_dst, observed, roots) {
                // FS成功・DB失敗: 再スキャンで収束する状態だが、操作としては
                // 失敗を報告する (黙って成功としない)。
                result.status = StepStatus::Failed;
                result.error = Some(FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("DB反映に失敗しました (再スキャンで回復します): {err}"),
                ));
            }
        }
        Ok(Placement::Trashed(entry, before)) => {
            result.status = StepStatus::Done;
            result.to = None;
            if let Some(stat) = before {
                result.undo = vec![UndoAction::RestoreFromTrash {
                    original: step.src.clone(),
                    trash_entry: entry,
                    expect: stat.token().into(),
                }];
            }
            if let Err(err) = reflect_db(store, step, kind, &step.src, None, roots) {
                result.status = StepStatus::Failed;
                result.error = Some(FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("DB反映に失敗しました (再スキャンで回復します): {err}"),
                ));
            }
        }
        Ok(Placement::Removed) | Ok(Placement::DirCreated) => {
            result.status = StepStatus::Done;
            if let Err(err) = reflect_db(store, step, kind, &effective_dst, None, roots) {
                result.status = StepStatus::Failed;
                result.error = Some(FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("DB反映に失敗しました (再スキャンで回復します): {err}"),
                ));
            }
        }
        Err(err) => {
            // 配置失敗: 上書き退避済みの旧ファイルを復帰させる
            // (ステップ不成立のため旧ファイルを元位置へ戻す)。
            if let Some((entry, old_stat)) = evacuated {
                if let Err(restore_err) =
                    fs.restore_trash(&entry, &effective_dst, old_stat.byte_size)
                {
                    result.error = Some(FsOpError::new(
                        FsOpErrorKind::Io,
                        format!(
                            "{err}。さらに退避した旧ファイルの復帰にも失敗しました \
                             (ゴミ箱から手動で復元してください): {restore_err}"
                        ),
                    ));
                    return result;
                }
            }
            result.error = Some(err);
        }
    }
    result
}

/// undo実行が一時ファイルを使う際のseq基点 (ステップseqと衝突しない
/// 専用帯。最大ステップ数に対して十分大きい値)。
const UNDO_TMP_SEQ_BASE: usize = 900_000;

/// 移動の結果 (跨界フォールバックでsrc削除に失敗した場合の警告を運ぶ —
/// 宛先は完成済みのため成功扱い)。
enum MoveOutcome {
    Completed,
    CompletedWithWarning(String),
}

/// 移動: 同一FSはNOREPLACEリネーム、跨界 (EXDEV) はcopy+noreplace配置+
/// src削除へフォールバック。src削除失敗は警告付き成功
/// (「宛先完成済みならstepをwarning付き成功扱い」)。
fn move_file(
    fs: &dyn FileOpsFs,
    src: &Path,
    dst: &Path,
    tmp: &Path,
    cancel: &AtomicBool,
) -> Result<MoveOutcome, FsOpError> {
    match fs.rename_noreplace(src, dst) {
        Ok(()) => Ok(MoveOutcome::Completed),
        // 同一FSでの失敗のうち跨界のみフォールバック (権限等は伝播)。
        Err(err) if is_cross_device(&err) => {
            fs.copy_file(src, dst, tmp, cancel)?;
            match fs.remove_file(src) {
                Ok(()) => Ok(MoveOutcome::Completed),
                Err(e) => Ok(MoveOutcome::CompletedWithWarning(format!(
                    "跨界移動のコピー後の元ファイル削除に失敗しました \
                     (コピーは完了済み。手動で削除してください): {e}"
                ))),
            }
        }
        Err(err) => Err(err),
    }
}

/// ステップ成功時の逆操作列を組み立てる (上書きステップは
/// 「新ファイル除去 + 旧ファイル復帰」の複合。実行順 = 記録順)。
fn undo_actions_of(
    kind: FileOpKind,
    step: &PlannedStep,
    effective_dst: &Path,
    observed: Option<&FileStat>,
    evacuated: Option<(TrashEntry, FileStat)>,
) -> Vec<UndoAction> {
    let mut undo = Vec::new();
    match kind {
        FileOpKind::Copy => {
            if let Some(stat) = observed {
                undo.push(UndoAction::DeleteCopy {
                    path: effective_dst.to_path_buf(),
                    expect: stat.token().into(),
                });
            }
        }
        FileOpKind::Move | FileOpKind::Rename => {
            if let Some(stat) = observed {
                undo.push(UndoAction::MoveBack {
                    from: effective_dst.to_path_buf(),
                    to: step.src.clone(),
                    expect: stat.token().into(),
                });
            }
        }
        FileOpKind::Trash | FileOpKind::DeleteForever | FileOpKind::Mkdir => {}
    }
    if let Some((entry, old_stat)) = evacuated {
        undo.push(UndoAction::RestoreFromTrash {
            original: effective_dst.to_path_buf(),
            trash_entry: entry,
            expect: old_stat.token().into(),
        });
    }
    undo
}

/// エラーが跨界 (EXDEV) か。実装側はメッセージに `CrossesDevices` を含める
/// 契約にする (io::ErrorKindそのものをtrait境界へ出さないため)。
fn is_cross_device(err: &FsOpError) -> bool {
    err.message.contains("CrossesDevices")
}

/// FS成功後のDB反映 (1ファイル=1トランザクション)。
fn reflect_db(
    store: &dyn RepoStore,
    step: &PlannedStep,
    kind: FileOpKind,
    effective_dst: &Path,
    observed: Option<FileStat>,
    roots: &[FileRootRef],
) -> Result<(), RepoError> {
    let file = step.file.clone();
    let dst = effective_dst.to_path_buf();
    let dst_root = root_id_of(roots, &dst);
    let roots_vec: Vec<FileRootRef> = roots.to_vec();
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        match kind {
            FileOpKind::Copy => {
                let Some(root_id) = dst_root else {
                    return Ok(()); // 宛先がルート外: 登録対象外
                };
                let Some(root_path) = roots_vec
                    .iter()
                    .find(|r| r.root_id == root_id)
                    .map(|r| r.root_path.clone())
                else {
                    return Ok(());
                };
                let Some(rel) = dst.strip_prefix(&root_path).ok() else {
                    return Ok(());
                };
                // 上書きで破壊された旧ファイル行を掃除 (AssetはRESTRICTで残る)
                if let Some(existing) = repos.find_by_path(root_id, rel)? {
                    repos.delete_file(existing.id)?;
                }
                let record = repos.upsert_file(FileUpsert {
                    root_id,
                    rel_path: rel.to_path_buf(),
                    display_name: display_name_of(&dst),
                    byte_size: observed.map(|s| s.byte_size).unwrap_or(0),
                    mtime_ms: observed.map(|s| s.mtime_ms).unwrap_or(0),
                    mtime_ns: observed.map(|s| s.mtime_ns).unwrap_or(0),
                    scan_state: ScanState::Present,
                    os_file_id: observed.and_then(|s| s.os_file_id),
                    detach_asset: false,
                })?;
                // コピーは内容不変のため、元のAssetへ即時共有リンク
                // (ハッシュ再計算を省き、評価等のメタデータを継承)。
                if let Some(asset_id) = file.asset_id {
                    repos.link_file(record.id, asset_id)?;
                }
                Ok(())
            }
            FileOpKind::Move | FileOpKind::Rename => {
                match dst_root {
                    Some(root_id) => {
                        let root_path = roots_vec
                            .iter()
                            .find(|r| r.root_id == root_id)
                            .map(|r| r.root_path.clone())
                            .expect("root resolvable");
                        let rel = dst
                            .strip_prefix(&root_path)
                            .map_err(|_| {
                                RepoError::Db(format!(
                                    "dst {} not under root {}",
                                    dst.display(),
                                    root_path.display()
                                ))
                            })?
                            .to_path_buf();
                        // 移動先を占う旧ファイル行 (上書き・stale missing) を掃除
                        if let Some(existing) = repos.find_by_path(root_id, &rel)? {
                            repos.delete_file(existing.id)?;
                        }
                        repos.relocate_file(
                            file.file_id,
                            FileUpsert {
                                root_id,
                                rel_path: rel,
                                display_name: display_name_of(&dst),
                                byte_size: observed.map(|s| s.byte_size).unwrap_or(0),
                                mtime_ms: observed.map(|s| s.mtime_ms).unwrap_or(0),
                                mtime_ns: observed.map(|s| s.mtime_ns).unwrap_or(0),
                                scan_state: ScanState::Present,
                                os_file_id: observed.and_then(|s| s.os_file_id),
                                detach_asset: false,
                            },
                        )?;
                        Ok(())
                    }
                    // 登録ルート外への移動: 行はmissing化 (再出現で復帰。
                    // rel_pathの仮想化は行わない — ゴミ箱と同一規約)。
                    None => repos.set_scan_state(file.file_id, ScanState::Missing),
                }
            }
            // ゴミ箱・完全削除: 登録ルート外へ出るためmissing扱い
            // (Assetは残る。hash共有の再利用可)。
            FileOpKind::Trash | FileOpKind::DeleteForever => {
                repos.set_scan_state(file.file_id, ScanState::Missing)
            }
            FileOpKind::Mkdir => Ok(()),
        }
    })
}

fn display_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

// ===== 実行 (mkdir — 対象ファイルを持たない単独操作) =====

/// 新規フォルダ作成 (親ディレクトリ内に `name` を作る)。file_operationsへ
/// 記録する (Undo対象外)。
pub fn execute_mkdir(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    parent: &Path,
    name: &str,
) -> Result<FileOpOutcome, RepoError> {
    let precheck = |error: FsOpError| FileOpOutcome {
        op_id: 0,
        kind: FileOpKind::Mkdir,
        status: FileOpStatus::Failed,
        undoable: false,
        steps: vec![StepResult {
            file_id: 0,
            from: parent.to_path_buf(),
            to: None,
            status: StepStatus::Failed,
            resolution: None,
            error: Some(error),
            warning: None,
            tmp: None,
            undo: Vec::new(),
        }],
    };
    if let Err(err) = validate_new_name(name) {
        return Ok(precheck(err));
    }
    if !fs.is_dir(parent) {
        return Ok(precheck(FsOpError::new(
            FsOpErrorKind::NotADirectory,
            format!("{} はディレクトリではありません", parent.display()),
        )));
    }
    let dst = parent.join(name);
    let request = serde_json::json!({
        "kind": "mkdir",
        "parent": parent.display().to_string(),
        "name": name,
    });
    let request_json = serde_json::to_string(&request)
        .map_err(|e| RepoError::Db(format!("serialize request: {e}")))?;
    let record = write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        repos.insert_operation(NewFileOp {
            operation_type: FileOpKind::Mkdir,
            status: FileOpStatus::Executing,
            undoable: false,
            request_json,
        })
    })?;
    let mut step = StepResult {
        file_id: 0,
        from: parent.to_path_buf(),
        to: Some(dst.clone()),
        status: StepStatus::Done,
        resolution: None,
        error: None,
        warning: None,
        tmp: None,
        undo: Vec::new(),
    };
    let status = match fs.mkdir(&dst) {
        Ok(()) => FileOpStatus::Done,
        Err(err) => {
            step.status = StepStatus::Failed;
            step.error = Some(err);
            FileOpStatus::Failed
        }
    };
    let outcome = FileOpOutcome {
        op_id: record.id,
        kind: FileOpKind::Mkdir,
        status,
        undoable: false,
        steps: vec![step],
    };
    let result_json = serde_json::to_string(&outcome)
        .map_err(|e| RepoError::Db(format!("serialize result: {e}")))?;
    let op_id = record.id;
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        repos.update_operation(op_id, status, Some(&result_json), false)
    })?;
    Ok(outcome)
}

// ===== Undo =====

/// Undo前提検証の拒否理由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum UndoRefusal {
    /// Undo対象の操作ではない (undoable=false)。
    NotUndoable,
    AlreadyUndone,
    NoUndoSteps,
    /// 逆操作の対象ファイルが存在しない。
    FileMissing {
        path: PathBuf,
    },
    /// 対象の現在トークンが記録と一致しない (外部で変更された)。
    TokenMismatch {
        path: PathBuf,
    },
    /// 戻し先にファイルが存在する (外部ファイルを破壊しない)。
    DestinationExists {
        path: PathBuf,
    },
    /// ゴミ箱内に復帰候補が無い。
    TrashEntryNotFound {
        path: PathBuf,
    },
    /// ゴミ箱エントリのサイズが記録 (トークンのsize成分) と一致しない
    /// (パス再利用・外部trashed混在での誤復帰防止)。
    TrashEntrySizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    Io {
        path: PathBuf,
        message: String,
    },
}

impl UndoRefusal {
    /// UI表示用の理由文。
    pub fn describe(&self) -> String {
        match self {
            UndoRefusal::NotUndoable => "この操作はUndoできません".to_string(),
            UndoRefusal::AlreadyUndone => "すでにUndo済みの操作です".to_string(),
            UndoRefusal::NoUndoSteps => "Undo可能なステップがありません".to_string(),
            UndoRefusal::FileMissing { path } => format!(
                "{} が見つかりません (外部で移動・削除されました)",
                path.display()
            ),
            UndoRefusal::TokenMismatch { path } => format!(
                "{} は操作後に外部で変更されているため、元に戻すと他の変更を破壊する恐れがあります",
                path.display()
            ),
            UndoRefusal::DestinationExists { path } => {
                format!("戻し先 {} に別のファイルが存在します", path.display())
            }
            UndoRefusal::TrashEntryNotFound { path } => {
                format!("ゴミ箱内に {} の復帰情報が見つかりません", path.display())
            }
            UndoRefusal::TrashEntrySizeMismatch {
                path,
                expected,
                actual,
            } => format!(
                "ゴミ箱内の {} は操作時のサイズ ({expected}バイト) と異なるため \
                 復帰を拒否しました (現状: {actual}バイト)",
                path.display()
            ),
            UndoRefusal::Io { path, message } => format!("{}: {}", path.display(), message),
        }
    }
}

/// Undo対象の操作 (repo記録 + 復元済みUndoAction列)。
#[derive(Debug, Clone, PartialEq)]
pub struct UndoCandidate {
    pub op_id: i64,
    /// file_operations行のuuid (undo実行が一時ファイルを作る場合の
    /// 所有権記録に使用 — 厳密形式の掃除対象)。
    pub op_uuid: String,
    pub kind: FileOpKind,
    pub status: FileOpStatus,
    pub undoable: bool,
    pub actions: Vec<UndoAction>,
}

/// 操作記録からUndo候補を組み立てる (result JSONから逆操作を復元)。
pub fn undo_candidate_of(record: &FileOpRecord) -> UndoCandidate {
    let actions = record
        .result_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<FileOpOutcome>(json).ok())
        .map(|outcome| {
            outcome
                .steps
                .iter()
                .filter(|s| s.status != StepStatus::Skipped)
                .flat_map(|s| s.undo.iter().cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    UndoCandidate {
        op_id: record.id,
        op_uuid: record.uuid.clone(),
        kind: record.operation_type,
        status: record.status,
        undoable: record.undoable,
        actions,
    }
}

/// Undo実行前の前提検証 (全アクション検証してから実行する — 部分Undoの
/// 不整合を防ぐ)。1つでも崩れていれば全理由を返す。
///
/// 検証は**記録順を考慮した逐次シミュレーション**: 先頭側アクションが
/// 空けるパス (`MoveBack.from`・`DeleteCopy.path`) は、後続アクションの
/// 「戻し先不在」判定から除外する (上書きステップの複合undo —
/// 「新ファイル除去 → 旧ファイル復帰」— で、復帰先が新ファイルに
/// 占められていることを正しく扱う)。
pub fn verify_undo(candidate: &UndoCandidate, fs: &dyn FileOpsFs) -> Result<(), Vec<UndoRefusal>> {
    let mut refusals = Vec::new();
    if candidate.status == FileOpStatus::Undone {
        refusals.push(UndoRefusal::AlreadyUndone);
    }
    if !candidate.undoable
        || matches!(
            candidate.kind,
            FileOpKind::Mkdir | FileOpKind::DeleteForever
        )
    {
        refusals.push(UndoRefusal::NotUndoable);
        return Err(refusals);
    }
    if candidate.actions.is_empty() {
        refusals.push(UndoRefusal::NoUndoSteps);
        return Err(refusals);
    }
    // 先行アクションが空けるパス (逐次シミュレーション)。
    let mut vacated: Vec<PathBuf> = Vec::new();
    for action in &candidate.actions {
        if let Err(refusal) = verify_undo_action(action, fs, &vacated) {
            refusals.push(refusal);
        }
        match action {
            UndoAction::MoveBack { from, .. } => vacated.push(from.clone()),
            UndoAction::DeleteCopy { path, .. } => vacated.push(path.clone()),
            UndoAction::RestoreFromTrash { .. } => {}
        }
    }
    if refusals.is_empty() {
        Ok(())
    } else {
        Err(refusals)
    }
}

fn verify_undo_action(
    action: &UndoAction,
    fs: &dyn FileOpsFs,
    vacated: &[PathBuf],
) -> Result<(), UndoRefusal> {
    let occupied = |path: &Path| fs.exists(path) && !vacated.iter().any(|v| v == path);
    match action {
        UndoAction::MoveBack { from, to, expect } => {
            verify_token(from, expect, fs)?;
            if occupied(to) {
                return Err(UndoRefusal::DestinationExists { path: to.clone() });
            }
            Ok(())
        }
        UndoAction::RestoreFromTrash {
            original,
            trash_entry,
            expect,
        } => {
            if occupied(original) {
                return Err(UndoRefusal::DestinationExists {
                    path: original.clone(),
                });
            }
            // 厳密同一性: 記録されたエントリの存在とサイズ (トークンの
            // size成分) を検証する (パス再利用・複数エントリ競合の誤復帰
            // 防止。verify_undoがエントリ不在すら見ていなかったレビュー
            // 指摘への対応)。
            let trashed = trash_entry
                .trash_dir
                .join("files")
                .join(&trash_entry.file_name);
            match fs.trash_entry_size(trash_entry) {
                None => Err(UndoRefusal::TrashEntryNotFound { path: trashed }),
                Some(actual) if actual != expect.byte_size => {
                    Err(UndoRefusal::TrashEntrySizeMismatch {
                        path: trashed,
                        expected: expect.byte_size,
                        actual,
                    })
                }
                Some(_) => Ok(()),
            }
        }
        UndoAction::DeleteCopy { path, expect } => verify_token(path, expect, fs),
    }
}

/// `path` の現在トークンが `expect` と一致するか (存在・無変更の検証)。
fn verify_token(path: &Path, expect: &TokenDto, fs: &dyn FileOpsFs) -> Result<(), UndoRefusal> {
    match fs.stat(path) {
        Ok(current) => {
            if current.token() == expect.to_token() {
                Ok(())
            } else {
                Err(UndoRefusal::TokenMismatch {
                    path: path.to_path_buf(),
                })
            }
        }
        Err(_) => Err(UndoRefusal::FileMissing {
            path: path.to_path_buf(),
        }),
    }
}

/// 1アクションのUndo結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoStepResult {
    #[serde(flatten)]
    pub action: UndoAction,
    pub status: StepStatus,
    pub error: Option<FsOpError>,
    /// 完了したが注意喚起がいる事象 (跨界undo移動のsrc削除失敗等)。
    #[serde(default)]
    pub warning: Option<String>,
}

/// Undo実行の全体結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoOutcome {
    pub op_id: i64,
    /// 全成功でのみtrue (このときのみ操作行は `undone` へ遷移する)。
    pub fully_undone: bool,
    pub steps: Vec<UndoStepResult>,
    /// 拒否された場合の理由 (verify失敗。何も実行していない)。
    pub refusals: Vec<UndoRefusal>,
    /// 補償ロールバックの結果報告 (失敗した補償がある場合のみ追加)。
    #[serde(default)]
    pub rollback_errors: Vec<String>,
}

/// Undoを実行する (逐次実行 + 補償ロールバック。file_ops.rsモジュールdoc
/// 「Undo原子性ポリシー」参照):
///
/// 1. 前提検証 ([`verify_undo`]) に失敗した場合は何も実行せず理由を返す。
/// 2. 各アクションは実行直前に前提を再検証してから実行する。
/// 3. アクションの失敗・キャンセル時、実行済みアクションを**逆順で補償**
///    する (各補償も前提検証付き)。全成功でのみ `undone`、それ以外は
///    `partial_undone` +理由を返す。
pub fn execute_undo(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    candidate: &UndoCandidate,
    roots: &[FileRootRef],
    cancel: &AtomicBool,
) -> Result<UndoOutcome, RepoError> {
    if let Err(refusals) = verify_undo(candidate, fs) {
        return Ok(UndoOutcome {
            op_id: candidate.op_id,
            fully_undone: false,
            steps: Vec::new(),
            refusals,
            rollback_errors: Vec::new(),
        });
    }
    let mut steps = Vec::with_capacity(candidate.actions.len());
    // 実行済みアクション (補償に必要な実行時情報付き)。
    let mut executed: Vec<ExecutedAction> = Vec::new();
    let mut failure: Option<(UndoAction, FsOpError)> = None;
    for (index, action) in candidate.actions.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            failure = Some((
                action.clone(),
                FsOpError::new(FsOpErrorKind::Cancelled, "キャンセルされました"),
            ));
            break;
        }
        match execute_undo_action(store, fs, action, roots, &candidate.op_uuid, index) {
            Ok(exec) => {
                steps.push(UndoStepResult {
                    action: action.clone(),
                    status: StepStatus::Done,
                    error: None,
                    warning: exec.warning.clone(),
                });
                executed.push(exec);
            }
            Err(boxed) => {
                let (err, partial_reflect) = *boxed;
                // DB反映のみ失敗した場合 (FS側は実行済み) は部分実行として
                // 補償対象に含めた上で失敗報告する。
                if let Some(partial) = partial_reflect {
                    executed.push(partial);
                }
                failure = Some((action.clone(), err));
                break;
            }
        }
    }
    let mut rollback_errors = Vec::new();
    let mut fully_undone = false;
    match failure {
        None => fully_undone = true,
        Some((action, err)) => {
            steps.push(UndoStepResult {
                action,
                status: StepStatus::Failed,
                error: Some(err.clone()),
                warning: None,
            });
            // 補償ロールバック: 実行済みアクションを逆順で巻き戻す。
            for exec in executed.iter().rev() {
                if let Err(comp_err) = compensate_executed(fs, exec) {
                    rollback_errors.push(format!(
                        "巻き戻し中に失敗しました (手動確認が必要です): {comp_err}"
                    ));
                }
            }
        }
    }
    let outcome = UndoOutcome {
        op_id: candidate.op_id,
        fully_undone,
        steps,
        refusals: Vec::new(),
        rollback_errors,
    };
    let op_id = candidate.op_id;
    if fully_undone {
        write_ret(store, move |repos: &dyn crate::library::AllRepos| {
            repos.mark_undone(op_id)
        })?;
    } else {
        // partial_undone: 操作行を明示的な状態へ確定し、再undo不能にする
        // (一部アクション実行済みのため、再実行は前提検証で破綻する)。
        let report = serde_json::to_string(&outcome)
            .map_err(|e| RepoError::Db(format!("serialize undo report: {e}")))?;
        write_ret(store, move |repos: &dyn crate::library::AllRepos| {
            repos.update_operation(op_id, FileOpStatus::PartialUndone, Some(&report), false)
        })?;
    }
    Ok(outcome)
}

/// 実行済みundoアクション (補償に必要な実行時情報を含む)。
struct ExecutedAction {
    action: UndoAction,
    /// 警告 (跨界undo移動のsrc削除失敗等)。
    warning: Option<String>,
    /// DeleteCopy実行時に生成されたゴミ箱エントリ (補償=元位置へ戻す
    /// 対象の厳密特定に使用)。
    captured_entry: Option<TrashEntry>,
}

/// 1逆操作の実行 (FS + DB反映)。失敗時は (エラー, FS実行済みでDB反映のみ
/// 失敗した場合の部分実行情報) を返す (size制約のためBox)。
fn execute_undo_action(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    action: &UndoAction,
    roots: &[FileRootRef],
    op_uuid: &str,
    index: usize,
) -> Result<ExecutedAction, Box<(FsOpError, Option<ExecutedAction>)>> {
    match action {
        UndoAction::MoveBack { from, to, expect } => {
            // 再検証 (verifyとexecuteの間の変化にも安全側で倒れる)
            if let Err(refusal) = verify_token(from, expect, fs) {
                return Err(Box::new((
                    FsOpError::precondition(refusal.describe()),
                    None,
                )));
            }
            if fs.exists(to) {
                return Err(Box::new((
                    FsOpError::precondition(
                        UndoRefusal::DestinationExists { path: to.clone() }.describe(),
                    ),
                    None,
                )));
            }
            // 跨界undo (元の移動が跨界だった場合) に備えフォールバック付き。
            // 一時名は元操作と同じuuid空間 + 専用seq帯 (所有権掃除対象)。
            let tmp = tmp_path_for(to, op_uuid, UNDO_TMP_SEQ_BASE + index);
            let outcome = match move_file(fs, from, to, &tmp, &AtomicBool::new(false)) {
                Ok(outcome) => outcome,
                Err(err) => return Err(Box::new((err, None))),
            };
            if let Err(err) = undo_reflect_move(store, fs, from, to, roots) {
                return Err(Box::new((
                    FsOpError::new(FsOpErrorKind::Io, format!("DB反映に失敗しました: {err}")),
                    Some(ExecutedAction {
                        action: action.clone(),
                        warning: None,
                        captured_entry: None,
                    }),
                )));
            }
            Ok(ExecutedAction {
                action: action.clone(),
                warning: match outcome {
                    MoveOutcome::Completed => None,
                    MoveOutcome::CompletedWithWarning(w) => Some(w),
                },
                captured_entry: None,
            })
        }
        UndoAction::RestoreFromTrash {
            original,
            trash_entry,
            expect,
        } => {
            if fs.exists(original) {
                return Err(Box::new((
                    FsOpError::precondition(
                        UndoRefusal::DestinationExists {
                            path: original.clone(),
                        }
                        .describe(),
                    ),
                    None,
                )));
            }
            match fs.restore_trash(trash_entry, original, expect.byte_size) {
                Ok(_) => {
                    if let Err(err) = undo_reflect_restore(store, fs, original, roots) {
                        return Err(Box::new((
                            FsOpError::new(
                                FsOpErrorKind::Io,
                                format!("DB反映に失敗しました: {err}"),
                            ),
                            Some(ExecutedAction {
                                action: action.clone(),
                                warning: None,
                                captured_entry: None,
                            }),
                        )));
                    }
                    Ok(ExecutedAction {
                        action: action.clone(),
                        warning: None,
                        captured_entry: None,
                    })
                }
                // サイズ不一致・不在は誤復帰防止の拒否 (識別情報を明示)。
                Err(err) => Err(Box::new((err, None))),
            }
        }
        UndoAction::DeleteCopy { path, .. } => {
            // 生成物のみをゴミ箱へ (即削除しない — 誤操作でも復帰可能)。
            // 補償 (元位置へ戻す) のため生成エントリを捕捉する。
            let entry = match fs.trash(path) {
                Ok(entry) => entry,
                Err(err) => return Err(Box::new((err, None))),
            };
            if let Err(err) = undo_reflect_delete(store, path, roots) {
                return Err(Box::new((
                    FsOpError::new(FsOpErrorKind::Io, format!("DB反映に失敗しました: {err}")),
                    Some(ExecutedAction {
                        action: action.clone(),
                        warning: None,
                        captured_entry: Some(entry),
                    }),
                )));
            }
            Ok(ExecutedAction {
                action: action.clone(),
                warning: None,
                captured_entry: Some(entry),
            })
        }
    }
}

/// 実行済みundoアクションの補償 (逆順ロールバックの1ステップ)。
/// 各補償も前提検証付き — 前提が崩れている場合 (外部変更等) は補償を
/// 行わずエラーを返す (結果は partial_undone + rollback_errors へ反映)。
fn compensate_executed(fs: &dyn FileOpsFs, exec: &ExecutedAction) -> Result<(), String> {
    match &exec.action {
        UndoAction::MoveBack { from, to, expect } => {
            // 実行時の移動 (from→to) を巻き戻す: to→from。
            let current = fs.stat(to).map_err(|e| format!("{}: {e}", to.display()))?;
            if current.token() != expect.to_token() {
                return Err(format!(
                    "{} はUndo実行後に外部で変更されているため巻き戻しを中止しました",
                    to.display()
                ));
            }
            if fs.exists(from) {
                return Err(format!(
                    "巻き戻し先 {} に別のファイルが存在するため中止しました",
                    from.display()
                ));
            }
            fs.rename_noreplace(to, from)
                .map_err(|e| format!("{} → {}: {e}", to.display(), from.display()))
        }
        UndoAction::RestoreFromTrash {
            original, expect, ..
        } => {
            // 復帰したファイルを再度ゴミ箱へ戻す (トークン検証付き)。
            let current = fs
                .stat(original)
                .map_err(|e| format!("{}: {e}", original.display()))?;
            if current.token() != expect.to_token() {
                return Err(format!(
                    "{} は復帰後に外部で変更されているため再退避を中止しました",
                    original.display()
                ));
            }
            fs.trash(original)
                .map(|_| ())
                .map_err(|e| format!("{}: {e}", original.display()))
        }
        UndoAction::DeleteCopy { path, expect } => {
            // ゴミ箱へ移した生成物を、実行時に捕捉したエントリで特定して
            // 元位置へ戻す (サイズ検証付き — 誤復帰防止)。
            let Some(entry) = &exec.captured_entry else {
                return Err(format!(
                    "{} の巻き戻し情報が無いため手動復帰が必要です (ゴミ箱に退避済み)",
                    path.display()
                ));
            };
            fs.restore_trash(entry, path, expect.byte_size)
                .map(|_| ())
                .map_err(|e| format!("{}: {e}", path.display()))
        }
    }
}

/// MoveBack後のDB反映: 現在地 (from) の行を元位置 (to) へrelocate。
fn undo_reflect_move(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    from: &Path,
    to: &Path,
    roots: &[FileRootRef],
) -> Result<(), RepoError> {
    let stat = fs.stat(to).ok();
    let from = from.to_path_buf();
    let to = to.to_path_buf();
    let from_root = root_id_of(roots, &from);
    let to_root = root_id_of(roots, &to);
    let roots_vec: Vec<FileRootRef> = roots.to_vec();
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        let Some(to_root_id) = to_root else {
            // 元位置がルート外へ出た: 現在地の行をmissing化
            if let Some(from_root_id) = from_root {
                let from_root_path = root_path_of(&roots_vec, from_root_id);
                if let Ok(rel) = from.strip_prefix(from_root_path) {
                    if let Some(record) = repos.find_by_path(from_root_id, rel)? {
                        repos.set_scan_state(record.id, ScanState::Missing)?;
                    }
                }
            }
            return Ok(());
        };
        let to_root_path = root_path_of(&roots_vec, to_root_id);
        let Ok(rel_to) = to.strip_prefix(to_root_path) else {
            return Ok(());
        };
        // 現在地の行を特定 (from側root + rel)
        let from_record = from_root.and_then(|from_root_id| {
            let from_root_path = root_path_of(&roots_vec, from_root_id);
            from.strip_prefix(from_root_path)
                .ok()
                .and_then(|rel_from| repos.find_by_path(from_root_id, rel_from).ok().flatten())
        });
        let upsert = FileUpsert {
            root_id: to_root_id,
            rel_path: rel_to.to_path_buf(),
            display_name: display_name_of(&to),
            byte_size: stat.map(|s| s.byte_size).unwrap_or(0),
            mtime_ms: stat.map(|s| s.mtime_ms).unwrap_or(0),
            mtime_ns: stat.map(|s| s.mtime_ns).unwrap_or(0),
            scan_state: ScanState::Present,
            os_file_id: stat.and_then(|s| s.os_file_id),
            detach_asset: false,
        };
        match from_record {
            Some(record) => {
                // 元位置を占めるstale行 (missing等) を掃除してrelocate
                if let Some(existing) = repos.find_by_path(to_root_id, rel_to)? {
                    repos.delete_file(existing.id)?;
                }
                repos.relocate_file(record.id, upsert)?;
                Ok(())
            }
            None => {
                // 行が無い (外部操作で消失等): 元位置へupsert (ハッシュ待ち)
                repos.upsert_file(upsert)?;
                Ok(())
            }
        }
    })
}

fn root_path_of(roots: &[FileRootRef], root_id: i64) -> &Path {
    roots
        .iter()
        .find(|r| r.root_id == root_id)
        .map(|r| r.root_path.as_path())
        .unwrap_or_else(|| Path::new(""))
}

/// RestoreFromTrash後のDB反映: 元位置の行をpresentへupsert。
fn undo_reflect_restore(
    store: &dyn RepoStore,
    fs: &dyn FileOpsFs,
    original: &Path,
    roots: &[FileRootRef],
) -> Result<(), RepoError> {
    let stat = fs.stat(original).ok();
    let original = original.to_path_buf();
    let root = root_id_of(roots, &original);
    let roots_vec: Vec<FileRootRef> = roots.to_vec();
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        let Some(root_id) = root else {
            return Ok(());
        };
        let root_path = root_path_of(&roots_vec, root_id);
        let Some(rel) = original.strip_prefix(root_path).ok() else {
            return Ok(());
        };
        repos.upsert_file(FileUpsert {
            root_id,
            rel_path: rel.to_path_buf(),
            display_name: display_name_of(&original),
            byte_size: stat.map(|s| s.byte_size).unwrap_or(0),
            mtime_ms: stat.map(|s| s.mtime_ms).unwrap_or(0),
            mtime_ns: stat.map(|s| s.mtime_ns).unwrap_or(0),
            scan_state: ScanState::Present,
            os_file_id: stat.and_then(|s| s.os_file_id),
            // missing期間に行が存続しているため、同一トークンならリンク保持。
            // 変わっていれば再ハッシュで正しいAssetへ再リンクされる。
            detach_asset: false,
        })?;
        Ok(())
    })
}

/// DeleteCopy後のDB反映: 生成物の行をmissing化。
fn undo_reflect_delete(
    store: &dyn RepoStore,
    path: &Path,
    roots: &[FileRootRef],
) -> Result<(), RepoError> {
    let path = path.to_path_buf();
    let root = root_id_of(roots, &path);
    let roots_vec: Vec<FileRootRef> = roots.to_vec();
    write_ret(store, move |repos: &dyn crate::library::AllRepos| {
        let Some(root_id) = root else {
            return Ok(());
        };
        let root_path = root_path_of(&roots_vec, root_id);
        let Some(rel) = path.strip_prefix(root_path).ok() else {
            return Ok(());
        };
        if let Some(record) = repos.find_by_path(root_id, rel)? {
            repos.set_scan_state(record.id, ScanState::Missing)?;
        }
        Ok(())
    })
}

// ===== ヘルパ =====

/// [`RepoStore::write`] で単一値を取り出すヘルパ (library.rsの非公開実装と
/// 同一規約。file_opsからも使うため再定義)。
fn write_ret<T, E: From<RepoError>>(
    store: &dyn RepoStore,
    f: impl FnOnce(&dyn crate::library::AllRepos) -> Result<T, E>,
) -> Result<T, E> {
    let mut slot: Option<Result<T, E>> = None;
    let mut f = Some(f);
    let mut closure = |repos: &dyn crate::library::AllRepos| -> Result<(), RepoError> {
        if let Some(f) = f.take() {
            slot = Some(f(repos));
        }
        Ok(())
    };
    store.write(&mut closure)?;
    slot.unwrap_or_else(|| Err(RepoError::Db("write callback did not run".to_string()).into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::RepoStore;
    use crate::repo::{
        AssetRecord, AssetRepo, FileRecord, FileRepo, LibraryRootRecord, LibraryRootRepo, NewAsset,
        RootSettings, ScanState,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};

    // ===== テスト用一時ディレクトリ (tempfile不使用規約。std実装) =====

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-fileops-{tag}-{}-{}",
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

    // ===== テスト用FS実装 (std + 障害注入フック) =====

    type TmpHook = Option<Box<dyn Fn(&Path) + Send>>;
    #[derive(Default)]
    struct TestFs {
        /// このパスのrenameを跨界 (CrossesDevices) で失敗させる。
        rename_exdev: StdMutex<std::collections::HashSet<PathBuf>>,
        /// このパスのcopy_fileを失敗させる (メッセージ)。
        copy_fail: StdMutex<std::collections::HashMap<PathBuf, String>>,
        /// このパスのtrashを失敗させる。
        trash_fail: StdMutex<std::collections::HashSet<PathBuf>>,
        /// このパスのremove_fileを失敗させる (EXDEV警告経路の検証用)。
        remove_fail: StdMutex<std::collections::HashSet<PathBuf>>,
        /// trash実体の退避場所 (TestFs::newで指定)。
        trash_dir: StdMutex<PathBuf>,
        /// trash退避の写像 (original → trashed)。
        trashed: StdMutex<std::collections::HashMap<PathBuf, PathBuf>>,
        /// copy_fileがテンポラリを作った直後に呼ぶ観測フック。
        on_tmp_created: StdMutex<TmpHook>,
        /// free_bytesの上書き (Noneなら実ディスク値)。
        free_override: StdMutex<Option<u64>>,
    }

    impl TestFs {
        fn new(trash_dir: PathBuf) -> Self {
            Self {
                trash_dir: StdMutex::new(trash_dir),
                ..Default::default()
            }
        }

        fn mtime_ns_of(md: &std::fs::Metadata) -> u64 {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                md.mtime()
                    .try_into()
                    .unwrap_or(u64::MAX)
                    .saturating_add(md.mtime_nsec().try_into().unwrap_or(0))
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

        fn stat_of(path: &Path) -> Result<FileStat, FsOpError> {
            let md = std::fs::symlink_metadata(path)
                .map_err(|e| FsOpError::not_found(format!("{}: {e}", path.display())))?;
            let mtime_ns = Self::mtime_ns_of(&md);
            Ok(FileStat {
                byte_size: md.len(),
                mtime_ms: mtime_ns / 1_000_000,
                mtime_ns,
                os_file_id: gento_platform_stub_os_file_id(&md),
            })
        }
    }

    /// coreはgento-platformへ依存しないため、テスト専用のos_file_id抽出。
    #[cfg(unix)]
    fn gento_platform_stub_os_file_id(md: &std::fs::Metadata) -> Option<OsFileId> {
        use std::os::unix::fs::MetadataExt;
        Some(OsFileId::new(md.dev(), md.ino()))
    }

    #[cfg(not(unix))]
    fn gento_platform_stub_os_file_id(_md: &std::fs::Metadata) -> Option<OsFileId> {
        None
    }

    impl FileOpsFs for TestFs {
        fn stat(&self, path: &Path) -> Result<FileStat, FsOpError> {
            Self::stat_of(path)
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
            std::fs::create_dir(path)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("{}: {e}", path.display())))
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<(), FsOpError> {
            if self.rename_exdev.lock().unwrap().contains(from) {
                return Err(FsOpError::new(
                    FsOpErrorKind::Io,
                    format!(
                        "{} -> {}: CrossesDevices (injected)",
                        from.display(),
                        to.display()
                    ),
                ));
            }
            // 実原子性 (renameat2) はRealFileOps側で担保するため、ここでは
            // チェック+renameで契約 (既存toはAlreadyExists) を模倣する。
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(FsOpError::new(
                    FsOpErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            std::fs::rename(from, to).map_err(|e| {
                FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("{} -> {}: {e}", from.display(), to.display()),
                )
            })
        }

        fn copy_file(
            &self,
            from: &Path,
            to: &Path,
            tmp: &Path,
            _cancel: &AtomicBool,
        ) -> Result<(), FsOpError> {
            if let Some(msg) = self.copy_fail.lock().unwrap().get(from) {
                return Err(FsOpError::new(FsOpErrorKind::PermissionDenied, msg.clone()));
            }
            // 本物と同じ規則: テンポラリ名で書き込み→NOREPLACE確定 (安全原則4)
            std::fs::copy(from, tmp).map_err(|e| {
                FsOpError::new(
                    FsOpErrorKind::Io,
                    format!("copy {} -> {}: {e}", from.display(), to.display()),
                )
            })?;
            if let Some(hook) = self.on_tmp_created.lock().unwrap().as_ref() {
                hook(tmp);
            }
            if std::fs::symlink_metadata(to).is_ok() {
                let _ = std::fs::remove_file(tmp);
                return Err(FsOpError::new(
                    FsOpErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            std::fs::rename(tmp, to)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("tmp rename: {e}")))
        }

        fn remove_file(&self, path: &Path) -> Result<(), FsOpError> {
            if self.remove_fail.lock().unwrap().contains(path) {
                return Err(FsOpError::new(
                    FsOpErrorKind::PermissionDenied,
                    format!("remove failed (injected): {}", path.display()),
                ));
            }
            std::fs::remove_file(path)
                .map_err(|e| FsOpError::not_found(format!("{}: {e}", path.display())))
        }

        fn trash(&self, path: &Path) -> Result<TrashEntry, FsOpError> {
            if self.trash_fail.lock().unwrap().contains(path) {
                return Err(FsOpError::new(
                    FsOpErrorKind::PermissionDenied,
                    format!("trash failed (injected): {}", path.display()),
                ));
            }
            let dir = self.trash_dir.lock().unwrap().clone();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            // 同名再trashは一意な退避名へ (実ゴミ箱のsuffix規則の模倣)。
            let mut file_name = name.clone();
            {
                let mut seq = 2;
                while dir.join(&file_name).exists() {
                    file_name = format!("{name}.{seq}");
                    seq += 1;
                }
            }
            let dest = dir.join(&file_name);
            std::fs::rename(path, &dest)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("trash: {e}")))?;
            self.trashed
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), dest);
            Ok(TrashEntry {
                trash_dir: dir,
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
                .map_err(|e| FsOpError::not_found(format!("{}: {e}", trashed.display())))?
                .len();
            if size != expect_byte_size {
                return Err(FsOpError::precondition(format!(
                    "ゴミ箱エントリ {} はサイズ不一致 (期待 {expect_byte_size}, 実際 {size})",
                    trashed.display()
                )));
            }
            if std::fs::symlink_metadata(original).is_ok() {
                return Err(FsOpError::precondition(format!(
                    "復帰先が存在: {}",
                    original.display()
                )));
            }
            std::fs::rename(&trashed, original)
                .map_err(|e| FsOpError::new(FsOpErrorKind::Io, format!("restore: {e}")))?;
            self.trashed.lock().unwrap().remove(original);
            Ok(original.to_path_buf())
        }

        fn trash_entry_size(&self, entry: &TrashEntry) -> Option<u64> {
            std::fs::metadata(entry.trash_dir.join(&entry.file_name))
                .ok()
                .map(|m| m.len())
        }

        fn free_bytes(&self, _dir: &Path) -> Result<u64, FsOpError> {
            if let Some(v) = *self.free_override.lock().unwrap() {
                return Ok(v);
            }
            Ok(u64::MAX)
        }

        fn hash_file(&self, path: &Path, _cancel: &AtomicBool) -> Result<String, FsOpError> {
            let bytes = std::fs::read(path)
                .map_err(|e| FsOpError::not_found(format!("{}: {e}", path.display())))?;
            // テスト用の安価な「ハッシュ」(等価判定のみに使用)
            let mut hash: u64 = 0xcbf29ce484222325;
            for b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            Ok(format!("testhash:{hash:016x}"))
        }
    }

    // ===== テスト用リポジトリ (4 trait + RepoStore) =====

    #[derive(Default)]
    struct TestStore {
        next_id: StdMutex<i64>,
        roots: StdMutex<Vec<LibraryRootRecord>>,
        files: StdMutex<Vec<FileRecord>>,
        assets: StdMutex<Vec<AssetRecord>>,
        operations: StdMutex<Vec<FileOpRecord>>,
    }

    impl TestStore {
        fn add_root(&self, path: &Path) -> LibraryRootRecord {
            let id = {
                let mut next = self.next_id.lock().unwrap();
                *next += 1;
                *next
            };
            let record = LibraryRootRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                path: path.to_path_buf(),
                display_path: path.to_string_lossy().into_owned(),
                enabled: true,
                watch_enabled: true,
                ai_enabled: false,
                send_exif: false,
                send_gps: false,
                created_at: "2026-08-31T00:00:00.000Z".to_string(),
                updated_at: "2026-08-31T00:00:00.000Z".to_string(),
            };
            self.roots.lock().unwrap().push(record.clone());
            record
        }

        fn add_file(&self, root_id: i64, abs: &Path, root_path: &Path, bytes: &[u8]) -> FileRecord {
            let stat = TestFs::stat_of(abs).unwrap();
            let rel = abs.strip_prefix(root_path).unwrap().to_path_buf();
            self.upsert_file(FileUpsert {
                root_id,
                rel_path: rel,
                display_name: abs.file_name().unwrap().to_string_lossy().into_owned(),
                byte_size: stat.byte_size.max(bytes.len() as u64),
                mtime_ms: stat.mtime_ms,
                mtime_ns: stat.mtime_ns,
                scan_state: ScanState::Present,
                os_file_id: stat.os_file_id,
                detach_asset: false,
            })
            .unwrap()
        }

        fn link(&self, file_id: i64, asset_id: i64) {
            AssetRepo::link_file(self, file_id, asset_id).unwrap();
        }

        fn make_asset(&self, hash: &str) -> AssetRecord {
            self.create_or_get_asset(NewAsset {
                content_hash: format!("testhash:{hash}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 1,
                mime_type: Some("image/png".to_string()),
                width: Some(1),
                height: Some(1),
                has_alpha: Some(false),
                is_animated: Some(false),
                frame_count: None,
            })
            .unwrap()
            .0
        }

        fn files_snapshot(&self) -> Vec<FileRecord> {
            self.files.lock().unwrap().clone()
        }

        fn find_file(&self, root_id: i64, rel: &Path) -> Option<FileRecord> {
            self.files
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.root_id == root_id && f.rel_path == rel)
                .cloned()
        }
    }

    impl RepoStore for TestStore {
        fn write(
            &self,
            f: &mut dyn FnMut(&dyn crate::library::AllRepos) -> Result<(), RepoError>,
        ) -> Result<(), RepoError> {
            f(self)
        }

        fn read(
            &self,
            f: &mut dyn FnMut(&dyn crate::library::AllRepos) -> Result<(), RepoError>,
        ) -> Result<(), RepoError> {
            f(self)
        }
    }

    impl LibraryRootRepo for TestStore {
        fn add_root(&self, path: &Path) -> Result<LibraryRootRecord, RepoError> {
            Ok(self.add_root(path))
        }
        fn list_roots(&self) -> Result<Vec<LibraryRootRecord>, RepoError> {
            Ok(self.roots.lock().unwrap().clone())
        }
        fn set_root_enabled(&self, _id: i64, _enabled: bool) -> Result<(), RepoError> {
            Ok(())
        }
        fn remove_root(&self, id: i64) -> Result<(), RepoError> {
            self.roots.lock().unwrap().retain(|r| r.id != id);
            self.files.lock().unwrap().retain(|f| f.root_id != id);
            Ok(())
        }
        fn overlapping_roots(&self, _path: &Path) -> Result<Vec<LibraryRootRecord>, RepoError> {
            Ok(Vec::new())
        }
        fn set_root_settings(&self, _id: i64, _s: RootSettings) -> Result<(), RepoError> {
            Ok(())
        }
    }

    impl FileRepo for TestStore {
        fn upsert_file(&self, file: FileUpsert) -> Result<FileRecord, RepoError> {
            let mut files = self.files.lock().unwrap();
            if let Some(existing) = files
                .iter_mut()
                .find(|f| f.root_id == file.root_id && f.rel_path == file.rel_path)
            {
                existing.byte_size = file.byte_size;
                existing.mtime_ms = file.mtime_ms;
                existing.mtime_ns = Some(file.mtime_ns);
                existing.scan_state = file.scan_state;
                existing.os_file_id = file.os_file_id;
                existing.display_name = file.display_name.clone();
                if file.detach_asset {
                    existing.asset_id = None;
                }
                return Ok(existing.clone());
            }
            let id = {
                let mut next = self.next_id.lock().unwrap();
                *next += 1;
                *next
            };
            let record = FileRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                root_id: file.root_id,
                asset_id: None,
                rel_path: file.rel_path.clone(),
                display_name: file.display_name.clone(),
                extension: None,
                byte_size: file.byte_size,
                mtime_ms: file.mtime_ms,
                mtime_ns: Some(file.mtime_ns),
                scan_state: file.scan_state,
                os_file_id: file.os_file_id,
                last_seen_at: "2026-08-31T01:00:00.000Z".to_string(),
                created_at: "2026-08-31T00:00:00.000Z".to_string(),
                updated_at: "2026-08-31T01:00:00.000Z".to_string(),
            };
            files.push(record.clone());
            Ok(record)
        }

        fn set_scan_state(&self, file_id: i64, state: ScanState) -> Result<(), RepoError> {
            let mut files = self.files.lock().unwrap();
            let record = files
                .iter_mut()
                .find(|f| f.id == file_id)
                .ok_or(RepoError::NotFound)?;
            record.scan_state = state;
            Ok(())
        }

        fn list_missing(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.root_id == root_id && f.scan_state == ScanState::Missing)
                .cloned()
                .collect())
        }

        fn find_by_path(
            &self,
            root_id: i64,
            rel_path: &Path,
        ) -> Result<Option<FileRecord>, RepoError> {
            Ok(self.find_file(root_id, rel_path))
        }

        fn get_file(&self, file_id: i64) -> Result<Option<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.id == file_id)
                .cloned())
        }

        fn upsert_files_batch(&self, files: &[FileUpsert]) -> Result<Vec<FileRecord>, RepoError> {
            files.iter().map(|f| self.upsert_file(f.clone())).collect()
        }

        fn find_by_os_file_id(
            &self,
            root_id: i64,
            os_file_id: &OsFileId,
        ) -> Result<Option<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.root_id == root_id && f.os_file_id == Some(*os_file_id))
                .cloned())
        }

        fn relocate_file(&self, file_id: i64, file: FileUpsert) -> Result<FileRecord, RepoError> {
            let mut files = self.files.lock().unwrap();
            if files.iter().any(|f| {
                f.id != file_id && f.root_id == file.root_id && f.rel_path == file.rel_path
            }) {
                return Err(RepoError::Duplicate);
            }
            let record = files
                .iter_mut()
                .find(|f| f.id == file_id)
                .ok_or(RepoError::NotFound)?;
            record.root_id = file.root_id;
            record.rel_path = file.rel_path.clone();
            record.display_name = file.display_name.clone();
            record.byte_size = file.byte_size;
            record.mtime_ms = file.mtime_ms;
            record.mtime_ns = Some(file.mtime_ns);
            record.os_file_id = file.os_file_id;
            record.scan_state = file.scan_state;
            Ok(record.clone())
        }

        fn set_scan_state_batch(
            &self,
            file_ids: &[i64],
            state: ScanState,
        ) -> Result<usize, RepoError> {
            let mut changed = 0;
            for id in file_ids {
                if self.set_scan_state(*id, state).is_ok() {
                    changed += 1;
                }
            }
            Ok(changed)
        }

        fn list_files_paged(
            &self,
            root_id: i64,
            offset: u32,
            limit: u32,
        ) -> Result<(Vec<FileRecord>, u64), RepoError> {
            let all: Vec<FileRecord> = self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.root_id == root_id)
                .cloned()
                .collect();
            let total = all.len() as u64;
            Ok((
                all.into_iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .collect(),
                total,
            ))
        }

        fn list_present_joined(&self) -> Result<Vec<(FileRecord, Option<AssetRecord>)>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.scan_state == ScanState::Present)
                .map(|f| {
                    let asset = f.asset_id.and_then(|id| {
                        self.assets
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|a| a.id == id)
                            .cloned()
                    });
                    (f.clone(), asset)
                })
                .collect())
        }

        fn list_files_without_asset(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.root_id == root_id && f.asset_id.is_none())
                .cloned()
                .collect())
        }

        fn delete_file(&self, file_id: i64) -> Result<(), RepoError> {
            let mut files = self.files.lock().unwrap();
            let before = files.len();
            files.retain(|f| f.id != file_id);
            if files.len() == before {
                return Err(RepoError::NotFound);
            }
            Ok(())
        }
    }

    impl AssetRepo for TestStore {
        fn create_asset(&self, asset: NewAsset) -> Result<AssetRecord, RepoError> {
            if self
                .assets
                .lock()
                .unwrap()
                .iter()
                .any(|a| a.content_hash == asset.content_hash)
            {
                return Err(RepoError::Duplicate);
            }
            let id = {
                let mut next = self.next_id.lock().unwrap();
                *next += 1;
                *next
            };
            let record = AssetRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                content_hash: asset.content_hash,
                hash_algorithm: asset.hash_algorithm,
                hash_state: crate::repo::HashState::Done,
                byte_size: asset.byte_size,
                mime_type: asset.mime_type,
                width: asset.width,
                height: asset.height,
                has_alpha: asset.has_alpha,
                is_animated: asset.is_animated,
                frame_count: asset.frame_count,
                rating: 0,
                triage: crate::repo::TriageState::Unselected,
                favorite: false,
                created_at: "2026-08-31T00:00:00.000Z".to_string(),
                updated_at: "2026-08-31T00:00:00.000Z".to_string(),
            };
            self.assets.lock().unwrap().push(record.clone());
            Ok(record)
        }

        fn find_by_hash(&self, content_hash: &str) -> Result<Option<AssetRecord>, RepoError> {
            Ok(self
                .assets
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.content_hash == content_hash)
                .cloned())
        }

        fn get_asset(&self, asset_id: i64) -> Result<Option<AssetRecord>, RepoError> {
            Ok(self
                .assets
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.id == asset_id)
                .cloned())
        }

        fn link_file(&self, file_id: i64, asset_id: i64) -> Result<(), RepoError> {
            let mut files = self.files.lock().unwrap();
            let record = files
                .iter_mut()
                .find(|f| f.id == file_id)
                .ok_or(RepoError::NotFound)?;
            record.asset_id = Some(asset_id);
            Ok(())
        }

        fn create_or_get_asset(&self, asset: NewAsset) -> Result<(AssetRecord, bool), RepoError> {
            if let Some(existing) = self.find_by_hash(&asset.content_hash)? {
                return Ok((existing, false));
            }
            Ok((self.create_asset(asset)?, true))
        }

        fn set_rating(&self, _asset_id: i64, _rating: u8) -> Result<(), RepoError> {
            Ok(())
        }
        fn set_cull_state(
            &self,
            _asset_id: i64,
            _triage: crate::repo::TriageState,
        ) -> Result<(), RepoError> {
            Ok(())
        }
        fn set_favorite(&self, _asset_id: i64, _favorite: bool) -> Result<(), RepoError> {
            Ok(())
        }
        fn set_rating_batch(&self, _ids: &[i64], _rating: u8) -> Result<(), RepoError> {
            Ok(())
        }
        fn set_cull_state_batch(
            &self,
            _ids: &[i64],
            _triage: crate::repo::TriageState,
        ) -> Result<(), RepoError> {
            Ok(())
        }
        fn set_favorite_batch(&self, _ids: &[i64], _favorite: bool) -> Result<(), RepoError> {
            Ok(())
        }
    }

    impl FileOpsRepo for TestStore {
        fn insert_operation(&self, op: NewFileOp) -> Result<FileOpRecord, RepoError> {
            let id = {
                let mut next = self.next_id.lock().unwrap();
                *next += 1;
                *next
            };
            let record = FileOpRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                operation_type: op.operation_type,
                status: op.status,
                undoable: op.undoable,
                request_json: op.request_json,
                result_json: None,
                created_at: "2026-08-31T00:00:00.000Z".to_string(),
                undone_at: None,
            };
            self.operations.lock().unwrap().push(record.clone());
            Ok(record)
        }

        fn update_operation(
            &self,
            id: i64,
            status: FileOpStatus,
            result_json: Option<&str>,
            undoable: bool,
        ) -> Result<(), RepoError> {
            let mut ops = self.operations.lock().unwrap();
            let record = ops
                .iter_mut()
                .find(|o| o.id == id)
                .ok_or(RepoError::NotFound)?;
            record.status = status;
            record.result_json = result_json.map(str::to_string);
            record.undoable = undoable;
            Ok(())
        }

        fn mark_undone(&self, id: i64) -> Result<(), RepoError> {
            let mut ops = self.operations.lock().unwrap();
            let record = ops
                .iter_mut()
                .find(|o| o.id == id)
                .ok_or(RepoError::NotFound)?;
            record.status = FileOpStatus::Undone;
            record.undoable = false;
            record.undone_at = Some("2026-08-31T02:00:00.000Z".to_string());
            Ok(())
        }

        fn get_operation(&self, id: i64) -> Result<Option<FileOpRecord>, RepoError> {
            Ok(self
                .operations
                .lock()
                .unwrap()
                .iter()
                .find(|o| o.id == id)
                .cloned())
        }

        fn list_recent_operations(&self, limit: u32) -> Result<Vec<FileOpRecord>, RepoError> {
            let mut ops = self.operations.lock().unwrap().clone();
            ops.sort_by_key(|o| std::cmp::Reverse(o.id));
            ops.truncate(limit as usize);
            Ok(ops)
        }

        fn list_executing_operations(&self) -> Result<Vec<FileOpRecord>, RepoError> {
            Ok(self
                .operations
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.status == FileOpStatus::Executing)
                .cloned()
                .collect())
        }
    }

    // ===== テストヘルパ: シナリオ土台 =====

    struct Fixture {
        _dir: TestDir,
        root: LibraryRootRecord,
        roots: Vec<FileRootRef>,
        store: Arc<TestStore>,
        fs: Arc<TestFs>,
        cancel: Arc<AtomicBool>,
    }

    fn fixture(tag: &str) -> Fixture {
        let dir = TestDir::new(tag);
        let trash = dir.mkdir(".test-trash");
        let store = Arc::new(TestStore::default());
        let fs = Arc::new(TestFs::new(trash));
        let root = store.add_root(&dir.0);
        let roots = vec![FileRootRef {
            root_id: root.id,
            root_path: dir.0.clone(),
        }];
        Fixture {
            _dir: dir,
            root,
            roots,
            store,
            fs,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    impl Fixture {
        fn op_file(&self, record: &FileRecord, content_hash: Option<String>) -> FileOpFile {
            FileOpFile {
                file_id: record.id,
                root_id: self.root.id,
                rel_path: record.rel_path.clone(),
                abs_path: self.root.path.join(&record.rel_path),
                display_name: record.display_name.clone(),
                content_hash,
                asset_id: record.asset_id,
            }
        }

        fn run(&self, plan: &FileOpPlan, resolutions: &ResolutionSpec) -> FileOpOutcome {
            let fs = self.fs.clone();
            let cancel = self.cancel.clone();
            let store: &TestStore = &self.store;
            execute_file_op(
                store,
                &*fs,
                plan,
                resolutions,
                &self.roots,
                &cancel,
                &mut |_| {},
            )
            .unwrap()
        }
    }

    // ===== 計画 (dry-run) =====

    #[test]
    fn plan_detects_conflict_with_hash_match_and_keep_both_name() {
        let fx = fixture("plan-conflict");
        let src = fx._dir.write("a.png", b"src-bytes");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"src-bytes").unwrap(); // 同一内容
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"src-bytes");
        let src_hash = fx.fs.hash_file(&src, &fx.cancel).unwrap();
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, Some(src_hash))],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        assert!(plan.plan_errors.is_empty());
        assert_eq!(plan.steps.len(), 1);
        let conflict = plan.steps[0].conflict.as_ref().expect("衝突検出");
        assert_eq!(conflict.dst, dst_dir.join("a.png"));
        assert_eq!(conflict.hash_matches, Some(true), "同一内容はhash一致");
        assert_eq!(conflict.keep_both_name, "a (2).png");
        assert_eq!(plan.dest_root_id, Some(fx.root.id));
    }

    #[test]
    fn plan_hash_mismatch_reported_when_content_differs() {
        let fx = fixture("plan-hashdiff");
        let src = fx._dir.write("a.png", b"src-bytes");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"DIFFERENT").unwrap();
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"src-bytes");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, Some("testhash:0000".to_string()))],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        assert_eq!(
            plan.steps[0].conflict.as_ref().unwrap().hash_matches,
            Some(false)
        );
    }

    #[test]
    fn plan_flags_missing_src_and_symlink() {
        let fx = fixture("plan-missing");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/nonexistent-target", fx._dir.0.join("link.png")).unwrap();
            let record = fx
                .store
                .upsert_file(FileUpsert {
                    root_id: fx.root.id,
                    rel_path: PathBuf::from("link.png"),
                    display_name: "link.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    mtime_ns: 1000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
                .unwrap();
            let plan = plan_file_op(
                &*fx.fs,
                &fx.cancel,
                FileOpKind::Trash,
                vec![fx.op_file(&record, None)],
                None,
                None,
                &fx.roots,
            );
            assert!(plan.steps[0].plan_error.is_some(), "symlinkは対象外");
        }
        let record2 = fx
            .store
            .upsert_file(FileUpsert {
                root_id: fx.root.id,
                rel_path: PathBuf::from("ghost.png"),
                display_name: "ghost.png".to_string(),
                byte_size: 1,
                mtime_ms: 1,
                mtime_ns: 1000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        let plan2 = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record2, None)],
            None,
            None,
            &fx.roots,
        );
        let err = plan2.steps[0].plan_error.as_ref().unwrap();
        assert!(matches!(err.kind, FsOpErrorKind::NotFound), "{err}");
    }

    #[test]
    fn plan_rename_validations() {
        let fx = fixture("plan-rename");
        let src = fx._dir.write("a.png", b"x");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"x");
        // 複数ファイルのrenameは計画エラー
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Rename,
            vec![fx.op_file(&record, None), fx.op_file(&record, None)],
            None,
            Some("b.png"),
            &fx.roots,
        );
        assert!(!plan.plan_errors.is_empty());
        // 名前の検証
        for bad in ["", "..", "a/b.png", ".gento-tmp-x.png"] {
            assert!(validate_new_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_new_name("ok name (2).png").is_ok());
        // 宛先がディレクトリでないcopy
        let plan3 = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&fx._dir.0.join("not-a-dir")),
            None,
            &fx.roots,
        );
        assert!(matches!(
            plan3.plan_errors[0].kind,
            FsOpErrorKind::NotADirectory
        ));
    }

    #[test]
    fn plan_reports_disk_shortfall() {
        let fx = fixture("plan-disk");
        let src = fx._dir.write("big.png", &[0u8; 1000]);
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, &[0u8; 1000]);
        *fx.fs.free_override.lock().unwrap() = Some(500);
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&fx._dir.0),
            None,
            &fx.roots,
        );
        assert_eq!(plan.disk_shortfall_bytes, Some(500));
    }

    // ===== 実行: 衝突3解決 + apply_all =====

    #[test]
    fn execute_copy_creates_row_with_shared_asset() {
        let fx = fixture("exec-copy");
        let src = fx._dir.write("a.png", b"png-bytes");
        let dst_dir = fx._dir.mkdir("dest");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"png-bytes");
        let asset = fx.store.make_asset("hash-a");
        fx.store.link(record.id, asset.id);
        // link後の最新行から操作対象を組み立てる (asset_idを含めるため)
        let record = fx.store.get_file(record.id).unwrap().unwrap();
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, Some(asset.content_hash.clone()))],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(outcome.undoable, "コピーのUndo=生成物削除");
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"png-bytes");
        // DB: 行が2つ (元+コピー)、同一Assetへリンク (hash共有)
        let copied = fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/a.png"))
            .expect("コピー先行");
        assert_eq!(copied.asset_id, Some(asset.id), "同一Assetを共有");
        assert_eq!(fx.store.files_snapshot().len(), 2);
    }

    #[test]
    fn execute_copy_conflict_overwrite_replaces() {
        let fx = fixture("exec-ow");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        let old = fx
            .store
            .add_file(fx.root.id, &dst_dir.join("a.png"), &fx.root.path, b"OLD");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        assert!(plan.steps[0].conflict.is_some());
        // 未指定 = スキップ (安全側)
        let skipped = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(skipped.steps[0].status, StepStatus::Skipped);
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"OLD");
        // 明示上書き = 置換 + 旧行掃除
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"NEW");
        assert!(
            fx.store.get_file(old.id).unwrap().is_none(),
            "上書きされた旧ファイル行は削除 (Assetは残る)"
        );
        let replaced = fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/a.png"))
            .unwrap();
        assert_eq!(replaced.byte_size, 3);
    }

    #[test]
    fn execute_copy_conflict_keep_both_uses_alt_name() {
        let fx = fixture("exec-kb");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        fx.store
            .add_file(fx.root.id, &dst_dir.join("a.png"), &fx.root.path, b"OLD");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::KeepBoth),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"OLD");
        assert_eq!(std::fs::read(dst_dir.join("a (2).png")).unwrap(), b"NEW");
        assert!(fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/a (2).png"))
            .is_some());
    }

    #[test]
    fn execute_copy_overwrite_onto_itself_refused() {
        let fx = fixture("exec-self");
        let src = fx._dir.write("a.png", b"SELF");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"SELF");
        // 同じディレクトリへコピー (dst == src)
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&fx._dir.0),
            None,
            &fx.roots,
        );
        assert!(plan.steps[0].conflict.is_some());
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Failed);
        assert!(matches!(
            outcome.steps[0].error.as_ref().unwrap().kind,
            FsOpErrorKind::Precondition
        ));
        assert_eq!(std::fs::read(&src).unwrap(), b"SELF", "元ファイルは無傷");
    }

    // ===== 実行: 移動・リネーム =====

    #[test]
    fn execute_move_relocates_row_keeping_id_asset_and_content() {
        let fx = fixture("exec-move");
        let src = fx._dir.write("a.png", b"move-me");
        let dst_dir = fx._dir.mkdir("dest");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"move-me");
        let asset = fx.store.make_asset("hash-m");
        fx.store.link(record.id, asset.id);
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, Some(asset.content_hash.clone()))],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!src.exists(), "元位置からは消える");
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"move-me");
        let moved = fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/a.png"))
            .expect("移動先行");
        assert_eq!(moved.id, record.id, "同一行 (uuid・id不変)");
        assert_eq!(moved.asset_id, Some(asset.id), "Assetリンク保持");
        #[cfg(unix)]
        assert_eq!(
            moved.os_file_id, record.os_file_id,
            "同一FS移動はos_file_id不変"
        );
        assert_eq!(fx.store.files_snapshot().len(), 1, "行は増えない");
    }

    #[test]
    fn execute_move_to_outside_roots_marks_missing() {
        let fx = fixture("exec-move-out");
        let outside = TestDir::new("exec-move-out-dst");
        let src = fx._dir.write("a.png", b"x");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"x");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&outside.0),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(outside.0.join("a.png").exists());
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(
            row.scan_state,
            ScanState::Missing,
            "ルート外へ出たらmissing"
        );
    }

    #[test]
    fn execute_move_cross_device_falls_back_to_copy_and_remove() {
        let fx = fixture("exec-exdev");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("a.png", b"cross-fs");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"cross-fs");
        fx.fs.rename_exdev.lock().unwrap().insert(src.clone());
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!src.exists(), "元は削除される (跨界移動)");
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"cross-fs");
        assert!(fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/a.png"))
            .is_some());
    }

    #[test]
    fn execute_rename_updates_display_and_rel_path() {
        let fx = fixture("exec-rename");
        let src = fx._dir.write("old.png", b"r");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"r");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Rename,
            vec![fx.op_file(&record, None)],
            None,
            Some("new name.png"),
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(fx._dir.0.join("new name.png").exists());
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.rel_path, PathBuf::from("new name.png"));
        assert_eq!(row.display_name, "new name.png");
    }

    // ===== 部分成功・キャンセル =====

    #[test]
    fn execute_partial_success_2_done_1_failed_db_consistent() {
        let fx = fixture("exec-partial");
        let dst_dir = fx._dir.mkdir("dest");
        let mut records = Vec::new();
        for i in 0..3 {
            let p = fx
                ._dir
                .write(&format!("p{i}.png"), format!("bytes-{i}").as_bytes());
            records.push(fx.store.add_file(fx.root.id, &p, &fx.root.path, b"x"));
        }
        // 1件だけコピー失敗を注入 (権限エラー相当)
        fx.fs.copy_fail.lock().unwrap().insert(
            fx.root.path.join("p1.png"),
            "permission denied (injected)".to_string(),
        );
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            records.iter().map(|r| fx.op_file(r, None)).collect(),
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Partial);
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(outcome.steps[1].status, StepStatus::Failed);
        assert!(matches!(
            outcome.steps[1].error.as_ref().unwrap().kind,
            FsOpErrorKind::PermissionDenied
        ));
        assert_eq!(outcome.steps[2].status, StepStatus::Done);
        assert_eq!(outcome.failed_step_ids(), vec![records[1].id]);
        // DB整合: 成功2件のみ宛先に行を持つ
        assert!(fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/p0.png"))
            .is_some());
        assert!(fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/p1.png"))
            .is_none());
        assert!(fx
            .store
            .find_file(fx.root.id, &PathBuf::from("dest/p2.png"))
            .is_some());
        assert!(!dst_dir.join("p1.png").exists());
    }

    #[test]
    fn execute_cancel_midway_records_only_completed() {
        let fx = fixture("exec-cancel");
        let dst_dir = fx._dir.mkdir("dest");
        let mut records = Vec::new();
        for i in 0..3 {
            let p = fx._dir.write(&format!("c{i}.png"), b"c");
            records.push(fx.store.add_file(fx.root.id, &p, &fx.root.path, b"c"));
        }
        // 2件目のコピーでテンポラリ作成時にキャンセルフラグを立てる
        // (協調キャンセル: ファイル境界で確実に止まる)
        {
            let cancel = fx.cancel.clone();
            *fx.fs.on_tmp_created.lock().unwrap() = Some(Box::new(move |tmp| {
                if tmp.file_name().unwrap().to_string_lossy().contains("c1") {
                    cancel.store(true, Ordering::SeqCst);
                }
            }));
        }
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            records.iter().map(|r| fx.op_file(r, None)).collect(),
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(
            outcome.steps[1].status,
            StepStatus::Done,
            "キャンセル要求されたファイル自体は完了"
        );
        assert_eq!(outcome.steps[2].status, StepStatus::Cancelled, "残りは中止");
        assert_eq!(outcome.status, FileOpStatus::Partial, "完了分のみ記録");
        assert!(dst_dir.join("c0.png").exists());
        assert!(dst_dir.join("c1.png").exists());
        assert!(fx._dir.0.join("c2.png").exists(), "残りは中止");
    }

    #[test]
    fn copy_tmp_not_registered_midway_and_final_named_after() {
        let fx = fixture("exec-tmp");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("t.png", b"tmp-check");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"tmp-check");
        // テンポラリ作成直後の状態を観測: tmp存在・最終名不在・DB行なし
        let observed = Arc::new(StdMutex::new(Vec::<String>::new()));
        {
            let observed = observed.clone();
            let store = fx.store.clone();
            *fx.fs.on_tmp_created.lock().unwrap() = Some(Box::new(move |tmp| {
                let final_path = tmp.with_file_name("t.png");
                assert!(tmp.exists(), "テンポラリ実体が存在");
                let tmp_name = tmp.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    tmp_name.starts_with(".t.png") && is_owned_tmp_name(&tmp_name),
                    "厳密形式の一時名であること: {tmp_name}"
                );
                assert!(
                    tmp_owner_uuid_simple(&tmp_name).is_some(),
                    "所有操作uuidを復元できる"
                );
                assert!(!final_path.exists(), "最終名はまだ無い");
                assert!(
                    store
                        .find_file(store_roots(&store), &PathBuf::from("dest/t.png"))
                        .is_none()
                        || store
                            .find_file(store_roots(&store), &PathBuf::from("dest/t.png"))
                            .is_none(),
                    "コピー完了前のファイルはDBに現れない"
                );
                *observed.lock().unwrap() = vec![tmp.display().to_string()];
            }));
        }
        // roots解決のためroot_idを環境から得る (fixtureのroot)
        // (観測クロージャ内ではstoreのみ使う — root_idは事前に一致させる)
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert_eq!(observed.lock().unwrap().len(), 1, "観測フックが走った");
        assert!(dst_dir.join("t.png").exists(), "最終名で存在");
        // テンポラリは残っていない
        for entry in std::fs::read_dir(&dst_dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.contains(".gento-tmp-"), "tmp残留: {name}");
        }
    }

    fn store_roots(store: &TestStore) -> i64 {
        store
            .roots
            .lock()
            .unwrap()
            .first()
            .map(|r| r.id)
            .unwrap_or(1)
    }

    // ===== ゴミ箱・完全削除 =====

    #[test]
    fn execute_trash_marks_missing_and_records_restore_undo() {
        let fx = fixture("exec-trash");
        let src = fx._dir.write("a.png", b"trash-me");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"trash-me");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(outcome.undoable);
        assert!(!src.exists(), "元位置から消える");
        assert!(
            fx.fs.trash_dir.lock().unwrap().join("a.png").exists(),
            "ゴミ箱へ"
        );
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.scan_state, ScanState::Missing);
        assert_eq!(outcome.steps[0].undo.len(), 1);
        match &outcome.steps[0].undo[0] {
            UndoAction::RestoreFromTrash { original, .. } => {
                assert_eq!(*original, src);
            }
            other => panic!("unexpected undo action: {other:?}"),
        }
    }

    #[test]
    fn execute_trash_failure_keeps_everything_untouched() {
        // 元ファイル保護: ゴミ箱操作が失敗したら何も削除しない (ロールバック検証)
        let fx = fixture("exec-trash-fail");
        let src = fx._dir.write("a.png", b"protected");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"protected");
        fx.fs.trash_fail.lock().unwrap().insert(src.clone());
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Failed);
        assert_eq!(
            std::fs::read(&src).unwrap(),
            b"protected",
            "元ファイルは無傷"
        );
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.scan_state, ScanState::Present, "DBもpresent維持");
        assert!(!outcome.undoable);
    }

    #[test]
    fn execute_delete_forever_marks_missing_not_undoable() {
        let fx = fixture("exec-df");
        let src = fx._dir.write("a.png", b"forever");
        let asset = fx.store.make_asset("hash-f");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"forever");
        fx.store.link(record.id, asset.id);
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::DeleteForever,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!outcome.undoable, "完全削除はUndo対象外");
        assert!(!src.exists());
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.scan_state, ScanState::Missing);
        assert!(
            fx.store.get_asset(asset.id).unwrap().is_some(),
            "Assetは残す"
        );
    }

    #[test]
    fn execute_mkdir_creates_and_records_not_undoable() {
        let fx = fixture("exec-mkdir");
        let outcome = execute_mkdir(&*fx.store, &*fx.fs, &fx._dir.0, "newdir").unwrap();
        assert_eq!(outcome.status, FileOpStatus::Done);
        assert!(!outcome.undoable);
        assert!(fx._dir.0.join("newdir").is_dir());
        assert_eq!(fx.store.operations.lock().unwrap().len(), 1);
        // 重複名は AlreadyExists 失敗
        let dup = execute_mkdir(&*fx.store, &*fx.fs, &fx._dir.0, "newdir").unwrap();
        assert_eq!(dup.status, FileOpStatus::Failed);
    }

    // ===== file_operations記録 (1論理操作=1行) =====

    #[test]
    fn file_operations_recorded_once_with_request_and_result() {
        let fx = fixture("recording");
        let dst_dir = fx._dir.mkdir("dest");
        let mut records = Vec::new();
        for i in 0..2 {
            let p = fx._dir.write(&format!("r{i}.png"), b"r");
            records.push(fx.store.add_file(fx.root.id, &p, &fx.root.path, b"r"));
        }
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            records.iter().map(|r| fx.op_file(r, None)).collect(),
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        // 1論理操作=1行
        let ops = fx.store.operations.lock().unwrap().clone();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].status, FileOpStatus::Done);
        assert!(ops[0].undoable);
        let request: serde_json::Value = serde_json::from_str(&ops[0].request_json).unwrap();
        assert_eq!(request["kind"], "copy");
        assert_eq!(request["files"].as_array().unwrap().len(), 2);
        let result: serde_json::Value =
            serde_json::from_str(ops[0].result_json.as_ref().unwrap()).unwrap();
        assert_eq!(
            result["steps"].as_array().unwrap().len(),
            2,
            "ファイル単位成否をresult JSONへ"
        );
        assert_eq!(result["steps"][0]["status"], "done");
    }

    // ===== Undo =====

    #[test]
    fn undo_rename_reverses_file_and_row() {
        let fx = fixture("undo-rename");
        let src = fx._dir.write("old.png", b"undo-me");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"undo-me");
        let asset = fx.store.make_asset("hash-u");
        fx.store.link(record.id, asset.id);
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Rename,
            vec![fx.op_file(&record, None)],
            None,
            Some("new.png"),
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert!(fx._dir.0.join("new.png").exists());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        assert_eq!(candidate.actions.len(), 1);
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone);
        assert!(fx._dir.0.join("old.png").exists(), "ファイルが戻る");
        assert!(!fx._dir.0.join("new.png").exists());
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.rel_path, PathBuf::from("old.png"), "DB行も戻る");
        assert_eq!(row.asset_id, Some(asset.id));
        // 操作行はundone
        let op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        assert_eq!(op.status, FileOpStatus::Undone);
        assert!(op.undone_at.is_some());
    }

    #[test]
    fn undo_move_back_across_dirs() {
        let fx = fixture("undo-move");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("m.png", b"m");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"m");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert!(dst_dir.join("m.png").exists());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone);
        assert!(src.exists(), "元位置へ戻る");
        assert!(!dst_dir.join("m.png").exists());
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.rel_path, PathBuf::from("m.png"));
    }

    #[test]
    fn undo_refuses_when_file_modified_after_operation() {
        // 安全原則: 外部変更で前提が崩れたUndoは拒否 + 理由
        let fx = fixture("undo-modified");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("v.png", b"original");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"original");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        // Undo前に外部からファイルを改変
        std::fs::write(dst_dir.join("v.png"), b"externally modified").unwrap();
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert_eq!(undo.refusals.len(), 1);
        assert!(
            matches!(undo.refusals[0], UndoRefusal::TokenMismatch { .. }),
            "{:?}",
            undo.refusals
        );
        assert!(
            undo.refusals[0].describe().contains("外部で変更"),
            "理由を人が読める形で: {}",
            undo.refusals[0].describe()
        );
        // ファイルは外部変更後のまま (破壊しない)
        assert_eq!(
            std::fs::read(dst_dir.join("v.png")).unwrap(),
            b"externally modified"
        );
        assert!(!src.exists());
        let op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        assert_eq!(op.status, FileOpStatus::Done, "操作行はundo済みにならない");
    }

    #[test]
    fn undo_refuses_when_destination_recreated() {
        let fx = fixture("undo-occupied");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("o.png", b"o");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"o");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        // 元位置に別ファイルが出現
        std::fs::write(&src, b"a different new file").unwrap();
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert!(matches!(
            undo.refusals[0],
            UndoRefusal::DestinationExists { .. }
        ));
        // 両ファイル無傷
        assert_eq!(std::fs::read(&src).unwrap(), b"a different new file");
        assert_eq!(std::fs::read(dst_dir.join("o.png")).unwrap(), b"o");
    }

    #[test]
    fn undo_all_or_nothing_when_batch_has_one_broken_step() {
        // 2ファイル移動後、片方だけ外部改変 → 全体拒否 (片方だけ戻さない)
        let fx = fixture("undo-batch");
        let dst_dir = fx._dir.mkdir("dest");
        let s1 = fx._dir.write("b1.png", b"one");
        let s2 = fx._dir.write("b2.png", b"two");
        let r1 = fx.store.add_file(fx.root.id, &s1, &fx.root.path, b"one");
        let r2 = fx.store.add_file(fx.root.id, &s2, &fx.root.path, b"two");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&r1, None), fx.op_file(&r2, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        std::fs::write(dst_dir.join("b2.png"), b"tampered").unwrap();
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert_eq!(undo.refusals.len(), 1);
        assert!(undo.steps.is_empty(), "一部だけ実行しない");
        // b1も戻っていない (全体拒否)
        assert!(dst_dir.join("b1.png").exists());
        assert!(!s1.exists());
    }

    #[test]
    fn undo_copy_removes_generated_file_and_marks_row_missing() {
        let fx = fixture("undo-copy");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("c.png", b"copy-undo");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"copy-undo");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert!(dst_dir.join("c.png").exists());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone);
        assert!(!dst_dir.join("c.png").exists(), "生成物が削除される");
        assert!(src.exists(), "元ファイルは無傷");
        // 生成物はテストtrashへ (安全側)
        assert!(fx.fs.trash_dir.lock().unwrap().join("c.png").exists());
        let copied_rel = PathBuf::from("dest/c.png");
        let row = fx
            .store
            .find_file(fx.root.id, &copied_rel)
            .expect("行はmissing化 (存続)");
        assert_eq!(row.scan_state, ScanState::Missing);
    }

    #[test]
    fn undo_trash_restores_file_and_row_present() {
        let fx = fixture("undo-trash");
        let src = fx._dir.write("t.png", b"resurrect");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"resurrect");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert!(!src.exists());
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone);
        assert_eq!(std::fs::read(&src).unwrap(), b"resurrect", "ゴミ箱から復帰");
        let row = fx.store.get_file(record.id).unwrap().unwrap();
        assert_eq!(row.scan_state, ScanState::Present, "行もpresent復帰");
        assert_eq!(row.rel_path, PathBuf::from("t.png"));
    }

    #[test]
    fn undo_after_already_undone_refuses() {
        let fx = fixture("undo-twice");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("x.png", b"x");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"x");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        let record_op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        let candidate = undo_candidate_of(&record_op);
        let first = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(first.fully_undone);
        // 2回目 (DB上はundone + undoable=0 → 再構築候補でも拒否)
        let record_op2 = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        let candidate2 = undo_candidate_of(&record_op2);
        let second = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate2,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!second.fully_undone);
        assert!(second
            .refusals
            .iter()
            .any(|r| matches!(r, UndoRefusal::AlreadyUndone | UndoRefusal::NotUndoable)));
    }

    #[test]
    fn undo_missing_file_refuses_with_reason() {
        let fx = fixture("undo-missing");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("y.png", b"y");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"y");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        // Undo前に移動先を外部から削除
        std::fs::remove_file(dst_dir.join("y.png")).unwrap();
        let record_op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        let candidate = undo_candidate_of(&record_op);
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert!(matches!(undo.refusals[0], UndoRefusal::FileMissing { .. }));
    }

    // ===== WP-7レビュー対応: 上書きTOCTOU・旧ファイル保護・厳密ゴミ箱同一性 =====

    /// 計画と実行の間に宛先が作成された場合、copy/moveは上書きせず失敗する
    /// (no-clobber。レビュー指摘: 上書きTOCTOU)。
    #[test]
    fn execute_refuses_destination_created_after_plan_copy_and_move() {
        for kind in [FileOpKind::Copy, FileOpKind::Move] {
            let fx = fixture("toctou");
            let src = fx._dir.write("a.png", b"src");
            let dst_dir = fx._dir.mkdir("dest");
            let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"src");
            // 計画時点では衝突なし
            let plan = plan_file_op(
                &*fx.fs,
                &fx.cancel,
                kind,
                vec![fx.op_file(&record, None)],
                Some(&dst_dir),
                None,
                &fx.roots,
            );
            assert!(plan.steps[0].conflict.is_none());
            // 計画→実行間に外部から宛先を作成 (TOCTOU窓)
            std::fs::write(dst_dir.join("a.png"), b"EXTERNAL").unwrap();
            let outcome = fx.run(&plan, &ResolutionSpec::default());
            assert_eq!(
                outcome.steps[0].status,
                StepStatus::Failed,
                "{kind}: 宛先出現後の実行は失敗"
            );
            assert!(matches!(
                outcome.steps[0].error.as_ref().unwrap().kind,
                FsOpErrorKind::AlreadyExists
            ));
            assert_eq!(
                std::fs::read(dst_dir.join("a.png")).unwrap(),
                b"EXTERNAL",
                "{kind}: 外部ファイルは上書きされない"
            );
            assert!(src.exists(), "{kind}: 元ファイルも無傷");
        }
    }

    /// keep_bothの事前計算名が計画後に占められた場合も配置を拒否する。
    #[test]
    fn execute_keep_both_refuses_name_taken_after_plan() {
        let fx = fixture("toctou-kb");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        fx.store
            .add_file(fx.root.id, &dst_dir.join("a.png"), &fx.root.path, b"OLD");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        assert_eq!(
            plan.steps[0].conflict.as_ref().unwrap().keep_both_name,
            "a (2).png"
        );
        // 計画後に代替名を外部が作成
        std::fs::write(dst_dir.join("a (2).png"), b"TAKEN").unwrap();
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::KeepBoth),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Failed);
        assert!(matches!(
            outcome.steps[0].error.as_ref().unwrap().kind,
            FsOpErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read(dst_dir.join("a (2).png")).unwrap(), b"TAKEN");
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"OLD");
    }

    /// 上書きcopyは旧宛先を先にゴミ箱へ退避し、undoで旧内容まで復元する
    /// (レビュー指摘: 上書き操作のundo可能性)。
    #[test]
    fn execute_overwrite_protects_old_file_and_undo_restores_it() {
        let fx = fixture("ow-copy");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        let old = fx
            .store
            .add_file(fx.root.id, &dst_dir.join("a.png"), &fx.root.path, b"OLD");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"NEW");
        // undoは複合: [新ファイル除去, 旧ファイル復帰]
        let undos = &outcome.steps[0].undo;
        assert_eq!(undos.len(), 2, "複合undoであること: {undos:?}");
        assert!(matches!(undos[0], UndoAction::DeleteCopy { .. }));
        assert!(matches!(undos[1], UndoAction::RestoreFromTrash { .. }));
        // undo実行 → 旧ファイルが内容まで復元される
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone, "{undo:?}");
        assert_eq!(
            std::fs::read(dst_dir.join("a.png")).unwrap(),
            b"OLD",
            "旧ファイルが内容まで復元"
        );
        assert!(src.exists(), "コピー元は無傷");
        let op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        assert_eq!(op.status, FileOpStatus::Undone);
        let _ = old;
    }

    /// 上書きmoveのundo: 新ファイルは元位置へ戻り、旧ファイルは復帰する。
    #[test]
    fn execute_overwrite_move_undo_restores_both() {
        let fx = fixture("ow-move");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        fx.store
            .add_file(fx.root.id, &dst_dir.join("a.png"), &fx.root.path, b"OLD");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Done);
        assert_eq!(std::fs::read(dst_dir.join("a.png")).unwrap(), b"NEW");
        let undos = &outcome.steps[0].undo;
        assert!(matches!(undos[0], UndoAction::MoveBack { .. }));
        assert!(matches!(undos[1], UndoAction::RestoreFromTrash { .. }));
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone, "{undo:?}");
        assert_eq!(
            std::fs::read(dst_dir.join("a.png")).unwrap(),
            b"OLD",
            "旧ファイル復帰"
        );
        assert_eq!(std::fs::read(&src).unwrap(), b"NEW", "新ファイルは元位置へ");
    }

    /// 上書き指定時に宛先が実行直前に消失していた場合、上書きせず失敗。
    #[test]
    fn execute_overwrite_refuses_when_dst_vanished() {
        let fx = fixture("ow-vanish");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        assert!(plan.steps[0].conflict.is_some());
        std::fs::remove_file(dst_dir.join("a.png")).unwrap();
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Failed);
        assert!(matches!(
            outcome.steps[0].error.as_ref().unwrap().kind,
            FsOpErrorKind::NotFound
        ));
    }

    /// 上書き配置に失敗した場合、退避した旧ファイルを復帰させる
    /// (ステップ不成立でも旧ファイルを失わない)。
    #[test]
    fn execute_overwrite_restores_old_when_placement_fails() {
        let fx = fixture("ow-fail");
        let src = fx._dir.write("a.png", b"NEW");
        let dst_dir = fx._dir.mkdir("dest");
        std::fs::write(dst_dir.join("a.png"), b"OLD").unwrap();
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"NEW");
        // 退避後に新配置が失敗するようコピーを注入失敗させる
        fx.fs
            .copy_fail
            .lock()
            .unwrap()
            .insert(src.clone(), "permission denied (injected)".to_string());
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(
            &plan,
            &ResolutionSpec {
                global: Some(ConflictResolution::Overwrite),
                per_file: HashMap::new(),
            },
        );
        assert_eq!(outcome.steps[0].status, StepStatus::Failed);
        assert_eq!(
            std::fs::read(dst_dir.join("a.png")).unwrap(),
            b"OLD",
            "旧ファイルは元位置へ復帰 (失われない)"
        );
    }

    /// ゴミ箱undoの厳密同一性: 同パスの複数エントリ・外部trashed混在下で
    /// 記録エントリのみを復帰する (レビュー指摘)。
    #[test]
    fn undo_trash_restores_exact_recorded_entry_among_same_path_entries() {
        let fx = fixture("trash-exact");
        // (1) 1回目: a.pngをtrash (内容 v1) → 手動復元 (外部trashed相当に残す)
        let src = fx._dir.write("a.png", b"v1");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"v1");
        let plan1 = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome1 = fx.run(&plan1, &ResolutionSpec::default());
        let entry1 = match &outcome1.steps[0].undo[0] {
            UndoAction::RestoreFromTrash { trash_entry, .. } => trash_entry.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        // 外部で同パスを別内容としてもう1つtrashed (エントリ追加)
        let src2 = fx._dir.write("a.png", b"v2-longer-content");
        let trashed2 = fx.fs.trash_dir.lock().unwrap().clone();
        std::fs::rename(&src2, trashed2.join("a.png.2")).unwrap();
        // (2) 2回目: a.png (v3) をtrash → この操作のundo対象はv3エントリ
        let src3 = fx._dir.write("a.png", b"v3");
        let record3 = fx.store.add_file(fx.root.id, &src3, &fx.root.path, b"v3");
        let plan3 = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record3, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome3 = fx.run(&plan3, &ResolutionSpec::default());
        let entry3 = match &outcome3.steps[0].undo[0] {
            UndoAction::RestoreFromTrash { trash_entry, .. } => trash_entry.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        assert_ne!(entry1.file_name, entry3.file_name, "別エントリとして特定");
        // undo → v3が復帰する (最新一致ではなく記録エントリ)
        let candidate = {
            let ops = fx.store.get_operation(outcome3.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(undo.fully_undone, "{undo:?}");
        assert_eq!(
            std::fs::read(&src3).unwrap(),
            b"v3",
            "記録したエントリ (=v3) が復帰"
        );
    }

    /// 記録されたゴミ箱エントリが不在 (外部で空にされた) 場合はundo拒否。
    #[test]
    fn undo_trash_refuses_when_recorded_entry_missing() {
        let fx = fixture("trash-missing-entry");
        let src = fx._dir.write("a.png", b"gone");
        let record = fx.store.add_file(fx.root.id, &src, &fx.root.path, b"gone");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        // ゴミ箱エントリを外部で削除
        let entry = match &outcome.steps[0].undo[0] {
            UndoAction::RestoreFromTrash { trash_entry, .. } => trash_entry.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        std::fs::remove_file(entry.trash_dir.join(&entry.file_name)).unwrap();
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert!(
            undo.refusals
                .iter()
                .any(|r| matches!(r, UndoRefusal::TrashEntryNotFound { .. })),
            "{:?}",
            undo.refusals
        );
    }

    /// エントリのサイズが記録 (トークンのsize成分) と一致しない場合は拒否。
    #[test]
    fn undo_trash_refuses_when_entry_size_mismatch() {
        let fx = fixture("trash-size");
        let src = fx._dir.write("a.png", b"original-size");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"original-size");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Trash,
            vec![fx.op_file(&record, None)],
            None,
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        // ゴミ箱内の実体を書き換え (サイズ変化)
        let entry = match &outcome.steps[0].undo[0] {
            UndoAction::RestoreFromTrash { trash_entry, .. } => trash_entry.clone(),
            other => panic!("unexpected: {other:?}"),
        };
        std::fs::write(entry.trash_dir.join(&entry.file_name), b"DIFFERENT").unwrap();
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert!(
            undo.refusals
                .iter()
                .any(|r| matches!(r, UndoRefusal::TrashEntrySizeMismatch { .. })),
            "{:?}",
            undo.refusals
        );
    }

    /// Undo原子性: 2番目アクションの失敗 → 実行済み1番目が補償され、
    /// statusはpartial_undone (レビュー指摘: all-or-nothingの構造化)。
    #[test]
    fn undo_rolls_back_executed_actions_when_later_action_fails() {
        let fx = fixture("undo-rollback");
        let dst_dir = fx._dir.mkdir("dest");
        // 2ファイルmove → undo候補は [MoveBack(m1), MoveBack(m2)]
        let s1 = fx._dir.write("m1.png", b"one");
        let s2 = fx._dir.write("m2.png", b"two");
        let r1 = fx.store.add_file(fx.root.id, &s1, &fx.root.path, b"one");
        let r2 = fx.store.add_file(fx.root.id, &s2, &fx.root.path, b"two");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&r1, None), fx.op_file(&r2, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        // undo実行中に2番目のMoveBack (m2) を失敗させる: 移動元が改変
        // されていれば再検証で失敗する — 実行直前に改変を注入するフック
        // として、trash失敗ではなく「移動先ファイル改変」を使う。
        std::fs::write(dst_dir.join("m2.png"), b"tampered").unwrap();
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        // verifyは全体拒否されるため、逐次実行のロールバックを起こすには
        // verify通過後に変化させる必要がある。ここではexecute_undo呼び出し
        // 前に改変するとverify拒否になるため、ロールバック経路の直接検証は
        // 「verify通過 → 実行時再検証失敗」を作る独立テストに譲り、
        // このテストではverify全体拒否 (all-or-nothing) を確認する。
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone);
        assert!(undo.steps.is_empty(), "何も実行しない");
        assert!(dst_dir.join("m1.png").exists(), "m1も戻らない");
        assert!(!s1.exists());
    }

    /// 逐次実行中の失敗 (verify後・実行時再検証で崩れたケース) の
    /// 補償ロールバック: 1番目実行後に2番目の前提が外部で崩れると、
    /// 1番目が巻き戻され partial_undone になる。
    #[test]
    fn undo_compensates_executed_action_when_second_fails_midway() {
        let fx = fixture("undo-comp");
        let dst_dir = fx._dir.mkdir("dest");
        let s1 = fx._dir.write("c1.png", b"one");
        let s2 = fx._dir.write("c2.png", b"two");
        let r1 = fx.store.add_file(fx.root.id, &s1, &fx.root.path, b"one");
        let r2 = fx.store.add_file(fx.root.id, &s2, &fx.root.path, b"two");
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            vec![fx.op_file(&r1, None), fx.op_file(&r2, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(outcome.status, FileOpStatus::Done);
        // undo = [DeleteCopy(dest/c1), DeleteCopy(dest/c2)]。
        // 2番目 (c2) のtrashを失敗注入すると、1番目 (c1) の実行済み
        // DeleteCopyが補償 (ゴミ箱から復帰) される。
        fx.fs
            .trash_fail
            .lock()
            .unwrap()
            .insert(dst_dir.join("c2.png"));
        let candidate = {
            let ops = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
            undo_candidate_of(&ops)
        };
        let undo = execute_undo(
            &*fx.store,
            &*fx.fs,
            &candidate,
            &fx.roots,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!undo.fully_undone, "{undo:?}");
        assert_eq!(undo.steps.len(), 2);
        assert_eq!(undo.steps[0].status, StepStatus::Done);
        assert_eq!(undo.steps[1].status, StepStatus::Failed);
        assert!(undo.rollback_errors.is_empty(), "補償は成功: {undo:?}");
        // 補償: c1は元位置 (dest) へ復帰している
        assert_eq!(
            std::fs::read(dst_dir.join("c1.png")).unwrap(),
            b"one",
            "実行済みDeleteCopyが補償され復帰"
        );
        assert!(dst_dir.join("c2.png").exists(), "失敗したc2はそのまま");
        assert!(s1.exists() && s2.exists(), "コピー元は無傷");
        // 操作行は partial_undone
        let op = fx.store.get_operation(outcome.op_id).unwrap().unwrap();
        assert_eq!(op.status, FileOpStatus::PartialUndone);
        assert!(!op.undoable, "再undoは不可");
    }

    /// 跨界移動でコピー後のsrc削除に失敗した場合、宛先完成済みのため
    /// warning付き成功となる (non-blocker対応)。
    #[test]
    fn execute_move_exdev_remove_failure_is_warning_success() {
        let fx = fixture("exdev-warn");
        let dst_dir = fx._dir.mkdir("dest");
        let src = fx._dir.write("a.png", b"cross-fs");
        let record = fx
            .store
            .add_file(fx.root.id, &src, &fx.root.path, b"cross-fs");
        fx.fs.rename_exdev.lock().unwrap().insert(src.clone());
        fx.fs.remove_fail.lock().unwrap().insert(src.clone());
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Move,
            vec![fx.op_file(&record, None)],
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let outcome = fx.run(&plan, &ResolutionSpec::default());
        assert_eq!(
            outcome.steps[0].status,
            StepStatus::Done,
            "宛先完成済みのため成功扱い"
        );
        assert!(
            outcome.steps[0]
                .warning
                .as_ref()
                .is_some_and(|w| w.contains("元ファイル削除に失敗")),
            "警告がある: {:?}",
            outcome.steps[0].warning
        );
        assert!(dst_dir.join("a.png").exists());
        assert!(src.exists(), "srcは残る (手動削除指示)");
    }

    /// 一時ファイル命名の厳密形式 (所有権検証の基盤)。
    #[test]
    fn tmp_name_strict_format_and_ownership() {
        let dst = Path::new("/r/sub/a.png");
        let tmp = tmp_path_for(dst, "01234567-89ab-cdef-0123-456789abcdef", 3);
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name,
            ".a.png.gento-tmp-0123456789abcdef0123456789abcdef-000003"
        );
        assert_eq!(tmp.parent().unwrap(), Path::new("/r/sub"));
        assert!(is_owned_tmp_name(&name));
        assert_eq!(
            tmp_owner_uuid_simple(&name).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        // 類似名の外部ファイルは所有扱いしない
        for ng in [
            ".gento-tmp-x.png",
            ".a.png.gento-tmp-nothex32-000001",
            ".a.png.gento-tmp-0123456789abcdef0123456789abcdef-1",
            ".a.png.gento-tmp-0123456789abcdef0123456789abcdef-00000x",
            "a.png.gento-tmp-0123456789abcdef0123456789abcdef-000001",
            ".a.png.gento-tmp",
        ] {
            assert!(!is_owned_tmp_name(ng), "{ng}");
            assert!(tmp_owner_uuid_simple(ng).is_none(), "{ng}");
        }
    }

    // ===== 進捗イベント =====

    #[test]
    fn progress_events_fire_per_file() {
        let fx = fixture("progress");
        let dst_dir = fx._dir.mkdir("dest");
        let mut records = Vec::new();
        for i in 0..2 {
            let p = fx._dir.write(&format!("g{i}.png"), b"g");
            records.push(fx.store.add_file(fx.root.id, &p, &fx.root.path, b"g"));
        }
        let plan = plan_file_op(
            &*fx.fs,
            &fx.cancel,
            FileOpKind::Copy,
            records.iter().map(|r| fx.op_file(r, None)).collect(),
            Some(&dst_dir),
            None,
            &fx.roots,
        );
        let events = Arc::new(StdMutex::new(Vec::new()));
        let sink = events.clone();
        let fs = fx.fs.clone();
        let cancel = fx.cancel.clone();
        execute_file_op(
            &*fx.store,
            &*fs,
            &plan,
            &ResolutionSpec::default(),
            &fx.roots,
            &cancel,
            &mut move |p: FileOpProgress| {
                sink.lock().unwrap().push(p);
            },
        )
        .unwrap();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 4, "ファイルごとに開始+完了: {:?}", events);
        assert!(!events[0].done);
        assert!(events[1].done);
        assert_eq!(events[0].total, 2);
    }
}
