//! キャッシュキー・キャッシュ読み書き・アトミック配置・上限会計
//! (Phase 4 / WP-3)。
//!
//! 設計契約 (WP-3委任仕様):
//! - キー入力: canonicalパス + byte_size + mtime_ms + size_class + ALGORITHM_VERSION
//! - ハッシュ: Xxh3 128bit → hex32文字。ファイル名 `{hex}-{size_class_id}-{version}.webp`
//! - 分散: `{cache_root}/{hex先頭2文字}/` の256分桶
//! - 一時ファイルは最終ディレクトリと同一場所に `.tmp-{pid}-{連番}.webp` →
//!   fsync → rename (同ディレクトリ内renameはアトミック)。Dropガードで失敗時削除
//! - 2GiB不変条件: 会計対象 = 最終ファイル + 生成中一時ファイル。
//!   書き込み成功ごとと起動時に検査し、超過なら目標容量までLRU削除
//!   (アクセス台帳はメモリ内。未登録ファイルはmtime順。実行中ジョブの
//!   キーとin-flight一時ファイルは削除禁止)

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gento_core::SizeClass;
use twox_hash::xxhash3_128;

use crate::stats::ThumbStats;

/// サムネイル生成アルゴリズムのバージョン。
/// デコード・リサイズ・エンコード条件 (Orientation設定を含む) を変えたら
/// インクリメントする。PNG対象外だが将来の設定差し込み用のプレースホルダ。
pub const ALGORITHM_VERSION: u32 = 1;

/// プロトタイプのキャッシュ上限 (docs/implementation-plan.md §2)。
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// キャッシュファイル名の長さ上限 (WP-3契約: 約48文字)。
pub const MAX_FILE_NAME_LEN: usize = 48;

/// 元アセットの検証可能な識別入力。
///
/// 将来の内容ハッシュ (SQLite Assetハッシュ) へ差し替え可能な関数境界:
/// この構造を作る側 ([`asset_fingerprint`]) を差し替えればキー計算と
/// ストアはそのまま流用できる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFingerprint {
    /// 実ファイルを一意に特定する正規化パス文字列。
    pub canonical_path: String,
    pub byte_size: u64,
    pub mtime_ms: u64,
}

/// パスからfingerprint入力を作る。canonicalize失敗 (ファイル消失等) は
/// 絶対パス化したlossy表現へフォールバックし、キー計算の決定性を保つ。
pub fn asset_fingerprint(path: &Path, byte_size: u64, mtime_ms: u64) -> AssetFingerprint {
    let canonical_path = match fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    };
    AssetFingerprint {
        canonical_path,
        byte_size,
        mtime_ms,
    }
}

/// キャッシュキー (ハッシュ + 配置場所)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Xxh3 128bitの16進小文字 (32文字固定)。
    pub hex: String,
    /// `{hex}-{size_class file_id}-{ALGORITHM_VERSION}.webp`。
    pub file_name: String,
    /// ルートからの相対パス (`{hex[..2]}/{file_name}`)。
    pub relative: PathBuf,
}

/// fingerprint + サイズクラスからキャッシュキーを計算する。
///
/// 決定的: 同一入力は常に同一キー。キー文字列にはバージョンとサイズクラスが
/// 織り込まれるため、アルゴリズム変更・クラス違いは別ファイルになる。
pub fn cache_key(fp: &AssetFingerprint, size_class: SizeClass) -> CacheKey {
    // NUL区切り: パス文字列内に区切り文字が現れないようにする。
    let key_input = format!(
        "{}\0{}\0{}\0{}\0{}",
        fp.canonical_path,
        fp.byte_size,
        fp.mtime_ms,
        size_class.file_id(),
        ALGORITHM_VERSION
    );
    let hash = xxhash3_128::Hasher::oneshot(key_input.as_bytes());
    let hex = format!("{hash:032x}");
    let file_name = format!(
        "{}-{}-{}.webp",
        hex,
        size_class.file_id(),
        ALGORITHM_VERSION
    );
    debug_assert!(
        validate_file_name(&file_name),
        "cache file name must stay filesystem-safe"
    );
    let relative = PathBuf::from(&hex[..2]).join(&file_name);
    CacheKey {
        hex,
        file_name,
        relative,
    }
}

/// ファイル名の長さ上限と使用文字種 ([A-Za-z0-9_.-]) を検証する。
pub fn validate_file_name(name: &str) -> bool {
    name.len() <= MAX_FILE_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// キャッシュ検索結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    /// キャッシュ済み。絶対パスを返す。
    Hit(PathBuf),
    Miss,
}

/// 台帳エントリ (1最終ファイル)。
#[derive(Debug, Clone)]
struct LedgerEntry {
    /// ファイル容量。退避会計と削除量の算出に使う。
    bytes: u64,
    /// 最終アクセス連番 (大きいほど新しい)。
    access_seq: u64,
    /// このエントリのキーhex (ピン判定用)。
    hex: String,
}

struct CacheInner {
    /// 最終ファイルの会計台帳 (絶対パス → エントリ)。
    ledger: HashMap<PathBuf, LedgerEntry>,
    /// 最終ファイルの合計容量。
    bytes_total: u64,
    /// 生成中 (コミット開始後・rename前) の一時ファイル会計。
    inflight_bytes: u64,
    /// アクセス連番発行器。
    access_clock: u64,
    /// 実行中ジョブのキーhexピン (refcount)。退避から保護する。
    pinned: HashMap<String, usize>,
    /// 生成中の一時ファイルパス (clearから保護する)。
    pinned_tmp: HashSet<PathBuf>,
    tmp_seq: u64,
}

impl CacheInner {
    fn new() -> Self {
        Self {
            ledger: HashMap::new(),
            bytes_total: 0,
            inflight_bytes: 0,
            access_clock: 0,
            pinned: HashMap::new(),
            pinned_tmp: HashSet::new(),
            tmp_seq: 0,
        }
    }
}

/// ディスクキャッシュ。`Arc` で共有してワーカーから使う。
pub struct ThumbnailCache {
    root: PathBuf,
    max_bytes: u64,
    /// 上限超過時にここまで削減する (maxの95%)。
    target_bytes: u64,
    stats: Arc<ThumbStats>,
    inner: Mutex<CacheInner>,
}

/// poisoning時も復旧扱い (走査側の規約に同じ)。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ThumbnailCache {
    /// 既定上限 (2GiB / 目標1.9GiB) で開く。起動時会計とenforceも行う。
    pub fn open(root: impl Into<PathBuf>, stats: Arc<ThumbStats>) -> io::Result<Self> {
        Self::open_with_limit(root, DEFAULT_MAX_BYTES, stats)
    }

    /// 上限を指定して開く (テスト用)。
    pub fn open_with_limit(
        root: impl Into<PathBuf>,
        max_bytes: u64,
        stats: Arc<ThumbStats>,
    ) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let target_bytes = max_bytes - max_bytes / 20; // 95%
        let cache = Self {
            root,
            max_bytes,
            target_bytes,
            stats,
            inner: Mutex::new(CacheInner::new()),
        };
        {
            let mut inner = lock(&cache.inner);
            cache.recover_startup_locked(&mut inner)?;
            cache.enforce_limit_locked(&mut inner);
        }
        Ok(cache)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// 会計上の合計容量 (最終ファイル + 生成中一時ファイル)。
    pub fn total_bytes(&self) -> u64 {
        let inner = lock(&self.inner);
        inner.bytes_total + inner.inflight_bytes
    }

    /// 最終ファイル数。
    pub fn entry_count(&self) -> usize {
        lock(&self.inner).ledger.len()
    }

    /// キャッシュ検索。ヒット時はアクセス台帳を更新する。
    pub fn lookup(&self, key: &CacheKey) -> CacheLookup {
        let path = self.root.join(&key.relative);
        let mut inner = lock(&self.inner);
        // symlinkでない正規ファイルのみヒットとする (WP-4プロトコルの
        // 「正規ファイルのみ」方針に先んじた防御)。
        match fs::metadata(&path) {
            Ok(md) if md.is_file() => {
                inner.access_clock += 1;
                let seq = inner.access_clock;
                inner
                    .ledger
                    .entry(path.clone())
                    .and_modify(|e| e.access_seq = seq)
                    .or_insert_with(|| LedgerEntry {
                        bytes: md.len(),
                        access_seq: seq,
                        hex: key.hex.clone(),
                    });
                CacheLookup::Hit(path)
            }
            _ => CacheLookup::Miss,
        }
    }

    /// 最終配置用の一時ファイルライターを作る。
    /// キーをピンし (退避保護)、一時ファイルを保護集合へ登録する。
    pub fn begin_write(self: &Arc<Self>, key: &CacheKey) -> io::Result<TempWriter> {
        let shard = self
            .root
            .join(&key.relative)
            .parent()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&shard)?;
        let (tmp_path, file) = {
            let mut inner = lock(&self.inner);
            inner.tmp_seq += 1;
            let seq = inner.tmp_seq;
            let tmp_path = shard.join(format!(".tmp-{}-{}.webp", std::process::id(), seq));
            let file = File::create(&tmp_path)?;
            *inner.pinned.entry(key.hex.clone()).or_insert(0) += 1;
            inner.pinned_tmp.insert(tmp_path.clone());
            (tmp_path, file)
        };
        Ok(TempWriter {
            cache: Arc::clone(self),
            key_hex: key.hex.clone(),
            final_path: self.root.join(&key.relative),
            tmp_path,
            file: Some(file),
            accounted: 0,
            committed: false,
        })
    }

    /// 全削除。ピン済み (生成中) の一時ファイルは残す。
    pub fn clear(&self) -> io::Result<ClearOutcome> {
        let mut inner = lock(&self.inner);
        let protected: HashSet<PathBuf> = inner.pinned_tmp.iter().cloned().collect();
        let mut removed_files = 0u64;
        let mut removed_bytes = 0u64;
        for entry in fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                match fs::read_dir(&path) {
                    Ok(children) => {
                        for child in children {
                            let Ok(child) = child else { continue };
                            let child_path = child.path();
                            if protected.contains(&child_path) {
                                continue;
                            }
                            if let Ok(md) = fs::metadata(&child_path) {
                                removed_bytes += md.len();
                            }
                            if fs::remove_file(&child_path).is_ok() {
                                removed_files += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(dir = %path.display(), error = %e, "clear: unreadable shard")
                    }
                }
                // 空になったシェルドディレクトリは削除する (保護ファイルが残る場合は失敗する)
                let _ = fs::remove_dir(&path);
            } else if !protected.contains(&path) {
                if let Ok(md) = fs::metadata(&path) {
                    removed_bytes += md.len();
                }
                if fs::remove_file(&path).is_ok() {
                    removed_files += 1;
                }
            }
        }
        inner.ledger.clear();
        inner.bytes_total = 0;
        Ok(ClearOutcome {
            removed_files,
            removed_bytes,
        })
    }

    /// 起動時回復: 孤立一時ファイルの削除と、既存ファイルの台帳登録
    /// (未登録ファイルはmtime順のアクセス連番で登録 = mtime順退避)。
    fn recover_startup_locked(&self, inner: &mut CacheInner) -> io::Result<()> {
        let mut discovered: Vec<(PathBuf, u64, u64)> = Vec::new(); // (path, bytes, mtime_ms)
        for shard in fs::read_dir(&self.root)? {
            let Ok(shard) = shard else { continue };
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            for file in fs::read_dir(shard.path())? {
                let Ok(file) = file else { continue };
                let path = file.path();
                let name = file.file_name().to_string_lossy().into_owned();
                if name.starts_with(".tmp-") {
                    // 前回実行のクラスタ等で残った一時ファイル。安全に削除する。
                    let _ = fs::remove_file(&path);
                    tracing::debug!(path = %path.display(), "removed orphaned temp file");
                    continue;
                }
                if !name.ends_with(".webp") {
                    continue;
                }
                match fs::metadata(&path) {
                    Ok(md) if md.is_file() => {
                        let mtime_ms = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        discovered.push((path, md.len(), mtime_ms));
                    }
                    _ => continue,
                }
            }
        }
        // mtime昇順 (同値はパス順) で古いものから小さいアクセス連番を付与。
        discovered.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        for (path, bytes, _) in discovered {
            inner.access_clock += 1;
            inner.bytes_total += bytes;
            let hex = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.split('-').next())
                .unwrap_or("")
                .to_string();
            inner.ledger.insert(
                path,
                LedgerEntry {
                    bytes,
                    access_seq: inner.access_clock,
                    hex,
                },
            );
        }
        Ok(())
    }

    /// 上限を超えていれば目標容量までLRU削除する。
    /// ピンされたキーのファイルは削除しない。
    fn enforce_limit_locked(&self, inner: &mut CacheInner) {
        loop {
            let total = inner.bytes_total + inner.inflight_bytes;
            if total <= self.max_bytes {
                return;
            }
            let victim = inner
                .ledger
                .iter()
                .filter(|(_, e)| !inner.pinned.contains_key(&e.hex))
                .min_by_key(|(_, e)| (e.access_seq, e.bytes))
                .map(|(path, e)| (path.clone(), e.clone()));
            let Some((path, entry)) = victim else {
                tracing::warn!(
                    total,
                    max = self.max_bytes,
                    "cache over limit but no evictable entry (all pinned?)"
                );
                return;
            };
            inner.ledger.remove(&path);
            inner.bytes_total = inner.bytes_total.saturating_sub(entry.bytes);
            let removed = fs::remove_file(&path).is_ok();
            if !removed {
                tracing::debug!(path = %path.display(), "evict: file already gone");
            }
            self.stats.record_eviction(entry.bytes);
            tracing::debug!(
                path = %path.display(),
                bytes = entry.bytes,
                remaining = inner.bytes_total,
                target = self.target_bytes,
                "cache evicted"
            );
            if inner.bytes_total + inner.inflight_bytes <= self.target_bytes {
                return;
            }
        }
    }
}

/// `clear` の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClearOutcome {
    pub removed_files: u64,
    pub removed_bytes: u64,
}

/// 一時ファイルライター。Drop時 (未コミットなら) 一時ファイルを削除し、
/// ピンとin-flight会計を解除する。
pub struct TempWriter {
    cache: Arc<ThumbnailCache>,
    key_hex: String,
    final_path: PathBuf,
    tmp_path: PathBuf,
    file: Option<File>,
    /// コミット開始時にin-flight会計したバイト数。
    accounted: u64,
    committed: bool,
}

impl TempWriter {
    /// データを書き込み、fsync後にアトミックrenameで最終配置する。
    /// 成功時はファイル容量を返す。
    pub fn commit(mut self, data: &[u8]) -> io::Result<u64> {
        let result = self.commit_inner(data);
        match result {
            Ok(len) => {
                self.committed = true;
                Ok(len)
            }
            Err(e) => Err(e), // Dropガードが後始末する
        }
    }

    fn commit_inner(&mut self, data: &[u8]) -> io::Result<u64> {
        let len = data.len() as u64;
        let mut file = self
            .file
            .take()
            .ok_or_else(|| io::Error::other("temp file already consumed"))?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        // ディスクに乗った分を先に会計 (保守方向: 一時ファイルも上限に数える)
        {
            let mut inner = lock(&self.cache.inner);
            inner.inflight_bytes += len;
        }
        self.accounted = len;
        // 同一ディレクトリ内rename = アトミック配置
        fs::rename(&self.tmp_path, &self.final_path)?;
        let (bytes_total, clock) = {
            let mut inner = lock(&self.cache.inner);
            inner.inflight_bytes = inner.inflight_bytes.saturating_sub(len);
            inner.bytes_total += len;
            inner.access_clock += 1;
            let clock = inner.access_clock;
            inner.ledger.insert(
                self.final_path.clone(),
                LedgerEntry {
                    bytes: len,
                    access_seq: clock,
                    hex: self.key_hex.clone(),
                },
            );
            (inner.bytes_total, clock)
        };
        // ディレクトリエントリのfsync (クラッシュ一貫性。失敗は無視)
        if let Ok(dir) = File::open(self.final_path.parent().unwrap_or(Path::new("."))) {
            let _ = dir.sync_all();
        }
        tracing::debug!(
            path = %self.final_path.display(),
            bytes = len,
            access_seq = clock,
            bytes_total,
            "cache committed"
        );
        let mut inner = lock(&self.cache.inner);
        unpin_locked(&mut inner, &self.key_hex);
        inner.pinned_tmp.remove(&self.tmp_path);
        self.cache.enforce_limit_locked(&mut inner);
        Ok(len)
    }
}

impl Drop for TempWriter {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(&self.tmp_path);
        let mut inner = lock(&self.cache.inner);
        inner.inflight_bytes = inner.inflight_bytes.saturating_sub(self.accounted);
        unpin_locked(&mut inner, &self.key_hex);
        inner.pinned_tmp.remove(&self.tmp_path);
    }
}

fn unpin_locked(inner: &mut CacheInner, key_hex: &str) {
    if let Some(count) = inner.pinned.get_mut(key_hex) {
        *count -= 1;
        if *count == 0 {
            inner.pinned.remove(key_hex);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gento-cache-test-{}-{}-{}",
                name,
                std::process::id(),
                seq
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// キャッシュルート配下の全ファイルパス。
        fn all_files(&self) -> Vec<PathBuf> {
            let mut out = Vec::new();
            for shard in fs::read_dir(&self.0).unwrap() {
                let shard = shard.unwrap();
                if shard.file_type().unwrap().is_dir() {
                    for f in fs::read_dir(shard.path()).unwrap() {
                        out.push(f.unwrap().path());
                    }
                } else {
                    out.push(shard.path());
                }
            }
            out.sort();
            out
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fp(path: &str, size: u64, mtime: u64) -> AssetFingerprint {
        AssetFingerprint {
            canonical_path: path.to_string(),
            byte_size: size,
            mtime_ms: mtime,
        }
    }

    fn cache(dir: &TestDir, max: u64) -> Arc<ThumbnailCache> {
        Arc::new(
            ThumbnailCache::open_with_limit(dir.path(), max, Arc::new(ThumbStats::new())).unwrap(),
        )
    }

    fn commit(c: &Arc<ThumbnailCache>, key: &CacheKey, data: &[u8]) -> u64 {
        c.begin_write(key).unwrap().commit(data).unwrap()
    }

    #[test]
    fn key_is_deterministic() {
        let a = cache_key(&fp("/x/a.png", 10, 20), SizeClass::S256);
        let b = cache_key(&fp("/x/a.png", 10, 20), SizeClass::S256);
        assert_eq!(a, b);
        assert_eq!(a.hex.len(), 32);
        assert!(a.hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn key_changes_when_any_component_changes() {
        let base = cache_key(&fp("/x/a.png", 10, 20), SizeClass::S256);
        assert_ne!(
            cache_key(&fp("/x/b.png", 10, 20), SizeClass::S256).hex,
            base.hex
        );
        assert_ne!(
            cache_key(&fp("/x/a.png", 11, 20), SizeClass::S256).hex,
            base.hex
        );
        assert_ne!(
            cache_key(&fp("/x/a.png", 10, 21), SizeClass::S256).hex,
            base.hex
        );
        assert_ne!(
            cache_key(&fp("/x/a.png", 10, 20), SizeClass::S512).hex,
            base.hex
        );
        // サイズクラス違いは別ファイル名 (衝突しない)
        assert_ne!(
            cache_key(&fp("/x/a.png", 10, 20), SizeClass::S512).file_name,
            base.file_name
        );
    }

    #[test]
    fn file_name_format_charset_and_length() {
        for class in SizeClass::ALL {
            let key = cache_key(&fp("/some/long/path/画像ファイル.png", 12345, 999), class);
            assert!(validate_file_name(&key.file_name), "{}", key.file_name);
            assert!(key.file_name.len() <= MAX_FILE_NAME_LEN);
            // 形式: {32hex}-{file_id}-{version}.webp
            let stem = key.file_name.strip_suffix(".webp").unwrap();
            let parts: Vec<&str> = stem.split('-').collect();
            assert_eq!(parts.len(), 3, "{}", key.file_name);
            assert_eq!(parts[0].len(), 32);
            assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()));
            assert_eq!(parts[1], class.file_id());
            assert_eq!(parts[2], ALGORITHM_VERSION.to_string());
        }
    }

    #[test]
    fn shard_dir_is_first_two_hex_chars() {
        let key = cache_key(&fp("/x/a.png", 1, 1), SizeClass::S256);
        assert_eq!(
            key.relative,
            PathBuf::from(&key.hex[..2]).join(&key.file_name)
        );
    }

    #[test]
    fn fingerprint_canonicalizes_real_paths() {
        let dir = TestDir::new("fingerprint");
        let file = dir.path().join("a.png");
        fs::write(&file, b"hi").unwrap();
        let direct = asset_fingerprint(&file, 1, 1);
        let obfuscated = dir.path().join(".").join("a.png");
        let via_dots = asset_fingerprint(&obfuscated, 1, 1);
        assert_eq!(direct, via_dots);
        // 存在しないパスはフォールバックでも決定的
        let missing1 = asset_fingerprint(Path::new("/nope/x.png"), 1, 1);
        let missing2 = asset_fingerprint(Path::new("/nope/x.png"), 1, 1);
        assert_eq!(missing1, missing2);
    }

    #[test]
    fn miss_then_hit_after_commit() {
        let dir = TestDir::new("hitmiss");
        let c = cache(&dir, 1 << 20);
        let key = cache_key(&fp("/x/a.png", 1, 1), SizeClass::S256);
        assert_eq!(c.lookup(&key), CacheLookup::Miss);
        commit(&c, &key, b"webp-bytes");
        let hit = match c.lookup(&key) {
            CacheLookup::Hit(p) => p,
            CacheLookup::Miss => panic!("expected hit"),
        };
        assert_eq!(hit, dir.path().join(&key.relative));
        assert_eq!(fs::read(&hit).unwrap(), b"webp-bytes");
    }

    #[test]
    fn dropped_writer_removes_temp_file() {
        let dir = TestDir::new("dropguard");
        let c = cache(&dir, 1 << 20);
        let key = cache_key(&fp("/x/a.png", 1, 1), SizeClass::S256);
        {
            let mut writer = c.begin_write(&key).unwrap();
            writer.file.as_mut().unwrap().write_all(b"partial").unwrap();
            // commitせずにdrop
        }
        assert!(dir.all_files().is_empty());
        assert_eq!(c.lookup(&key), CacheLookup::Miss);
        assert_eq!(c.total_bytes(), 0);
    }

    #[test]
    fn stray_temp_file_is_not_a_hit() {
        let dir = TestDir::new("straytmp");
        let c = cache(&dir, 1 << 20);
        let key = cache_key(&fp("/x/a.png", 1, 1), SizeClass::S256);
        let shard = dir.path().join(&key.hex[..2]);
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join(".tmp-123-4.webp"), b"half-written").unwrap();
        assert_eq!(c.lookup(&key), CacheLookup::Miss);
    }

    #[test]
    fn accounting_tracks_committed_bytes() {
        let dir = TestDir::new("accounting");
        let c = cache(&dir, 1 << 20);
        let k1 = cache_key(&fp("/x/a.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/b.png", 2, 2), SizeClass::S256);
        commit(&c, &k1, &[0u8; 100]);
        commit(&c, &k2, &[0u8; 200]);
        assert_eq!(c.total_bytes(), 300);
        assert_eq!(c.entry_count(), 2);
    }

    #[test]
    fn eviction_to_target_in_lru_order() {
        let dir = TestDir::new("evict");
        // max=100, target=95
        let c = cache(&dir, 100);
        let k1 = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/2.png", 2, 2), SizeClass::S256);
        let k3 = cache_key(&fp("/x/3.png", 3, 3), SizeClass::S256);
        commit(&c, &k1, &[0u8; 40]);
        commit(&c, &k2, &[0u8; 40]);
        commit(&c, &k3, &[0u8; 40]); // 120 > 100 → k1 (最も使われていない) が退避され 80 ≤ 95
        assert_eq!(c.total_bytes(), 80);
        assert_eq!(c.entry_count(), 2);
        assert_eq!(c.lookup(&k1), CacheLookup::Miss);
        assert!(matches!(c.lookup(&k2), CacheLookup::Hit(_)));
        assert!(matches!(c.lookup(&k3), CacheLookup::Hit(_)));
    }

    #[test]
    fn access_refresh_changes_eviction_victim() {
        let dir = TestDir::new("lru-refresh");
        let c = cache(&dir, 100);
        let k1 = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/2.png", 2, 2), SizeClass::S256);
        let k3 = cache_key(&fp("/x/3.png", 3, 3), SizeClass::S256);
        commit(&c, &k1, &[0u8; 40]);
        commit(&c, &k2, &[0u8; 40]);
        // k1を再アクセス → k2が最も古い
        assert!(matches!(c.lookup(&k1), CacheLookup::Hit(_)));
        commit(&c, &k3, &[0u8; 40]);
        assert_eq!(c.total_bytes(), 80);
        assert!(matches!(c.lookup(&k1), CacheLookup::Hit(_)));
        assert_eq!(c.lookup(&k2), CacheLookup::Miss);
    }

    #[test]
    fn pinned_key_survives_eviction() {
        let dir = TestDir::new("pinned");
        let c = cache(&dir, 100);
        let k1 = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/2.png", 2, 2), SizeClass::S256);
        commit(&c, &k1, &[0u8; 40]);
        // k1をピン (生成中ジョブ相当。書き込みはまだ)
        let _writer = c.begin_write(&k1).unwrap();
        commit(&c, &k2, &[0u8; 40]);
        let k3 = cache_key(&fp("/x/3.png", 3, 3), SizeClass::S256);
        commit(&c, &k3, &[0u8; 40]); // 120 > 100。k1はピン → k2が退避
        assert!(matches!(c.lookup(&k1), CacheLookup::Hit(_)));
        assert_eq!(c.lookup(&k2), CacheLookup::Miss);
        // writerdrop後もファイルは残る (コミット済みファイルのため)
    }

    #[test]
    fn startup_recovers_ledger_and_evicts_by_mtime() {
        let dir = TestDir::new("startup");
        // キャッシュオブジェクトを介さず直接ファイルを作る (前回クラッシュ模擬)
        let old = cache_key(&fp("/x/old.png", 1, 1), SizeClass::S256);
        let mid = cache_key(&fp("/x/mid.png", 2, 2), SizeClass::S256);
        let new = cache_key(&fp("/x/new.png", 3, 3), SizeClass::S256);
        let shard = dir.path().join(&old.hex[..2]);
        fs::create_dir_all(&shard).unwrap();
        fs::create_dir_all(dir.path().join(&mid.relative).parent().unwrap()).unwrap();
        fs::create_dir_all(dir.path().join(&new.relative).parent().unwrap()).unwrap();
        fs::write(dir.path().join(&old.relative), [0u8; 60]).unwrap();
        fs::write(dir.path().join(&mid.relative), [0u8; 60]).unwrap();
        fs::write(dir.path().join(&new.relative), [0u8; 60]).unwrap();
        // mtime: old < mid < new (書き込み順)
        set_mtime(dir.path().join(&old.relative), 1000);
        set_mtime(dir.path().join(&mid.relative), 2000);
        set_mtime(dir.path().join(&new.relative), 3000);
        // 孤立一時ファイル
        fs::write(shard.join(".tmp-999-9.webp"), b"junk").unwrap();
        // max=150 → 起動時点で退避し target=142 まで → old (mtime最古) が退避
        let c = cache(&dir, 150);
        assert_eq!(c.total_bytes(), 120);
        assert_eq!(c.entry_count(), 2);
        assert_eq!(c.lookup(&old), CacheLookup::Miss);
        assert!(matches!(c.lookup(&mid), CacheLookup::Hit(_)));
        assert!(matches!(c.lookup(&new), CacheLookup::Hit(_)));
        // 孤立一時ファイルは削除済み
        assert!(!shard.join(".tmp-999-9.webp").exists());
    }

    #[test]
    fn clear_removes_all_but_protects_inflight_temp() {
        let dir = TestDir::new("clear");
        let c = cache(&dir, 1 << 20);
        let k1 = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/2.png", 2, 2), SizeClass::S256);
        commit(&c, &k1, &[1u8; 50]);
        // k2の生成中 (一時ファイル作成済み・未コミット)
        let mut writer = c.begin_write(&k2).unwrap();
        writer.file.as_mut().unwrap().write_all(&[2u8; 30]).unwrap();
        let outcome = c.clear().unwrap();
        assert_eq!(outcome.removed_files, 1);
        assert_eq!(outcome.removed_bytes, 50);
        assert_eq!(c.total_bytes(), 0);
        assert_eq!(c.entry_count(), 0);
        assert_eq!(c.lookup(&k1), CacheLookup::Miss);
        // 生成中の書き込みはclear後もコミットできる (テンポラリは保護された)
        let len = writer.commit(&[2u8; 30]).unwrap();
        assert_eq!(len, 30);
        assert!(matches!(c.lookup(&k2), CacheLookup::Hit(_)));
        assert_eq!(c.total_bytes(), 30);
    }

    #[test]
    fn hit_touch_updates_access_order_for_future_eviction() {
        // lookupが台帳access_seqを更新することの直接的確認
        let dir = TestDir::new("touch");
        let c = cache(&dir, 1000);
        let k1 = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let k2 = cache_key(&fp("/x/2.png", 2, 2), SizeClass::S256);
        commit(&c, &k1, &[0u8; 40]);
        commit(&c, &k2, &[0u8; 40]);
        assert!(matches!(c.lookup(&k1), CacheLookup::Hit(_)));
        let inner = lock(&c.inner);
        let seq1 = inner.ledger[&dir.path().join(&k1.relative)].access_seq;
        let seq2 = inner.ledger[&dir.path().join(&k2.relative)].access_seq;
        drop(inner);
        assert!(seq1 > seq2, "lookup should refresh access_seq");
    }

    #[test]
    fn commit_failure_leaves_no_temp() {
        // 失敗注入: 最終パスをディレクトリで塞ぎ、renameを確実に失敗させる
        // (shardを読み取り専用にする方法はFile::create時点で失敗するため使えない)
        let dir = TestDir::new("failinject");
        let c = cache(&dir, 1 << 20);
        let key = cache_key(&fp("/x/1.png", 1, 1), SizeClass::S256);
        let final_path = dir.path().join(&key.relative);
        fs::create_dir_all(&final_path).unwrap();
        fs::write(final_path.join("blocker"), b"x").unwrap();
        let result = c.begin_write(&key).unwrap().commit(b"payload");
        assert!(result.is_err());
        // Dropガードにより一時ファイルは残っていない
        let shard = final_path.parent().unwrap();
        let names: Vec<String> = fs::read_dir(shard)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![key.file_name.clone()]); // 塞ぎ用ディレクトリのみ
        assert!(names.iter().all(|n| !n.starts_with(".tmp-")));
        // 会計にも残らない
        assert_eq!(c.total_bytes(), 0);
        assert_eq!(c.entry_count(), 0);
    }

    #[cfg(unix)]
    fn set_mtime(path: PathBuf, ms_since_epoch: u64) {
        use std::time::{Duration, SystemTime};
        let time = SystemTime::UNIX_EPOCH + Duration::from_millis(ms_since_epoch);
        let f = File::options().write(true).open(&path).unwrap();
        f.set_modified(time).unwrap();
    }
}
