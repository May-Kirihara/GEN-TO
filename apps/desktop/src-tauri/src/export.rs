//! SD学習用エクスポート (MVP WP-14)。
//!
//! 選択/コレクション等で解決済みのasset id列 + 出力先 + 構造オプションを
//! 入力に、画像コピーと同名`.txt` (現行caption・無ければタグ列", "結合)
//! を出力先へ書き出す。
//!
//! # 安全規約 (PM承認済み設計)
//!
//! - **元ファイル・DBは完全不変**: ソース読取はすべて
//!   [`gento_core::open_regular_beneath`] (登録ルートfdからの全コンポーネント
//!   歩行。FG-WP13-2)。DB参照は `with_reader` のみ (reader接続は
//!   `PRAGMA query_only` で書き込みSQLを構造的に拒否)。
//! - **無断上書き厳禁**: 出力先に同名既存ファイルがある場合、サイズ一致なら
//!   内容比較し、一致=「既に書き出し済み」でskip (冪等)、不一致=skip +
//!   衝突報告。新規書き出しは tmp書き → no-clobber rename による原子公開
//!   (上書き競合は失敗扱い)。tmp名は fileops (WP-7) と同一の厳密形式
//!   `.{名前}.gento-tmp-{32hex}-{6桁seq}` — 出力先が登録ルート内だった
//!   場合に起動時掃除の対象となり、クラッシュ残骸が片付く。
//!   **failure経路でtmpをpathname unlinkしない** (FG-WP16-5 I2) —
//!   失敗時の残留は自己識別的なdotfileとして許容し、成功時はrenameが
//!   tmpを消費する (create_exclusiveのEEXIST失敗で他人のentryを消す・
//!   rename失敗後に差し替えられたentryを消す、いずれのTOCTOUも構造的
//!   に存在しない)。
//! - **出力経路の包含 (FG-WP16-3 B1)**: 出力rootは `O_DIRECTORY|O_NOFOLLOW`
//!   でfd固定 (root最終成分がsymlinkなら拒否)。サブディレクトリは
//!   `openat(O_NOFOLLOW)` 成分歩行 + `mkdirat` 競合安全作成、書き出し
//!   (tmp→sync→`renameat2 NOREPLACE`) はすべてdir fd相対。出力dir内の
//!   既存symlink (`DEST/sub -> /outside`) を辿った外部書き出しは構造的に
//!   起きない (該当項目はfailedとして報告)。
//! - **代表fileの決定論**: 同一assetの複数file (重複登録) は
//!   `scan_state='present'` の最小file id (無ければ全体の最小id) を代表に。
//! - **命名の決定論**: 出力名 = 代表fileのdisplay_name。異なるasset間の
//!   同名衝突は asset uuid 辞書順で `{stem}-{n}.{ext}` suffixを振る
//!   (再実行で同一結果)。
//! - **manifest**: `export-manifest.json` を出力dirへ
//!   tmp→sync→`renameat2 NOREPLACE` で出版。**既存ファイルは所有者を
//!   問わず (GEN-TO自身の旧manifestも含め) 一切削除・上書きしない**
//!   (FG-WP16-4 R3: 所有確認→unlink間の差し替え競合でユーザーファイル
//!   を消しうるTOCTOUを、unlink自体をなくす方針で解消。FG-WP16-5 I2:
//!   failure経路のtmp unlinkも全廃 — 失敗時のtmp残留は自己識別的
//!   dotfileとして許容)。正規名が占有されていれば回避名
//!   `export-manifest-{run uuid}.json` へ出版し、結果DTOの `manifestPath`
//!   が実際のパスを報告する。
//!   キャンセル・部分失敗も含めた全エントリを記録する。
//! - キャンセルは協調 ([`ExportService::cancel`] → AtomicBool、per-item境界)。
//!   キャンセル時もそれまでの成果 + manifestは残す。

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo};
use gento_core::repo::{AssetRepo, FileRecord, FileRepo, LibraryRootRepo, ScanState};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::catalog::AppState;
use crate::metadata::{current_mode, parse_stable_asset_ids};

/// 出力manifestのファイル名。
pub const EXPORT_MANIFEST_FILE: &str = "export-manifest.json";
/// manifest形式version。
const EXPORT_MANIFEST_VERSION: u32 = 1;
/// 結果DTOに載せる失敗/衝突エントリの上限 (全量はmanifest参照)。
const ISSUE_REPORT_LIMIT: usize = 100;

// ===== ドメイン型 (DTOでない内部表現) =====

/// フォルダ構造オプション。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStructure {
    /// 代表fileのrel_path親dir構造を出力先に再現。
    Preserve,
    /// 全て出力直下へ (名前衝突はsuffixで解決)。
    Flat,
}

impl ExportStructure {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExportStructure::Preserve => "preserve",
            ExportStructure::Flat => "flat",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "preserve" => Ok(ExportStructure::Preserve),
            "flat" => Ok(ExportStructure::Flat),
            other => Err(format!("不明なフォルダ構造です: {other} (preserve|flat)")),
        }
    }
}

/// `.txt` の内容の由来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionOrigin {
    SdTags,
    Description,
    Tags,
    None,
}

impl CaptionOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            CaptionOrigin::SdTags => "caption_sd_tags",
            CaptionOrigin::Description => "caption_description",
            CaptionOrigin::Tags => "tags",
            CaptionOrigin::None => "none",
        }
    }
}

/// 出力先の既存ファイルとの関係 (plan/実行時判定)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingState {
    /// 存在しない (新規書き出し)。
    NotExists,
    /// 存在し内容一致 (= 既に書き出し済み。skip冪等)。
    Identical,
    /// 存在するが内容不一致 (skip + 衝突報告)。
    Conflict,
}

/// plan済みの書き出し項目 (1 asset = 1項目)。
#[derive(Debug, Clone)]
pub struct PlannedItem {
    pub asset_id: i64,
    pub asset_uuid: String,
    /// 代表fileの表示名 (出力ファイル名のbase)。
    pub display_name: String,
    /// 登録ルートの絶対パス (path_safety歩行のroot)。
    pub root_path: PathBuf,
    /// 代表fileのrel_path成分列 (ソース読取 + Preserve構造の親dir)。
    pub rel_components: Vec<OsString>,
    /// 表示用ソースパス (lossy)。
    pub source_display: String,
    /// 出力ファイル名 (衝突suffix解決済み)。
    pub output_name: OsString,
    /// Preserve構造時の出力サブディレクトリ成分 (ファイル名を除く)。
    pub subdir: Vec<OsString>,
    /// `.txt` 本文。
    pub caption: String,
    pub caption_origin: CaptionOrigin,
}

/// plan段階の失敗 (asset不在・file不在・経路不正等)。
#[derive(Debug, Clone)]
pub struct PlannedError {
    pub asset_id: i64,
    pub reason: String,
}

/// 出力計画 (dry-run = この内容 + 既存衝突分類。書き込みは行わない)。
#[derive(Debug, Clone)]
pub struct ExportPlan {
    pub dest_dir: PathBuf,
    pub structure: ExportStructure,
    /// asset uuid昇順 (命名割当の決定論)。
    pub items: Vec<PlannedItem>,
    pub errors: Vec<PlannedError>,
}

/// 1ファイル (画像 or .txt) の処理結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    Written,
    IdenticalSkip,
    Conflict,
    Failed(String),
}

impl FileOutcome {
    pub const fn as_str(&self) -> &'static str {
        match self {
            FileOutcome::Written => "written",
            FileOutcome::IdenticalSkip => "identical",
            FileOutcome::Conflict => "conflict",
            FileOutcome::Failed(_) => "failed",
        }
    }
}

/// 実行済み項目の結果。
#[derive(Debug, Clone)]
pub struct ItemResult {
    pub asset_id: i64,
    pub asset_uuid: String,
    pub display_name: String,
    /// 表示用ソースパス (lossy)。
    pub source_display: String,
    /// 出力先相対の画像パス (lossy・'/'結合の表示用)。
    pub output_rel: String,
    /// 出力先相対の.txtパス。
    pub txt_rel: String,
    pub image: FileOutcome,
    pub txt: FileOutcome,
    pub caption_origin: CaptionOrigin,
}

impl ItemResult {
    /// 項目単位の集約status (進捗イベント・manifest集計で使用)。
    pub fn status(&self) -> &'static str {
        if matches!(self.image, FileOutcome::Failed(_))
            || matches!(self.txt, FileOutcome::Failed(_))
        {
            "failed"
        } else if self.image == FileOutcome::Conflict || self.txt == FileOutcome::Conflict {
            "conflict"
        } else if self.image == FileOutcome::IdenticalSkip && self.txt == FileOutcome::IdenticalSkip
        {
            "identical"
        } else {
            "exported"
        }
    }
}

/// 実行結果 (manifest出版前の内部表現)。
#[derive(Debug, Clone)]
pub struct ExportRun {
    pub dest_dir: PathBuf,
    pub structure: ExportStructure,
    pub items: Vec<ItemResult>,
    pub planning_errors: Vec<PlannedError>,
    pub cancelled: bool,
    /// キャンセルで処理しなかった残り項目数。
    pub skipped_by_cancel: u64,
}

// ===== 純粋関数 (命名・構造。vitestミラーの正本) =====

/// 画像ファイル名から `.txt` ファイル名を導出 (stem + ".txt")。
/// 拡張子なしの名前には ".txt" を付ける。
pub fn txt_file_name(image_name: &OsStr) -> OsString {
    let text = image_name.to_string_lossy().into_owned();
    let stem = Path::new(&text)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(text);
    OsString::from(format!("{stem}.txt"))
}

/// fdをストリーム読みしBLAKE3 hex (プレフィックスなし) を算出後、先頭へ巻き戻す。
fn hash_and_rewind(file: &mut File) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    Some(hasher.finalize().to_hex().to_string())
}

/// tmp名 (fileops WP-7と同一の厳密形式。出力先が登録ルート内なら起動時
/// 掃除の対象となる)。NAME_MAX超過は File 作成のエラーに任せる。
fn tmp_name_for(final_name: &OsStr, run_uuid: &str, seq: &mut u32) -> OsString {
    let name = final_name.to_string_lossy();
    let out = format!(".{name}.gento-tmp-{run_uuid}-{:06}", seq);
    *seq += 1;
    OsString::from(out)
}

/// ディレクトリハンドル配下の既存画像の分類 (ソース実体fdと比較)。
/// サイズ一致時のみBLAKE3ストリーム比較。ソースfdは比較後に先頭へ巻き戻す。
/// 最終成分がsymlink・判定不能は安全側 (Conflict = 書き込まない)。
fn classify_file_at(
    dir: &gento_core::OutputDir,
    name: &OsStr,
    source: &mut File,
    source_len: u64,
) -> ExistingState {
    let (mut existing, md) = match dir.open_regular_at(name) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ExistingState::NotExists,
        // symlink最終成分 (ELOOP)・権限等 → 安全側 (書き込まない)
        Err(_) => return ExistingState::Conflict,
    };
    if !md.is_file() || md.len() != source_len {
        return ExistingState::Conflict;
    }
    let Some(target_hash) = hash_and_rewind(&mut existing) else {
        return ExistingState::Conflict;
    };
    let Some(source_hash) = hash_and_rewind(source) else {
        return ExistingState::Conflict;
    };
    if target_hash == source_hash {
        ExistingState::Identical
    } else {
        ExistingState::Conflict
    }
}

/// ディレクトリハンドル配下の既存 `.txt` の分類 (期待bytesと直接比較)。
fn classify_bytes_at(dir: &gento_core::OutputDir, name: &OsStr, expect: &[u8]) -> ExistingState {
    match dir.open_regular_at(name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ExistingState::NotExists,
        Err(_) => ExistingState::Conflict,
        Ok((mut file, md)) => {
            if !md.is_file() || md.len() != expect.len() as u64 {
                return ExistingState::Conflict;
            }
            let mut buf = Vec::with_capacity(expect.len());
            if file.read_to_end(&mut buf).is_err() {
                return ExistingState::Conflict;
            }
            if buf == expect {
                ExistingState::Identical
            } else {
                ExistingState::Conflict
            }
        }
    }
}

/// 書き出し結果の失敗分類 (publish共通)。
enum PublishIssue {
    /// 宛先既存 (上書きしない → Conflict)。
    DestinationExists,
    /// その他のIO失敗。
    Failed(std::io::Error),
}

/// ソース (先頭巻き戻し済み) をtmpへcopy → sync → 原子公開
/// (すべてdirハンドルfd相対 — 出力dir包含の担保)。
fn publish_copy_at(
    dir: &gento_core::OutputDir,
    final_name: &OsStr,
    source: &mut File,
    run_uuid: &str,
    seq: &mut u32,
) -> FileOutcome {
    publish_at(dir, final_name, run_uuid, seq, |out| {
        source.seek(SeekFrom::Start(0))?;
        std::io::copy(source, out)?;
        Ok(())
    })
}

/// bytesをtmp書き→sync→原子公開 (.txt用。同様にfd相対)。
fn publish_bytes_at(
    dir: &gento_core::OutputDir,
    final_name: &OsStr,
    bytes: &[u8],
    run_uuid: &str,
    seq: &mut u32,
) -> FileOutcome {
    publish_at(dir, final_name, run_uuid, seq, |out| out.write_all(bytes))
}

/// 公共のtmp書き→sync→no-clobber rename。上書き競合
/// (DestinationExists) はConflict、その他失敗はFailed。
/// FG-WP16-5 I2: **失敗経路でtmpをunlinkしない** — pathnameによる削除は
/// 一切行わない (作成直後のtmp entryも、失敗までの間に他actorが差し替え
/// ていれば自分のものではないため)。tmp名はrun uuid+seqで一意かつ自己
/// 識別的 (`.{名前}.gento-tmp-{32hex}-{6桁seq}`) なため、失敗時の
/// dotfile残留を許容する (成功時はrenameがtmpを消費。出力先が登録
/// ルート内なら起動時掃除の対象)。
fn publish_at(
    dir: &gento_core::OutputDir,
    final_name: &OsStr,
    run_uuid: &str,
    seq: &mut u32,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> FileOutcome {
    let tmp = tmp_name_for(final_name, run_uuid, seq);
    let result = (|| -> Result<(), PublishIssue> {
        // create失敗 (tmp名の事前占有を含む) はそのまま失敗 — 既存entryは不接触
        let mut out = dir
            .create_exclusive_at(&tmp)
            .map_err(PublishIssue::Failed)?;
        let written = write(&mut out);
        let synced = written.and_then(|()| out.sync_all());
        drop(out);
        synced.map_err(PublishIssue::Failed)?;
        match dir.rename_noreplace_at(&tmp, final_name) {
            Ok(()) => Ok(()),
            Err(gento_core::RenameAtNoReplaceError::DestinationExists) => {
                Err(PublishIssue::DestinationExists)
            }
            Err(e) => Err(PublishIssue::Failed(std::io::Error::other(e.to_string()))),
        }
    })();
    match result {
        Ok(()) => FileOutcome::Written,
        Err(PublishIssue::DestinationExists) => FileOutcome::Conflict,
        Err(PublishIssue::Failed(e)) => FileOutcome::Failed(format!(
            "{}/{}: {e}",
            dir.display(),
            final_name.to_string_lossy()
        )),
    }
}

/// 1項目 (画像 + .txt) の書き出し。単一失敗で他方へ影響させない。
/// subdirのopen-or-createはsymlink成分を拒否する (包含担保) —
/// その場合も両方Failedとして報告し、外部へは書かない。
fn export_one(
    item: &PlannedItem,
    root: &gento_core::OutputDir,
    run_uuid: &str,
    seq: &mut u32,
) -> (FileOutcome, FileOutcome) {
    let txt_name = txt_file_name(&item.output_name);
    let dir = match root.open_or_create_subdir(&item.subdir) {
        Ok(dir) => dir,
        Err(e) => {
            let reason = format!(
                "{}/{}: {e}",
                root.display(),
                rel_display(&item.subdir, &OsString::new())
            );
            return (
                FileOutcome::Failed(reason.clone()),
                FileOutcome::Failed(reason),
            );
        }
    };
    let image = match open_source(item) {
        Ok((mut source, md)) => {
            match classify_file_at(&dir, &item.output_name, &mut source, md.len()) {
                ExistingState::NotExists => {
                    publish_copy_at(&dir, &item.output_name, &mut source, run_uuid, seq)
                }
                ExistingState::Identical => FileOutcome::IdenticalSkip,
                ExistingState::Conflict => FileOutcome::Conflict,
            }
        }
        Err(reason) => FileOutcome::Failed(reason),
    };
    let txt_bytes = item.caption.as_bytes();
    let txt = match classify_bytes_at(&dir, &txt_name, txt_bytes) {
        ExistingState::NotExists => publish_bytes_at(&dir, &txt_name, txt_bytes, run_uuid, seq),
        ExistingState::Identical => FileOutcome::IdenticalSkip,
        ExistingState::Conflict => FileOutcome::Conflict,
    };
    (image, txt)
}

/// 衝突suffix付き名前 `{stem}-{n}.{ext}` (拡張子なしは `{stem}-{n}`)。
pub fn suffixed_name(image_name: &OsStr, n: u32) -> OsString {
    let text = image_name.to_string_lossy().into_owned();
    let path = Path::new(&text);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| text.clone());
    match path.extension() {
        Some(ext) => OsString::from(format!("{stem}-{n}.{}", ext.to_string_lossy())),
        None => OsString::from(format!("{stem}-{n}")),
    }
}

/// 出力ファイル名の割当: `used` に未使用の名前を `base` から決定論的に
/// 選ぶ (base → base-1 → base-2 …)。割当たら `used` へ登録して返す。
pub fn allocate_output_name(base: &OsStr, used: &mut HashSet<OsString>) -> OsString {
    if used.insert(base.to_os_string()) {
        return base.to_os_string();
    }
    let mut n: u32 = 1;
    loop {
        let candidate = suffixed_name(base, n);
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// 出力ファイル名/ディレクトリ成分として不正な値の検査。
/// (display_nameは走査由来の単一成分だが、防御的に空・`.`・`..`・区切り
/// 文字・NULを拒否する)
fn is_valid_output_component(component: &OsStr) -> bool {
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(component);
    // 非unix: lossy UTF-8列で字句検証する (NUL・区切りの検出目的には足りる。
    // to_string_lossyの一時値は束縛して寿命を確保する)。
    #[cfg(not(unix))]
    let owned = component.to_string_lossy();
    #[cfg(not(unix))]
    let bytes = owned.as_bytes();
    !bytes.is_empty()
        && bytes != b"."
        && bytes != b".."
        && !bytes.contains(&0)
        && !bytes.contains(&b'/')
        && !bytes.contains(&b'\\')
}

/// rel_pathを出力・読取に使える成分列へ検証付きで分解
/// (全成分がNormalであること — `..` 等を構造的に拒否)。
fn rel_components_of(rel: &Path) -> Option<Vec<OsString>> {
    let mut out = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => {
                if !is_valid_output_component(part) {
                    return None;
                }
                out.push(part.to_os_string());
            }
            _ => return None,
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// 同一assetの重複登録から代表fileを選ぶ:
/// `scan_state='present'` の最小file id、presentが無ければ全体の最小id。
pub fn representative_file(files: &[FileRecord]) -> Option<FileRecord> {
    files
        .iter()
        .filter(|f| f.scan_state == ScanState::Present)
        .min_by_key(|f| f.id)
        .or_else(|| files.iter().min_by_key(|f| f.id))
        .cloned()
}

// ===== plan (dry-run) =====

/// asset id列から出力計画を組む。書き込みは一切行わない
/// (既存衝突分類のために出力先とソースを読み取り専用で開くことはある)。
pub fn plan_export(
    db: &gento_db::Db,
    asset_ids: &[i64],
    dest_dir: &Path,
    structure: ExportStructure,
) -> ExportPlan {
    // 1) 重複排除 (1 asset = 1出力) + asset解決
    let mut unique: Vec<i64> = asset_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut errors: Vec<PlannedError> = Vec::new();
    let mut resolved: Vec<(String, i64)> = Vec::new();
    for id in &unique {
        match db.with_reader(|repos| repos.get_asset(*id)) {
            Ok(Some(asset)) => resolved.push((asset.uuid, asset.id)),
            Ok(None) => errors.push(PlannedError {
                asset_id: *id,
                reason: "Assetが見つかりません (削除された可能性があります)".to_string(),
            }),
            Err(e) => errors.push(PlannedError {
                asset_id: *id,
                reason: format!("データベースエラー: {e}"),
            }),
        }
    }
    // 命名割当の決定論: asset uuid辞書順
    resolved.sort();

    // 2) ルート情報キャッシュ (表示パス + 歩行用絶対パス)
    let roots: std::collections::HashMap<i64, (PathBuf, String)> = db
        .with_reader(|repos| {
            Ok(repos
                .list_roots()?
                .into_iter()
                .map(|r| (r.id, (r.path, r.display_path)))
                .collect())
        })
        .unwrap_or_default();

    // 3) 項目構築 (名前割当前)
    let mut items: Vec<PlannedItem> = Vec::with_capacity(resolved.len());
    for (asset_uuid, asset_id) in &resolved {
        let planned = db
            .with_reader(|repos| {
                Ok::<_, gento_core::repo::RepoError>(plan_one_item(
                    repos, *asset_id, asset_uuid, &roots, structure,
                ))
            })
            .unwrap_or_else(|e| Err(format!("データベースエラー: {e}")));
        match planned {
            Ok(mut item) => {
                item.output_name = OsString::from(item.display_name.clone());
                items.push(item);
            }
            Err(reason) => errors.push(PlannedError {
                asset_id: *asset_id,
                reason,
            }),
        }
    }

    // 4) 出力名割当 (サブdirごとのused集合。uuid順 = items挿入順で決定論)
    let mut used: UsedNamesByDir = BTreeMap::new();
    for item in &mut items {
        let entry = used.entry(item.subdir.clone()).or_default();
        let base = item.output_name.clone();
        item.output_name = allocate_output_name(&base, entry);
    }

    ExportPlan {
        dest_dir: dest_dir.to_path_buf(),
        structure,
        items,
        errors,
    }
}

// 画像/テキスト出力先の既存分類はディレクトリハンドル相対
// (`classify_file_at` / `classify_bytes_at`) で行う — FG-WP16-3 B1。

/// 1 asset分のplan項目を組む (plan_export内の per-asset処理。
/// ユーザー向け日本語エラーはここで確定する)。
fn plan_one_item(
    repos: &gento_db::SqliteRepos<'_>,
    asset_id: i64,
    asset_uuid: &str,
    roots: &std::collections::HashMap<i64, (PathBuf, String)>,
    structure: ExportStructure,
) -> Result<PlannedItem, String> {
    let files = repos
        .list_files_of_asset(asset_id)
        .map_err(|e| format!("データベースエラー: {e}"))?;
    let Some(rep) = representative_file(&files) else {
        return Err(
            "このAssetにファイルが登録されていません (再スキャンをお試しください)".to_string(),
        );
    };
    let Some(root) = roots.get(&rep.root_id) else {
        return Err("登録ルートが見つかりません (解除された可能性があります)".to_string());
    };
    let Some(components) = rel_components_of(&rep.rel_path) else {
        return Err("ソースの相対パスが不正です".to_string());
    };
    let name_os = OsString::from(rep.display_name.clone());
    if !is_valid_output_component(&name_os) {
        return Err("ファイル名を出力に使用できません".to_string());
    }
    // caption: sd_tags現行 → description現行 → タグ列 (normalized_name ASC)
    let (caption, caption_origin) = if let Some(c) = repos
        .get_current_caption(asset_id, CaptionKind::SdTags)
        .map_err(|e| format!("データベースエラー: {e}"))?
    {
        (c.body, CaptionOrigin::SdTags)
    } else if let Some(c) = repos
        .get_current_caption(asset_id, CaptionKind::Description)
        .map_err(|e| format!("データベースエラー: {e}"))?
    {
        (c.body, CaptionOrigin::Description)
    } else {
        let tags = repos
            .list_asset_tags(asset_id)
            .map_err(|e| format!("データベースエラー: {e}"))?;
        if tags.is_empty() {
            (String::new(), CaptionOrigin::None)
        } else {
            let joined = tags
                .iter()
                .map(|t| t.tag.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (joined, CaptionOrigin::Tags)
        }
    };
    let subdir = match structure {
        ExportStructure::Preserve => components[..components.len() - 1].to_vec(),
        ExportStructure::Flat => Vec::new(),
    };
    let source_display = format!(
        "{}/{}",
        root.1,
        rel_display(
            &components[..components.len() - 1],
            &components[components.len() - 1]
        )
    );
    Ok(PlannedItem {
        asset_id,
        asset_uuid: asset_uuid.to_string(),
        display_name: rep.display_name.clone(),
        root_path: root.0.clone(),
        rel_components: components,
        source_display,
        output_name: OsString::new(), // 割当はplan_export後段で一括
        subdir,
        caption,
        caption_origin,
    })
}

/// 実行: planを組み直して (クライアント由来のplanは信頼しない) 項目ごとに
/// 書き出す。単一失敗で続行 (manifestに記録)。cancel は per-item境界で確認。
/// 出力rootはfdピン留め (`OutputDir::open_root` — 最終成分symlink拒否) し、
/// 以降の書き出しはすべてfd相対 (FG-WP16-3 B1の出力包含担保)。
pub fn run_export(
    db: &gento_db::Db,
    asset_ids: &[i64],
    dest_dir: &Path,
    structure: ExportStructure,
    cancel: &AtomicBool,
    progress: &dyn Fn(&ExportProgressDto),
) -> Result<ExportRun, String> {
    let plan = plan_export(db, asset_ids, dest_dir, structure);
    let total = plan.items.len() as u64;
    let root = gento_core::OutputDir::open_root(dest_dir).map_err(|e| {
        // root最終成分がsymlink (ELOOP/ENOTDIR) は分かりやすく報告
        let code = e.raw_os_error();
        if code == Some(libc::ELOOP)
            || code == Some(libc::ENOTDIR)
            || e.to_string().contains("symlink")
        {
            format!(
                "出力先ディレクトリがシンボリックリンクであるため安全に開けません: {} ({e})",
                dest_dir.display()
            )
        } else {
            format!(
                "出力先ディレクトリを作成/オープンできません: {} ({e})",
                dest_dir.display()
            )
        }
    })?;
    let run_uuid = Uuid::now_v7().simple().to_string();
    let mut seq: u32 = 0;
    let mut items: Vec<ItemResult> = Vec::with_capacity(plan.items.len());
    let mut cancelled = false;
    for item in &plan.items {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let (image, txt) = export_one(item, &root, &run_uuid, &mut seq);
        let txt_name = txt_file_name(&item.output_name);
        let result = ItemResult {
            asset_id: item.asset_id,
            asset_uuid: item.asset_uuid.clone(),
            display_name: item.display_name.clone(),
            source_display: item.source_display.clone(),
            output_rel: rel_display(&item.subdir, &item.output_name),
            txt_rel: rel_display(&item.subdir, &txt_name),
            caption_origin: item.caption_origin,
            image,
            txt,
        };
        let status = result.status().to_string();
        let processed = items.len() as u64 + 1;
        let asset_uuid = result.asset_uuid.clone();
        items.push(result);
        progress(&ExportProgressDto {
            processed,
            total,
            asset_uuid,
            status,
        });
    }
    let skipped_by_cancel = total - items.len() as u64;
    Ok(ExportRun {
        dest_dir: dest_dir.to_path_buf(),
        structure,
        items,
        planning_errors: plan.errors,
        cancelled,
        skipped_by_cancel,
    })
}

// ===== 出力manifest =====

/// manifest JSON (on-disk。snake_case — backup artifactと同一規約)。
#[derive(Serialize)]
struct ExportManifest {
    format_version: u32,
    created_at: String,
    cancelled: bool,
    structure: String,
    dest_dir: String,
    asset_count: u64,
    summary: ExportManifestSummary,
    entries: Vec<ExportManifestEntry>,
    errors: Vec<ExportManifestError>,
}

#[derive(Serialize)]
struct ExportManifestSummary {
    exported: u64,
    identical: u64,
    conflict: u64,
    failed: u64,
    planning_errors: u64,
    cancelled: u64,
}

#[derive(Serialize)]
struct ExportManifestEntry {
    asset_id: String,
    asset_uuid: String,
    display_name: String,
    source_display: String,
    output_rel: String,
    caption_source: String,
    image: String,
    image_error: Option<String>,
    txt: String,
    txt_error: Option<String>,
}

#[derive(Serialize)]
struct ExportManifestError {
    asset_id: String,
    reason: String,
}

/// manifestを組み立てて出版し、パスを返す。出版の所有権規約は
/// [`publish_manifest_file`] (FG-WP16-4 R3: 既存ファイルは一切削除・
/// 上書きしない — 占有時は回避名へ出版)。
fn publish_manifest(run: &ExportRun, created_at: &str) -> Result<PathBuf, String> {
    let mut exported = 0u64;
    let mut identical = 0u64;
    let mut conflict = 0u64;
    let mut failed = 0u64;
    let mut entries = Vec::with_capacity(run.items.len());
    for item in &run.items {
        match item.status() {
            "exported" => exported += 1,
            "identical" => identical += 1,
            "conflict" => conflict += 1,
            _ => failed += 1,
        }
        let error_of = |o: &FileOutcome| match o {
            FileOutcome::Failed(reason) => Some(reason.clone()),
            _ => None,
        };
        entries.push(ExportManifestEntry {
            asset_id: item.asset_id.to_string(),
            asset_uuid: item.asset_uuid.clone(),
            display_name: item.display_name.clone(),
            source_display: item.source_display.clone(),
            output_rel: item.output_rel.clone(),
            caption_source: item.caption_origin.as_str().to_string(),
            image: item.image.as_str().to_string(),
            image_error: error_of(&item.image),
            txt: item.txt.as_str().to_string(),
            txt_error: error_of(&item.txt),
        });
    }
    let manifest = ExportManifest {
        format_version: EXPORT_MANIFEST_VERSION,
        created_at: created_at.to_string(),
        cancelled: run.cancelled,
        structure: run.structure.as_str().to_string(),
        dest_dir: run.dest_dir.display().to_string(),
        // asset_count = 要求した一意asset数 (処理済み + plan失敗 + キャンセル
        // 未処理を含む — 結果DTOのtotalと同一定義。FG-WP16-3 N2)
        asset_count: run.items.len() as u64
            + run.planning_errors.len() as u64
            + run.skipped_by_cancel,
        summary: ExportManifestSummary {
            exported,
            identical,
            conflict,
            failed,
            planning_errors: run.planning_errors.len() as u64,
            cancelled: run.skipped_by_cancel,
        },
        entries,
        errors: run
            .planning_errors
            .iter()
            .map(|e| ExportManifestError {
                asset_id: e.asset_id.to_string(),
                reason: e.reason.clone(),
            })
            .collect(),
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("manifest直列化に失敗しました: {e}"))?;
    publish_manifest_file(&run.dest_dir, json.as_bytes())
}

/// manifestファイルを出版する (tmp書き→sync→`renameat2 NOREPLACE`)。
/// FG-WP16-4 R3: **既存ファイルは所有者を問わず一切削除・上書きしない**
/// (GEN-TO自身の旧manifestも含む — 「所有確認→unlink」間の差し替え競合で
/// ユーザーファイルを消しうるTOCTOUを、unlink自体をなくす方針で解消)。
/// FG-WP16-5 I2: failure経路のtmp unlinkも行わない (失敗時のtmp残留は
/// 自己識別的dotfileとして許容。成功時はrenameがtmpを消費)。
/// 正規名 `export-manifest.json` が空きならそこへ出版。占有されていれば
/// 回避名 `export-manifest-{run uuid}.json` へNOREPLACE出版し (既存は
/// 不接触) 、実パスを呼び出し側へ返す (結果DTOの `manifestPath`)。
fn publish_manifest_file(dest_dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let run_uuid = Uuid::now_v7().simple().to_string();
    publish_manifest_file_at(dest_dir, bytes, &run_uuid)
}

/// [`publish_manifest_file`] の実体。tmp名を決定するrun uuidを注入可能に
/// してある (テストがtmp名を事前占有してcreate_exclusiveのEEXIST経路を
/// 決定論的に再現できるようにする — FG-WP16-5 I2)。
fn publish_manifest_file_at(
    dest_dir: &Path,
    bytes: &[u8],
    run_uuid: &str,
) -> Result<PathBuf, String> {
    let root = gento_core::OutputDir::open_root(dest_dir).map_err(|e| {
        format!(
            "出力先ディレクトリを開けません: {} ({e})",
            dest_dir.display()
        )
    })?;
    let tmp = OsString::from(format!(
        ".{EXPORT_MANIFEST_FILE}.gento-tmp-{run_uuid}-000001"
    ));
    let write = (|| -> Result<(), String> {
        let mut out = root
            .create_exclusive_at(&tmp)
            .map_err(|e| format!("{}/{}: {e}", root.display(), tmp.to_string_lossy()))?;
        out.write_all(bytes)
            .map_err(|e| format!("{}/{}: {e}", root.display(), tmp.to_string_lossy()))?;
        out.sync_all()
            .map_err(|e| format!("{}/{}: {e}", root.display(), tmp.to_string_lossy()))?;
        drop(out);
        Ok(())
    })();
    // FG-WP16-5 I2: 失敗経路でtmpをunlinkしない (create_exclusiveの
    // EEXIST失敗時に自分が作っていないentryを消しうるため。残留は
    // 自己識別的dotfileとして許容)。
    write?;
    let final_name = OsString::from(EXPORT_MANIFEST_FILE);
    match root.rename_noreplace_at(&tmp, &final_name) {
        Ok(()) => {
            sync_dir_best_effort(dest_dir);
            Ok(dest_dir.join(EXPORT_MANIFEST_FILE))
        }
        // 正規名占有 (ユーザーファイル・GEN-TO旧manifestを問わず) —
        // unlinkせず回避名へ (既存は不接触)
        Err(gento_core::RenameAtNoReplaceError::DestinationExists) => {
            let alt = OsString::from(format!("export-manifest-{run_uuid}.json"));
            match root.rename_noreplace_at(&tmp, &alt) {
                Ok(()) => {
                    sync_dir_best_effort(dest_dir);
                    Ok(dest_dir.join(alt.to_string_lossy().into_owned()))
                }
                // いずれの失敗経路でもtmpはunlinkしない (FG-WP16-5 I2)
                Err(gento_core::RenameAtNoReplaceError::DestinationExists) => Err(format!(
                    "manifestの出版先が決定できません: {}",
                    dest_dir.display()
                )),
                Err(e) => Err(format!("manifestの公開に失敗しました: {e}")),
            }
        }
        Err(e) => Err(format!("{}/{}: {e}", root.display(), EXPORT_MANIFEST_FILE)),
    }
}

/// 親ディレクトリのエントリ反映 (best-effort — 出版済みファイルのfsyncは
/// 済んでおり、ここは追加点検のみ)。
fn sync_dir_best_effort(dest_dir: &Path) {
    if let Ok(dir) = File::open(dest_dir) {
        let _ = dir.sync_all();
    }
}

// ===== サービス・ホスト =====

/// 進捗イベントのUI通知境界 (実AppHandle / テスト: 記録)。
pub trait ExportHost: Send + Sync {
    fn progress(&self, payload: &ExportProgressDto);
}

/// AppHandle経由の実装 (`export:progress` イベント)。
pub struct TauriExportHost {
    pub app: AppHandle,
}

impl ExportHost for TauriExportHost {
    fn progress(&self, payload: &ExportProgressDto) {
        let _ = self.app.emit("export:progress", payload);
    }
}

/// エクスポートサービス (AppStateへmanage。同時実行は1件に制限)。
pub struct ExportService {
    db: Arc<gento_db::Db>,
    host: Arc<dyn ExportHost>,
    cancel: Arc<AtomicBool>,
    /// 実行排他 (同時多重実行の禁止)。
    running: Mutex<Option<()>>,
}

impl ExportService {
    pub fn new(db: Arc<gento_db::Db>, host: Arc<dyn ExportHost>) -> Arc<Self> {
        Arc::new(Self {
            db,
            host,
            cancel: Arc::new(AtomicBool::new(false)),
            running: Mutex::new(None),
        })
    }

    /// dry-run (export_preview)。書き込み一切なし。
    pub fn preview(
        &self,
        asset_ids: &[i64],
        dest_dir: &Path,
        structure: ExportStructure,
    ) -> Result<ExportPreviewDto, String> {
        let plan = plan_export(&self.db, asset_ids, dest_dir, structure);
        Ok(build_preview_dto(&plan, dest_dir, structure))
    }

    /// 実行 (export_execute)。manifest出版後に結果を返す。
    ///
    /// 排他: 実行中は `running` をSomeにする (2件目の同時実行は即エラー)。
    /// 通常終了 (Ok/Err両方) でNoneへ戻す。実行中のpanicでSomeが残った
    /// 場合は以後のエクスポートを拒否し続ける (安全側 — 再起動で回復)。
    pub fn execute(
        &self,
        asset_ids: &[i64],
        dest_dir: &Path,
        structure: ExportStructure,
    ) -> Result<ExportResultDto, String> {
        let mut guard = self.lock_running()?;
        *guard = Some(());
        let result = self.run_exclusive(asset_ids, dest_dir, structure);
        *guard = None;
        result
    }

    /// 排他確立後の実行本体。
    fn run_exclusive(
        &self,
        asset_ids: &[i64],
        dest_dir: &Path,
        structure: ExportStructure,
    ) -> Result<ExportResultDto, String> {
        self.cancel.store(false, Ordering::SeqCst);
        let cancel = Arc::clone(&self.cancel);
        let host = Arc::clone(&self.host);
        let progress = move |payload: &ExportProgressDto| host.progress(payload);
        let run = run_export(&self.db, asset_ids, dest_dir, structure, &cancel, &progress)?;
        let created_at = now_utc_rfc3339();
        let manifest_path = publish_manifest(&run, &created_at)?;
        Ok(build_result_dto(&run, &manifest_path))
    }

    /// 実行中エクスポートのキャンセル (協調。per-item境界で効く)。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn lock_running(&self) -> Result<MutexGuard<'_, Option<()>>, String> {
        let guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Err("別のエクスポートが実行中です".to_string());
        }
        Ok(guard)
    }
}

fn now_utc_rfc3339() -> String {
    gento_db::rfc3339_after_now(0)
}

// ===== DTO (camelCase — api-contract §48) =====

/// `export:progress` イベントペイロード。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressDto {
    pub processed: u64,
    pub total: u64,
    pub asset_uuid: String,
    /// "exported" | "identical" | "conflict" | "failed"。
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreviewEntryDto {
    pub asset_id: String,
    pub asset_uuid: String,
    pub display_name: String,
    /// 表示用ソースパス (lossy)。
    pub source_path: String,
    /// 出力先相対の画像パス (lossy・'/'結合)。error項目は空文字。
    pub output_rel_path: String,
    /// 出力先相対の.txtパス。error項目は空文字。
    pub txt_rel_path: String,
    /// "none" | "identical" | "conflict" (error項目は "n/a")。
    pub image_existing: String,
    pub txt_existing: String,
    /// "caption_sd_tags" | "caption_description" | "tags" | "none"。
    pub caption_source: String,
    /// caption本文の先頭 (最大80文字)。
    pub caption_preview: String,
    /// plan失敗理由 (asset不在・file不在・経路不正等)。
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreviewSummaryDto {
    /// 要求した一意asset数。
    pub total: u64,
    /// 新規書き出し予定 (少なくとも1ファイルを書く)。
    pub to_write: u64,
    /// 全ファイル既に書き出し済み (再実行で何も起きない)。
    pub already_exported: u64,
    /// 既存出力との内容不一致 (skip + 報告)。
    pub conflict: u64,
    /// plan失敗。
    pub error: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreviewDto {
    pub dest_dir: String,
    /// "preserve" | "flat"。
    pub structure: String,
    pub entries: Vec<ExportPreviewEntryDto>,
    pub summary: ExportPreviewSummaryDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRunSummaryDto {
    pub total: u64,
    pub exported: u64,
    pub identical: u64,
    pub conflict: u64,
    pub failed: u64,
    pub planning_errors: u64,
    /// キャンセルで未処理に終わった項目数。
    pub cancelled: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportIssueDto {
    pub asset_uuid: String,
    pub display_name: String,
    pub output_rel_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResultDto {
    pub manifest_path: String,
    pub cancelled: bool,
    pub summary: ExportRunSummaryDto,
    /// 失敗項目 (上限あり。全量はmanifest)。
    pub failures: Vec<ExportIssueDto>,
    /// 衝突skip項目 (上限あり)。
    pub conflicts: Vec<ExportIssueDto>,
    pub issues_truncated: bool,
}

/// plan項目の出力先相対パス (lossy・'/'結合) を作る。
fn rel_display(subdir: &[OsString], name: &OsStr) -> String {
    let mut out = String::new();
    for component in subdir {
        out.push_str(&component.to_string_lossy());
        out.push('/');
    }
    out.push_str(&name.to_string_lossy());
    out
}

fn existing_str(state: ExistingState) -> &'static str {
    match state {
        ExistingState::NotExists => "none",
        ExistingState::Identical => "identical",
        ExistingState::Conflict => "conflict",
    }
}

fn truncate_preview(text: &str) -> String {
    const MAX: usize = 80;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(MAX).collect();
        truncated
    }
}

/// planからpreview DTOを組む (既存衝突分類をここで読み取り専用に判定)。
fn build_preview_dto(
    plan: &ExportPlan,
    dest_dir: &Path,
    structure: ExportStructure,
) -> ExportPreviewDto {
    let mut entries = Vec::with_capacity(plan.items.len() + plan.errors.len());
    // plan段階の失敗 (asset不在等) はerror entryとして可視化する
    for error in &plan.errors {
        entries.push(ExportPreviewEntryDto {
            asset_id: error.asset_id.to_string(),
            asset_uuid: String::new(),
            display_name: String::new(),
            source_path: String::new(),
            output_rel_path: String::new(),
            txt_rel_path: String::new(),
            image_existing: "n/a".to_string(),
            txt_existing: "n/a".to_string(),
            caption_source: "none".to_string(),
            caption_preview: String::new(),
            error: Some(error.reason.clone()),
        });
    }
    let mut to_write = 0u64;
    let mut already = 0u64;
    let mut conflict = 0u64;
    // 既存分類はdest (とsubdir) が実在する場合のみ読み取り専用ハンドルで
    // 行う (dry-runは一切作成しない — open_existing系のみ)。
    let dest_root = gento_core::OutputDir::open_existing_root(dest_dir).ok();
    for item in &plan.items {
        let txt_name = txt_file_name(&item.output_name);
        let caption_bytes = item.caption.as_bytes();
        // subdir (またはdest自体) が無ければ全て新規扱い
        let subdir = dest_root
            .as_ref()
            .and_then(|root| root.open_existing_subdir(&item.subdir).ok());
        let image_state = match open_source(item) {
            Ok((mut source, md)) => match &subdir {
                Some(dir) => classify_file_at(dir, &item.output_name, &mut source, md.len()),
                None => ExistingState::NotExists,
            },
            Err(reason) => {
                // ソース読取不能はplan失敗扱い (dry-runで気づかせる)
                entries.push(ExportPreviewEntryDto {
                    asset_id: item.asset_id.to_string(),
                    asset_uuid: item.asset_uuid.clone(),
                    display_name: item.display_name.clone(),
                    source_path: item.source_display.clone(),
                    output_rel_path: String::new(),
                    txt_rel_path: String::new(),
                    image_existing: "n/a".to_string(),
                    txt_existing: "n/a".to_string(),
                    caption_source: item.caption_origin.as_str().to_string(),
                    caption_preview: truncate_preview(&item.caption),
                    error: Some(reason),
                });
                continue;
            }
        };
        let txt_state = match &subdir {
            Some(dir) => classify_bytes_at(dir, &txt_name, caption_bytes),
            None => ExistingState::NotExists,
        };
        match (image_state, txt_state) {
            (ExistingState::Identical, ExistingState::Identical) => already += 1,
            (ExistingState::Conflict, _) | (_, ExistingState::Conflict) => conflict += 1,
            _ => to_write += 1,
        }
        entries.push(ExportPreviewEntryDto {
            asset_id: item.asset_id.to_string(),
            asset_uuid: item.asset_uuid.clone(),
            display_name: item.display_name.clone(),
            source_path: item.source_display.clone(),
            output_rel_path: rel_display(&item.subdir, &item.output_name),
            txt_rel_path: rel_display(&item.subdir, &txt_name),
            image_existing: existing_str(image_state).to_string(),
            txt_existing: existing_str(txt_state).to_string(),
            caption_source: item.caption_origin.as_str().to_string(),
            caption_preview: truncate_preview(&item.caption),
            error: None,
        });
    }
    let error_count = entries.iter().filter(|e| e.error.is_some()).count() as u64;
    ExportPreviewDto {
        dest_dir: dest_dir.display().to_string(),
        structure: structure.as_str().to_string(),
        entries,
        summary: ExportPreviewSummaryDto {
            total: plan.items.len() as u64 + plan.errors.len() as u64,
            to_write,
            already_exported: already,
            conflict,
            error: error_count,
        },
    }
}

/// runから結果DTOを組む。summaryのconflict/failedは**truncation前の全件数**
/// (issues配列だけが先頭100件に切り詰められる — FG-WP16-3 N1)。
fn build_result_dto(run: &ExportRun, manifest_path: &Path) -> ExportResultDto {
    let mut exported = 0u64;
    let mut identical = 0u64;
    let mut conflict_items = Vec::new();
    let mut failed_items = Vec::new();
    for item in &run.items {
        match item.status() {
            "exported" => exported += 1,
            "identical" => identical += 1,
            "conflict" => conflict_items.push(ExportIssueDto {
                asset_uuid: item.asset_uuid.clone(),
                display_name: item.display_name.clone(),
                output_rel_path: item.output_rel.clone(),
                reason: conflict_reason(item),
            }),
            _ => failed_items.push(ExportIssueDto {
                asset_uuid: item.asset_uuid.clone(),
                display_name: item.display_name.clone(),
                output_rel_path: item.output_rel.clone(),
                reason: failure_reason(item),
            }),
        }
    }
    // 集計は全件ベースで確定させてから配列のみ切り詰める
    let conflict_count = conflict_items.len() as u64;
    let failed_count = failed_items.len() as u64;
    let mut truncated = false;
    if failed_items.len() > ISSUE_REPORT_LIMIT {
        failed_items.truncate(ISSUE_REPORT_LIMIT);
        truncated = true;
    }
    if conflict_items.len() > ISSUE_REPORT_LIMIT {
        conflict_items.truncate(ISSUE_REPORT_LIMIT);
        truncated = true;
    }
    ExportResultDto {
        manifest_path: manifest_path.display().to_string(),
        cancelled: run.cancelled,
        summary: ExportRunSummaryDto {
            // total = 処理済み + plan失敗 + キャンセル未処理 (要求した一意asset数)
            total: run.items.len() as u64
                + run.planning_errors.len() as u64
                + run.skipped_by_cancel,
            exported,
            identical,
            conflict: conflict_count,
            failed: failed_count,
            planning_errors: run.planning_errors.len() as u64,
            cancelled: run.skipped_by_cancel,
        },
        failures: failed_items,
        conflicts: conflict_items,
        issues_truncated: truncated,
    }
}

fn conflict_reason(item: &ItemResult) -> String {
    let mut parts = Vec::new();
    if item.image == FileOutcome::Conflict {
        parts.push("画像".to_string());
    }
    if item.txt == FileOutcome::Conflict {
        parts.push(".txt".to_string());
    }
    format!(
        "出力先に同名の異なる内容が既に存在します ({})",
        parts.join("・")
    )
}

fn failure_reason(item: &ItemResult) -> String {
    if let FileOutcome::Failed(reason) = &item.image {
        return reason.clone();
    }
    if let FileOutcome::Failed(reason) = &item.txt {
        return reason.clone();
    }
    "不明な失敗".to_string()
}

/// ソースをpath_safety歩行で読み取り専用open。
fn open_source(item: &PlannedItem) -> Result<(File, std::fs::Metadata), String> {
    gento_core::open_regular_beneath(&item.root_path, &item.rel_components).map_err(|e| {
        format!(
            "ソースファイルを読み取れません: {} ({e})",
            item.source_display
        )
    })
}

// ===== Tauri commands =====

fn service_of(state: &State<'_, AppState>) -> Result<Arc<ExportService>, String> {
    state
        .export
        .clone()
        .ok_or_else(|| "エクスポートはDBが無効なため利用できません".to_string())
}

fn export_args(
    state: &AppState,
    asset_ids: &[String],
    dest_dir: &str,
    structure: &str,
) -> Result<(Vec<i64>, PathBuf, ExportStructure), String> {
    let ids = parse_stable_asset_ids(current_mode(state), asset_ids)?;
    let dest = PathBuf::from(dest_dir.trim());
    if dest.as_os_str().is_empty() {
        return Err("出力先ディレクトリを指定してください".to_string());
    }
    Ok((ids, dest, ExportStructure::parse(structure)?))
}

/// dry-run: 出力予定エントリ + 集計 (ファイル書込なし)。
#[tauri::command]
pub async fn export_preview(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    dest_dir: String,
    structure: String,
) -> Result<ExportPreviewDto, String> {
    let service = service_of(&state)?;
    let (ids, dest, structure) = export_args(&state, &asset_ids, &dest_dir, &structure)?;
    tauri::async_runtime::spawn_blocking(move || service.preview(&ids, &dest, structure))
        .await
        .map_err(|e| e.to_string())?
}

/// 実行 (進捗は `export:progress` イベント)。blocking FS操作は
/// spawn_blockingでUIスレッド外で実行する (backup.rsと同一規約)。
#[tauri::command]
pub async fn export_execute(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    dest_dir: String,
    structure: String,
) -> Result<ExportResultDto, String> {
    let service = service_of(&state)?;
    let (ids, dest, structure) = export_args(&state, &asset_ids, &dest_dir, &structure)?;
    tauri::async_runtime::spawn_blocking(move || service.execute(&ids, &dest, structure))
        .await
        .map_err(|e| e.to_string())?
}

/// 実行中エクスポートのキャンセル (協調)。
#[tauri::command]
pub fn export_cancel(state: State<'_, AppState>) -> Result<(), String> {
    let service = service_of(&state)?;
    service.cancel();
    Ok(())
}

/// 出力dir (サブdir構造) ごとの使用済み名前集合 (命名割当用)。
type UsedNamesByDir = BTreeMap<Vec<OsString>, HashSet<OsString>>;

#[cfg(test)]
mod tests {
    use super::*;
    use gento_core::metadata::CaptionKind;
    use gento_core::repo::{FileUpsert, NewAsset};
    use std::sync::Mutex as StdMutex;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-tauri-export-{tag}-{}-{}",
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
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 内容が異なる最小PNG (seedで画素を変える — ハッシュ衝突回避)。
    fn test_png(path: &std::path::Path, seed: u8) {
        use image::{Rgb, RgbImage};
        let img = RgbImage::from_fn(32, 24, |x, y| {
            Rgb([((x + y) % 256) as u8, seed, 0x40 ^ seed])
        });
        img.write_with_encoder(image::codecs::png::PngEncoder::new(
            std::fs::File::create(path).unwrap(),
        ))
        .unwrap();
    }

    /// 進捗記録host (n件処理後にキャンセルを立てるオプション付き)。
    struct RecordingHost {
        progress: StdMutex<Vec<ExportProgressDto>>,
        cancel_after: StdMutex<Option<u64>>,
        cancel_flag: StdMutex<Option<Arc<AtomicBool>>>,
    }

    impl RecordingHost {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                progress: StdMutex::new(Vec::new()),
                cancel_after: StdMutex::new(None),
                cancel_flag: StdMutex::new(None),
            })
        }

        fn cancel_after(self: &Arc<Self>, n: u64, flag: Arc<AtomicBool>) {
            *self.cancel_after.lock().unwrap() = Some(n);
            *self.cancel_flag.lock().unwrap() = Some(flag);
        }

        fn disarm(&self) {
            *self.cancel_after.lock().unwrap() = None;
        }

        fn payloads(&self) -> Vec<ExportProgressDto> {
            self.progress.lock().unwrap().clone()
        }
    }

    impl ExportHost for RecordingHost {
        fn progress(&self, payload: &ExportProgressDto) {
            let mut log = self.progress.lock().unwrap();
            let processed = payload.processed;
            log.push(payload.clone());
            drop(log);
            if let (Some(threshold), Some(flag)) = (
                *self.cancel_after.lock().unwrap(),
                self.cancel_flag.lock().unwrap().clone(),
            ) {
                if processed >= threshold {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        }
    }

    /// assetを1件作る (実ファイル + files行 + assets行 + link)。
    /// 戻り値: (asset_id, file_id, bytes)
    fn add_asset(
        db: &gento_db::Db,
        root_id: i64,
        root_dir: &Path,
        rel: &str,
        seed: u8,
    ) -> (i64, i64, Vec<u8>) {
        let abs = root_dir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        test_png(&abs, seed);
        let bytes = std::fs::read(&abs).unwrap();
        let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        let (file_id, asset_id) = db
            .with_writer(|repos| {
                let file = repos.upsert_file(FileUpsert {
                    root_id,
                    rel_path: PathBuf::from(rel),
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
                })?;
                let (asset, _) = repos.create_or_get_asset(NewAsset {
                    content_hash: hash,
                    hash_algorithm: "blake3".to_string(),
                    byte_size: bytes.len() as u64,
                    mime_type: Some("image/png".to_string()),
                    width: Some(32),
                    height: Some(24),
                    has_alpha: Some(false),
                    is_animated: Some(false),
                    frame_count: None,
                })?;
                repos.link_file(file.id, asset.id)?;
                Ok((file.id, asset.id))
            })
            .unwrap();
        (asset_id, file_id, bytes)
    }

    fn root_id_of(db: &gento_db::Db) -> i64 {
        db.with_reader(|repos| Ok(repos.list_roots().unwrap()[0].id))
            .unwrap()
    }

    fn make_service(db: Arc<gento_db::Db>, host: Arc<dyn ExportHost>) -> Arc<ExportService> {
        ExportService::new(db, host)
    }

    fn flat_service(
        tag: &str,
    ) -> (
        TestDir,
        Arc<gento_db::Db>,
        Arc<ExportService>,
        Arc<RecordingHost>,
        PathBuf,
    ) {
        let dir = TestDir::new(tag);
        let root_dir = dir.0.join("photos");
        std::fs::create_dir_all(&root_dir).unwrap();
        let db = Arc::new(gento_db::Db::open(dir.0.join("db.sqlite3")).unwrap());
        db.with_writer(|repos| repos.add_root(&root_dir)).unwrap();
        let host = RecordingHost::new();
        let service = make_service(Arc::clone(&db), host.clone());
        (dir, db, service, host, root_dir)
    }

    fn file_sha(path: &Path) -> String {
        blake3::hash(&std::fs::read(path).unwrap()).to_string()
    }

    fn mtime_ns_of(path: &Path) -> u128 {
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::symlink_metadata(path).unwrap();
        md.mtime() as i128 as u128 * 1_000_000_000 + md.mtime_nsec() as i128 as u128
    }

    // ===== 純粋関数: 命名 =====

    #[test]
    fn txt_file_name_stem_rules() {
        assert_eq!(
            txt_file_name(OsStr::new("img.png")),
            OsString::from("img.txt")
        );
        assert_eq!(
            txt_file_name(OsStr::new("archive.tar.gz")),
            OsString::from("archive.tar.txt")
        );
        assert_eq!(
            txt_file_name(OsStr::new("README")),
            OsString::from("README.txt")
        );
        // 先頭dotのみの名前は拡張子なし扱い (vitestミラーと同一期待値)
        assert_eq!(
            txt_file_name(OsStr::new(".hidden")),
            OsString::from(".hidden.txt")
        );
    }

    #[test]
    fn suffixed_name_rules() {
        assert_eq!(
            suffixed_name(OsStr::new("img.png"), 1),
            OsString::from("img-1.png")
        );
        assert_eq!(
            suffixed_name(OsStr::new("a.b.png"), 2),
            OsString::from("a.b-2.png")
        );
        assert_eq!(
            suffixed_name(OsStr::new("README"), 3),
            OsString::from("README-3")
        );
    }

    #[test]
    fn allocate_output_name_first_free_suffix() {
        let mut used: HashSet<OsString> = HashSet::new();
        assert_eq!(
            allocate_output_name(OsStr::new("img.png"), &mut used),
            OsString::from("img.png")
        );
        // 同名の2件目 → -1
        assert_eq!(
            allocate_output_name(OsStr::new("img.png"), &mut used),
            OsString::from("img-1.png")
        );
        // 3件目 → -2
        assert_eq!(
            allocate_output_name(OsStr::new("img.png"), &mut used),
            OsString::from("img-2.png")
        );
        // 実在の「img-1.png」名を持つ別asset → -1は使用済みなので -2…ではなく
        // img-1-1 になる (base img-1.png の第1空き)
        let mut used2: HashSet<OsString> = HashSet::new();
        used2.insert(OsString::from("img.png"));
        used2.insert(OsString::from("img-1.png"));
        assert_eq!(
            allocate_output_name(OsStr::new("img-1.png"), &mut used2),
            OsString::from("img-1-1.png")
        );
    }

    #[test]
    fn representative_file_prefers_present_min_id() {
        // FileRecordは最小限のフィールドだけ埋める (対象はidとscan_state)
        let mk = |id: i64, present: bool| FileRecord {
            id,
            uuid: format!("u{id}"),
            root_id: 1,
            asset_id: Some(1),
            rel_path: PathBuf::from(format!("f{id}.png")),
            display_name: format!("f{id}.png"),
            extension: Some("png".to_string()),
            byte_size: 1,
            mtime_ms: 1,
            mtime_ns: Some(1),
            scan_state: if present {
                ScanState::Present
            } else {
                ScanState::Missing
            },
            os_file_id: None,
            last_seen_at: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        // present最小id
        let files = vec![mk(3, false), mk(7, true), mk(5, true)];
        assert_eq!(representative_file(&files).unwrap().id, 5);
        // present無し → 全体の最小id
        let files = vec![mk(4, false), mk(2, false)];
        assert_eq!(representative_file(&files).unwrap().id, 2);
        // 空
        assert!(representative_file(&[]).is_none());
    }

    // ===== 統合: 命名の決定論 =====

    #[test]
    fn naming_suffix_deterministic_and_idempotent() {
        let (dir, db, service, _host, root_dir) = flat_service("naming");
        let root_id = root_id_of(&db);
        // 同名 "img.png" を持つ3 asset (uuidは生成順でない並びになる可能性が
        // あるため、割当はuuid辞書順であることを結果から検証する)
        let mut assets = Vec::new();
        for (i, rel) in ["a/img.png", "b/img.png", "c/img.png"].iter().enumerate() {
            let (asset_id, _file_id, _bytes) = add_asset(&db, root_id, &root_dir, rel, i as u8 + 1);
            assets.push(asset_id);
        }
        let uuid_of = |id: i64| {
            db.with_reader(|repos| Ok(repos.get_asset(id).unwrap().unwrap().uuid))
                .unwrap()
        };
        let mut ordered: Vec<(String, i64)> = assets.iter().map(|id| (uuid_of(*id), *id)).collect();
        ordered.sort();
        let dest = dir.0.join("out");
        let result = service
            .execute(&assets, &dest, ExportStructure::Flat)
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.summary.exported, 3, "{:?}", result.summary);
        // uuid最小がbase名を維持、以降は -1, -2
        let expected: Vec<(i64, &str)> = vec![
            (ordered[0].1, "img.png"),
            (ordered[1].1, "img-1.png"),
            (ordered[2].1, "img-2.png"),
        ];
        for (_id, name) in &expected {
            assert!(dest.join(name).exists(), "missing {name}");
            assert!(dest.join(txt_file_name(OsStr::new(name))).exists());
        }
        // 再実行 (同一dest) → 全部identical (冪等) で割当不変
        let rerun = service
            .execute(&assets, &dest, ExportStructure::Flat)
            .unwrap();
        assert_eq!(rerun.summary.identical, 3, "{:?}", rerun.summary);
        // 別destへの再実行でも同一割当 (決定論)
        let dest2 = dir.0.join("out2");
        let fresh = service
            .execute(&assets, &dest2, ExportStructure::Flat)
            .unwrap();
        assert_eq!(fresh.summary.exported, 3);
        for (_id, name) in &expected {
            assert!(dest2.join(name).exists(), "fresh missing {name}");
        }
        // dedup: 同一idを重複入力しても1出力
        let dup = service
            .execute(
                &[assets[0], assets[0]],
                &dir.0.join("out3"),
                ExportStructure::Flat,
            )
            .unwrap();
        assert_eq!(dup.summary.total, 1, "{:?}", dup.summary);
    }

    // ===== 構造: 維持/平坦化 =====

    #[test]
    fn structure_preserve_and_flat() {
        let (dir, db, service, _host, root_dir) = flat_service("structure");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "sub/dir/x.png", 1);
        let (b, _, _) = add_asset(&db, root_id, &root_dir, "y.png", 2);
        // 維持: 親dir構造を再現
        let preserve_dest = dir.0.join("preserve");
        service
            .execute(&[a, b], &preserve_dest, ExportStructure::Preserve)
            .unwrap();
        assert!(preserve_dest.join("sub/dir/x.png").exists());
        assert!(preserve_dest.join("sub/dir/x.txt").exists());
        assert!(preserve_dest.join("y.png").exists());
        assert!(preserve_dest.join("y.txt").exists());
        // 平坦化: 全て直下
        let flat_dest = dir.0.join("flat");
        service
            .execute(&[a, b], &flat_dest, ExportStructure::Flat)
            .unwrap();
        assert!(flat_dest.join("x.png").exists());
        assert!(flat_dest.join("x.txt").exists());
        assert!(flat_dest.join("y.png").exists());
    }

    // ===== .txt内容: caption / tag fallback / 空 =====

    #[test]
    fn txt_content_caption_tag_fallback_and_empty() {
        let (dir, db, service, _host, root_dir) = flat_service("txt");
        let root_id = root_id_of(&db);
        // A: sd_tags captionが正本
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "a.png", 1);
        db.with_writer(|repos| {
            repos.set_caption(a, CaptionKind::SdTags, "masterpiece, best quality")?;
            repos.set_caption(a, CaptionKind::Description, "A scenic photo.")?;
            Ok(())
        })
        .unwrap();
        // B: descriptionのみ
        let (b, _, _) = add_asset(&db, root_id, &root_dir, "b.png", 2);
        db.with_writer(|repos| repos.set_caption(b, CaptionKind::Description, "A cat."))
            .unwrap();
        // C: caption無し → タグ列 (normalized_name ASC = a, b, Z→z順)
        let (c, _, _) = add_asset(&db, root_id, &root_dir, "c.png", 3);
        db.with_writer(|repos| repos.add_tags_batch(&[c], &["Zebra", "apple", "blue sky"]))
            .unwrap();
        // D: 何も無し → 空ファイル
        let (d, _, _) = add_asset(&db, root_id, &root_dir, "d.png", 4);

        let dest = dir.0.join("out");
        let result = service
            .execute(&[a, b, c, d], &dest, ExportStructure::Flat)
            .unwrap();
        assert_eq!(result.summary.exported, 4, "{:?}", result.summary);
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "masterpiece, best quality"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("b.txt")).unwrap(),
            "A cat."
        );
        // タグ列: display_nameを", "結合 (順序はnormalized_name ASC)
        assert_eq!(
            std::fs::read_to_string(dest.join("c.txt")).unwrap(),
            "apple, blue sky, Zebra"
        );
        let d_meta = std::fs::symlink_metadata(dest.join("d.txt")).unwrap();
        assert_eq!(d_meta.len(), 0, "両方無しは空ファイル");
        // previewのcaptionPreviewにも反映
        let preview = service
            .preview(&[a, b, c, d], &dest, ExportStructure::Flat)
            .unwrap();
        let by_name = |n: &str| {
            preview
                .entries
                .iter()
                .find(|e| e.display_name == n)
                .unwrap()
                .clone()
        };
        assert_eq!(by_name("a.png").caption_source, "caption_sd_tags");
        assert_eq!(by_name("b.png").caption_source, "caption_description");
        assert_eq!(by_name("c.png").caption_source, "tags");
        assert_eq!(by_name("d.png").caption_source, "none");
        assert_eq!(by_name("c.png").caption_preview, "apple, blue sky, Zebra");
    }

    // ===== 既存出力との衝突 =====

    #[test]
    fn existing_conflict_no_silent_overwrite() {
        let (dir, db, service, _host, root_dir) = flat_service("conflict");
        let root_id = root_id_of(&db);
        let (a, _, bytes) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        let dest = dir.0.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        // 既存: 同名・同サイズ・異内容 (BLAKE3比較まで到達する衝突)
        let mut hostile = bytes.clone();
        let last = hostile.len() - 1;
        hostile[last] ^= 0xFF;
        assert_eq!(hostile.len(), bytes.len());
        std::fs::write(dest.join("img.png"), &hostile).unwrap();
        let hostile_hash = file_sha(&dest.join("img.png"));

        let result = service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_eq!(result.summary.conflict, 1, "{:?}", result.summary);
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].display_name, "img.png");
        // 既存ファイルは無断上書きされない
        assert_eq!(file_sha(&dest.join("img.png")), hostile_hash);
        // .txt側は新規なので書かれる
        assert!(dest.join("img.txt").exists());
    }

    #[test]
    fn existing_identical_skip_is_idempotent() {
        let (dir, db, service, _host, root_dir) = flat_service("identical");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        db.with_writer(|repos| repos.set_caption(a, CaptionKind::SdTags, "tags here"))
            .unwrap();
        let dest = dir.0.join("out");
        let first = service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_eq!(first.summary.exported, 1, "{:?}", first.summary);
        let img_hash = file_sha(&dest.join("img.png"));
        let txt_hash = file_sha(&dest.join("img.txt"));
        // 再実行 → 全部identical (書き直さない)
        let second = service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_eq!(second.summary.identical, 1, "{:?}", second.summary);
        assert_eq!(file_sha(&dest.join("img.png")), img_hash);
        assert_eq!(file_sha(&dest.join("img.txt")), txt_hash);
        // previewもidenticalと分類
        let preview = service.preview(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_eq!(preview.entries[0].image_existing, "identical");
        assert_eq!(preview.entries[0].txt_existing, "identical");
        assert_eq!(preview.summary.already_exported, 1);
    }

    // ===== 部分失敗 =====

    #[test]
    fn partial_failure_continues_and_manifest_records() {
        let (dir, db, service, _host, root_dir) = flat_service("partial");
        let root_id = root_id_of(&db);
        // 最初に壊れたfile行を作る (最小file id = 代表になる)
        let ghost_rel = "ghost/missing.png";
        let ghost = db
            .with_writer(|repos| {
                repos.upsert_file(FileUpsert {
                    root_id,
                    rel_path: PathBuf::from(ghost_rel),
                    display_name: "missing.png".to_string(),
                    byte_size: 10,
                    mtime_ms: 1,
                    mtime_ns: 1_000,
                    scan_state: ScanState::Present,
                    os_file_id: None,
                    detach_asset: false,
                })
            })
            .unwrap();
        // 実ファイルは置かない
        let ghost_bytes = vec![1u8, 2, 3];
        let ghost_hash = format!("blake3:{}", blake3::hash(&ghost_bytes).to_hex());
        let bad_asset = db
            .with_writer(|repos| {
                let (asset, _) = repos.create_or_get_asset(NewAsset {
                    content_hash: ghost_hash,
                    hash_algorithm: "blake3".to_string(),
                    byte_size: ghost_bytes.len() as u64,
                    mime_type: Some("image/png".to_string()),
                    width: None,
                    height: None,
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })?;
                repos.link_file(ghost.id, asset.id)?;
                Ok(asset.id)
            })
            .unwrap();
        // 正常asset 2件 (bad_assetよりfile idが大きい = 代表にならない)
        let (ok1, _, _) = add_asset(&db, root_id, &root_dir, "ok1.png", 1);
        let (ok2, _, _) = add_asset(&db, root_id, &root_dir, "ok2.png", 2);

        let dest = dir.0.join("out");
        let result = service
            .execute(&[ok1, bad_asset, ok2], &dest, ExportStructure::Flat)
            .unwrap();
        assert_eq!(result.summary.failed, 1, "{:?}", result.summary);
        assert_eq!(result.summary.exported, 2, "{:?}", result.summary);
        assert!(dest.join("ok1.png").exists());
        assert!(dest.join("ok2.png").exists());
        assert!(!dest.join("missing.png").exists());
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].display_name, "missing.png");
        // manifestにも記録
        let manifest = std::fs::read_to_string(dest.join(EXPORT_MANIFEST_FILE)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["summary"]["failed"], 1);
        let entries = parsed["entries"].as_array().unwrap();
        let failed_entry = entries
            .iter()
            .find(|e| e["display_name"] == "missing.png")
            .unwrap();
        assert_eq!(failed_entry["image"], "failed");
        // previewでもerror扱い (ソース読取不能)
        let preview = service
            .preview(&[ok1, bad_asset, ok2], &dest, ExportStructure::Flat)
            .unwrap();
        assert_eq!(preview.summary.error, 1, "{:?}", preview.summary);
        assert!(preview
            .entries
            .iter()
            .any(|e| e.display_name == "missing.png" && e.error.is_some()));
    }

    // ===== キャンセル =====

    #[test]
    fn cancel_stops_at_item_boundary_and_keeps_results() {
        let (dir, db, service, host, root_dir) = flat_service("cancel");
        let root_id = root_id_of(&db);
        let mut assets = Vec::new();
        for i in 0..3u8 {
            let (asset_id, _, _) = add_asset(&db, root_id, &root_dir, &format!("i{i}.png"), i + 1);
            assets.push(asset_id);
        }
        // 1件目の進捗後にキャンセルを立てる
        host.cancel_after(1, service_cancel_flag(&service));
        let dest = dir.0.join("out");
        let result = service
            .execute(&assets, &dest, ExportStructure::Flat)
            .unwrap();
        assert!(result.cancelled, "{:?}", result.summary);
        // 1件目は完了し、以降は未処理
        assert_eq!(
            result.summary.exported + result.summary.identical + result.summary.conflict,
            1,
            "{:?}",
            result.summary
        );
        assert!(result.summary.cancelled >= 1);
        assert_eq!(result.summary.total, 3);
        let written: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                if name.ends_with(".png") {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(written.len(), 1, "1件だけ書かれている: {written:?}");
        // manifestに打切りが記録される
        let manifest = std::fs::read_to_string(dest.join(EXPORT_MANIFEST_FILE)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["cancelled"], true);
        assert_eq!(parsed["summary"]["cancelled"], result.summary.cancelled);
        // 進捗イベントは1件目まで
        let payloads = host.payloads();
        assert_eq!(payloads.len(), 1, "{payloads:?}");
        // cancelフラグは実行前に毎回リセットされ、トリガを外せば
        // 直後の再実行が全件完了する
        host.disarm();
        let full = service
            .execute(&assets, &dest, ExportStructure::Flat)
            .unwrap();
        assert!(!full.cancelled);
        assert_eq!(full.summary.total, 3);
    }

    fn service_cancel_flag(service: &Arc<ExportService>) -> Arc<AtomicBool> {
        Arc::clone(&service.cancel)
    }

    // ===== dry-run: 書き込みなし =====

    #[test]
    fn preview_writes_nothing() {
        let (dir, db, service, _host, root_dir) = flat_service("dryrun");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "sub/img.png", 1);
        let dest = dir.0.join("out");
        // 出力先が存在しない状態でもpreviewは成立
        let preview = service
            .preview(&[a], &dest, ExportStructure::Preserve)
            .unwrap();
        assert_eq!(preview.summary.to_write, 1, "{:?}", preview.summary);
        assert!(!dest.exists(), "dry-runが出力先を作成してはならない");
        // 既存dirに対してもファイルを1つも作らない
        std::fs::create_dir_all(&dest).unwrap();
        let _preview2 = service
            .preview(&[a], &dest, ExportStructure::Preserve)
            .unwrap();
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
        // 不在assetはerror entry
        let bad = service
            .preview(&[999999], &dest, ExportStructure::Flat)
            .unwrap();
        assert_eq!(bad.summary.error, 1);
        assert!(bad.entries[0].error.is_some());
    }

    // ===== 元ファイル不変 =====

    #[test]
    fn source_files_and_directories_are_untouched() {
        let (dir, db, service, _host, root_dir) = flat_service("immutable");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "keep/img.png", 1);
        let (b, _, _) = add_asset(&db, root_id, &root_dir, "keep/img2.png", 2);
        let src1 = root_dir.join("keep/img.png");
        let src2 = root_dir.join("keep/img2.png");
        let before: Vec<(PathBuf, String, u128, u64)> = [&src1, &src2]
            .iter()
            .map(|p| {
                (
                    (*p).to_path_buf(),
                    file_sha(p),
                    mtime_ns_of(p),
                    std::fs::symlink_metadata(p).unwrap().len(),
                )
            })
            .collect();
        let dest = dir.0.join("out");
        service
            .execute(&[a, b], &dest, ExportStructure::Preserve)
            .unwrap();
        for (path, hash, mtime, size) in &before {
            assert_eq!(&file_sha(path), hash, "内容が変わった: {path:?}");
            assert_eq!(&mtime_ns_of(path), mtime, "mtimeが変わった: {path:?}");
            assert_eq!(&std::fs::symlink_metadata(path).unwrap().len(), size);
        }
        // 出力はコピー (実体相違 — ハッシュ一致だが別inodeであることまでは
        // ここでは求めない: 内容一致で十分)
        assert_eq!(file_sha(&dest.join("keep/img.png")), file_sha(&src1));
    }

    // ===== manifestの原子出版 =====

    #[test]
    fn manifest_published_atomically_without_tmp_leftovers() {
        let (dir, db, service, _host, root_dir) = flat_service("manifest");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        let dest = dir.0.join("out");
        service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert!(dest.join(EXPORT_MANIFEST_FILE).exists());
        let leftovers: Vec<String> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                if name.contains("gento-tmp") {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        assert!(leftovers.is_empty(), "tmp残骸: {leftovers:?}");
        // manifest内容の基本形
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dest.join(EXPORT_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["format_version"], EXPORT_MANIFEST_VERSION);
        assert_eq!(manifest["structure"], "flat");
        assert_eq!(manifest["summary"]["exported"], 1);
        let entry = &manifest["entries"][0];
        assert_eq!(entry["image"], "written");
        assert_eq!(entry["txt"], "written");
        assert_eq!(entry["caption_source"], "none");
        // 再実行: 正規名は最初のrunのままで、新manifestは回避名へ
        // (FG-WP16-4 R3 — 既存は一切削除・上書きしない)
        let second = service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_ne!(
            second.manifest_path,
            dest.join(EXPORT_MANIFEST_FILE).display().to_string(),
            "正規名占有時は回避名へ出版: {}",
            second.manifest_path
        );
        assert!(second.manifest_path.contains("export-manifest-"));
        let manifest2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&second.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest2["summary"]["identical"], 1);
        assert_eq!(manifest2["summary"]["exported"], 0);
        // 正規名の旧manifestは無傷 (最初のrunの内容のまま)
        let manifest1_again: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dest.join(EXPORT_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest1_again["summary"]["exported"], 1);
    }

    // ===== 代表file: 重複登録 =====

    #[test]
    fn duplicate_registration_exports_once_with_min_present_file() {
        let (dir, db, service, _host, root_dir) = flat_service("dupfile");
        let root_id = root_id_of(&db);
        // 同一内容を2箇所に (同一assetにlink)
        let bytes_rel_a = "orig/a.png";
        let bytes_rel_b = "copy/b.png";
        let abs_a = root_dir.join(bytes_rel_a);
        if let Some(parent) = abs_a.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        test_png(&abs_a, 7);
        let bytes = std::fs::read(&abs_a).unwrap();
        std::fs::create_dir_all(root_dir.join("copy")).unwrap();
        std::fs::write(root_dir.join(bytes_rel_b), &bytes).unwrap();
        let hash = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        let (asset_id, file_a, file_b) = db
            .with_writer(|repos| {
                let mk = |rel: &str| {
                    repos
                        .upsert_file(FileUpsert {
                            root_id,
                            rel_path: PathBuf::from(rel),
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
                        .unwrap()
                };
                let fa = mk(bytes_rel_a);
                let fb = mk(bytes_rel_b);
                let (asset, _) = repos
                    .create_or_get_asset(NewAsset {
                        content_hash: hash,
                        hash_algorithm: "blake3".to_string(),
                        byte_size: bytes.len() as u64,
                        mime_type: Some("image/png".to_string()),
                        width: None,
                        height: None,
                        has_alpha: None,
                        is_animated: None,
                        frame_count: None,
                    })
                    .unwrap();
                repos.link_file(fa.id, asset.id).unwrap();
                repos.link_file(fb.id, asset.id).unwrap();
                Ok((asset.id, fa.id, fb.id))
            })
            .unwrap();
        assert!(file_a < file_b, "前提: aの方がfile id小");
        let dest = dir.0.join("out");
        // Preserve: 代表file (= min id = orig/a.png) の親構造
        service
            .execute(&[asset_id], &dest, ExportStructure::Preserve)
            .unwrap();
        assert!(dest.join("orig/a.png").exists(), "代表はfile id最小");
        assert!(!dest.join("copy/b.png").exists(), "重複登録は1出力");
        // file id最小がmissingでもpresentな複製を選ぶ
        db.with_writer(|repos| repos.set_scan_state(file_a, ScanState::Missing))
            .unwrap();
        let dest2 = dir.0.join("out2");
        service
            .execute(&[asset_id], &dest2, ExportStructure::Preserve)
            .unwrap();
        assert!(dest2.join("copy/b.png").exists(), "present優先の代表選択");
    }

    // ===== FG-WP16-3 B1: 出力経路の包含 (symlink脱出防止) =====

    /// dir直下のファイル/ディレクトリ列挙 (パス文字列化)。
    fn entry_names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    /// preserve構造で出力dir内の既存symlink (`DEST/sub -> /outside`) を
    /// 辿って外部へ書き出さない (中間ディレクトリ成分の構造的拒否)。
    /// 失敗は項目単位で報告され、外部には1ファイルも作られない。
    #[test]
    #[cfg(unix)]
    fn preserve_export_does_not_escape_through_subdir_symlink() {
        let (dir, db, service, _host, root_dir) = flat_service("symsub");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "sub/dir/x.png", 1);
        let dest = dir.0.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("sub")).unwrap();

        let result = service
            .execute(&[a], &dest, ExportStructure::Preserve)
            .unwrap();
        assert_eq!(
            result.summary.failed, 1,
            "symlink成分への書き出しは失敗報告: {:?}",
            result.summary
        );
        assert!(
            result
                .failures
                .iter()
                .any(|f| f.output_rel_path.contains("sub")),
            "失敗理由に出力先が現れる: {:?}",
            result.failures
        );
        // 外部へは1ファイルも作られない (blockerの本体)
        assert!(
            entry_names(&outside).is_empty(),
            "symlink先の外部へファイルが作られた: {:?}",
            entry_names(&outside)
        );
        // symlink自体は無傷 (指す先を辿って差し替えない)
        assert!(std::fs::symlink_metadata(dest.join("sub"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    /// 出力root自体の最終成分がsymlinkなら実行を拒否する (分かりやすい
    /// エラー報告)。リンク先の実ディレクトリには何も書かない。
    #[test]
    #[cfg(unix)]
    fn symlinked_destination_root_is_rejected() {
        let (dir, db, service, _host, root_dir) = flat_service("symroot-out");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        let real = dir.0.join("real-out");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.0.join("out-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = service
            .execute(&[a], &link, ExportStructure::Flat)
            .unwrap_err();
        assert!(
            err.contains("シンボリックリンク"),
            "root symlinkは日本語で報告: {err}"
        );
        assert!(
            entry_names(&real).is_empty(),
            "リンク先へ書き込まれた: {:?}",
            entry_names(&real)
        );
    }

    /// 最終ファイル名が (外部実体への) symlinkの場合はConflict扱い —
    /// リンクもリンク先の実体も書き換えない。
    #[test]
    #[cfg(unix)]
    fn final_name_symlink_in_destination_is_conflict_not_overwrite() {
        let (dir, db, service, _host, root_dir) = flat_service("symlast");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        let dest = dir.0.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let outside_target = dir.0.join("outside-img.png");
        std::fs::write(&outside_target, b"user file").unwrap();
        std::os::unix::fs::symlink(&outside_target, dest.join("img.png")).unwrap();

        let result = service.execute(&[a], &dest, ExportStructure::Flat).unwrap();
        assert_eq!(result.summary.conflict, 1, "{:?}", result.summary);
        assert_eq!(std::fs::read(&outside_target).unwrap(), b"user file");
        assert!(std::fs::symlink_metadata(dest.join("img.png"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    // ===== FG-WP16-3 N1/N2/N3: summary集計・asset_count・manifest所有権 =====

    /// summaryのconflict/failedはtruncation前の全件から計算される (>100件)。
    #[test]
    fn result_summary_counts_full_issues_before_truncation() {
        let mk_item = |i: i64, image: FileOutcome, txt: FileOutcome| ItemResult {
            asset_id: i,
            asset_uuid: format!("u{i}"),
            display_name: format!("i{i}.png"),
            source_display: format!("/src/i{i}.png"),
            output_rel: format!("i{i}.png"),
            txt_rel: format!("i{i}.txt"),
            image,
            txt,
            caption_origin: CaptionOrigin::None,
        };
        let mut items = Vec::new();
        for i in 0..150 {
            items.push(mk_item(
                i,
                FileOutcome::Failed("x".to_string()),
                FileOutcome::Failed("x".to_string()),
            ));
        }
        for i in 150..260 {
            items.push(mk_item(i, FileOutcome::Conflict, FileOutcome::Conflict));
        }
        let run = ExportRun {
            dest_dir: PathBuf::from("/tmp/unused"),
            structure: ExportStructure::Flat,
            items,
            planning_errors: Vec::new(),
            cancelled: false,
            skipped_by_cancel: 0,
        };
        let dto = build_result_dto(&run, Path::new("/tmp/unused/export-manifest.json"));
        assert_eq!(dto.summary.failed, 150, "{:?}", dto.summary);
        assert_eq!(dto.summary.conflict, 110, "{:?}", dto.summary);
        assert_eq!(dto.summary.total, 260);
        assert!(dto.issues_truncated);
        assert_eq!(dto.failures.len(), ISSUE_REPORT_LIMIT);
        assert_eq!(dto.conflicts.len(), ISSUE_REPORT_LIMIT);
    }

    /// manifestのasset_countはキャンセル未処理を含む (DTOのtotalと同一定義)。
    #[test]
    fn manifest_asset_count_includes_cancelled_skips() {
        let dir = TestDir::new("mcount");
        let item = ItemResult {
            asset_id: 1,
            asset_uuid: "u1".to_string(),
            display_name: "a.png".to_string(),
            source_display: "/src/a.png".to_string(),
            output_rel: "a.png".to_string(),
            txt_rel: "a.txt".to_string(),
            image: FileOutcome::Written,
            txt: FileOutcome::Written,
            caption_origin: CaptionOrigin::None,
        };
        let run = ExportRun {
            dest_dir: dir.0.clone(),
            structure: ExportStructure::Flat,
            items: vec![item],
            planning_errors: vec![PlannedError {
                asset_id: 2,
                reason: "不在".to_string(),
            }],
            cancelled: true,
            skipped_by_cancel: 3,
        };
        let manifest_path = publish_manifest(&run, "2026-09-03T00:00:00Z").unwrap();
        assert_eq!(manifest_path, dir.0.join(EXPORT_MANIFEST_FILE));
        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.0.join(EXPORT_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        // 1 (処理済み) + 1 (plan失敗) + 3 (キャンセル) = 5
        assert_eq!(parsed["asset_count"], 5, "{parsed}");
        assert_eq!(parsed["summary"]["cancelled"], 3);
        assert_eq!(parsed["cancelled"], true);
    }

    /// FG-WP16-4 R3: 正規名の占有者がGEN-TO自身の旧manifestでも置換
    /// (unlink) しない — 回避名へ出版し、旧manifestは残置、戻り値が
    /// 実際のパスを報告する (所有確認→unlink間の差し替えでユーザー
    /// ファイルを消しうるTOCTOUの構造的解消)。
    #[test]
    fn manifest_never_replaces_even_gento_owned_file() {
        let dir = TestDir::new("mown");
        // 先行runのmanifest (正規名をGEN-TO自身が占有)
        let first = cancelled_run_base(dir.0.clone());
        publish_manifest(&first, "2026-09-03T00:00:00Z").unwrap();
        assert!(dir.0.join(EXPORT_MANIFEST_FILE).exists());
        // 再実行 → 回避名へ (旧manifestは残置)
        let second = cancelled_run_base(dir.0.clone());
        let path = publish_manifest(&second, "2026-09-03T00:01:00Z").unwrap();
        assert_ne!(path, dir.0.join(EXPORT_MANIFEST_FILE), "回避名へ出版される");
        assert!(path.to_string_lossy().contains("export-manifest-"));
        // 旧manifestは無傷 (最初のrunの内容のまま)
        let old: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.0.join(EXPORT_MANIFEST_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(old["created_at"], "2026-09-03T00:00:00Z");
        // 新manifestは回避名にあり、最新runの内容
        let new: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(new["created_at"], "2026-09-03T00:01:00Z");
    }

    fn cancelled_run_base(dest_dir: PathBuf) -> ExportRun {
        ExportRun {
            dest_dir,
            structure: ExportStructure::Flat,
            items: Vec::new(),
            planning_errors: Vec::new(),
            cancelled: true,
            skipped_by_cancel: 0,
        }
    }

    /// ユーザー所有の `export-manifest.json` は上書きしない — 回避名へ出版し、
    /// ユーザーファイルは無傷 (FG-WP16-3 N3)。
    #[test]
    fn manifest_never_overwrites_user_owned_file() {
        let dir = TestDir::new("muser");
        let user_file = dir.0.join(EXPORT_MANIFEST_FILE);
        std::fs::write(&user_file, "{\"not\":\"a gento manifest\"}").unwrap();
        let run = cancelled_run_base(dir.0.clone());
        let path = publish_manifest(&run, "2026-09-03T00:00:00Z").unwrap();
        assert_ne!(path, user_file, "回避名へ出版される");
        assert!(path.to_string_lossy().contains("export-manifest-"));
        // ユーザーファイルは無傷
        assert_eq!(
            std::fs::read_to_string(&user_file).unwrap(),
            "{\"not\":\"a gento manifest\"}"
        );
        // 回避名のmanifestも読める
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["format_version"], EXPORT_MANIFEST_VERSION);
        // tmp残骸なし
        let leftovers: Vec<String> = std::fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.contains("gento-tmp").then_some(name)
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    // ===== FG-WP16-5 I2: failure経路のunlink全廃 (never-unlink) =====

    /// create_exclusiveのEEXIST失敗 (tmp名を第三者が事前占有) でも既存
    /// entryを削除しない — 旧実装はwrite失敗のcleanupで
    /// `remove_file_at(&tmp)` を呼び、**自分が作っていないentryを消した**
    /// (r3 reject指摘)。失敗はエラー報告のみで、既存entryは不変。
    #[test]
    fn manifest_tmp_name_eexist_never_deletes_preexisting_entry() {
        let dir = TestDir::new("meexist");
        let uuid = "0123456789abcdef0123456789abcdef";
        let tmp = dir
            .0
            .join(format!(".{EXPORT_MANIFEST_FILE}.gento-tmp-{uuid}-000001"));
        std::fs::write(&tmp, b"preexisting sentinel").unwrap();

        let err = publish_manifest_file_at(&dir.0, b"{}", uuid).unwrap_err();
        assert!(err.contains("gento-tmp"), "tmp名が報告される: {err}");

        // 既存entryは不変 (unlinkされない)
        assert_eq!(
            std::fs::read(&tmp).unwrap(),
            b"preexisting sentinel",
            "EEXIST失敗で事前占有entryを削除してはならない"
        );
        // 正規名・回避名とも作られない
        assert!(!dir.0.join(EXPORT_MANIFEST_FILE).exists());
        assert!(!dir.0.join(format!("export-manifest-{uuid}.json")).exists());
    }

    /// 出版競合 (最終名占有) の失敗経路でもtmpをunlinkしない — 残留は
    /// 自己識別的dotfile `.{名前}.gento-tmp-{32hex}-{6桁seq}` として
    /// 許容される (成功時はrenameがtmpを消費するため残らない。出力先が
    /// 登録ルート内なら起動時掃除の対象)。
    #[test]
    fn publish_conflict_leaves_self_identifying_tmp_residue() {
        let dir = TestDir::new("pubconflict");
        let root = gento_core::OutputDir::open_root(&dir.0).unwrap();
        std::fs::write(dir.0.join("img.png"), b"occupied").unwrap();
        let uuid = "0123456789abcdef0123456789abcdef";
        let mut seq: u32 = 0;
        let outcome =
            publish_bytes_at(&root, OsStr::new("img.png"), b"new content", uuid, &mut seq);
        assert_eq!(outcome, FileOutcome::Conflict);
        // 既存の最終名entryは不変
        assert_eq!(std::fs::read(dir.0.join("img.png")).unwrap(), b"occupied");
        // tmpは残留するが自己識別的 (dot + 厳密形式)
        let tmp = dir
            .0
            .join(".img.png.gento-tmp-0123456789abcdef0123456789abcdef-000000");
        assert!(tmp.exists(), "失敗時のtmp残留を許容 (unlinkしない)");
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with('.') && name.contains(".gento-tmp-"),
            "自己識別的: {name}"
        );
    }

    /// 画像/.txt出版でもtmp名の事前占有 (create_exclusiveのEEXIST) で
    /// 既存entryを削除しない — Failedとして報告し、最終名も作らない。
    #[test]
    fn publish_tmp_name_eexist_never_deletes_preexisting_entry() {
        let dir = TestDir::new("pubeexist");
        let root = gento_core::OutputDir::open_root(&dir.0).unwrap();
        let uuid = "0123456789abcdef0123456789abcdef";
        let tmp = dir
            .0
            .join(".img.png.gento-tmp-0123456789abcdef0123456789abcdef-000000");
        std::fs::write(&tmp, b"sentinel").unwrap();
        let mut seq: u32 = 0;
        let outcome = publish_bytes_at(&root, OsStr::new("img.png"), b"data", uuid, &mut seq);
        assert!(matches!(outcome, FileOutcome::Failed(_)), "{outcome:?}");
        assert_eq!(
            std::fs::read(&tmp).unwrap(),
            b"sentinel",
            "EEXIST失敗で事前占有entryを削除してはならない"
        );
        assert!(!dir.0.join("img.png").exists(), "最終名は作られない");
    }

    // ===== サービス排他 =====

    #[test]
    fn service_rejects_concurrent_execute() {
        let (dir, db, service, _host, root_dir) = flat_service("exclusive");
        let root_id = root_id_of(&db);
        let (a, _, _) = add_asset(&db, root_id, &root_dir, "img.png", 1);
        // running状態を手動で占有 (実行中をシミュレート — fileopsの
        // service_rejects_concurrent_executionと同一方式)
        {
            let mut guard = service.running.lock().unwrap();
            *guard = Some(());
        }
        let err = service
            .execute(&[a], &dir.0.join("out"), ExportStructure::Flat)
            .unwrap_err();
        assert!(err.contains("実行中"), "{err}");
        // シミュレート済みの「実行中」状態を解除 (panicでSomeが残った場合の
        // 回復操作と同じ — 通常実行は終了時に自動でNoneへ戻す)
        {
            let mut guard = service.running.lock().unwrap();
            *guard = None;
        }
        service
            .execute(&[a], &dir.0.join("out"), ExportStructure::Flat)
            .unwrap();
    }
}
