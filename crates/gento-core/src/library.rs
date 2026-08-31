//! 登録ルートのスキャン・同期エンジン (MVP WP-2)。
//!
//! Tauri非依存・永続化実装非依存 (計画書§5「外部機能はtraitを介して注入」):
//!
//! - [`RepoStore`]: DBへのrepoアクセス境界。実装は `gento-db::Db`
//!   (writer/reader分離。読み取りはwriterをブロックしない)。
//! - [`ScanFs`]: ファイルシステム操作 (走査・プローブ・ハッシュ)。
//!   実装は `gento-image::PlatformFs` (walkdir + PNG probe + BLAKE3)。
//!
//! # スキャンアルゴリズム (size+mtime高速判定。spec-draft §6.2)
//!
//! 1. 対象ルートの既存Fileを全件読み出し、rel_path照合表とos_file_id照合表を構築
//! 2. walkで列挙した各エントリを照合:
//!    - 既存・変化なし → 書き込みなしの高速パス (last_seen_atも更新しない。
//!      存在確認の反映はupsert (発見) のみというWP-1の規約を維持)
//!    - 既存・size/mtime変化 → upsert + **assetリンク解除** (内容変化の
//!      可能性。再ハッシュで同一内容なら同一Assetへ再リンクされ、
//!      評価等のメタデータは失われない)
//!    - 既存・missing再出現 → upsert (present復帰。size/mtime同一なら
//!      リンク保持)
//!    - 未知パス + os_file_idが既存レコードと一致 **かつ size/mtimeも一致**
//!      → 外部移動・改名と判定し [`FileRepo::relocate_file`] で同一レコードの
//!      rel_pathを更新 (uuid・assetリンク保持)。size/mtimeが不一致なら移動と
//!      みなさず新規扱い (inode再利用・移動+内容変化からuuid/assetを保護)
//!    - 未知パス (照合なし) → 新規upsert
//! 3. walk後に一度も見なかった既存File (scope内) は `missing` 化
//!    (削除しない。メタデータ保持)。ただし列挙に失敗したパス配下
//!    (権限不足等。[`WalkError::blocks_missing`]) は保守的に現状維持とする
//! 4. asset未リンクのFile (新規・変更・ハッシュ未完了) をハッシュ待ちとして
//!    返す。呼び出し側が低優先度 (Background) でBLAKE3を計算し
//!    [`hash_and_link_file`] でAssetへ統合する。リンクは
//!    「ハッシュ時の世代トークン (os_file_id, size, mtime_ns) がDBレコードの
//!    現在トークンと一致する場合にのみ」行われる (世代検証。古いジョブの
//!    完了が新しいスキャンを追い越しても誤リンクしない)
//!
//! # キャンセルと再開
//!
//! キャンセルは [`AtomicBool`] をバッチ境界で検査する協調キャンセル。
//! 進行状況はDB (files/assets) が正本のため、再開 = 再スキャンで冪等に
//! 続きから拾う (ハッシュ待ちも `asset_id IS NULL` から再構成される)。
//!
//! # 監視プロトコル (Codex計画レビュー指摘の反映)
//!
//! 監視確立 → (イベントバッファリング) → 初回スキャン → キューイベント
//! 適用 (スコープ差分スキャン=reconciliation) 。イベント欠落 (inotify
//! overflow等) は [`WatchBuffer::mark_overflow`] → 全体再スキャンで収束する。
//! 「大量イベント後も最終状態一致」は差分スキャンの冪等性 (上記1〜3が
//! ファイルシステムの最終状態のみに依存) による。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::repo::{
    AssetRecord, FileRecord, FileRepo, FileUpsert, LibraryRootRecord, NewAsset, OsFileId,
    RepoError, ScanState,
};
use crate::AssetId;

/// 1回のwriterトランザクションに含めるupsertの既定件数 (data-model §13)。
pub const DEFAULT_SCAN_BATCH_SIZE: usize = 100;

/// 既存File一覧の読み出しページサイズ。
const LIST_PAGE_SIZE: u32 = 1_000;

/// 走査エントリ (fs実装がrel_pathを解決済みの形で渡す)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    /// 登録ルートからの相対パス。
    pub rel_path: PathBuf,
    pub byte_size: u64,
    pub mtime_ms: u64,
    /// ns精度mtime (世代トークン成分)。
    pub mtime_ns: u64,
    pub os_file_id: Option<OsFileId>,
}

/// 走査中の個別エラー (ルート相対パス解決済み)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkError {
    /// エラー対象のルート相対パス。ルート自体の失敗は空パス。
    pub rel_path: PathBuf,
    pub reason: String,
    /// `true` = このパス (配下含む) の不在化を禁じる。
    /// 「パスは存在するが読めない」(権限不足等) 場合にtrue。
    /// パス自体が消えた (走査中削除・ルート不在) 場合はfalse —
    /// そこは実際に不在のためmissing化が正しい。
    pub blocks_missing: bool,
}

impl WalkError {
    /// 進捗・ログ用の1行表現。
    pub fn to_display(&self) -> String {
        let path = if self.rel_path.as_os_str().is_empty() {
            ".".to_string()
        } else {
            self.rel_path.display().to_string()
        };
        format!("{path}: {}", self.reason)
    }
}

/// walk全体の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkOutcome {
    /// 列挙したファイルエントリ総数 (拡張子を問わない)。
    pub entries_walked: u64,
    /// 個別エラー (権限不足等。走査は継続する)。
    pub errors: Vec<WalkError>,
}

/// ファイルハッシュ計算の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashFileError {
    Io(String),
    Cancelled,
    /// 計算対象の世代が古い (計算中の変化・新旧スキャン間での不一致)。
    /// 結果は破棄され、ファイルは未リンクのまま次回スキャンで再ハッシュされる。
    Stale(String),
}

impl std::fmt::Display for HashFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HashFileError::Io(msg) => write!(f, "hash io error: {msg}"),
            HashFileError::Cancelled => f.write_str("hash cancelled"),
            HashFileError::Stale(msg) => write!(f, "hash stale: {msg}"),
        }
    }
}

impl std::error::Error for HashFileError {}

/// ファイル内容ハッシュの計算結果。検証メタデータ (byte_size, mtime_ns,
/// os_file_id) は計算に使った開いたfdのfstat観測で、計算期間を通じて
/// 不変だった値。リンク時の世代検証 ([`hash_and_link_file`]) に使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContentHash {
    /// `blake3:<64hex小文字>` 形式。
    pub content_hash: String,
    pub byte_size: u64,
    pub mtime_ms: u64,
    /// ns精度mtime (FG-WP2-2世代トークン成分)。
    pub mtime_ns: u64,
    /// fd観測のOSファイルID (Unix: dev+ino)。
    pub os_file_id: Option<OsFileId>,
}

/// 不変世代トークン (FG-WP2-2): `(os_file_id, size, mtime_ns)`。
///
/// ファイル実体の同一性を表す。同パス再利用 (新inode) はトークン不一致で
/// Stale扱いになり、同一inodeの移動・改名は同一バイト (トークン一致) として
/// 正しくリンクされる。ms丸めによる同一ms内置換はns精度で識別される。
/// mtime_nsは同一環境内の比較にのみ使う (取得不能環境の0詰めフォールバック
/// と一貫するため)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenerationToken {
    pub os_file_id: Option<OsFileId>,
    pub byte_size: u64,
    pub mtime_ns: u64,
}

impl GenerationToken {
    /// 走査エントリの観測から構成する。
    pub fn from_scan_entry(entry: &ScanEntry) -> Self {
        Self {
            os_file_id: entry.os_file_id,
            byte_size: entry.byte_size,
            mtime_ns: entry.mtime_ns,
        }
    }

    /// DB行の現在トークンから構成する。`mtime_ns` がNULL (migration #4より
    /// 前の未再走査行) のレコードは実観測と決して一致しない値 (0) を返す —
    /// 次の走査でnsが書き込まれ自己修復する。
    pub fn from_record(record: &FileRecord) -> Self {
        Self {
            os_file_id: record.os_file_id,
            byte_size: record.byte_size,
            mtime_ns: record.mtime_ns.unwrap_or(0),
        }
    }

    /// DB行の現在世代とこのトークンが一致するか (リンク可否の照合)。
    /// NULL mtime_ns行 (未再走査) は一致しない。
    pub fn matches_record(&self, record: &FileRecord) -> bool {
        record.os_file_id == self.os_file_id
            && record.byte_size == self.byte_size
            && record.mtime_ns == Some(self.mtime_ns)
    }
}

impl FileContentHash {
    /// 検証済みfd観測のトークン成分。
    pub fn token(&self) -> GenerationToken {
        GenerationToken {
            os_file_id: self.os_file_id,
            byte_size: self.byte_size,
            mtime_ns: self.mtime_ns,
        }
    }
}

/// プローブで判別した画像形式 (WP-3)。実デコード基盤 (image crate) に
/// 依存しない値オブジェクト — gento-coreは画像ライブラリへ依存しない
/// (計画書§5) ため、対応表はgento-image側が持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbedFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
    Tiff,
    Avif,
}

impl ProbedFormat {
    /// 正式MIMEタイプ (プローブが正本。拡張子はフォールバック用)。
    pub const fn mime(self) -> &'static str {
        match self {
            ProbedFormat::Png => "image/png",
            ProbedFormat::Jpeg => "image/jpeg",
            ProbedFormat::Webp => "image/webp",
            ProbedFormat::Gif => "image/gif",
            ProbedFormat::Bmp => "image/bmp",
            ProbedFormat::Tiff => "image/tiff",
            ProbedFormat::Avif => "image/avif",
        }
    }
}

/// プローブ結果 (magic判別 + ヘッダ寸法 + アニメーション情報)。
/// 全項目が**同一fd**から観測される (ハッシュとの束ね契約は変えない)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbedImage {
    pub format: ProbedFormat,
    pub width: u32,
    pub height: u32,
    /// デコード時のカラータイプにalpha成分があるか (GIFは常にtrue扱い)。
    pub has_alpha: bool,
    /// GIF: frame_count > 1。WebP: ANIMチャンク存在。AVIF: avisブランド。
    pub is_animated: bool,
    /// GIF/WebPはフレーム数、AVIFは不明 (`None`)。
    pub frame_count: Option<u32>,
}

/// 1ファイルのハッシュ+プローブ結果。ハッシュとプローブは**同一の開いたfd**
/// から観測されており、別世代の混入は構造的に不可能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedFile {
    pub content: FileContentHash,
    /// 形式プローブ (非画像・破損・未対応magicは `None`)。
    pub probe: Option<ProbedImage>,
}

/// ファイルシステム操作の抽象 (実装: gento-image)。
pub trait ScanFs: Send + Sync {
    /// `root` (またはその配下 `scope`) を再帰走査し、画像拡張子のファイルを
    /// `sink` へ渡す。`rel_path` は常に `root` 基準。シンボリックリンクは
    /// 辿らない。個別エラーで走査を停止しない。
    fn walk(
        &self,
        root: &Path,
        scope: Option<&Path>,
        sink: &mut dyn FnMut(ScanEntry),
    ) -> WalkOutcome;

    /// 1つの開いたfdからハッシュとプローブを束ねて計算する (FG-WP2-2)。
    ///
    /// - fdのfstatトークンが `expect` (走査時観測) と一致しない場合は
    ///   計算前に [`HashFileError::Stale`] (無駄な読み取りをしない)。
    /// - BLAKE3計算後、**同一fd** から形式プローブ (magic+寸法+アニメーション
    ///   情報) を得る (パスの再オープン禁止 — 置換ファイルの寸法混入を
    ///   構造的に排除)。
    /// - 計算前後でfdのトークンが不変であることを検証し、変化していれば
    ///   [`HashFileError::Stale`]。
    fn hash_and_probe(
        &self,
        path: &Path,
        expect: &GenerationToken,
        cancel: &AtomicBool,
    ) -> Result<HashedFile, HashFileError>;
}

/// 4 repository traitの統合ビュー (dyn運用用)。WP-7でFileOpsRepoを追加。
pub trait AllRepos:
    crate::repo::LibraryRootRepo + FileRepo + crate::repo::AssetRepo + crate::file_ops::FileOpsRepo
{
}

impl<T> AllRepos for T where
    T: crate::repo::LibraryRootRepo
        + FileRepo
        + crate::repo::AssetRepo
        + crate::file_ops::FileOpsRepo
{
}

/// DBアクセス境界の抽象 (実装: gento-db)。
///
/// コールバック型APIはオブジェクト安全のため戻り値を運ばず、
/// 呼び出し側のクロージャで結果を捕捉する ([`read_vec`] 等のヘルパ参照)。
pub trait RepoStore: Send + Sync {
    /// 書き込み用接続 (単一writer。実装が直列化を担保)。
    fn write(
        &self,
        f: &mut dyn FnMut(&dyn AllRepos) -> Result<(), RepoError>,
    ) -> Result<(), RepoError>;
    /// 読み取り用接続 (WAL。writerをブロックしない)。
    fn read(
        &self,
        f: &mut dyn FnMut(&dyn AllRepos) -> Result<(), RepoError>,
    ) -> Result<(), RepoError>;
}

/// [`RepoStore::read`] で `Vec` を取り出すヘルパ。
pub fn read_vec<T>(
    store: &dyn RepoStore,
    f: impl FnOnce(&dyn AllRepos) -> Result<Vec<T>, RepoError>,
) -> Result<Vec<T>, RepoError> {
    read_one(store, f)
}

/// [`RepoStore::read`] で単一値を取り出すヘルパ。
fn read_one<T>(
    store: &dyn RepoStore,
    f: impl FnOnce(&dyn AllRepos) -> Result<T, RepoError>,
) -> Result<T, RepoError> {
    let mut slot: Option<Result<T, RepoError>> = None;
    let mut f = Some(f);
    let mut closure = |repos: &dyn AllRepos| -> Result<(), RepoError> {
        if let Some(f) = f.take() {
            slot = Some(f(repos));
        }
        Ok(())
    };
    store.read(&mut closure)?;
    slot.unwrap_or_else(|| Err(RepoError::Db("read callback did not run".to_string())))
}

/// [`RepoStore::write`] で単一値を取り出すヘルパ。`E: From<RepoError>` により
/// 接続自体の失敗も呼び出し側のエラー型へ変換される。
fn write_ret<T, E: From<RepoError>>(
    store: &dyn RepoStore,
    f: impl FnOnce(&dyn AllRepos) -> Result<T, E>,
) -> Result<T, E> {
    let mut slot: Option<Result<T, E>> = None;
    let mut f = Some(f);
    let mut closure = |repos: &dyn AllRepos| -> Result<(), RepoError> {
        if let Some(f) = f.take() {
            slot = Some(f(repos));
        }
        Ok(())
    };
    store.write(&mut closure)?;
    slot.unwrap_or_else(|| Err(RepoError::Db("write callback did not run".to_string()).into()))
}

/// スキャンの位相。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    /// ファイルシステムwalk + DB同期中。
    Scanning,
    /// バックグラウンドハッシュ計算中。
    Hashing,
}

impl ScanPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            ScanPhase::Scanning => "scanning",
            ScanPhase::Hashing => "hashing",
        }
    }
}

/// 進捗スナップショット (イベント送出用。単調増加カウンタ)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanProgress {
    pub phase: Option<ScanPhase>,
    pub root_id: i64,
    pub entries_walked: u64,
    /// 新規発見。
    pub found: u64,
    /// 変更検出 (upsert + assetリンク解除)。
    pub updated: u64,
    /// 外部移動・改名の再発見。
    pub relocated: u64,
    /// 不在化 (missing遷移)。
    pub missing: u64,
    /// ハッシュ計算完了。
    pub hashed: u64,
    /// ハッシュ待ち残数。
    pub pending_hash: u64,
    pub errors: u64,
}

/// スキャン1回の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    pub progress: ScanProgress,
    /// ハッシュ待ちファイル (呼び出し側がBackground優先度で処理する)。
    pub pending_hash: Vec<PendingHash>,
    pub cancelled: bool,
    pub error_samples: Vec<String>,
}

/// ハッシュ待ちファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingHash {
    pub file_id: i64,
    /// 絶対パス。
    pub path: PathBuf,
    /// 走査時観測の世代トークン (ハッシュジョブが携帯する)。
    pub token: GenerationToken,
}

/// スキャンオプション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    /// 1トランザクションのupsert件数上限。
    pub batch_size: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_SCAN_BATCH_SIZE,
        }
    }
}

/// 1ルートの差分スキャン (モジュールdocのアルゴリズム参照)。
///
/// `scope` はルート相対の部分ディレクトリ (監視イベントの差分適用用)。
/// `None` ならルート全体。不在化判定もscope内のrel_pathに限定する。
/// DB書き込み自体の失敗は [`RepoError`] で中断する (打ち切り時の整合性は
/// 「コミット済みバッチまで」で、再スキャンが冪等に続きを拾う)。
pub fn scan_root(
    store: &dyn RepoStore,
    fs: &dyn ScanFs,
    root: &LibraryRootRecord,
    scope: Option<&Path>,
    cancel: &AtomicBool,
    options: &ScanOptions,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanSummary, RepoError> {
    let mut ctx = ScanCtx::new(root, scope, options);
    ctx.load_existing(store)?;

    let outcome = fs.walk(&root.path, scope, &mut |entry| {
        if cancel.load(Ordering::Relaxed) || ctx.write_error.is_some() {
            return;
        }
        ctx.observe(entry);
        if ctx.pending.len() >= options.batch_size {
            // sinkはResultを返せないため、失敗はctx.write_errorへ退避し
            // walk完了後に伝播する (以降のエントリ処理も停止)。
            ctx.flush(store);
        }
    });
    ctx.entries_walked = outcome.entries_walked;
    ctx.walk_errors = outcome.errors;
    ctx.flush(store);
    if let Some(err) = ctx.write_error.take() {
        return Err(err);
    }

    // キャンセル時は不在化を確定しない (walk打ち切りの未参照をmissingに
    // しないため。再開時の再スキャンで収束する)。
    if !cancel.load(Ordering::Relaxed) {
        ctx.mark_missing(store, cancel)?;
    }
    ctx.collect_pending(store);

    let summary = ScanSummary {
        progress: ctx.progress(),
        pending_hash: std::mem::take(&mut ctx.pending_hash),
        cancelled: cancel.load(Ordering::Relaxed),
        error_samples: ctx
            .walk_errors
            .iter()
            .take(ERROR_SAMPLE_LIMIT)
            .map(|e| e.to_display())
            .collect(),
    };
    progress(summary.progress);
    Ok(summary)
}

/// 進捗イベントに載せるエラーサンプルの上限。
const ERROR_SAMPLE_LIMIT: usize = 5;

/// 書き込み保留操作。
enum PendingOp {
    Upsert(FileUpsert),
    /// 外部移動・改名: (既存file_id, 移動後の属性)。
    Relocate(i64, FileUpsert),
}

/// 1バッチのコミット結果。
struct BatchOutcome {
    /// upsert (新規 + relocationフォールバック) の結果行。
    records: Vec<FileRecord>,
    /// relocate成功行。
    relocated: Vec<FileRecord>,
    /// relocationがDuplicate/NotFoundでフォールバックした旧file_id
    /// (seenから解除して不在化対象へ戻す)。
    fallback_old_ids: Vec<i64>,
}

struct ScanCtx<'a> {
    root: &'a LibraryRootRecord,
    scope: Option<&'a Path>,
    batch_size: usize,
    /// rel_path → 既存レコード。
    existing: HashMap<PathBuf, FileRecord>,
    /// os_file_id → rel_path (移動検出用。走査前に存在したもの)。
    by_os_file_id: HashMap<OsFileId, PathBuf>,
    /// 本次走査で存在を確認したレコードid。
    seen_ids: std::collections::HashSet<i64>,
    pending: Vec<PendingOp>,
    pending_hash: Vec<PendingHash>,
    found: u64,
    updated: u64,
    relocated: u64,
    missing: u64,
    entries_walked: u64,
    walk_errors: Vec<WalkError>,
    write_error: Option<RepoError>,
}

impl<'a> ScanCtx<'a> {
    fn new(root: &'a LibraryRootRecord, scope: Option<&'a Path>, options: &ScanOptions) -> Self {
        Self {
            root,
            scope,
            batch_size: options.batch_size.max(1),
            existing: HashMap::new(),
            by_os_file_id: HashMap::new(),
            seen_ids: std::collections::HashSet::new(),
            pending: Vec::new(),
            pending_hash: Vec::new(),
            found: 0,
            updated: 0,
            relocated: 0,
            missing: 0,
            entries_walked: 0,
            walk_errors: Vec::new(),
            write_error: None,
        }
    }

    fn load_existing(&mut self, store: &dyn RepoStore) -> Result<(), RepoError> {
        let root_id = self.root.id;
        let mut offset = 0u32;
        loop {
            let (page, total) = read_one(store, move |repos| {
                repos.list_files_paged(root_id, offset, LIST_PAGE_SIZE)
            })?;
            let page_len = page.len();
            for record in page {
                if let Some(id) = record.os_file_id {
                    self.by_os_file_id.insert(id, record.rel_path.clone());
                }
                self.existing.insert(record.rel_path.clone(), record);
            }
            offset += LIST_PAGE_SIZE;
            if offset as u64 >= total || page_len < LIST_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(())
    }

    fn observe(&mut self, entry: ScanEntry) {
        self.entries_walked += 1;
        let display_name = display_name_of(&entry.rel_path);
        if let Some(record) = self.existing.get(&entry.rel_path).cloned() {
            self.seen_ids.insert(record.id);
            // 世代トークン全体 (size, mtime_ms, mtime_ns, os_file_id) のいずれかが
            // 変わったら内容変化の可能性として扱う。ns差のみの置換 (同一ms内の
            // 差し替え) も検出するため、mtime_ns比較を省かない。os_file_id差は
            // 実体が別ファイル (inode差し替え) のため再ハッシュする。
            let changed = record.byte_size != entry.byte_size
                || record.mtime_ms != entry.mtime_ms
                || record.mtime_ns != Some(entry.mtime_ns)
                || record.os_file_id != entry.os_file_id;
            let reappeared = record.scan_state == ScanState::Missing;
            if changed || reappeared {
                // 変化時のみassetリンク解除。missing再出現でトークン同一なら
                // 内容不変とみなしリンクを保持する。
                let detach = changed && record.asset_id.is_some();
                if changed {
                    self.updated += 1;
                }
                self.pending.push(PendingOp::Upsert(FileUpsert {
                    root_id: self.root.id,
                    rel_path: entry.rel_path,
                    display_name,
                    byte_size: entry.byte_size,
                    mtime_ms: entry.mtime_ms,
                    mtime_ns: entry.mtime_ns,
                    scan_state: ScanState::Present,
                    os_file_id: entry.os_file_id,
                    detach_asset: detach,
                }));
            }
            return;
        }

        // 未知パス: os_file_idによる移動・改名検出 (同一ルート内)。
        if let Some(id) = entry.os_file_id {
            if let Some(old_rel) = self.by_os_file_id.get(&id).cloned() {
                if let Some(old) = self.existing.get(&old_rel).cloned() {
                    if !self.seen_ids.contains(&old.id) {
                        // inode一致だけでなく世代トークン全体 (size+mtime_ns含む)
                        // が一致する場合のみ移動と判定する (ハードリンク・inode
                        // 再利用・移動+内容変化からuuid/assetリンクを保護。
                        // missing行の無期限保持下でのinode再利用対策を兼ねる)。
                        // 不一致は新規扱いへ落下し、旧パス行はmark_missingが不在化する。
                        let content_intact = old.byte_size == entry.byte_size
                            && old.mtime_ns == Some(entry.mtime_ns)
                            && old.os_file_id == entry.os_file_id;
                        if content_intact {
                            self.seen_ids.insert(old.id);
                            self.relocated += 1;
                            // 移動は内容不変のためassetリンク保持 (detachしない)。
                            self.pending.push(PendingOp::Relocate(
                                old.id,
                                FileUpsert {
                                    root_id: self.root.id,
                                    rel_path: entry.rel_path,
                                    display_name,
                                    byte_size: entry.byte_size,
                                    mtime_ms: entry.mtime_ms,
                                    mtime_ns: entry.mtime_ns,
                                    scan_state: ScanState::Present,
                                    os_file_id: entry.os_file_id,
                                    detach_asset: false,
                                },
                            ));
                            return;
                        }
                    }
                }
            }
        }

        self.found += 1;
        self.pending.push(PendingOp::Upsert(FileUpsert {
            root_id: self.root.id,
            rel_path: entry.rel_path,
            display_name,
            byte_size: entry.byte_size,
            mtime_ms: entry.mtime_ms,
            mtime_ns: entry.mtime_ns,
            scan_state: ScanState::Present,
            os_file_id: entry.os_file_id,
            detach_asset: false,
        }));
    }

    /// 蓄積した操作を小バッチでコミットする (1バッチ=1トランザクション)。
    /// 失敗は `write_error` へ退避し、以降の処理を停止する。
    ///
    /// relocationの失敗は種別ごとに扱う: `Duplicate`/`NotFound` は通常
    /// upsertへフォールバック (旧パス行のseenを解除しmark_missingが不在化)。
    /// DB・制約エラーはバッチ全体を失敗させスキャンエラーとして伝播する
    /// (黙って捨てない)。
    fn flush(&mut self, store: &dyn RepoStore) {
        if self.write_error.is_some() {
            return;
        }
        while !self.pending.is_empty() {
            let take = self.batch_size.min(self.pending.len());
            let chunk: Vec<PendingOp> = self.pending.drain(..take).collect();
            let root_path = self.root.path.clone();
            let result = write_ret(store, move |repos| {
                let mut plain: Vec<FileUpsert> = Vec::with_capacity(chunk.len());
                let mut relocated: Vec<FileRecord> = Vec::new();
                let mut fallback_old_ids: Vec<i64> = Vec::new();
                for op in &chunk {
                    match op {
                        PendingOp::Upsert(upsert) => plain.push(upsert.clone()),
                        PendingOp::Relocate(file_id, upsert) => {
                            match repos.relocate_file(*file_id, upsert.clone()) {
                                Ok(record) => relocated.push(record),
                                // Duplicate: 移動先rel_pathに既存行 (load後の並行
                                // 書き込み)。NotFound: 旧行が既に消えた。いずれも
                                // 新パスを通常upsertで確定させる。
                                Err(RepoError::Duplicate | RepoError::NotFound) => {
                                    fallback_old_ids.push(*file_id);
                                    plain.push(upsert.clone());
                                }
                                Err(err) => return Err(err),
                            }
                        }
                    }
                }
                let records = if plain.is_empty() {
                    Vec::new()
                } else {
                    repos.upsert_files_batch(&plain)?
                };
                Ok(BatchOutcome {
                    records,
                    relocated,
                    fallback_old_ids,
                })
            });
            match result {
                Ok(outcome) => {
                    // asset未リンクの結果行 (upsert・relocate両方) はハッシュ待ちへ。
                    // トークンは結果行の現在観測から構成する。
                    for record in outcome.records.iter().chain(outcome.relocated.iter()) {
                        if record.asset_id.is_none() {
                            self.pending_hash.push(PendingHash {
                                file_id: record.id,
                                path: root_path.join(&record.rel_path),
                                token: GenerationToken::from_record(record),
                            });
                        }
                    }
                    // フォールバックした旧idはseenから外し、不在化対象へ戻す。
                    for id in outcome.fallback_old_ids {
                        self.seen_ids.remove(&id);
                    }
                }
                Err(err) => {
                    self.write_error = Some(err);
                    return;
                }
            }
        }
    }

    /// 本次走査で未参照の既存Fileをmissing化 (scope内のみ。削除しない)。
    /// 列挙失敗 (`blocks_missing`) パス配下のレコードは現状維持とする —
    /// permission-denied等で「見えなかった」ことを「不在」と混同しない。
    fn mark_missing(
        &mut self,
        store: &dyn RepoStore,
        cancel: &AtomicBool,
    ) -> Result<(), RepoError> {
        let scope = self.scope;
        let blocked: Vec<&WalkError> = self
            .walk_errors
            .iter()
            .filter(|e| e.blocks_missing)
            .collect();
        let ids: Vec<i64> = self
            .existing
            .values()
            .filter(|record| {
                !self.seen_ids.contains(&record.id)
                    && record.scan_state != ScanState::Missing
                    && scope
                        .map(|s| record.rel_path.starts_with(s))
                        .unwrap_or(true)
                    && !blocked.iter().any(|e| {
                        e.rel_path.as_os_str().is_empty()
                            || record.rel_path.starts_with(&e.rel_path)
                    })
            })
            .map(|record| record.id)
            .collect();
        for chunk in ids.chunks(self.batch_size) {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let changed = write_ret(store, |repos| {
                repos.set_scan_state_batch(chunk, ScanState::Missing)
            })?;
            self.missing += changed as u64;
        }
        Ok(())
    }

    /// ハッシュ待ち追加: 従来から未リンクで今回確認済みのFile
    /// (初回スキャン直後の残留・ハッシュ中止残の再開)。
    fn collect_pending(&mut self, store: &dyn RepoStore) {
        let root_id = self.root.id;
        let root_path = self.root.path.clone();
        let Ok(unlinked) = read_vec(store, |repos| repos.list_files_without_asset(root_id)) else {
            return;
        };
        for record in unlinked {
            if self.seen_ids.contains(&record.id)
                && record.scan_state == ScanState::Present
                && !self.pending_hash.iter().any(|p| p.file_id == record.id)
            {
                self.pending_hash.push(PendingHash {
                    file_id: record.id,
                    path: root_path.join(&record.rel_path),
                    token: GenerationToken::from_record(&record),
                });
            }
        }
    }

    fn progress(&self) -> ScanProgress {
        ScanProgress {
            phase: Some(ScanPhase::Scanning),
            root_id: self.root.id,
            entries_walked: self.entries_walked,
            found: self.found,
            updated: self.updated,
            relocated: self.relocated,
            missing: self.missing,
            hashed: 0,
            pending_hash: self.pending_hash.len() as u64,
            errors: self.walk_errors.len() as u64,
        }
    }
}

fn display_name_of(rel_path: &Path) -> String {
    rel_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel_path.to_string_lossy().into_owned())
}

/// リンク失敗の内訳 (DB失敗と世代不一致を呼び出し側で区別する)。
#[derive(Debug)]
enum LinkError {
    Repo(RepoError),
    /// ハッシュ結果がDBレコードの直近観測世代と一致しない (結果破棄・要再走査)。
    Stale,
}

impl From<RepoError> for LinkError {
    fn from(err: RepoError) -> Self {
        LinkError::Repo(err)
    }
}

/// 1ファイルのハッシュ計算 + Asset統合 (新規なら作成) + リンク。
/// ハッシュワーカー (Background優先度) から呼ばれる。
///
/// 世代検証 (FG-WP2-2): [`ScanFs::hash_and_probe`] は走査時観測トークン
/// `expect` と開いたfdのfstatトークンが一致する場合のみ計算を進め、
/// ハッシュと寸法を同一fdから束ねて返す (計算前後のfdトークン不変も検証)。
/// リンクは書き込みトランザクション内でDBレコードの現在トークンとfdトークン
/// を突合し、一致する場合のみ実行する。これにより「同パス再利用 (新inode)」
/// 「同一ms内の置換 (ns差)」「計算中の変更」「新しいスキャン後到着した古い
/// ジョブ」はいずれも誤リンクされず [`HashFileError::Stale`] として破棄され、
/// 再提出 (呼び出し側の決定論リトライ) または次回スキャンで再ハッシュされる。
/// 同一inodeの移動・改名は同一バイトのためトークン一致で正しくリンクされる。
pub fn hash_and_link_file(
    store: &dyn RepoStore,
    fs: &dyn ScanFs,
    file_id: i64,
    path: &Path,
    expect: &GenerationToken,
    cancel: &AtomicBool,
) -> Result<AssetRecord, HashFileError> {
    let hashed = fs.hash_and_probe(path, expect, cancel)?;
    let content = hashed.content;
    let probe = hashed.probe;
    // プローブの形式が正本。非プローブ時のみ拡張子から推測する。
    let mime = probe
        .map(|p| p.format.mime().to_string())
        .or_else(|| mime_of(path));
    let link = write_ret(store, |repos| {
        let Some(record) = repos.get_file(file_id)? else {
            return Err(LinkError::Repo(RepoError::NotFound));
        };
        if !content.token().matches_record(&record) {
            return Err(LinkError::Stale);
        }
        let (asset, _created) = repos.create_or_get_asset(NewAsset {
            content_hash: content.content_hash.clone(),
            hash_algorithm: "blake3".to_string(),
            byte_size: content.byte_size,
            mime_type: mime.clone(),
            width: probe.map(|p| p.width),
            height: probe.map(|p| p.height),
            has_alpha: probe.map(|p| p.has_alpha),
            is_animated: probe.map(|p| p.is_animated),
            frame_count: probe.and_then(|p| p.frame_count),
        })?;
        let asset_id = asset.id;
        repos.link_file(file_id, asset_id)?;
        Ok(asset)
    });
    match link {
        Ok(asset) => Ok(asset),
        Err(LinkError::Stale) => Err(HashFileError::Stale(format!(
            "{}: file generation changed; result discarded",
            path.display()
        ))),
        Err(LinkError::Repo(err)) => Err(HashFileError::Io(format!("link failed: {err}"))),
    }
}

/// 拡張子からMIME typeへの簡易マップ (WP-2では表示用のみ。WP-3で形式
/// adapterが正本化する)。
fn mime_of(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        _ => return None,
    };
    Some(mime.to_string())
}

// ===== ライブラリカタログのAssetId符号化 (api-contract.md §17) =====

/// ハッシュ未完了ファイルの仮IDフラグ (最上位bit)。
/// SQLiteのINTEGER PRIMARY KEYは実質i64範囲のため、bit63が立ったIDは
/// assets.id と衝突しない。
pub const PROVISIONAL_FILE_FLAG: u64 = 1 << 63;

/// Asset確定済みファイルの安定AssetId (= assets.id。restart後も不変)。
pub fn library_asset_id(asset_row_id: i64) -> AssetId {
    AssetId::new(asset_row_id as u64)
}

/// ハッシュ未完了ファイルの仮AssetId (= bit63 | files.id)。
/// ハッシュ完了でAssetへリンクされると安定IDへ切り替わる (フロントは
/// catalog:changed のページ再取得で追従する)。
pub fn provisional_file_asset_id(file_row_id: i64) -> AssetId {
    AssetId::new(PROVISIONAL_FILE_FLAG | file_row_id as u64)
}

/// ライブラリAssetIdの復号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryAssetRef {
    Asset(i64),
    ProvisionalFile(i64),
}

pub fn decode_library_asset_id(id: AssetId) -> LibraryAssetRef {
    let value = id.value();
    if value & PROVISIONAL_FILE_FLAG != 0 {
        LibraryAssetRef::ProvisionalFile((value & !PROVISIONAL_FILE_FLAG) as i64)
    } else {
        LibraryAssetRef::Asset(value as i64)
    }
}

// ===== 監視イベントの結合 (debounceウィンドウ内の合流) =====

/// 結合済みファイル変化種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsChange {
    Created,
    Modified,
    Removed,
}

impl FsChange {
    /// 直前の状態と新しい観測を合并する (最終状態がわかる方を残す)。
    fn merge(previous: FsChange, next: FsChange) -> FsChange {
        use FsChange::*;
        match (previous, next) {
            // 削除後に再出現 → 内容は変わっている可能性がある
            (Removed, Created) | (Removed, Modified) => Modified,
            // 出現・変更後に削除 → 消えた
            (_, Removed) => Removed,
            // それ以外は新しい観測で上書き
            (_, next) => next,
        }
    }
}

/// 監視イベントの結合バッファ (WP-2)。
///
/// debounceウィンドウ内のイベントをパス単位で合流させ、最終状態のみを
/// 扱う。イベント欠落 (inotify overflow等) は [`WatchBuffer::mark_overflow`]
/// で記録し、呼び出し側は全体再スキャンで収束させる。
///
/// WP-7: アプリ内ファイル操作由来のイベントを抑制するキー照合を持つ
/// (二重反映防止)。操作実行前に [`WatchBuffer::suppress`] で対象パスを
/// 登録すると、そのパスのイベントは [`WatchBuffer::push`] で破棄される。
/// **抑制中に破棄したイベントのパスは記録され**、操作完了後の
/// 和解スキャン ([`WatchBuffer::take_suppressed_dropped`]) で外部変更を
/// 見逃さない (過剰抑制で外部変更が失われる問題への対応。走査の冪等性
/// により抑制漏れは無害だが、抑制中の外部変更は明示的な差分スキャンで
/// 収束させる)。
pub struct WatchBuffer {
    changes: Mutex<HashMap<PathBuf, FsChange>>,
    overflow: AtomicBool,
    count: AtomicU64,
    /// アプリ内操作中のパス集合 (抑制キー)。
    suppressed: Mutex<std::collections::HashSet<PathBuf>>,
    /// 抑制中に破棄したイベントのパス (和解スキャン用)。
    suppressed_dropped: Mutex<Vec<PathBuf>>,
}

impl Default for WatchBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchBuffer {
    pub fn new() -> Self {
        Self {
            changes: Mutex::new(HashMap::new()),
            overflow: AtomicBool::new(false),
            count: AtomicU64::new(0),
            suppressed: Mutex::new(std::collections::HashSet::new()),
            suppressed_dropped: Mutex::new(Vec::new()),
        }
    }

    /// 生イベントを1件投入。抑制キー ([`WatchBuffer::suppress`]) に一致する
    /// パスのイベントは破棄する (アプリ内操作由来 — 二重反映防止) が、
    /// パスを [`WatchBuffer::take_suppressed_dropped`] で取り出せるよう
    /// 記録する (操作完了後のscoped差分スキャンで外部変更を見逃さない)。
    pub fn push(&self, change: FsChange, path: PathBuf) {
        if self.is_suppressed(&path) {
            if let Ok(mut dropped) = self.suppressed_dropped.lock() {
                if !dropped.contains(&path) {
                    dropped.push(path);
                }
            }
            return;
        }
        let mut guard = self.changes.lock().unwrap_or_else(|p| p.into_inner());
        let next = match guard.get(&path) {
            Some(previous) => FsChange::merge(*previous, change),
            None => change,
        };
        guard.insert(path, next);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// アプリ内ファイル操作の対象パスを抑制キーへ登録 (操作開始前)。
    pub fn suppress(&self, paths: &[PathBuf]) {
        let mut guard = self.suppressed.lock().unwrap_or_else(|p| p.into_inner());
        guard.extend(paths.iter().cloned());
    }

    /// 抑制キーを解除 (操作のDB反映完了後)。漏れたイベントは走査の冪等性
    /// で最終状態へ収束するため安全)。
    pub fn unsuppress(&self, paths: &[PathBuf]) {
        let mut guard = self.suppressed.lock().unwrap_or_else(|p| p.into_inner());
        for path in paths {
            guard.remove(path);
        }
    }

    /// パスが抑制キーに一致するか。
    pub fn is_suppressed(&self, path: &Path) -> bool {
        self.suppressed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(path)
    }

    /// 抑制中に破棄したイベントパスを取り出す (クリアする)。
    /// 操作完了後の和解スキャン (影響ディレクトリのscoped差分) で使用する。
    pub fn take_suppressed_dropped(&self) -> Vec<PathBuf> {
        std::mem::take(
            &mut *self
                .suppressed_dropped
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        )
    }

    /// イベント欠落を記録 (全体再スキャンが必要)。
    pub fn mark_overflow(&self) {
        self.overflow.store(true, Ordering::SeqCst);
    }

    pub fn is_overflow(&self) -> bool {
        self.overflow.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.changes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    /// 受領した生イベントの累積数 (テスト・診断用)。
    pub fn observed_events(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// 結合結果を取り出す (バッファはクリア。overflowフラグはリセット)。
    pub fn drain(&self) -> CoalescedChanges {
        let mut guard = self.changes.lock().unwrap_or_else(|p| p.into_inner());
        CoalescedChanges {
            changes: std::mem::take(&mut *guard),
            overflow: self.overflow.swap(false, Ordering::SeqCst),
        }
    }
}

/// `drain` の結果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CoalescedChanges {
    /// パス → 最終状態。
    pub changes: HashMap<PathBuf, FsChange>,
    /// true ならイベント欠落があった (全体再スキャンで収束)。
    pub overflow: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{ProbedFormat, ProbedImage};
    use crate::repo::{
        AssetRepo, HashState, LibraryRootRecord, LibraryRootRepo, RepoError, RootSettings,
        TriageState,
    };
    use std::sync::{Arc, Mutex as StdMutex};

    // ===== フェイク実装 (coreのアルゴリズム検証。rusqlite不要) =====

    #[derive(Default)]
    struct FakeRepos {
        next_id: StdMutex<i64>,
        roots: StdMutex<Vec<LibraryRootRecord>>,
        files: StdMutex<Vec<FileRecord>>,
        assets: StdMutex<Vec<AssetRecord>>,
        operations: StdMutex<Vec<crate::file_ops::FileOpRecord>>,
        write_calls: StdMutex<Vec<&'static str>>,
        /// テスト注入: relocate_fileをこのエラーで失敗させる。
        relocate_failure: StdMutex<Option<RepoError>>,
    }

    impl FakeRepos {
        fn add_test_root(&self, path: &str) -> LibraryRootRecord {
            let id = *self.next_id.lock().unwrap() + 1;
            *self.next_id.lock().unwrap() += 1;
            let record = LibraryRootRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                path: PathBuf::from(path),
                display_path: path.to_string(),
                enabled: true,
                watch_enabled: true,
                ai_enabled: false,
                send_exif: false,
                send_gps: false,
                created_at: "2026-08-30T00:00:00.000Z".to_string(),
                updated_at: "2026-08-30T00:00:00.000Z".to_string(),
            };
            self.roots.lock().unwrap().push(record.clone());
            record
        }

        fn files_cloned(&self) -> Vec<FileRecord> {
            self.files.lock().unwrap().clone()
        }
    }

    impl LibraryRootRepo for FakeRepos {
        fn add_root(&self, path: &Path) -> Result<LibraryRootRecord, RepoError> {
            self.add_test_root(&path.to_string_lossy());
            Ok(self.roots.lock().unwrap().last().unwrap().clone())
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
        fn set_root_settings(&self, _id: i64, _settings: RootSettings) -> Result<(), RepoError> {
            Ok(())
        }
    }

    impl FileRepo for FakeRepos {
        fn upsert_file(&self, file: FileUpsert) -> Result<FileRecord, RepoError> {
            self.write_calls.lock().unwrap().push("upsert_file");
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
                if file.detach_asset {
                    existing.asset_id = None;
                }
                return Ok(existing.clone());
            }
            let id = *self.next_id.lock().unwrap() + 1;
            *self.next_id.lock().unwrap() += 1;
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
                last_seen_at: "2026-08-30T01:00:00.000Z".to_string(),
                created_at: "2026-08-30T00:00:00.000Z".to_string(),
                updated_at: "2026-08-30T01:00:00.000Z".to_string(),
            };
            files.push(record.clone());
            Ok(record)
        }
        fn set_scan_state(&self, file_id: i64, state: ScanState) -> Result<(), RepoError> {
            self.write_calls.lock().unwrap().push("set_scan_state");
            let mut files = self.files.lock().unwrap();
            let record = files
                .iter_mut()
                .find(|f| f.id == file_id)
                .ok_or(RepoError::NotFound)?;
            record.scan_state = state;
            Ok(())
        }
        fn list_missing(&self, _root_id: i64) -> Result<Vec<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.scan_state == ScanState::Missing)
                .cloned()
                .collect())
        }
        fn find_by_path(
            &self,
            root_id: i64,
            rel_path: &Path,
        ) -> Result<Option<FileRecord>, RepoError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.root_id == root_id && f.rel_path == rel_path)
                .cloned())
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
            self.write_calls.lock().unwrap().push("upsert_files_batch");
            let mut out = Vec::with_capacity(files.len());
            for file in files {
                out.push(FileRepo::upsert_file(self, file.clone())?);
            }
            Ok(out)
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
            self.write_calls.lock().unwrap().push("relocate_file");
            if let Some(err) = self.relocate_failure.lock().unwrap().take() {
                return Err(err);
            }
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
            self.write_calls
                .lock()
                .unwrap()
                .push("set_scan_state_batch");
            let mut changed = 0;
            for id in file_ids {
                let mut files = self.files.lock().unwrap();
                // 並行削除済みの行はスキップ (SQLiteのUPDATEと同じく無視)。
                if let Some(record) = files.iter_mut().find(|f| f.id == *id) {
                    if record.scan_state != state {
                        record.scan_state = state;
                        changed += 1;
                    }
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

    impl crate::file_ops::FileOpsRepo for FakeRepos {
        fn insert_operation(
            &self,
            op: crate::file_ops::NewFileOp,
        ) -> Result<crate::file_ops::FileOpRecord, RepoError> {
            let id = *self.next_id.lock().unwrap() + 1;
            *self.next_id.lock().unwrap() += 1;
            let record = crate::file_ops::FileOpRecord {
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
            status: crate::file_ops::FileOpStatus,
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
            record.status = crate::file_ops::FileOpStatus::Undone;
            record.undoable = false;
            record.undone_at = Some("2026-08-31T01:00:00.000Z".to_string());
            Ok(())
        }

        fn get_operation(
            &self,
            id: i64,
        ) -> Result<Option<crate::file_ops::FileOpRecord>, RepoError> {
            Ok(self
                .operations
                .lock()
                .unwrap()
                .iter()
                .find(|o| o.id == id)
                .cloned())
        }

        fn list_recent_operations(
            &self,
            limit: u32,
        ) -> Result<Vec<crate::file_ops::FileOpRecord>, RepoError> {
            let mut ops = self.operations.lock().unwrap().clone();
            ops.sort_by_key(|o| std::cmp::Reverse(o.id));
            ops.truncate(limit as usize);
            Ok(ops)
        }

        fn list_executing_operations(
            &self,
        ) -> Result<Vec<crate::file_ops::FileOpRecord>, RepoError> {
            Ok(self
                .operations
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.status == crate::file_ops::FileOpStatus::Executing)
                .cloned()
                .collect())
        }
    }

    impl AssetRepo for FakeRepos {
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
            let id = *self.next_id.lock().unwrap() + 1;
            *self.next_id.lock().unwrap() += 1;
            let record = AssetRecord {
                id,
                uuid: format!("00000000-0000-7000-8000-{id:012}"),
                content_hash: asset.content_hash,
                hash_algorithm: asset.hash_algorithm,
                hash_state: HashState::Done,
                byte_size: asset.byte_size,
                mime_type: asset.mime_type,
                width: asset.width,
                height: asset.height,
                has_alpha: asset.has_alpha,
                is_animated: asset.is_animated,
                frame_count: asset.frame_count,
                rating: 0,
                triage: TriageState::Unselected,
                favorite: false,
                created_at: "2026-08-30T00:00:00.000Z".to_string(),
                updated_at: "2026-08-30T00:00:00.000Z".to_string(),
            };
            self.assets.lock().unwrap().push(record.clone());
            Ok(record)
        }
        fn find_by_hash(&self, hash: &str) -> Result<Option<AssetRecord>, RepoError> {
            Ok(self
                .assets
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.content_hash == hash)
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
        fn set_rating(&self, asset_id: i64, rating: u8) -> Result<(), RepoError> {
            if rating > 5 {
                return Err(RepoError::Constraint("rating must be 0..=5".to_string()));
            }
            let mut assets = self.assets.lock().unwrap();
            let record = assets
                .iter_mut()
                .find(|a| a.id == asset_id)
                .ok_or(RepoError::NotFound)?;
            record.rating = rating;
            Ok(())
        }
        fn set_cull_state(&self, asset_id: i64, triage: TriageState) -> Result<(), RepoError> {
            let mut assets = self.assets.lock().unwrap();
            let record = assets
                .iter_mut()
                .find(|a| a.id == asset_id)
                .ok_or(RepoError::NotFound)?;
            record.triage = triage;
            Ok(())
        }
        fn set_favorite(&self, asset_id: i64, favorite: bool) -> Result<(), RepoError> {
            let mut assets = self.assets.lock().unwrap();
            let record = assets
                .iter_mut()
                .find(|a| a.id == asset_id)
                .ok_or(RepoError::NotFound)?;
            record.favorite = favorite;
            Ok(())
        }
        fn set_rating_batch(&self, asset_ids: &[i64], rating: u8) -> Result<(), RepoError> {
            for id in asset_ids {
                self.set_rating(*id, rating)?;
            }
            Ok(())
        }
        fn set_cull_state_batch(
            &self,
            asset_ids: &[i64],
            triage: TriageState,
        ) -> Result<(), RepoError> {
            for id in asset_ids {
                self.set_cull_state(*id, triage)?;
            }
            Ok(())
        }
        fn set_favorite_batch(&self, asset_ids: &[i64], favorite: bool) -> Result<(), RepoError> {
            for id in asset_ids {
                self.set_favorite(*id, favorite)?;
            }
            Ok(())
        }
    }

    struct FakeStore {
        repos: Arc<FakeRepos>,
        fail_writes: AtomicBool,
    }

    impl RepoStore for FakeStore {
        fn write(
            &self,
            f: &mut dyn FnMut(&dyn AllRepos) -> Result<(), RepoError>,
        ) -> Result<(), RepoError> {
            if self.fail_writes.load(Ordering::Relaxed) {
                return Err(RepoError::Db("injected".to_string()));
            }
            f(self.repos.as_ref())
        }
        fn read(
            &self,
            f: &mut dyn FnMut(&dyn AllRepos) -> Result<(), RepoError>,
        ) -> Result<(), RepoError> {
            f(self.repos.as_ref())
        }
    }

    /// フェイクfs: 事前に登録したエントリ群を返す。ハッシュ+プローブ結果・
    /// 走査エラーも設定可能 (世代検証の敵対的テスト用)。
    /// `hash_and_probe` は実装の契約を模倣する: 設定済みハッシュ結果の
    /// トークンが `expect` と一致しなければStale、一致すればハッシュと寸法を
    /// 1回の呼び出しで束ねて返す。
    #[derive(Default)]
    struct FakeFs {
        entries: StdMutex<Vec<ScanEntry>>,
        walk_errors: StdMutex<Vec<WalkError>>,
        /// hash_and_probeの戻す検証済み観測 (トークン成分含む)。
        hash: StdMutex<Option<FileContentHash>>,
        probe: StdMutex<Option<ProbedImage>>,
        /// hash_and_probeの呼び出し回数 (fd単一APIの検証用)。
        hash_calls: std::sync::atomic::AtomicU64,
    }

    impl FakeFs {
        /// 指定トークンに合うハッシュ結果を設定する (mtime_msはnsからの
        /// 換算値を自動設定)。
        fn hash_matches(&self, os: Option<(u64, u64)>, size: u64, mtime_ns: u64, hash: &str) {
            *self.hash.lock().unwrap() = Some(FileContentHash {
                content_hash: hash.to_string(),
                byte_size: size,
                mtime_ms: mtime_ns / 1_000_000,
                mtime_ns,
                os_file_id: os.map(|(dev, ino)| OsFileId::new(dev, ino)),
            });
        }
    }

    impl ScanFs for FakeFs {
        fn walk(
            &self,
            _root: &Path,
            scope: Option<&Path>,
            sink: &mut dyn FnMut(ScanEntry),
        ) -> WalkOutcome {
            for entry in self.entries.lock().unwrap().iter() {
                if scope.is_some_and(|s| !entry.rel_path.starts_with(s)) {
                    continue;
                }
                sink(entry.clone());
            }
            WalkOutcome {
                entries_walked: self.entries.lock().unwrap().len() as u64,
                errors: self.walk_errors.lock().unwrap().clone(),
            }
        }
        fn hash_and_probe(
            &self,
            path: &Path,
            expect: &GenerationToken,
            _cancel: &AtomicBool,
        ) -> Result<HashedFile, HashFileError> {
            use std::sync::atomic::Ordering::Relaxed;
            self.hash_calls.fetch_add(1, Relaxed);
            let content = self
                .hash
                .lock()
                .unwrap()
                .clone()
                .ok_or(HashFileError::Io("hash not configured".to_string()))?;
            if content.token() != *expect {
                return Err(HashFileError::Stale(format!(
                    "{}: fd token {:?} != expect {:?}",
                    path.display(),
                    content.token(),
                    expect
                )));
            }
            Ok(HashedFile {
                content,
                probe: *self.probe.lock().unwrap(),
            })
        }
    }

    /// entry(rel, size, mtime_ms, os): nsはmsからの換算 (同一ms内差分は
    /// entry_nsで明示指定する)。
    fn entry(rel: &str, size: u64, mtime: u64, os: Option<(u64, u64)>) -> ScanEntry {
        entry_ns(rel, size, mtime, mtime * 1_000_000, os)
    }

    fn entry_ns(
        rel: &str,
        size: u64,
        mtime: u64,
        mtime_ns: u64,
        os: Option<(u64, u64)>,
    ) -> ScanEntry {
        ScanEntry {
            rel_path: PathBuf::from(rel),
            byte_size: size,
            mtime_ms: mtime,
            mtime_ns,
            os_file_id: os.map(|(dev, ino)| OsFileId::new(dev, ino)),
        }
    }

    fn setup() -> (
        Arc<FakeRepos>,
        Arc<FakeStore>,
        Arc<FakeFs>,
        LibraryRootRecord,
    ) {
        let repos = Arc::new(FakeRepos::default());
        let root = repos.add_test_root("/data/photos");
        let store = Arc::new(FakeStore {
            repos: Arc::clone(&repos),
            fail_writes: AtomicBool::new(false),
        });
        let fs = Arc::new(FakeFs::default());
        (repos, store, fs, root)
    }

    fn run_scan(store: &FakeStore, fs: &FakeFs, root: &LibraryRootRecord) -> ScanSummary {
        let cancel = AtomicBool::new(false);
        scan_root(
            store,
            fs,
            root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        )
        .unwrap()
    }

    #[test]
    fn first_scan_registers_files_with_os_file_id() {
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().extend([
            entry("a.png", 10, 1, Some((1, 100))),
            entry("sub/b.png", 20, 2, Some((1, 101))),
        ]);
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.found, 2);
        assert!(!summary.cancelled);
        let files = repos.files_cloned();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].os_file_id, Some(OsFileId::new(1, 100)));
        assert_eq!(files[0].scan_state, ScanState::Present);
        // 両方ともasset未リンク → ハッシュ待ち
        assert_eq!(summary.pending_hash.len(), 2);
    }

    #[test]
    fn unchanged_second_scan_is_write_free_fast_path() {
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("a.png", 10, 1, Some((1, 100))));
        run_scan(&store, &fs, &root);
        repos.write_calls.lock().unwrap().clear();

        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.found, 0);
        assert_eq!(summary.progress.updated, 0);
        assert_eq!(summary.progress.missing, 0);
        assert!(
            repos.write_calls.lock().unwrap().is_empty(),
            "変化なしは書き込みゼロ (高速パス): {:?}",
            repos.write_calls.lock().unwrap()
        );
    }

    #[test]
    fn changed_file_upserts_and_detaches_asset() {
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().push(entry("a.png", 10, 1, None));
        run_scan(&store, &fs, &root);
        // ハッシュ→リンクを模擬
        let asset = AssetRepo::create_asset(
            repos.as_ref(),
            NewAsset {
                content_hash: "blake3:old".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            },
        )
        .unwrap();
        AssetRepo::link_file(repos.as_ref(), repos.files_cloned()[0].id, asset.id).unwrap();

        // 事前に「新しい内容」と同一ハッシュのAssetが存在する状況を作る
        // (他ファイル由来と想定。FakeFsは blake3:cafe を返す)。
        let cafe = AssetRepo::create_asset(
            repos.as_ref(),
            NewAsset {
                content_hash: "blake3:cafe".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 99,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            },
        )
        .unwrap();

        // 内容変化 (size+mtime)
        fs.entries.lock().unwrap()[0] = entry("a.png", 99, 2, None);
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.updated, 1);
        let record = repos.files_cloned()[0].clone();
        assert_eq!(record.byte_size, 99);
        assert_eq!(record.asset_id, None, "変更検出でassetリンク解除");
        assert_eq!(summary.pending_hash.len(), 1, "再ハッシュ待ちに登録");

        // 再ハッシュ → 新内容と同一ハッシュの既存Assetへ再リンク
        fs.hash_matches(None, 99, 2_000_000, "blake3:cafe");
        let cancel = AtomicBool::new(false);
        let pending = summary.pending_hash[0].clone();
        let relinked = hash_and_link_file(
            &*store,
            &*fs,
            pending.file_id,
            &pending.path,
            &pending.token,
            &cancel,
        )
        .unwrap();
        assert_eq!(relinked.id, cafe.id, "同一内容の既存Assetを再利用");
        let record = repos.files_cloned()[0].clone();
        assert_eq!(record.asset_id, Some(cafe.id));
        assert_eq!(
            repos.assets.lock().unwrap().len(),
            2,
            "Assetは新規作成しない (old + cafe)"
        );
    }

    #[test]
    fn removed_file_becomes_missing_not_deleted() {
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .extend([entry("keep.png", 1, 1, None), entry("gone.png", 2, 2, None)]);
        run_scan(&store, &fs, &root);
        fs.entries
            .lock()
            .unwrap()
            .retain(|e| e.rel_path != *"gone.png");
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.missing, 1);
        let gone = repos
            .files_cloned()
            .into_iter()
            .find(|f| f.rel_path == *"gone.png")
            .expect("missing化でもレコード削除しない");
        assert_eq!(gone.scan_state, ScanState::Missing);

        // 再出現 → present復帰
        fs.entries
            .lock()
            .unwrap()
            .push(entry("gone.png", 2, 2, None));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.found, 0, "再出現は新規ではない");
        let back = repos
            .files_cloned()
            .into_iter()
            .find(|f| f.rel_path == *"gone.png")
            .unwrap();
        assert_eq!(back.scan_state, ScanState::Present);
        assert_eq!(summary.progress.missing, 0);
    }

    #[test]
    fn moved_file_rediscovered_by_os_file_id_keeps_identity() {
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("dir/a.png", 10, 5, Some((7, 42))));
        run_scan(&store, &fs, &root);
        let before = repos.files_cloned()[0].clone();

        // 外部で改名・移動 (同一inode)
        fs.entries.lock().unwrap()[0] = entry("moved/renamed.png", 10, 5, Some((7, 42)));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.relocated, 1);
        assert_eq!(summary.progress.missing, 0, "旧パスのmissing化なし");

        let files = repos.files_cloned();
        assert_eq!(files.len(), 1, "レコードは1つのまま");
        let after = &files[0];
        assert_eq!(after.id, before.id);
        assert_eq!(after.uuid, before.uuid, "同一ファイル実体");
        assert_eq!(after.rel_path, PathBuf::from("moved/renamed.png"));
    }

    #[test]
    fn scope_limits_missing_marking() {
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().extend([
            entry("sub/x.png", 1, 1, None),
            entry("other/y.png", 1, 1, None),
        ]);
        run_scan(&store, &fs, &root);

        // sub配下だけ「削除」された状態でscope走査
        fs.entries
            .lock()
            .unwrap()
            .retain(|e| e.rel_path.starts_with("other"));
        let cancel = AtomicBool::new(false);
        let summary = scan_root(
            &*store,
            &*fs,
            &root,
            Some(Path::new("sub")),
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(summary.progress.missing, 1, "scope内のみmissing化");
        let other = repos
            .files_cloned()
            .into_iter()
            .find(|f| f.rel_path == *"other/y.png")
            .unwrap();
        assert_eq!(other.scan_state, ScanState::Present, "scope外は触らない");
    }

    #[test]
    fn writes_are_committed_in_small_batches() {
        let (repos, store, fs, root) = setup();
        for i in 0..5 {
            fs.entries
                .lock()
                .unwrap()
                .push(entry(&format!("f{i}.png"), 1, 1, None));
        }
        let cancel = AtomicBool::new(false);
        let _ = scan_root(
            &*store,
            &*fs,
            &root,
            None,
            &cancel,
            &ScanOptions { batch_size: 2 },
            &mut |_| {},
        )
        .unwrap();
        let calls = repos.write_calls.lock().unwrap().clone();
        // 5件を batch_size=2 → 3コミット
        assert_eq!(
            calls.iter().filter(|c| **c == "upsert_files_batch").count(),
            3,
            "{calls:?}"
        );
    }

    #[test]
    fn cancel_stops_scan_before_missing_marking() {
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().push(entry("a.png", 1, 1, None));
        run_scan(&store, &fs, &root);
        fs.entries.lock().unwrap().clear();
        let cancel = AtomicBool::new(true); // 最初からキャンセル
        let summary = scan_root(
            &*store,
            &*fs,
            &root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        )
        .unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.progress.missing, 0, "キャンセル時は不在化しない");
        let record = repos.files_cloned()[0].clone();
        assert_eq!(record.scan_state, ScanState::Present);
    }

    #[test]
    fn hash_and_link_shares_asset_between_identical_files() {
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().extend([
            entry("a.png", 10, 1, None),
            entry("copy/b.png", 10, 1, None),
        ]);
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.pending_hash.len(), 2);
        fs.hash_matches(None, 10, 1_000_000, "blake3:cafe");
        let cancel = AtomicBool::new(false);
        for pending in &summary.pending_hash {
            hash_and_link_file(
                &*store,
                &*fs,
                pending.file_id,
                &pending.path,
                &pending.token,
                &cancel,
            )
            .unwrap();
        }
        let files = repos.files_cloned();
        assert_eq!(
            files[0].asset_id, files[1].asset_id,
            "同一内容は1 Assetを共有"
        );
        assert_eq!(repos.assets.lock().unwrap().len(), 1);
    }

    #[test]
    fn unlinked_leftovers_are_requeued_on_next_scan() {
        // ハッシュ中止残 (asset NULL のまま残ったFile) は次回スキャンで
        // 再びハッシュ待ちに乗る (resume)。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("a.png", 10, 1, Some((1, 5))));
        let first = run_scan(&store, &fs, &root);
        assert_eq!(first.pending_hash.len(), 1);
        // ハッシュせずに再スキャン (fast path) → 待ちに復帰
        let second = run_scan(&store, &fs, &root);
        assert_eq!(second.progress.updated, 0);
        assert_eq!(second.pending_hash.len(), 1, "resume: 未リンクは再キュー");
        let _ = repos;
    }

    #[test]
    fn write_failure_aborts_scan_with_error() {
        let (_repos, store, fs, root) = setup();
        store.fail_writes.store(true, Ordering::Relaxed);
        for i in 0..150 {
            fs.entries
                .lock()
                .unwrap()
                .push(entry(&format!("f{i}.png"), 1, 1, None));
        }
        let cancel = AtomicBool::new(false);
        let result = scan_root(
            &*store,
            &*fs,
            &root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        );
        assert!(result.is_err(), "DB書き込み失敗はErrで中断: {result:?}");
    }

    // ===== FG-WP2: 敵対的テスト (移動検出・relocation失敗・世代検証・保守的reconciliation) =====

    #[test]
    fn relocation_with_size_mismatch_is_not_a_move() {
        // inode一致でもsize不一致 → 移動扱いしない (uuid/assetを引き継がない)。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("dir/a.png", 10, 5, Some((7, 42))));
        run_scan(&store, &fs, &root);
        let before = repos.files_cloned()[0].clone();
        let asset = AssetRepo::create_asset(
            repos.as_ref(),
            NewAsset {
                content_hash: "blake3:orig".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            },
        )
        .unwrap();
        AssetRepo::link_file(repos.as_ref(), before.id, asset.id).unwrap();

        // 同一inode・別パス・内容変化 (size相違)
        fs.entries.lock().unwrap()[0] = entry("moved/x.png", 99, 5, Some((7, 42)));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.relocated, 0, "移動扱いしない");
        assert_eq!(summary.progress.found, 1, "新規として登録");

        let files = repos.files_cloned();
        assert_eq!(files.len(), 2, "旧レコードは残る");
        let old = files.iter().find(|f| f.id == before.id).unwrap();
        assert_eq!(old.rel_path, PathBuf::from("dir/a.png"), "旧パスのまま");
        assert_eq!(old.scan_state, ScanState::Missing, "旧パスは不在化");
        assert_eq!(old.asset_id, Some(asset.id), "旧assetリンクは保持");
        let new = files.iter().find(|f| f.rel_path == *"moved/x.png").unwrap();
        assert_ne!(new.id, before.id, "別レコード (uuid不継承)");
        assert_eq!(new.asset_id, None, "assetは引き継がれず再ハッシュ待ち");
    }

    #[test]
    fn relocation_with_mtime_mismatch_is_not_a_move() {
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("dir/a.png", 10, 5, Some((7, 42))));
        run_scan(&store, &fs, &root);
        let before = repos.files_cloned()[0].clone();

        // 同一inode・同size・mtime変化 = 内容変化の可能性 → 移動扱いしない
        fs.entries.lock().unwrap()[0] = entry("moved/x.png", 10, 9, Some((7, 42)));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.relocated, 0);
        assert_eq!(summary.progress.missing, 1, "旧パスは不在化");
        let files = repos.files_cloned();
        let new = files.iter().find(|f| f.rel_path == *"moved/x.png").unwrap();
        assert_ne!(new.id, before.id);
        let _ = new;
    }

    #[test]
    fn inode_reuse_against_missing_row_with_size_mismatch_is_not_relocation() {
        // missing行を無期限保持する前提: 別ファイルがinodeを再利用しても
        // size照合で移動と誤判定しない。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry("old/gone.png", 10, 5, Some((7, 42))));
        run_scan(&store, &fs, &root);
        let gone = repos.files_cloned()[0].clone();
        let asset = AssetRepo::create_asset(
            repos.as_ref(),
            NewAsset {
                content_hash: "blake3:gone".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            },
        )
        .unwrap();
        AssetRepo::link_file(repos.as_ref(), gone.id, asset.id).unwrap();

        // 旧ファイル削除 → missing化 → 別コンテンツがinode (7,42) を再利用
        fs.entries.lock().unwrap().clear();
        run_scan(&store, &fs, &root);
        assert_eq!(repos.files_cloned()[0].scan_state, ScanState::Missing);

        fs.entries
            .lock()
            .unwrap()
            .push(entry("brand/new.png", 777, 1, Some((7, 42))));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.relocated, 0, "inode再利用は移動でない");
        let files = repos.files_cloned();
        let missing = files.iter().find(|f| f.id == gone.id).unwrap();
        assert_eq!(missing.scan_state, ScanState::Missing, "missing行のまま");
        assert_eq!(missing.asset_id, Some(asset.id), "旧assetも維持");
        let unrelated = files
            .iter()
            .find(|f| f.rel_path == *"brand/new.png")
            .unwrap();
        assert_eq!(unrelated.asset_id, None, "無関係レコードは未リンク");
    }

    /// walk開始時に任意の操作を差し込めるfs (relocation失敗の競合注入用)。
    struct RiggedFs {
        entries: Vec<ScanEntry>,
        before_walk: Box<dyn Fn() + Send + Sync>,
    }

    impl ScanFs for RiggedFs {
        fn walk(
            &self,
            _root: &Path,
            _scope: Option<&Path>,
            sink: &mut dyn FnMut(ScanEntry),
        ) -> WalkOutcome {
            (self.before_walk)();
            for e in &self.entries {
                sink(e.clone());
            }
            WalkOutcome {
                entries_walked: self.entries.len() as u64,
                errors: Vec::new(),
            }
        }
        fn hash_and_probe(
            &self,
            _path: &Path,
            _expect: &GenerationToken,
            _cancel: &AtomicBool,
        ) -> Result<HashedFile, HashFileError> {
            Err(HashFileError::Io("not configured".to_string()))
        }
    }

    #[test]
    fn relocate_duplicate_falls_back_to_upsert_and_missing_marks_old() {
        // load_existing後に移動先へ並行書き込みがある競合: relocateが
        // Duplicateでも新パスをupsertで確定させ、旧パス行を確実にmissing化する
        // (黙って捨てない)。
        let (repos, store, _fs, root) = setup();
        repos.files.lock().unwrap().push(FileRecord {
            id: 1,
            uuid: "00000000-0000-7000-8000-000000000001".to_string(),
            root_id: root.id,
            asset_id: None,
            rel_path: PathBuf::from("old/a.png"),
            display_name: "a.png".to_string(),
            extension: Some("png".to_string()),
            byte_size: 10,
            mtime_ms: 5,
            mtime_ns: Some(5_000_000),
            scan_state: ScanState::Present,
            os_file_id: Some(OsFileId::new(7, 42)),
            last_seen_at: "2026-08-30T00:00:00.000Z".to_string(),
            created_at: "2026-08-30T00:00:00.000Z".to_string(),
            updated_at: "2026-08-30T00:00:00.000Z".to_string(),
        });
        *repos.next_id.lock().unwrap() = 1;
        // 競合: walk開始時 (load_existing後) に移動先パスへ別行が既にある
        let racer = Arc::clone(&repos);
        let fs = RiggedFs {
            entries: vec![entry("new/b.png", 10, 5, Some((7, 42)))],
            before_walk: Box::new(move || {
                let _ = racer.upsert_file(FileUpsert {
                    root_id: 1,
                    rel_path: PathBuf::from("new/b.png"),
                    display_name: "b.png".to_string(),
                    byte_size: 1,
                    mtime_ms: 1,
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                });
            }),
        };
        let cancel = AtomicBool::new(false);
        let summary = scan_root(
            &*store,
            &fs,
            &root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        )
        .unwrap();

        let files = repos.files_cloned();
        let old = files.iter().find(|f| f.rel_path == *"old/a.png").unwrap();
        assert_eq!(old.scan_state, ScanState::Missing, "旧パスは不在化");
        let new = files.iter().find(|f| f.rel_path == *"new/b.png").unwrap();
        assert_eq!(new.byte_size, 10, "新パスはupsertで確定");
        assert_eq!(new.scan_state, ScanState::Present);
        assert_eq!(summary.progress.missing, 1);
    }

    #[test]
    fn relocate_not_found_falls_back_to_upsert() {
        // 旧行が走査中に消えた場合: NotFoundでも新パスを確定させる。
        let (repos, store, _fs, root) = setup();
        repos.files.lock().unwrap().push(FileRecord {
            id: 1,
            uuid: "00000000-0000-7000-8000-000000000001".to_string(),
            root_id: root.id,
            asset_id: None,
            rel_path: PathBuf::from("old/a.png"),
            display_name: "a.png".to_string(),
            extension: Some("png".to_string()),
            byte_size: 10,
            mtime_ms: 5,
            mtime_ns: Some(5_000_000),
            scan_state: ScanState::Present,
            os_file_id: Some(OsFileId::new(7, 42)),
            last_seen_at: "2026-08-30T00:00:00.000Z".to_string(),
            created_at: "2026-08-30T00:00:00.000Z".to_string(),
            updated_at: "2026-08-30T00:00:00.000Z".to_string(),
        });
        *repos.next_id.lock().unwrap() = 1;
        let racer = Arc::clone(&repos);
        let fs = RiggedFs {
            entries: vec![entry("new/b.png", 10, 5, Some((7, 42)))],
            before_walk: Box::new(move || {
                racer.files.lock().unwrap().clear();
            }),
        };
        let cancel = AtomicBool::new(false);
        let summary = scan_root(
            &*store,
            &fs,
            &root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        )
        .unwrap();

        let files = repos.files_cloned();
        assert_eq!(files.len(), 1, "新パスのみ残る");
        assert_eq!(files[0].rel_path, PathBuf::from("new/b.png"));
        assert_eq!(files[0].scan_state, ScanState::Present);
        assert_eq!(summary.progress.missing, 0);
    }

    #[test]
    fn relocate_db_error_fails_scan() {
        // Duplicate/NotFound以外のrelocation失敗はスキャンエラーとして伝播。
        let (repos, store, _fs, root) = setup();
        repos.files.lock().unwrap().push(FileRecord {
            id: 1,
            uuid: "00000000-0000-7000-8000-000000000001".to_string(),
            root_id: root.id,
            asset_id: None,
            rel_path: PathBuf::from("old/a.png"),
            display_name: "a.png".to_string(),
            extension: Some("png".to_string()),
            byte_size: 10,
            mtime_ms: 5,
            mtime_ns: Some(5_000_000),
            scan_state: ScanState::Present,
            os_file_id: Some(OsFileId::new(7, 42)),
            last_seen_at: "2026-08-30T00:00:00.000Z".to_string(),
            created_at: "2026-08-30T00:00:00.000Z".to_string(),
            updated_at: "2026-08-30T00:00:00.000Z".to_string(),
        });
        *repos.relocate_failure.lock().unwrap() = Some(RepoError::Db("boom".to_string()));
        let fs = RiggedFs {
            entries: vec![entry("new/b.png", 10, 5, Some((7, 42)))],
            before_walk: Box::new(|| {}),
        };
        let cancel = AtomicBool::new(false);
        let result = scan_root(
            &*store,
            &fs,
            &root,
            None,
            &cancel,
            &ScanOptions::default(),
            &mut |_| {},
        );
        let err = result.expect_err("DBエラーはスキャン失敗");
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn hash_discarded_when_generation_differs_from_record() {
        // ハッシュ結果のfdトークンがDBレコードと不一致 → 破棄 (Stale)。
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().push(entry("a.png", 10, 5, None));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.pending_hash.len(), 1);

        // 計算中にmtimeが変わった状況を模擬 (fd観測nsがレコードとズレる)
        fs.hash_matches(None, 10, 6_000_000, "blake3:cafe");
        let cancel = AtomicBool::new(false);
        let pending = summary.pending_hash[0].clone();
        let result = hash_and_link_file(
            &*store,
            &*fs,
            pending.file_id,
            &pending.path,
            &pending.token,
            &cancel,
        );
        assert!(
            matches!(result, Err(HashFileError::Stale(_))),
            "世代不一致はStale: {result:?}"
        );
        assert_eq!(repos.files_cloned()[0].asset_id, None, "リンクされない");
        assert!(repos.assets.lock().unwrap().is_empty(), "Assetも作らない");
    }

    #[test]
    fn stale_hash_job_after_newer_scan_does_not_relink() {
        // 古いジョブの完了が新しいスキャンの後に到着しても誤リンクしない。
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().push(entry("a.png", 10, 5, None));
        let first = run_scan(&store, &fs, &root);
        assert_eq!(first.pending_hash.len(), 1);

        // ハッシュ前に内容変化 + 再スキャン (detach + 新観測 (20,6))
        fs.entries.lock().unwrap()[0] = entry("a.png", 20, 6, None);
        let second = run_scan(&store, &fs, &root);
        assert_eq!(second.progress.updated, 1);
        assert_eq!(second.pending_hash.len(), 1);

        // 古い世代 (10,5) のジョブ結果 → 破棄
        fs.hash_matches(None, 10, 5_000_000, "blake3:old");
        let cancel = AtomicBool::new(false);
        let old_pending = first.pending_hash[0].clone();
        let result = hash_and_link_file(
            &*store,
            &*fs,
            old_pending.file_id,
            &old_pending.path,
            &old_pending.token,
            &cancel,
        );
        assert!(matches!(result, Err(HashFileError::Stale(_))), "{result:?}");
        assert_eq!(repos.files_cloned()[0].asset_id, None);

        // 新しい世代 (20,6) はリンクされる
        fs.hash_matches(None, 20, 6_000_000, "blake3:new");
        let new_pending = second.pending_hash[0].clone();
        let linked = hash_and_link_file(
            &*store,
            &*fs,
            new_pending.file_id,
            &new_pending.path,
            &new_pending.token,
            &cancel,
        )
        .unwrap();
        let files = repos.files_cloned();
        assert_eq!(files[0].asset_id, Some(linked.id));
        assert_eq!(
            repos.assets.lock().unwrap().len(),
            1,
            "古い結果のAssetは作らない"
        );
    }

    #[test]
    fn hash_and_probe_is_single_fd_bound_call() {
        // FG-WP2-2: ハッシュとプローブは1回の hash_and_probe 呼び出しに束ねら
        // れる (パス再オープンする独立プローブ経路は存在しない = 置換ファイル
        // の寸法混入が構造的に不可能)。寸法はハッシュ結果と同じ呼び出しの
        // ものでのみAssetへ載る。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .extend([entry("a.png", 10, 5, None), entry("b.png", 10, 5, None)]);
        let summary = run_scan(&store, &fs, &root);
        let cancel = AtomicBool::new(false);

        // a.png: 寸法付きでリンク
        fs.hash_matches(None, 10, 5_000_000, "blake3:cafe");
        *fs.probe.lock().unwrap() = Some(ProbedImage {
            format: ProbedFormat::Png,
            width: 640,
            height: 480,
            has_alpha: true,
            is_animated: false,
            frame_count: None,
        });
        let a = summary
            .pending_hash
            .iter()
            .find(|p| p.path.ends_with("a.png"))
            .unwrap();
        let asset_a =
            hash_and_link_file(&*store, &*fs, a.file_id, &a.path, &a.token, &cancel).unwrap();
        assert_eq!(asset_a.width, Some(640));
        assert_eq!(asset_a.height, Some(480));

        // b.png: プーブ失敗 (非PNG等) は寸法NULLでリンク
        fs.hash_matches(None, 10, 5_000_000, "blake3:beef");
        *fs.probe.lock().unwrap() = None;
        let b = summary
            .pending_hash
            .iter()
            .find(|p| p.path.ends_with("b.png"))
            .unwrap();
        let asset_b =
            hash_and_link_file(&*store, &*fs, b.file_id, &b.path, &b.token, &cancel).unwrap();
        assert_eq!(asset_b.width, None, "プローブ失敗は寸法NULL");
        let _ = repos;

        // 2ファイル = 2呼び出しのみ (プローブの独立呼び出し経路なし)
        assert_eq!(fs.hash_calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn path_reuse_with_fresh_inode_and_matching_ms_makes_job_stale() {
        // FG-WP2-2: 同パス再利用 (新inode・同size・同一ms・ns近傍) の古い
        // ジョブ結果はリンクされない。inode差し替え = 別ファイル実体。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry_ns("a.png", 10, 5, 5_000, Some((1, 100))));
        let first = run_scan(&store, &fs, &root);
        assert_eq!(first.pending_hash.len(), 1);

        // パス再利用: 新inode (1,999)・同size・同ms・nsが僅かにズレる
        fs.entries.lock().unwrap()[0] = entry_ns("a.png", 10, 5, 5_400, Some((1, 999)));
        let second = run_scan(&store, &fs, &root);
        assert_eq!(second.progress.updated, 1, "トークン変化を検出");

        // 旧ジョブ (inode 1,100 のfd観測) → 行トークン (1,999) と不一致でStale
        fs.hash_matches(Some((1, 100)), 10, 5_000, "blake3:old-bytes");
        let cancel = AtomicBool::new(false);
        let old = first.pending_hash[0].clone();
        let result = hash_and_link_file(&*store, &*fs, old.file_id, &old.path, &old.token, &cancel);
        assert!(matches!(result, Err(HashFileError::Stale(_))), "{result:?}");
        assert_eq!(repos.files_cloned()[0].asset_id, None, "誤リンクしない");
        assert!(repos.assets.lock().unwrap().is_empty(), "Assetも作らない");

        // 新ジョブはリンクされる
        fs.hash_matches(Some((1, 999)), 10, 5_400, "blake3:new-bytes");
        let new = second.pending_hash[0].clone();
        let linked =
            hash_and_link_file(&*store, &*fs, new.file_id, &new.path, &new.token, &cancel).unwrap();
        assert_eq!(repos.files_cloned()[0].asset_id, Some(linked.id));
    }

    #[test]
    fn mtime_ns_difference_alone_discards_result() {
        // 同一inode・同size・同一msでもnsが違えば別世代 → Stale。
        // (ms丸め問題: 同一ms内の置換を検出するのがns精度の役割)
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry_ns("a.png", 10, 5, 5_000, Some((1, 100))));
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.pending_hash.len(), 1);

        // fd観測のnsだけがズレた (同一ms内のin-place変更) 結果 → Stale
        fs.hash_matches(Some((1, 100)), 10, 5_001, "blake3:cafe");
        let cancel = AtomicBool::new(false);
        let pending = summary.pending_hash[0].clone();
        let result = hash_and_link_file(
            &*store,
            &*fs,
            pending.file_id,
            &pending.path,
            &pending.token,
            &cancel,
        );
        assert!(matches!(result, Err(HashFileError::Stale(_))), "{result:?}");
        assert_eq!(repos.files_cloned()[0].asset_id, None);
    }

    #[test]
    fn same_inode_move_links_under_new_path() {
        // FG-WP2-2: 同一inodeの移動・改名は同一バイト。移動後パスの行に
        // 対して旧ジョブのfd観測 (トークン一致) は正しくリンクされる。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .push(entry_ns("dir/a.png", 10, 5, 5_000, Some((7, 42))));
        let first = run_scan(&store, &fs, &root);
        assert_eq!(first.pending_hash.len(), 1);

        // 外部移動 (同一inode・同一トークン) → relocation
        fs.entries.lock().unwrap()[0] = entry_ns("moved/renamed.png", 10, 5, 5_000, Some((7, 42)));
        let second = run_scan(&store, &fs, &root);
        assert_eq!(second.progress.relocated, 1);
        let record = repos.files_cloned()[0].clone();
        assert_eq!(record.rel_path, PathBuf::from("moved/renamed.png"));

        // 移動前に発行されたジョブのfd観測は同一トークン → リンクされる
        fs.hash_matches(Some((7, 42)), 10, 5_000, "blake3:moved");
        let cancel = AtomicBool::new(false);
        let pending = first.pending_hash[0].clone();
        let linked = hash_and_link_file(
            &*store,
            &*fs,
            pending.file_id,
            &pending.path,
            &pending.token,
            &cancel,
        )
        .unwrap();
        let after = repos.files_cloned()[0].clone();
        assert_eq!(after.asset_id, Some(linked.id), "同一実体は移動後もリンク");
        assert_eq!(after.rel_path, PathBuf::from("moved/renamed.png"));
    }

    #[test]
    fn partial_walk_blocked_subtree_is_not_missing_marked() {
        // permission-denied等で列挙できなかったサブツリー配下はmissing化しない。
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().extend([
            entry("ok.png", 1, 1, None),
            entry("locked/a.png", 2, 2, None),
            entry("locked/deep/b.png", 3, 3, None),
        ]);
        run_scan(&store, &fs, &root);

        fs.entries
            .lock()
            .unwrap()
            .retain(|e| !e.rel_path.starts_with("locked"));
        *fs.walk_errors.lock().unwrap() = vec![WalkError {
            rel_path: PathBuf::from("locked"),
            reason: "permission denied".to_string(),
            blocks_missing: true,
        }];
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.missing, 0, "列挙失敗サブツリーは現状維持");
        assert_eq!(summary.progress.errors, 1, "不完全走査は警告として残る");
        assert!(summary.error_samples[0].contains("locked"));
        for record in repos.files_cloned() {
            assert_eq!(
                record.scan_state,
                ScanState::Present,
                "{}",
                record.rel_path.display()
            );
        }

        // 列挙に成功した次回はmissing化が進む
        fs.walk_errors.lock().unwrap().clear();
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.missing, 2, "今度は不在化");
    }

    #[test]
    fn vanished_paths_do_not_block_missing_marking() {
        // パス自体が消えたエラー (走査中削除・ルート不在) は実際の不在のため
        // missing化をブロックしない。
        let (repos, store, fs, root) = setup();
        fs.entries.lock().unwrap().extend([
            entry("keep.png", 1, 1, None),
            entry("gone/dir/x.png", 2, 2, None),
        ]);
        run_scan(&store, &fs, &root);

        fs.entries
            .lock()
            .unwrap()
            .retain(|e| e.rel_path == *"keep.png");
        *fs.walk_errors.lock().unwrap() = vec![WalkError {
            rel_path: PathBuf::from("gone"),
            reason: "No such file or directory".to_string(),
            blocks_missing: false,
        }];
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.missing, 1, "消えたパスはmissing化");
        let _ = repos;
    }

    #[test]
    fn root_level_walk_error_blocks_all_missing_marking() {
        // ルート自体の列挙失敗 (空rel_path) はスコープ全体の不在化を禁じる。
        let (repos, store, fs, root) = setup();
        fs.entries
            .lock()
            .unwrap()
            .extend([entry("a.png", 1, 1, None), entry("b.png", 2, 2, None)]);
        run_scan(&store, &fs, &root);

        fs.entries.lock().unwrap().clear();
        *fs.walk_errors.lock().unwrap() = vec![WalkError {
            rel_path: PathBuf::new(),
            reason: "permission denied".to_string(),
            blocks_missing: true,
        }];
        let summary = run_scan(&store, &fs, &root);
        assert_eq!(summary.progress.missing, 0);
        assert_eq!(repos.files_cloned().len(), 2, "レコードはそのまま");
        for record in repos.files_cloned() {
            assert_eq!(record.scan_state, ScanState::Present);
        }
    }

    // ===== WatchBuffer =====

    #[test]
    fn watch_buffer_coalesces_burst_to_final_state() {
        let buffer = WatchBuffer::new();
        for _ in 0..100 {
            buffer.push(FsChange::Modified, PathBuf::from("/r/a.png"));
        }
        buffer.push(FsChange::Created, PathBuf::from("/r/new.png"));
        buffer.push(FsChange::Modified, PathBuf::from("/r/new.png"));
        buffer.push(FsChange::Removed, PathBuf::from("/r/gone.png"));
        buffer.push(FsChange::Created, PathBuf::from("/r/gone.png"));
        assert_eq!(buffer.observed_events(), 104);
        let coalesced = buffer.drain();
        assert_eq!(coalesced.changes.len(), 3, "パス単位に合流");
        assert_eq!(
            coalesced.changes.get(&PathBuf::from("/r/a.png")),
            Some(&FsChange::Modified)
        );
        assert_eq!(
            coalesced.changes.get(&PathBuf::from("/r/new.png")),
            Some(&FsChange::Modified),
            "Create+Modify → Modified"
        );
        assert_eq!(
            coalesced.changes.get(&PathBuf::from("/r/gone.png")),
            Some(&FsChange::Modified),
            "Remove+Create → Modified (内容変化の可能性)"
        );
        assert!(!coalesced.overflow);
        assert!(buffer.is_empty());
    }

    #[test]
    fn watch_buffer_removal_wins_and_overflow_flag() {
        let buffer = WatchBuffer::new();
        buffer.push(FsChange::Created, PathBuf::from("/r/x.png"));
        buffer.push(FsChange::Modified, PathBuf::from("/r/x.png"));
        buffer.push(FsChange::Removed, PathBuf::from("/r/x.png"));
        assert_eq!(
            buffer.drain().changes.get(&PathBuf::from("/r/x.png")),
            Some(&FsChange::Removed)
        );
        buffer.mark_overflow();
        assert!(buffer.is_overflow());
        assert!(
            buffer.drain().overflow,
            "drainで overflow を受け取りリセット"
        );
        assert!(!buffer.is_overflow());
    }

    // ===== WP-7: アプリ内操作由来イベントの抑制 (二重反映防止) =====

    #[test]
    fn watch_buffer_suppresses_self_operation_paths() {
        let buffer = WatchBuffer::new();
        let op_paths = vec![PathBuf::from("/r/from.png"), PathBuf::from("/r/to.png")];
        buffer.suppress(&op_paths);
        assert!(buffer.is_suppressed(Path::new("/r/from.png")));
        // 抑制中の自操作由来イベントは記録も計上もされない
        buffer.push(FsChange::Removed, PathBuf::from("/r/from.png"));
        buffer.push(FsChange::Created, PathBuf::from("/r/to.png"));
        assert_eq!(buffer.observed_events(), 0);
        assert!(buffer.is_empty());
        // 無関係パスは通常どおり通す
        buffer.push(FsChange::Modified, PathBuf::from("/r/other.png"));
        assert_eq!(buffer.observed_events(), 1);
        // 解除後は再び通る
        buffer.unsuppress(&op_paths);
        assert!(!buffer.is_suppressed(Path::new("/r/from.png")));
        buffer.push(FsChange::Modified, PathBuf::from("/r/from.png"));
        assert_eq!(buffer.observed_events(), 2);
        let coalesced = buffer.drain();
        assert_eq!(coalesced.changes.len(), 2);
    }

    /// 抑制中に破棄したイベントパスは記録され、和解スキャンで取り出せる
    /// (FG-WP7: 過剰抑制で外部変更を見逃さない)。
    #[test]
    fn watch_buffer_records_suppressed_dropped_paths() {
        let buffer = WatchBuffer::new();
        // 実操作と同様に src/dst 両パスを抑制する
        let op_paths = vec![PathBuf::from("/r/moving.png"), PathBuf::from("/r/dest.png")];
        buffer.suppress(&op_paths);
        // 抑制中: 自操作由来に加え、抑制パスへの外部変更も破棄されるが、
        // パスは和解スキャン用に記録される
        buffer.push(FsChange::Removed, PathBuf::from("/r/moving.png"));
        buffer.push(FsChange::Created, PathBuf::from("/r/dest.png"));
        buffer.push(FsChange::Modified, PathBuf::from("/r/dest.png"));
        assert!(buffer.is_empty(), "バッファへは入らない");
        let dropped = buffer.take_suppressed_dropped();
        assert_eq!(dropped.len(), 2, "破棄パスを記録 (重複は圧縮): {dropped:?}");
        assert!(dropped.contains(&PathBuf::from("/r/moving.png")));
        assert!(dropped.contains(&PathBuf::from("/r/dest.png")));
        // 取り出しはクリアを兼ねる (二重スキャンしない)
        assert!(buffer.take_suppressed_dropped().is_empty());
        // 無関係パス (抑制外) は通常どおりバッファへ入る
        buffer.push(FsChange::Created, PathBuf::from("/r/external.png"));
        assert!(!buffer.is_empty());
        assert!(buffer.take_suppressed_dropped().is_empty());
    }

    // ===== AssetId符号化 =====

    #[test]
    fn library_asset_id_encoding_round_trip() {
        let asset = library_asset_id(12345);
        assert_eq!(asset.value(), 12_345);
        assert_eq!(
            decode_library_asset_id(asset),
            LibraryAssetRef::Asset(12_345)
        );

        let provisional = provisional_file_asset_id(678);
        assert_ne!(provisional.value() & PROVISIONAL_FILE_FLAG, 0);
        assert_eq!(
            decode_library_asset_id(provisional),
            LibraryAssetRef::ProvisionalFile(678)
        );
        // 仮IDは穏当なassets.idと衝突しない
        assert_ne!(provisional, library_asset_id(678));
    }

    #[test]
    fn mime_of_covers_mvp_extensions() {
        assert_eq!(mime_of(Path::new("/a/b.PNG")).as_deref(), Some("image/png"));
        assert_eq!(
            mime_of(Path::new("/a/b.JPG")).as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            mime_of(Path::new("/a/b.avif")).as_deref(),
            Some("image/avif")
        );
        assert_eq!(mime_of(Path::new("/a/b.txt")), None);
        assert_eq!(mime_of(Path::new("/a/b")), None);
    }
}
