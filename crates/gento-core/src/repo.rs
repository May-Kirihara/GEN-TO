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
    /// 一意制約・外部キー以外の制約違反 (CHECK・NOT NULL等)。
    /// メッセージにSQLiteの診断文 (例: "NOT NULL constraint failed: …")。
    Constraint(String),
    /// 下層の失敗。外部キー違反を含む (メッセージで判別)。
    Db(String),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::NotFound => f.write_str("not found"),
            RepoError::Duplicate => f.write_str("duplicate: unique constraint violation"),
            RepoError::Constraint(msg) => write!(f, "constraint violation: {msg}"),
            RepoError::Db(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for RepoError {}

/// OS固有のファイル識別子 (WP-2。spec-draft §6.2「利用可能な場合はOS固有のファイル識別子」)。
///
/// Linuxでは `stat` の `st_dev + st_ino`。同一ファイルシステム内で
/// リネーム・移動しても不変のため、外部移動・改名の再発見に使う。
/// DBには16バイトリトルエンディアンBLOB (dev, ino の順) で格納する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OsFileId {
    pub dev: u64,
    pub ino: u64,
}

impl OsFileId {
    pub const fn new(dev: u64, ino: u64) -> Self {
        Self { dev, ino }
    }

    /// DB格納BLOB (16バイトLE)。
    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.dev.to_le_bytes());
        out[8..].copy_from_slice(&self.ino.to_le_bytes());
        out
    }

    /// BLOBからの復元。長さが16でなければ `None`。
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 16 {
            return None;
        }
        let mut dev = [0u8; 8];
        let mut ino = [0u8; 8];
        dev.copy_from_slice(&bytes[..8]);
        ino.copy_from_slice(&bytes[8..]);
        Some(Self {
            dev: u64::from_le_bytes(dev),
            ino: u64::from_le_bytes(ino),
        })
    }
}

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
    /// フォルダ監視 (WP-2のwatcherが使用。data-model §2.3)。
    pub watch_enabled: bool,
    /// AI自動処理 (WP-10で使用)。
    pub ai_enabled: bool,
    pub send_exif: bool,
    pub send_gps: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// [`crate::repo::LibraryRootRepo::set_root_settings`] の入力。
/// `settings_json` は将来用の逃がし列でWP-2では対象外。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootSettings {
    pub watch_enabled: bool,
    pub ai_enabled: bool,
    pub send_exif: bool,
    pub send_gps: bool,
}

impl Default for RootSettings {
    fn default() -> Self {
        // schema #1の列既定値と同一 (watch=1, ai/exif/gps=0)。
        Self {
            watch_enabled: true,
            ai_enabled: false,
            send_exif: false,
            send_gps: false,
        }
    }
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
    /// OSファイルID (取得可能な場合)。外部移動・改名の再発見に使用。
    pub os_file_id: Option<OsFileId>,
    /// `true` の場合、upsert時に asset リンクを解除する。
    /// サイズ+mtime変化を検出した際、内容が変わった可能性があるため
    /// 再ハッシュで正しいAssetへ再リンクされるまで Asset確定前 (NULL) に
    /// 戻す。内容が同一だった場合はハッシュ一致で同一Assetへ再リンクされ
    /// メタデータは失われない。
    pub detach_asset: bool,
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
    /// OSファイルID (Linux: dev+inode)。取得不能な環境・未走査は `None`。
    pub os_file_id: Option<OsFileId>,
    /// 最後にファイルの**存在を確認した**時刻 (UTC RFC3339)。
    /// missingへの状態変更では更新されない — 存在確認を反映するのは
    /// 発見時の [`crate::repo::FileRepo::upsert_file`] のみ。
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
    /// プローブ可能だった場合の寸法 (WP-2ではPNGのみ。他形式はNULL)。
    pub width: Option<u32>,
    pub height: Option<u32>,
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
    /// 寸法 (プローブ時点の値。未プローブは `None`)。
    pub width: Option<u32>,
    pub height: Option<u32>,
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

    /// ルート設定列 (watch/ai/exif/gps) を更新する。
    /// 存在しないIDは [`RepoError::NotFound`]。
    fn set_root_settings(&self, root_id: i64, settings: RootSettings) -> Result<(), RepoError>;
}

/// ファイルのrepository。
pub trait FileRepo {
    /// `root_id` + `rel_path` をキーにupsertする。
    /// 既存時は byte_size / mtime / scan_state / 表示名 / `last_seen_at` を
    /// 更新し、uuid・asset_id は変えない。新規時は asset_id NULL で作成する。
    /// upsertは「存在を確認した」ことを意味するため `last_seen_at` を現在
    /// 時刻へ進める。
    fn upsert_file(&self, file: FileUpsert) -> Result<FileRecord, RepoError>;

    /// 走査状態を更新する (missing と削除の区別はここで担保)。
    /// `last_seen_at` は更新しない — 同一刻は「最後に存在を確認した時刻」
    /// を意味し、存在確認は [`FileRepo::upsert_file`] 等の明示的な
    /// 更新経由でのみ反映される。
    /// 存在しないIDは [`RepoError::NotFound`]。
    fn set_scan_state(&self, file_id: i64, state: ScanState) -> Result<(), RepoError>;

    /// ルート配下の `scan_state = 'missing'` のFile一覧。
    fn list_missing(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError>;

    /// `root_id` + `rel_path` でFileを引く。
    fn find_by_path(&self, root_id: i64, rel_path: &Path) -> Result<Option<FileRecord>, RepoError>;

    /// [`FileRepo::upsert_file`] のバッチ版 (data-model §13「小さなバッチで
    /// コミット」)。1呼び出し=1トランザクション。呼び出し側がバッチサイズを
    /// 制御し、writer接続を長時間専有しない。
    fn upsert_files_batch(&self, files: &[FileUpsert]) -> Result<Vec<FileRecord>, RepoError>;

    /// OSファイルIDでFileを引く (外部移動・改名の再発見。WP-2)。
    /// 同一ルート内のみ探索 (クロスルートの再関連付けは将来課題)。
    fn find_by_os_file_id(
        &self,
        root_id: i64,
        os_file_id: &OsFileId,
    ) -> Result<Option<FileRecord>, RepoError>;

    /// ファイルの実体が移動・改名されたことを反映する (同一レコードの
    /// rel_path を更新。uuid・assetリンクは保持)。
    /// 移動先rel_pathに既存レコードが存在する場合は [`RepoError::Duplicate`]
    /// (呼び出し側は通常upsertへフォールバック)。
    fn relocate_file(&self, file_id: i64, file: FileUpsert) -> Result<FileRecord, RepoError>;

    /// 走査状態を一括更新 (不在化バッチ用)。更新した行数を返す。
    fn set_scan_state_batch(&self, file_ids: &[i64], state: ScanState) -> Result<usize, RepoError>;

    /// ルート配下のFileをページングで列挙 (offset昇順・id順)。
    /// 戻り値は (ページ, 全件数)。
    fn list_files_paged(
        &self,
        root_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<(Vec<FileRecord>, u64), RepoError>;

    /// 全ルートの `scan_state='present'` なFileをAsset情報 (LEFT JOIN) 付きで
    /// 列挙する (永続カタログのスナップショット生成。WP-2)。
    /// 件数はカタログ規模 (5万件程度) を想定した全件読み込み。
    fn list_present_joined(&self) -> Result<Vec<(FileRecord, Option<AssetRecord>)>, RepoError>;

    /// ルート配下でAsset未リンク (asset_id NULL = ハッシュ未計算) のFile一覧。
    /// ハッシュ処理の再開 (resume) 判定に使用。
    fn list_files_without_asset(&self, root_id: i64) -> Result<Vec<FileRecord>, RepoError>;
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

    /// 同一内容ハッシュのAssetがあればそれを返し、無ければ作成する。
    /// 戻り値は (Asset, 新規作成か)。[`AssetRepo::create_asset`] の
    /// Duplicate処理 (find→create→競合時再find) を冪等にまとめたもの。
    fn create_or_get_asset(&self, asset: NewAsset) -> Result<(AssetRecord, bool), RepoError>;
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
            RepoError::Constraint("NOT NULL constraint failed: files.display_name".to_string())
                .to_string(),
            "constraint violation: NOT NULL constraint failed: files.display_name"
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

    #[test]
    fn os_file_id_bytes_round_trip() {
        let id = OsFileId::new(0x0102_0304_0506_0708, 42);
        let bytes = id.to_bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(OsFileId::from_bytes(&bytes), Some(id));
        // LE順: dev先頭8バイト
        assert_eq!(&bytes[..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&bytes[8..], &42u64.to_le_bytes());
        assert_eq!(OsFileId::from_bytes(&[0u8; 15]), None);
        assert_eq!(OsFileId::from_bytes(&[0u8; 17]), None);
    }

    #[test]
    fn root_settings_default_matches_schema_defaults() {
        let s = RootSettings::default();
        assert!(s.watch_enabled);
        assert!(!s.ai_enabled);
        assert!(!s.send_exif);
        assert!(!s.send_gps);
    }
}
