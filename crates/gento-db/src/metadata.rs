//! タグ・キャプションrepositoryのSQLite実装 (MVP WP-5)。
//!
//! [`SqliteRepos`] へ [`gento_core::metadata`] の3 traitを実装する。
//! トランザクション境界は呼び出し側 (`Db::with_writer`) が持つ —
//! 複数文にまたがる操作 (一括付与・キャプション現行入れ替え) のみ
//! メソッド内で `unchecked_transaction` を開く (一部失敗時ロールバック)。

use gento_core::metadata::{
    normalize_tag_name, AssetTagRecord, AssetTagRepo, CaptionKind, CaptionRecord, CaptionRepo,
    CaptionSource, TagRecord, TagRepo, TagSource,
};
use gento_core::repo::{AssetRepo, RepoError};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_err, SqliteRepos};

/// tags行のSELECT列一覧 + 付与数 (LEFT JOIN集計)。
/// `usage.cnt` はCOALESCEで0詰め。
const TAG_COLUMNS: &str = "t.id, t.uuid, t.display_name, t.normalized_name, t.category, \
     t.created_at, t.updated_at, COALESCE(u.cnt, 0)";

/// tags + 使用数集計のFROM句 (共通)。
const TAG_FROM: &str = "FROM tags t \
     LEFT JOIN (SELECT tag_id, count(*) AS cnt FROM asset_tags GROUP BY tag_id) u \
     ON u.tag_id = t.id";

/// asset_tags行のSELECT列一覧 (tags JOIN)。
const ASSET_TAG_COLUMNS: &str = "at.asset_id, at.source, at.manually_edited, at.created_at, \
     t.id, t.uuid, t.display_name, t.normalized_name, t.category, t.created_at, t.updated_at, \
     COALESCE(u.cnt, 0)";

const ASSET_TAG_FROM: &str = "FROM asset_tags at \
     JOIN tags t ON t.id = at.tag_id \
     LEFT JOIN (SELECT tag_id, count(*) AS cnt FROM asset_tags GROUP BY tag_id) u \
     ON u.tag_id = t.id";

const CAPTION_COLUMNS: &str = "id, uuid, asset_id, kind, body, source, language, current, \
     manually_edited, created_at, updated_at";

fn tag_source_from_db(s: &str) -> rusqlite::Result<TagSource> {
    match s {
        "manual" => Ok(TagSource::Manual),
        "ai" => Ok(TagSource::Ai),
        "import" => Ok(TagSource::Import),
        "embedded_metadata" => Ok(TagSource::EmbeddedMetadata),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown asset_tags.source: {other}").into(),
        )),
    }
}

fn caption_source_from_db(s: &str) -> rusqlite::Result<CaptionSource> {
    match s {
        "manual" => Ok(CaptionSource::Manual),
        "ai" => Ok(CaptionSource::Ai),
        "import" => Ok(CaptionSource::Import),
        "embedded_metadata" => Ok(CaptionSource::EmbeddedMetadata),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown captions.source: {other}").into(),
        )),
    }
}

fn caption_kind_from_db(s: &str) -> rusqlite::Result<CaptionKind> {
    match s {
        "description" => Ok(CaptionKind::Description),
        "sd_tags" => Ok(CaptionKind::SdTags),
        "user_note" => Ok(CaptionKind::UserNote),
        "ocr" => Ok(CaptionKind::Ocr),
        "embedded_prompt" => Ok(CaptionKind::EmbeddedPrompt),
        "embedded_negative_prompt" => Ok(CaptionKind::EmbeddedNegativePrompt),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown captions.kind: {other}").into(),
        )),
    }
}

/// タグ使用数 (i64) をu64へ (負値はschema上発生しないため飽和)。
fn usage_of(v: i64) -> u64 {
    v.max(0) as u64
}

fn map_tag(row: &rusqlite::Row<'_>) -> rusqlite::Result<TagRecord> {
    Ok(TagRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        display_name: row.get(2)?,
        normalized_name: row.get(3)?,
        category: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        usage_count: usage_of(row.get(7)?),
    })
}

fn map_asset_tag(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetTagRecord> {
    let source: String = row.get(1)?;
    Ok(AssetTagRecord {
        asset_id: row.get(0)?,
        source: tag_source_from_db(&source)?,
        manually_edited: row.get::<_, i64>(2)? != 0,
        created_at: row.get(3)?,
        tag: TagRecord {
            id: row.get(4)?,
            uuid: row.get(5)?,
            display_name: row.get(6)?,
            normalized_name: row.get(7)?,
            category: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            usage_count: usage_of(row.get(11)?),
        },
    })
}

fn map_caption(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaptionRecord> {
    let kind: String = row.get(3)?;
    let source: String = row.get(5)?;
    Ok(CaptionRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        asset_id: row.get(2)?,
        kind: caption_kind_from_db(&kind)?,
        body: row.get(4)?,
        source: caption_source_from_db(&source)?,
        language: row.get(6)?,
        is_current: row.get::<_, i64>(7)? != 0,
        manually_edited: row.get::<_, i64>(8)? != 0,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

/// LIKE句のワイルドカード (`%` `_` `\`) をエスケープしたパターンを返す。
/// エスケープ文字は `\` (SQL内でESCAPE '\'と対で使用)。
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// LIKEパターン文字列中の `\` をさらに文字列リテラルとしてエスケープ。
/// (patternをparamsで渡すためSQLリテラルエスケープは不要 — この関数は
/// テストでの期待値構築にのみ使う。実装誤差を防ぐため公開しない。)
#[cfg(test)]
fn debug_like_pattern(input: &str) -> String {
    escape_like(input)
}

/// タグを正規化名で検索 (1接続。usage付き)。
fn find_tag_by_normalized(
    conn: &Connection,
    normalized: &str,
) -> Result<Option<TagRecord>, RepoError> {
    let sql = format!("SELECT {TAG_COLUMNS} {TAG_FROM} WHERE t.normalized_name = ?1");
    conn.query_row(&sql, [normalized], map_tag)
        .optional()
        .map_err(map_err)
}

/// タグを挿入する。normalized_nameのUNIQUE衝突は [`RepoError::Duplicate`]。
fn insert_tag(conn: &Connection, display_name: &str, normalized: &str) -> Result<i64, RepoError> {
    let now = now_utc_rfc3339();
    conn.execute(
        "INSERT INTO tags (uuid, display_name, normalized_name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![new_uuid_v7(), display_name, normalized, now],
    )
    .map_err(map_err)?;
    Ok(conn.last_insert_rowid())
}

/// タグ主キー検索 (usage付き)。見つからなければNone。
fn get_tag_by_id(conn: &Connection, tag_id: i64) -> Result<Option<TagRecord>, RepoError> {
    let sql = format!("SELECT {TAG_COLUMNS} {TAG_FROM} WHERE t.id = ?1");
    conn.query_row(&sql, [tag_id], map_tag)
        .optional()
        .map_err(map_err)
}

impl TagRepo for SqliteRepos<'_> {
    fn create_tag(&self, name: &str) -> Result<TagRecord, RepoError> {
        let normalized = normalize_tag_name(name);
        if normalized.is_empty() {
            return Err(RepoError::Constraint(
                "tag name is empty after normalization".to_string(),
            ));
        }
        if let Some(existing) = find_tag_by_normalized(self.conn(), &normalized)? {
            return Ok(existing);
        }
        match insert_tag(self.conn(), name.trim(), &normalized) {
            Ok(id) => get_tag_by_id(self.conn(), id)?
                .ok_or_else(|| RepoError::Db("inserted tag row is not readable".to_string())),
            // 並行書き込みでUNIQUEに敗れた場合も既存タグへ統合 (冪等)
            Err(RepoError::Duplicate) => {
                find_tag_by_normalized(self.conn(), &normalized)?.ok_or(RepoError::Duplicate)
            }
            Err(err) => Err(err),
        }
    }

    fn find_tag_by_name(&self, name: &str) -> Result<Option<TagRecord>, RepoError> {
        find_tag_by_normalized(self.conn(), &normalize_tag_name(name))
    }

    fn get_tag(&self, tag_id: i64) -> Result<Option<TagRecord>, RepoError> {
        get_tag_by_id(self.conn(), tag_id)
    }

    fn suggest_tags(&self, prefix: &str, limit: u32) -> Result<Vec<TagRecord>, RepoError> {
        let pattern = format!("{}%", escape_like(&normalize_tag_name(prefix)));
        let sql = format!(
            "SELECT {TAG_COLUMNS} {TAG_FROM} \
             WHERE t.normalized_name LIKE ?1 ESCAPE '\\' \
             ORDER BY COALESCE(u.cnt, 0) DESC, t.normalized_name ASC LIMIT ?2"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![pattern, limit], map_tag)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn list_tags(&self) -> Result<Vec<TagRecord>, RepoError> {
        let sql = format!(
            "SELECT {TAG_COLUMNS} {TAG_FROM} \
             ORDER BY COALESCE(u.cnt, 0) DESC, t.normalized_name ASC"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([], map_tag)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn delete_tag(&self, tag_id: i64) -> Result<(), RepoError> {
        // 削除は「使用中のasset_tagsごと削除」で実行する (付随解除)。
        // tags削除に対しasset_tagsはON DELETE CASCADE。
        let changed = self
            .conn()
            .execute("DELETE FROM tags WHERE id = ?1", [tag_id])
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
}

impl AssetTagRepo for SqliteRepos<'_> {
    fn add_tags_batch(
        &self,
        asset_ids: &[i64],
        names: &[&str],
    ) -> Result<Vec<AssetTagRecord>, RepoError> {
        if asset_ids.is_empty() || names.is_empty() {
            return Ok(Vec::new());
        }
        // 事前検証 (tx内でも再検証するが、正規化エラーを先に確定させる)
        let mut normalized_names: Vec<(String, String)> = Vec::with_capacity(names.len());
        for name in names {
            let normalized = normalize_tag_name(name);
            if normalized.is_empty() {
                return Err(RepoError::Constraint(
                    "tag name is empty after normalization".to_string(),
                ));
            }
            normalized_names.push((name.trim().to_string(), normalized));
        }
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let out = {
            // 1) タグを作成 or 既存統合 (batch内で一意化)
            let mut tag_ids: Vec<(i64, String)> = Vec::with_capacity(normalized_names.len());
            for (display, normalized) in &normalized_names {
                if let Some((id, n)) = tag_ids.iter().find(|(_, n)| n == normalized) {
                    let id = *id;
                    let n = n.clone();
                    tag_ids.push((id, n));
                    continue;
                }
                let id = match find_tag_by_normalized(&tx, normalized)? {
                    Some(existing) => existing.id,
                    None => insert_tag(&tx, display, normalized)?,
                };
                tag_ids.push((id, normalized.clone()));
            }
            // 2) Asset存在検証 + 付与 (upsert。手動追加はmanually_edited=1)
            let now = now_utc_rfc3339();
            {
                let mut check = tx
                    .prepare("SELECT 1 FROM assets WHERE id = ?1")
                    .map_err(map_err)?;
                for asset_id in asset_ids {
                    check
                        .query_row([asset_id], |_| Ok(()))
                        .optional()
                        .map_err(map_err)?
                        .ok_or(RepoError::NotFound)?;
                }
            }
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO asset_tags
                             (asset_id, tag_id, source, manually_edited, created_at)
                         VALUES (?1, ?2, 'manual', 1, ?3)
                         ON CONFLICT (asset_id, tag_id) DO UPDATE SET
                             -- 手動再付与は手動編集済みとして保護対象にする。
                             -- sourceは初回由来を保持する (AI由来ならaiのまま)。
                             manually_edited = 1",
                    )
                    .map_err(map_err)?;
                for asset_id in asset_ids {
                    for (tag_id, _) in &tag_ids {
                        stmt.execute(params![asset_id, tag_id, now])
                            .map_err(map_err)?;
                    }
                }
            }
            // 3) 結果を読み戻す (asset×tagの全組)
            let mut out = Vec::with_capacity(asset_ids.len() * tag_ids.len());
            let sql = format!(
                "SELECT {ASSET_TAG_COLUMNS} {ASSET_TAG_FROM} \
                 WHERE at.asset_id = ?1 AND at.tag_id = ?2"
            );
            let mut read = tx.prepare(&sql).map_err(map_err)?;
            for asset_id in asset_ids {
                for (tag_id, _) in &tag_ids {
                    if let Some(record) = read
                        .query_row(params![asset_id, tag_id], map_asset_tag)
                        .optional()
                        .map_err(map_err)?
                    {
                        out.push(record);
                    }
                }
            }
            out
        };
        tx.commit().map_err(map_err)?;
        Ok(out)
    }

    fn remove_tag_from_asset(&self, asset_id: i64, tag_id: i64) -> Result<(), RepoError> {
        let changed = self
            .conn()
            .execute(
                "DELETE FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
                params![asset_id, tag_id],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    fn list_asset_tags(&self, asset_id: i64) -> Result<Vec<AssetTagRecord>, RepoError> {
        let sql = format!(
            "SELECT {ASSET_TAG_COLUMNS} {ASSET_TAG_FROM} \
             WHERE at.asset_id = ?1 ORDER BY t.normalized_name ASC"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([asset_id], map_asset_tag)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn assets_of_tag(&self, tag_id: i64) -> Result<Vec<i64>, RepoError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?1 ORDER BY asset_id")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([tag_id], |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(map_err)?;
        Ok(rows)
    }
}

impl CaptionRepo for SqliteRepos<'_> {
    fn set_caption(
        &self,
        asset_id: i64,
        kind: CaptionKind,
        body: &str,
    ) -> Result<CaptionRecord, RepoError> {
        // Asset不在を先に確定 (FK違反より親切なエラー)
        if self.get_asset(asset_id)?.is_none() {
            return Err(RepoError::NotFound);
        }
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let record = {
            let now = now_utc_rfc3339();
            // 旧現行行を履歴へ降格 (削除しない。data-model §5)
            tx.execute(
                "UPDATE captions SET current = 0, updated_at = ?3
                 WHERE asset_id = ?1 AND kind = ?2 AND current = 1",
                params![asset_id, kind.as_str(), now],
            )
            .map_err(map_err)?;
            tx.execute(
                "INSERT INTO captions
                     (uuid, asset_id, kind, body, source, current, manually_edited,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'manual', 1, 1, ?5, ?5)",
                params![new_uuid_v7(), asset_id, kind.as_str(), body, now],
            )
            .map_err(map_err)?;
            let sql = format!(
                "SELECT {CAPTION_COLUMNS} FROM captions \
                 WHERE asset_id = ?1 AND kind = ?2 AND current = 1"
            );
            tx.query_row(&sql, params![asset_id, kind.as_str()], map_caption)
                .map_err(map_err)?
        };
        tx.commit().map_err(map_err)?;
        Ok(record)
    }

    fn get_current_caption(
        &self,
        asset_id: i64,
        kind: CaptionKind,
    ) -> Result<Option<CaptionRecord>, RepoError> {
        let sql = format!(
            "SELECT {CAPTION_COLUMNS} FROM captions \
             WHERE asset_id = ?1 AND kind = ?2 AND current = 1"
        );
        self.conn()
            .query_row(&sql, params![asset_id, kind.as_str()], map_caption)
            .optional()
            .map_err(map_err)
    }

    fn list_caption_history(
        &self,
        asset_id: i64,
        kind: CaptionKind,
        limit: u32,
    ) -> Result<Vec<CaptionRecord>, RepoError> {
        let sql = format!(
            "SELECT {CAPTION_COLUMNS} FROM captions \
             WHERE asset_id = ?1 AND kind = ?2 \
             ORDER BY current DESC, id DESC LIMIT ?3"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![asset_id, kind.as_str(), limit], map_caption)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use gento_core::metadata::normalize_tag_name;
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState, TriageState,
    };
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-meta-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn with_repos<T>(tag: &str, f: impl FnOnce(&SqliteRepos<'_>) -> T) -> T {
        let tmp = TempDir::new(tag);
        let mut conn = Connection::open(tmp.0.join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let repos = SqliteRepos::new(&conn);
        f(&repos)
    }

    fn new_asset(hash: &str) -> NewAsset {
        NewAsset {
            content_hash: format!("blake3:{hash}"),
            hash_algorithm: "blake3".to_string(),
            byte_size: 10,
            mime_type: None,
            width: None,
            height: None,
            has_alpha: None,
            is_animated: None,
            frame_count: None,
        }
    }

    fn make_asset(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        repos.create_asset(new_asset(hash)).unwrap().id
    }

    fn upsert_file(repos: &SqliteRepos<'_>, root_id: i64, rel: &str) -> i64 {
        repos
            .upsert_file(FileUpsert {
                root_id,
                rel_path: PathBuf::from(rel),
                display_name: rel.to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap()
            .id
    }

    // ===== 評価・選別・お気に入り =====

    #[test]
    fn rating_bounds_and_updates() {
        with_repos("rating", |repos| {
            let a = make_asset(repos, "aa");
            assert_eq!(repos.get_asset(a).unwrap().unwrap().rating, 0);
            for ok in [0u8, 1, 3, 5] {
                repos.set_rating(a, ok).unwrap();
                assert_eq!(repos.get_asset(a).unwrap().unwrap().rating, ok);
            }
            for ng in [6u8, 255] {
                assert!(matches!(
                    repos.set_rating(a, ng),
                    Err(RepoError::Constraint(_))
                ));
            }
            assert_eq!(repos.set_rating(9999, 3).unwrap_err(), RepoError::NotFound);
        });
    }

    #[test]
    fn cull_state_transitions_and_favorite() {
        with_repos("cull", |repos| {
            let a = make_asset(repos, "bb");
            let triage_of = |repos: &SqliteRepos<'_>| repos.get_asset(a).unwrap().unwrap().triage;
            assert_eq!(triage_of(repos), TriageState::Unselected);
            repos.set_cull_state(a, TriageState::Picked).unwrap();
            assert_eq!(triage_of(repos), TriageState::Picked);
            repos.set_cull_state(a, TriageState::Rejected).unwrap();
            assert_eq!(triage_of(repos), TriageState::Rejected);
            // 除外→未選別へ戻せる (除外は削除ではない)
            repos.set_cull_state(a, TriageState::Unselected).unwrap();
            assert_eq!(triage_of(repos), TriageState::Unselected);

            assert!(!repos.get_asset(a).unwrap().unwrap().favorite);
            repos.set_favorite(a, true).unwrap();
            assert!(repos.get_asset(a).unwrap().unwrap().favorite);
            repos.set_favorite(a, false).unwrap();
            assert!(!repos.get_asset(a).unwrap().unwrap().favorite);

            assert_eq!(
                repos.set_cull_state(9999, TriageState::Picked).unwrap_err(),
                RepoError::NotFound
            );
            assert_eq!(
                repos.set_favorite(9999, true).unwrap_err(),
                RepoError::NotFound
            );
        });
    }

    #[test]
    fn batch_updates_are_atomic_on_missing_id() {
        with_repos("batchatomic", |repos| {
            let a1 = make_asset(repos, "c1");
            let a2 = make_asset(repos, "c2");
            // a3は作らない → バッチ途中でNotFound → 全体ロールバック
            let err = repos.set_rating_batch(&[a1, a2, 9999], 5).unwrap_err();
            assert_eq!(err, RepoError::NotFound);
            assert_eq!(
                repos.get_asset(a1).unwrap().unwrap().rating,
                0,
                "ロールバック済み"
            );
            assert_eq!(repos.get_asset(a2).unwrap().unwrap().rating, 0);

            // 正常系: 全件適用
            repos.set_rating_batch(&[a1, a2], 4).unwrap();
            assert_eq!(repos.get_asset(a1).unwrap().unwrap().rating, 4);
            assert_eq!(repos.get_asset(a2).unwrap().unwrap().rating, 4);

            // バッチでも評価の範囲検証は効く
            assert!(matches!(
                repos.set_rating_batch(&[a1], 6),
                Err(RepoError::Constraint(_))
            ));

            repos
                .set_cull_state_batch(&[a1, a2], TriageState::Rejected)
                .unwrap();
            repos.set_favorite_batch(&[a1], true).unwrap();
            let record = repos.get_asset(a1).unwrap().unwrap();
            assert_eq!(record.triage, TriageState::Rejected);
            assert!(record.favorite);
        });
    }

    #[test]
    fn logical_metadata_shared_across_files_of_same_asset() {
        // 受入条件1: 同一ハッシュの2 File → 片方の評価がもう片方からも見える
        with_repos("share", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "shared");
            let f1 = upsert_file(repos, root.id, "a.png");
            let f2 = upsert_file(repos, root.id, "copy/a.png");
            repos.link_file(f1, asset).unwrap();
            repos.link_file(f2, asset).unwrap();

            repos.set_rating(asset, 5).unwrap();
            repos.set_cull_state(asset, TriageState::Picked).unwrap();
            repos.set_favorite(asset, true).unwrap();
            repos.add_tags_batch(&[asset], &["sky"]).unwrap();
            repos
                .set_caption(asset, CaptionKind::UserNote, "メモ")
                .unwrap();

            // 両Fileが同一Asset (同一メタデータ) を参照
            for file in [f1, f2] {
                let record = repos.get_file(file).unwrap().unwrap();
                assert_eq!(record.asset_id, Some(asset));
            }
            let loaded = repos.get_asset(asset).unwrap().unwrap();
            assert_eq!((loaded.rating, loaded.favorite), (5, true));
            assert_eq!(loaded.triage, TriageState::Picked);
            let tags = repos.list_asset_tags(asset).unwrap();
            assert_eq!(tags.len(), 1);
            assert_eq!(tags[0].tag.normalized_name, "sky");
            let note = repos
                .get_current_caption(asset, CaptionKind::UserNote)
                .unwrap()
                .unwrap();
            assert_eq!(note.body, "メモ");
            assert!(note.manually_edited);
            assert_eq!(note.source, CaptionSource::Manual);
        });
    }

    // ===== タグ正規化・一覧・候補 =====

    #[test]
    fn tag_normalization_merges_variants() {
        with_repos("tagnorm", |repos| {
            let t1 = repos.create_tag("  Sky  ").unwrap();
            // 大文字・前後空白・内部空白連続は同一タグへ統合される
            let t2 = repos.create_tag("SKY").unwrap();
            let t3 = repos.create_tag(" sky ").unwrap();
            assert_eq!(t1.id, t2.id);
            assert_eq!(t1.id, t3.id);
            assert_eq!(t1.normalized_name, "sky");
            // 表示名は初回登録時のまま
            assert_eq!(t1.display_name, "Sky");

            // 全角空白も空白扱い (半角1つへ)
            let wide = repos.create_tag("青　空").unwrap();
            assert_eq!(wide.normalized_name, "青 空");
            let wide2 = repos.create_tag("青 空").unwrap();
            assert_eq!(wide.id, wide2.id, "全角空白と半角空白は統合");

            // NFC: 分解形の濁点は合成形へ
            let composed = repos.create_tag("が").unwrap();
            assert_eq!(composed.normalized_name, normalize_tag_name("が"));
            let decomposed = repos.create_tag("\u{304B}\u{3099}").unwrap();
            assert_eq!(composed.id, decomposed.id, "NFC正規化で統合");

            // 全角英字はNFKCしないため別タグ
            let fullwidth = repos.create_tag("ＴＡＧ").unwrap();
            let halfwidth = repos.create_tag("TAG").unwrap();
            assert_ne!(fullwidth.id, halfwidth.id);

            // 空・空白のみ拒否
            assert!(matches!(
                repos.create_tag("   "),
                Err(RepoError::Constraint(_))
            ));
        });
    }

    #[test]
    fn suggest_orders_by_usage_and_prefix() {
        with_repos("suggest", |repos| {
            let a1 = make_asset(repos, "s1");
            let a2 = make_asset(repos, "s2");
            let a3 = make_asset(repos, "s3");
            repos.add_tags_batch(&[a1, a2, a3], &["sky"]).unwrap();
            repos.add_tags_batch(&[a1, a2], &["skyline"]).unwrap();
            repos.add_tags_batch(&[a1], &["sunset"]).unwrap();
            repos.add_tags_batch(&[a1], &["other"]).unwrap();

            let hits = repos.suggest_tags("sky", 10).unwrap();
            let names: Vec<&str> = hits.iter().map(|t| t.normalized_name.as_str()).collect();
            assert_eq!(names, vec!["sky", "skyline"], "使用数降順");
            assert_eq!(hits[0].usage_count, 3);
            assert_eq!(hits[1].usage_count, 2);

            // 前方一致のみ (部分一致しない)
            let hits = repos.suggest_tags("sun", 10).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].normalized_name, "sunset");
            assert!(repos.suggest_tags("kie", 10).unwrap().is_empty());

            // 正規化を挟む (大文字入力)
            let hits = repos.suggest_tags("SKY", 10).unwrap();
            assert_eq!(hits.len(), 2);

            // limit
            assert_eq!(repos.suggest_tags("sky", 1).unwrap().len(), 1);

            // LIKE特殊文字を含むタグ
            repos.add_tags_batch(&[a1], &["100%"]).unwrap();
            let hits = repos.suggest_tags("100%", 10).unwrap();
            assert_eq!(hits.len(), 1, "%はリテラル扱い");
            let hits = repos.suggest_tags("10", 10).unwrap();
            assert_eq!(hits.len(), 1, "prefix検索");
        });
    }

    #[test]
    fn asset_tags_manual_flag_and_removal() {
        with_repos("assettags", |repos| {
            let a1 = make_asset(repos, "t1");
            let a2 = make_asset(repos, "t2");
            // 一括付与 (2 asset × 2 tag) = 4組。すべてmanual+manually_edited
            let added = repos.add_tags_batch(&[a1, a2], &["Red", "blue"]).unwrap();
            assert_eq!(added.len(), 4);
            for record in &added {
                assert_eq!(record.source, TagSource::Manual);
                assert!(record.manually_edited, "手動追加はmanually_edited=1");
            }
            let tags1 = repos.list_asset_tags(a1).unwrap();
            assert_eq!(tags1.len(), 2);
            // 正規化名昇順
            assert_eq!(tags1[0].tag.normalized_name, "blue");
            assert_eq!(tags1[1].tag.normalized_name, "red");

            // 再付与は冪等 (重複行を作らない)
            repos.add_tags_batch(&[a1], &["red"]).unwrap();
            assert_eq!(repos.list_asset_tags(a1).unwrap().len(), 2);

            // 使用数
            let red_id = tags1[1].tag.id;
            let list = repos.list_tags().unwrap();
            let red = list.iter().find(|t| t.id == red_id).unwrap();
            assert_eq!(red.usage_count, 2, "a1+a2で2");

            // 解除
            repos.remove_tag_from_asset(a1, red_id).unwrap();
            assert_eq!(repos.list_asset_tags(a1).unwrap().len(), 1);
            assert_eq!(
                repos.remove_tag_from_asset(a1, red_id).unwrap_err(),
                RepoError::NotFound
            );

            // 不在assetへの付与はバッチ全体がロールバック
            let err = repos.add_tags_batch(&[a2, 9999], &["x1"]).unwrap_err();
            assert_eq!(err, RepoError::NotFound);
            assert!(
                repos
                    .list_asset_tags(a2)
                    .unwrap()
                    .iter()
                    .all(|t| t.tag.normalized_name != "x1"),
                "ロールバックでx1は付与されていない"
            );

            // タグ削除は付随解除 (asset_tagsごと)
            let blue_id = repos.list_asset_tags(a1).unwrap()[0].tag.id;
            repos.delete_tag(blue_id).unwrap();
            assert!(repos.list_asset_tags(a1).unwrap().is_empty());
            assert!(repos.get_tag(blue_id).unwrap().is_none());
            assert_eq!(repos.delete_tag(blue_id).unwrap_err(), RepoError::NotFound);
        });
    }

    // ===== キャプション =====

    #[test]
    fn caption_current_single_row_and_history() {
        with_repos("caption", |repos| {
            let a = make_asset(repos, "cap");
            // 初回設定
            let c1 = repos
                .set_caption(a, CaptionKind::Description, "初回")
                .unwrap();
            assert!(c1.is_current);
            assert!(c1.manually_edited);
            assert_eq!(c1.source, CaptionSource::Manual);

            // 編集 → 旧現行は履歴へ (現行は1行だけ)
            let c2 = repos
                .set_caption(a, CaptionKind::Description, "編集後")
                .unwrap();
            assert_ne!(c1.id, c2.id);
            let current = repos
                .get_current_caption(a, CaptionKind::Description)
                .unwrap()
                .unwrap();
            assert_eq!(current.body, "編集後");
            let history = repos
                .list_caption_history(a, CaptionKind::Description, 10)
                .unwrap();
            assert_eq!(history.len(), 2);
            assert!(history[0].is_current);
            assert!(!history[1].is_current);
            assert_eq!(history[1].body, "初回");

            // kindごとに独立 (現行一行制約はkind単位)
            repos
                .set_caption(a, CaptionKind::SdTags, "tag1, tag2")
                .unwrap();
            repos.set_caption(a, CaptionKind::UserNote, "メモ").unwrap();
            for kind in CaptionKind::EDITABLE {
                let current = repos.get_current_caption(a, kind).unwrap().unwrap();
                assert_eq!(current.kind, kind);
            }
            // 編集UI対象外のkindも構造上は保持可能 (WP-9/10で使用)
            repos
                .set_caption(a, CaptionKind::EmbeddedPrompt, "prompt")
                .unwrap();

            // 不在asset
            assert_eq!(
                repos
                    .set_caption(9999, CaptionKind::UserNote, "x")
                    .unwrap_err(),
                RepoError::NotFound
            );
            assert!(repos
                .get_current_caption(9999, CaptionKind::UserNote)
                .unwrap()
                .is_none());

            // 空本文も許容 (ユーザーによるクリア)
            let cleared = repos.set_caption(a, CaptionKind::UserNote, "").unwrap();
            assert_eq!(cleared.body, "");
        });
    }

    #[test]
    fn caption_current_unique_constraint_direct() {
        // schemaの部分UNIQUE indexを直接検証 (現行2行の挿入は拒否)
        with_repos("capunique", |repos| {
            let a = make_asset(repos, "cu");
            repos.set_caption(a, CaptionKind::UserNote, "1").unwrap();
            let err = repos
                .conn()
                .execute(
                    "INSERT INTO captions (uuid, asset_id, kind, body, source, current,
                        manually_edited, created_at, updated_at)
                     VALUES ('00000000-0000-7000-8000-0000000000c1', ?1, 'user_note', 'x',
                        'manual', 1, 0, '2026-08-31T00:00:00.000Z', '2026-08-31T00:00:00.000Z')",
                    [a],
                )
                .unwrap_err();
            assert!(
                matches!(map_err(err), RepoError::Duplicate),
                "現行2行はUNIQUE違反"
            );
            // 履歴 (current=0) は複数行OK
            repos
                .conn()
                .execute(
                    "INSERT INTO captions (uuid, asset_id, kind, body, source, current,
                        manually_edited, created_at, updated_at)
                     VALUES ('00000000-0000-7000-8000-0000000000c2', ?1, 'user_note', 'y',
                        'manual', 0, 0, '2026-08-31T00:00:00.000Z', '2026-08-31T00:00:00.000Z')",
                    [a],
                )
                .unwrap();
        });
    }

    #[test]
    fn metadata_survives_crash_like_reopen() {
        // 異常終了整合の担保要素: 各setterは単一txでコミットされるため、
        // 更新途中の強制終了で部分状態が残らない。ここでは同一接続での
        // commit後の読み戻しと、Db::open相当の再接続で値が復元されることを確認
        // (WALクラッシュ回帰はgento-db lib.testで既証明)。
        let tmp = TempDir::new("reopen-meta");
        let db_path = tmp.0.join("db.sqlite3");
        let asset_id = {
            let mut conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "foreign_keys", true).unwrap();
            run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
            let repos = SqliteRepos::new(&conn);
            let a = make_asset(&repos, "persist");
            repos.set_rating(a, 3).unwrap();
            repos.add_tags_batch(&[a], &["keep"]).unwrap();
            repos
                .set_caption(a, CaptionKind::Description, "説明")
                .unwrap();
            a
        };
        // 接続を閉じて開き直す (プロセス再起動相当)
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let repos = SqliteRepos::new(&conn);
        let asset = repos.get_asset(asset_id).unwrap().unwrap();
        assert_eq!(asset.rating, 3);
        assert_eq!(repos.list_asset_tags(asset_id).unwrap().len(), 1);
        assert_eq!(
            repos
                .get_current_caption(asset_id, CaptionKind::Description)
                .unwrap()
                .unwrap()
                .body,
            "説明"
        );
    }

    #[test]
    fn like_escape_helper() {
        assert_eq!(debug_like_pattern("a%b_c\\d"), "a\\%b\\_c\\\\d");
        assert_eq!(debug_like_pattern("通常"), "通常");
    }
}
