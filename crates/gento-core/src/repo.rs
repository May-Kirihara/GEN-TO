//! 永続化repository抽象 (MVP WP-1)。
//!
//! traitとレコード型はドメイン層 (このcrate) に置き、SQLite実装は
//! `gento-db` が担う (計画書§5「外部機能はtraitを介して注入」。
//! 依存方向: gento-db → gento-core。逆方向禁止)。
//!
//! 格納規約 (PM計画§6-4/5、docs/dev/architecture.md):
//! - 内部ID: INTEGER (i64)。外部へ出すID・backup/restoreはUUID (TEXT 36文字小文字)
//! - DBレコードのライフサイクル時刻: UTC RFC3339 TEXT
//!   (ファイル由来のmtime等はepoch ms整数 — gento-dbスキーマ参照)
//! - enum: TEXT + CHECK制約 (このモジュールの `as_str` が格納値の正本)

use std::fmt;
use std::path::{Path, PathBuf};

/// ファイルの走査状態 (data-model §2.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    Present,
    Missing,
    Unreadable,
    Unsupported,
    Corrupt,
}

impl ScanState {
    pub const ALL: [ScanState; 5] = [
        ScanState::Present,
        ScanState::Missing,
        ScanState::Unreadable,
        ScanState::Unsupported,
        ScanState::Corrupt,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ScanState::Present => "present",
            ScanState::Missing => "missing",
            ScanState::Unreadable => "unreadable",
            ScanState::Unsupported => "unsupported",
            ScanState::Corrupt => "corrupt",
        }
    }
}

impl fmt::Display for ScanState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 不正な走査状態文字列のパース失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseScanStateError;

impl fmt::Display for ParseScanStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown scan state: expected one of present|missing|unreadable|unsupported|corrupt"
        )
    }
}

impl std::error::Error for ParseScanStateError {}

impl std::str::FromStr for ScanState {
    type Err = ParseScanStateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "present" => Ok(ScanState::Present),
            "missing" => Ok(ScanState::Missing),
            "unreadable" => Ok(ScanState::Unreadable),
            "unsupported" => Ok(ScanState::Unsupported),
            "corrupt" => Ok(ScanState::Corrupt),
            _ => Err(ParseScanStateError),
        }
    }
}

/// 選別状態 (data-model §2.1)。
///
/// スパイク期の `catalog::Triage` (undecided/keep/reject) とは別の製品用型。
/// DB格納値はdata-modelの `unselected` | `picked` | `rejected`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageState {
    Unselected,
    Picked,
    Rejected,
}

impl TriageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            TriageState::Unselected => "unselected",
            TriageState::Picked => "picked",
            TriageState::Rejected => "rejected",
        }
    }
}

impl fmt::Display for TriageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Asset内容ハッシュの計算状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashState {
    Pending,
    Done,
}

impl HashState {
    pub const fn as_str(self) -> &'static str {
        match self {
            HashState::Pending => "pending",
            HashState::Done => "done",
        }
    }
}

/// repository層の失敗。実装詳細 (SQLエラー文字列等) は [`RepoError::Db`]
/// にのみ持ち、コアは永続化技術 (rusqlite等) の型に依存しない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoError {
    /// 対象行が存在しない (更新・削除・リンク対象の不在)。
    NotFound,
    /// 一意制約違反 (登録ルートパス重複・content_hash重複等)。
    Duplicate,
    /// 下層の失敗。外部キー違反を含む (メッセージで判別)。
    Db(String),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::NotFound => f.write_str("not found"),
            RepoError::Duplicate => f.write_str("duplicate: unique constraint violation"),
            RepoError::Db(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for RepoError {}

/// 登録ルート (library_roots) のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRootRecord {
    pub id: i64,
    pub uuid: String,
    /// OSネイティブの絶対パス (非UTF-8バイトを保持し得る)。
    pub path: PathBuf,
    /// 表示用パス (lossy変換。非UTF-8部分は置換文字になる)。
    pub display_path: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// [`crate::repo::FileRepo::upsert_file`] の入力。
///
/// `rel_path` は登録ルートからの相対パス (OSネイティブ表現。
/// 非UTF-8バイトを含み得る)。`asset_id` は持たない — Asset作成前に
/// 走査中Fileを管理できることが WP-1の受け入れ条件のため、
/// Fileはasset_id NULLで作成され、リンクは
/// [`crate::repo::AssetRepo::link_file`] で行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUpsert {
    pub root_id: i64,
    pub rel_path: PathBuf,
    /// 表示用ファイル名 (lossy)。
    pub display_name: String,
    pub byte_size: u64,
    pub mtime_ms: u64,
    pub scan_state: ScanState,
}

/// ファイル (files) のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: i64,
    pub uuid: String,
    pub root_id: i64,
    /// 未リンク (Asset未作成) は `None`。
    pub asset_id: Option<i64>,
    pub rel_path: PathBuf,
    pub display_name: String,
    pub extension: Option<String>,
    pub byte_size: u64,
    pub mtime_ms: u64,
    pub scan_state: ScanState,
    pub last_seen_at: String,
    pub created_at: String,
    pub updated_at: String,
}

/// [`crate::repo::AssetRepo::create_asset`] の入力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAsset {
    /// ハッシュの正規表現は `<algorithm>:<hex>` (例: `blake3:…64hex`)。
    pub content_hash: String,
    pub hash_algorithm: String,
    pub byte_size: u64,
    pub mime_type: Option<String>,
}

/// 画像内容 (assets) のレコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRecord {
    pub id: i64,
    pub uuid: String,
    pub content_hash: String,
    pub hash_algorithm: String,
    pub hash_state: HashState,
    pub byte_size: u64,
    pub mime_type: Option<String>,
    pub rating: u8,
    pub triage: TriageState,
    pub favorite: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 登録ルートのrepository。
pub trait LibraryRootRepo {
    /// ルートを追加する。パス重複 (既存ルートと同一パス) は
    /// [`RepoError::Duplicate`]。親子二重登録の検出は
    /// [`LibraryRootRepo::overlapping_roots`] を呼び出し側で使う
    /// (UNIQUE制約は同パスのみ検出するため)。
    fn add_root(&self, path: &Path) -> Result<LibraryRootRecord, RepoError>;

    fn list_roots(&self) -> Result<Vec<LibraryRootRecord>, RepoError>;

    /// 有効状態を更新する。存在しないIDは [`RepoError::NotFound`]。
    fn set_root_enabled(&self, root_id: i64, enabled: bool) -> Result<(), RepoError>;

    /// ルートを解除する (配下のFileも削除。Asset自体は残る)。
    /// 存在しないIDは [`RepoError::NotFound`]。
    fn remove_root(&self, root_id: i64) -> Result<(), RepoError>;

    /// 親子二重登録検出の雛形 (WP-2本実装の土台)。
    /// `path` が既存ルートの配下、または既存ルートが `path` の配下に
    /// ある場合にそれらのルートを返す (同一パスを含む)。
    fn overlapping_roots(&self, path: &Path) -> Result<Vec<LibraryRootRecord>, RepoError>;
}

/// ファイルのrepository。
pub trait FileRepo {
    /// `root_id` + `rel_path` をキーにupsertする。
    /// 既存時は byte_size / mtime / scan_state / 表示名を更新し、
    /// uuid・asset_id は変えない。新規時は asset_id NULL で作成する。
    fn upsert_file(&self, file: FileUpsert) -> Result<FileRecord, RepoError>;

    /// 走査状態を更新する (missing と削除の区別はここで担保)。
    /// 存在しないIDは [`RepoError::NotFound`]。
    fn set_scan_state(&self, file_id: i64, state: ScanState) -> Result<(), RepoError>;

    /// ルート配下の `scan_state = 'missing'` のFile一覧。
    fn list_missing(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError>;

    /// `root_id` + `rel_path` でFileを引く。
    fn find_by_path(&self, root_id: i64, rel_path: &Path) -> Result<Option<FileRecord>, RepoError>;
}

/// 画像内容 (Asset) のrepository。
pub trait AssetRepo {
    /// Assetを作成する。content_hash重複は [`RepoError::Duplicate`]
    /// (呼び出し側は先に [`AssetRepo::find_by_hash`] で統合判定する)。
    fn create_asset(&self, asset: NewAsset) -> Result<AssetRecord, RepoError>;

    fn find_by_hash(&self, content_hash: &str) -> Result<Option<AssetRecord>, RepoError>;

    /// File → Asset のリンクを設定する。
    /// FileまたはAssetが存在しない場合は [`RepoError::NotFound`]。
    fn link_file(&self, file_id: i64, asset_id: i64) -> Result<(), RepoError>;
}

/// `child` が `parent` 配下 (または同一) のパスかをコンポーネント単位で
/// 判定する。シンボリックリンクや `..` を解決しない字面比較のため、
/// 呼び出し側は正規化済みパスを渡すこと (WP-2でcanonicalizeを挟む)。
pub fn path_is_within(child: &Path, parent: &Path) -> bool {
    child.starts_with(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn scan_state_str_round_trip() {
        for state in ScanState::ALL {
            assert_eq!(ScanState::from_str(state.as_str()).unwrap(), state);
            assert_eq!(state.to_string(), state.as_str());
        }
    }

    #[test]
    fn scan_state_rejects_unknown() {
        assert_eq!("".parse::<ScanState>().unwrap_err(), ParseScanStateError);
        assert_eq!(
            "gone".parse::<ScanState>().unwrap_err(),
            ParseScanStateError
        );
        // 大文字・前後空白は許容しない (格納値は正規形のみ)
        assert!("Present".parse::<ScanState>().is_err());
        assert!(" present".parse::<ScanState>().is_err());
    }

    #[test]
    fn triage_state_str_values() {
        assert_eq!(TriageState::Unselected.as_str(), "unselected");
        assert_eq!(TriageState::Picked.as_str(), "picked");
        assert_eq!(TriageState::Rejected.as_str(), "rejected");
    }

    #[test]
    fn hash_state_str_values() {
        assert_eq!(HashState::Pending.as_str(), "pending");
        assert_eq!(HashState::Done.as_str(), "done");
    }

    #[test]
    fn repo_error_display() {
        assert_eq!(RepoError::NotFound.to_string(), "not found");
        assert_eq!(
            RepoError::Duplicate.to_string(),
            "duplicate: unique constraint violation"
        );
        assert_eq!(
            RepoError::Db("boom".to_string()).to_string(),
            "database error: boom"
        );
    }

    #[test]
    fn path_is_within_component_semantics() {
        let parent = Path::new("/data/photos");
        assert!(path_is_within(Path::new("/data/photos"), parent)); // 同一も含む
        assert!(path_is_within(Path::new("/data/photos/a/b.png"), parent));
        assert!(!path_is_within(Path::new("/data/photos2"), parent)); // 接頭辞でもコンポーネント不同
        assert!(!path_is_within(Path::new("/data"), parent));
        // コンポーネント単位: "/data/photosx" は "/data/photos" 配下ではない
        assert!(!path_is_within(Path::new("/data/photosx/x.png"), parent));
    }
}
