//! `asset_metadata` のSQLite実装 (WP-9)。
//!
//! - 1 asset = 複数parser行 (UNIQUE(asset_id, parser_name))。同version行は
//!   冪等スキップ (内容はcontent hashから決定的)、version違いは置換。
//! - positive_prompt (呼び出し側優先順位で選択済み) を `embedded_prompt`
//!   キャプション (source=embedded_metadata) へ同一txで書き、検索文書を
//!   再構築する。手動編集済みの現行行は上書きしない (仕様§10保護)。
//! - 再解析候補: 現行parser集合に対し行が欠落/古いasset + 代表presentファイル。

use gento_core::assetmeta::{
    AssetMetadataParsed, AssetMetadataRecord, AssetMetadataRepo, MetadataColumns, ReparseTarget,
};
use gento_core::repo::RepoError;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_err, map_file, SqliteRepos};

const METADATA_COLUMNS: &str = "id, uuid, asset_id, parser_name, parser_version, taken_at, \
     camera_make, camera_model, lens, gps_latitude, gps_longitude, orientation, \
     generator, generation_model, vae, sampler, scheduler, steps, cfg_scale, seed, \
     positive_prompt, negative_prompt, generation_extra, raw_exif, raw_iptc, \
     raw_png_metadata, raw_workflow, created_at, updated_at";

fn map_metadata_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetMetadataRecord> {
    Ok(AssetMetadataRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        asset_id: row.get(2)?,
        parsed: AssetMetadataParsed {
            parser_name: row.get(3)?,
            parser_version: row.get(4)?,
            columns: MetadataColumns {
                taken_at: row.get(5)?,
                camera_make: row.get(6)?,
                camera_model: row.get(7)?,
                lens: row.get(8)?,
                gps_latitude: row.get(9)?,
                gps_longitude: row.get(10)?,
                orientation: row.get(11)?,
                generator: row.get(12)?,
                generation_model: row.get(13)?,
                vae: row.get(14)?,
                sampler: row.get(15)?,
                scheduler: row.get(16)?,
                steps: row.get(17)?,
                cfg_scale: row.get(18)?,
                seed: row.get(19)?,
                positive_prompt: row.get(20)?,
                negative_prompt: row.get(21)?,
                generation_extra: row.get(22)?,
            },
            raw_exif: row.get(23)?,
            raw_iptc: row.get(24)?,
            raw_png_metadata: row.get(25)?,
            raw_workflow: row.get(26)?,
        },
        created_at: row.get(27)?,
        updated_at: row.get(28)?,
    })
}

/// 行を挿入する (既存行のDELETE後。同一tx内で呼ぶ)。
fn insert_metadata_row(
    tx: &Connection,
    asset_id: i64,
    item: &AssetMetadataParsed,
) -> Result<(), RepoError> {
    let now = now_utc_rfc3339();
    tx.execute(
        "INSERT INTO asset_metadata
             (uuid, asset_id, parser_name, parser_version, taken_at, camera_make,
              camera_model, lens, gps_latitude, gps_longitude, orientation,
              generator, generation_model, vae, sampler, scheduler, steps,
              cfg_scale, seed, positive_prompt, negative_prompt, generation_extra,
              raw_exif, raw_iptc, raw_png_metadata, raw_workflow,
              created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?27)",
        params![
            new_uuid_v7(),
            asset_id,
            item.parser_name,
            item.parser_version,
            item.columns.taken_at,
            item.columns.camera_make,
            item.columns.camera_model,
            item.columns.lens,
            item.columns.gps_latitude,
            item.columns.gps_longitude,
            item.columns.orientation,
            item.columns.generator,
            item.columns.generation_model,
            item.columns.vae,
            item.columns.sampler,
            item.columns.scheduler,
            item.columns.steps,
            item.columns.cfg_scale,
            item.columns.seed,
            item.columns.positive_prompt,
            item.columns.negative_prompt,
            item.columns.generation_extra,
            item.raw_exif,
            item.raw_iptc,
            item.raw_png_metadata,
            item.raw_workflow,
            now,
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// 現行embedded_promptキャプションを書き換える (手動編集行は保護)。
/// 戻り値 = 変更があったか。同一tx内で呼ぶ。
fn upsert_embedded_prompt(tx: &Connection, asset_id: i64, prompt: &str) -> Result<bool, RepoError> {
    let manually_edited: Option<i64> = tx
        .query_row(
            "SELECT manually_edited FROM captions \
             WHERE asset_id = ?1 AND kind = 'embedded_prompt' AND current = 1",
            [asset_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_err)?;
    match manually_edited {
        Some(1) => return Ok(false), // 手動編集済み — 上書きしない
        Some(0) => {
            let current_body: Option<String> = tx
                .query_row(
                    "SELECT body FROM captions \
                     WHERE asset_id = ?1 AND kind = 'embedded_prompt' AND current = 1",
                    [asset_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_err)?;
            if current_body.as_deref() == Some(prompt) {
                return Ok(false); // 同一内容 — 書き換え不要
            }
        }
        _ => {}
    }
    let now = now_utc_rfc3339();
    // 旧現行行を履歴へ降格してから新現行行を挿入 (set_captionと同じ形式。
    // 手動編集でないため current=0 で履歴に残す意味は薄いが、形式を統一し
    // unique index (asset_id, kind) WHERE current=1 を守る)
    tx.execute(
        "UPDATE captions SET current = 0, updated_at = ?3 \
         WHERE asset_id = ?1 AND kind = 'embedded_prompt' AND current = 1",
        params![asset_id, "embedded_prompt", now],
    )
    .map_err(map_err)?;
    tx.execute(
        "INSERT INTO captions
             (uuid, asset_id, kind, body, source, current, manually_edited,
              created_at, updated_at)
         VALUES (?1, ?2, 'embedded_prompt', ?3, 'embedded_metadata', 1, 0, ?4, ?4)",
        params![new_uuid_v7(), asset_id, prompt, now],
    )
    .map_err(map_err)?;
    Ok(true)
}

impl AssetMetadataRepo for SqliteRepos<'_> {
    fn save_asset_metadata(
        &self,
        asset_id: i64,
        parsed: &[AssetMetadataParsed],
    ) -> Result<bool, RepoError> {
        if parsed.is_empty() {
            return Ok(false);
        }
        if gento_core::repo::AssetRepo::get_asset(self, asset_id)?.is_none() {
            return Err(RepoError::NotFound);
        }
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let mut changed = false;
        let mut prompt: Option<String> = None;
        for item in parsed {
            if item.is_empty_result() {
                continue;
            }
            let existing_version: Option<String> = tx
                .query_row(
                    "SELECT parser_version FROM asset_metadata \
                     WHERE asset_id = ?1 AND parser_name = ?2",
                    params![asset_id, item.parser_name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_err)?;
            let row_changed = match &existing_version {
                Some(v) if v == &item.parser_version => false, // 冪等スキップ
                _ => {
                    tx.execute(
                        "DELETE FROM asset_metadata WHERE asset_id = ?1 AND parser_name = ?2",
                        params![asset_id, item.parser_name],
                    )
                    .map_err(map_err)?;
                    insert_metadata_row(&tx, asset_id, item)?;
                    true
                }
            };
            changed |= row_changed;
            // positive_promptの優先順位: sd-params > comfy > nai > その他
            // (1ファイルに複数形式が混在する敵対的ケースの決定論化)
            if prompt.is_none() {
                if let Some(p) = item.columns.positive_prompt.as_ref() {
                    if !p.is_empty() {
                        prompt = Some(p.clone());
                    }
                }
            }
        }
        // embedded_promptキャプション + 検索文書 (同一tx)
        if let Some(prompt) = prompt {
            changed |= upsert_embedded_prompt(&tx, asset_id, &prompt)?;
        }
        if changed {
            crate::search::rebuild_docs_for_assets(&tx, &[asset_id])?;
        }
        tx.commit().map_err(map_err)?;
        Ok(changed)
    }

    fn list_asset_metadata(&self, asset_id: i64) -> Result<Vec<AssetMetadataRecord>, RepoError> {
        let sql = format!(
            "SELECT {METADATA_COLUMNS} FROM asset_metadata \
             WHERE asset_id = ?1 ORDER BY parser_name"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([asset_id], map_metadata_row)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    fn assets_needing_reparse(
        &self,
        current: &[(String, String)],
        limit: u64,
    ) -> Result<Vec<ReparseTarget>, RepoError> {
        // 条件: presentファイルを持つ、かつ (行が1つも無い OR 既存行の
        // いずれかが現行より古いversion)。形式ごとに生成されるparser行は
        // 異なる (JPEGはpng-text行を持たない等) ため「全parser行の存在」は
        // 要求しない — parser集合の変更 (新parser追加) は全parserのversion
        // 繰り上げで全assetを再解析対象にする運用 (gento-image CURRENT_PARSERS
        // のdoc参照)。
        let mut conditions = vec![
            "NOT EXISTS (SELECT 1 FROM asset_metadata am WHERE am.asset_id = a.id)".to_string(),
        ];
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        for (name, version) in current {
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM asset_metadata am \
                 WHERE am.asset_id = a.id AND am.parser_name = ?{} AND am.parser_version != ?{})",
                values.len() + 1,
                values.len() + 2
            ));
            values.push(rusqlite::types::Value::Text(name.clone()));
            values.push(rusqlite::types::Value::Text(version.clone()));
        }
        // 現行集合に存在しないparser名の行 (廃止parser) も再解析対象
        let names: Vec<String> = (0..current.len())
            .map(|i| format!("?{}", values.len() + 1 + i))
            .collect();
        conditions.push(format!(
            "EXISTS (SELECT 1 FROM asset_metadata am \
             WHERE am.asset_id = a.id AND am.parser_name NOT IN ({}))",
            names.join(", ")
        ));
        for (name, _) in current {
            values.push(rusqlite::types::Value::Text(name.clone()));
        }
        values.push(rusqlite::types::Value::Integer(limit as i64));
        let sql = format!(
            "SELECT a.id, f.id FROM assets a \
             JOIN files f ON f.asset_id = a.id AND f.scan_state = 'present' \
             WHERE ({}) \
             GROUP BY a.id ORDER BY a.id LIMIT ?{}",
            conditions.join(" OR "),
            values.len()
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let pairs = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<(i64, i64)>>>()
            .map_err(map_err)?;
        // FileRecordを取得 (代表presentファイル。reparseのfd対象)
        let mut out = Vec::with_capacity(pairs.len());
        for (asset_id, file_id) in pairs {
            let sql = format!(
                "SELECT {} FROM files WHERE id = ?1",
                crate::repos::FILE_COLUMNS
            );
            let file = self
                .conn()
                .query_row(&sql, [file_id], map_file)
                .optional()
                .map_err(map_err)?;
            if let Some(file) = file {
                out.push(ReparseTarget {
                    asset_id,
                    root_id: file.root_id,
                    file,
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use gento_core::repo::{AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState};
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-assetmeta-{tag}-{}-{}",
                std::process::id(),
                crate::ids::now_utc_compact()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn with_repos<T>(tag: &str, f: impl FnOnce(&SqliteRepos<'_>) -> T) -> T {
        let tmp = TempDir::new(tag);
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        f(&SqliteRepos::new(&conn))
    }

    fn make_asset(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        let asset = repos
            .create_asset(NewAsset {
                content_hash: hash.to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: Some("image/png".to_string()),
                width: Some(1),
                height: Some(1),
                has_alpha: Some(true),
                is_animated: Some(false),
                frame_count: None,
            })
            .unwrap();
        asset.id
    }

    fn link_present_file(repos: &SqliteRepos<'_>, root_id: i64, rel: &str, asset_id: i64) {
        let file = repos
            .upsert_file(FileUpsert {
                root_id,
                rel_path: std::path::PathBuf::from(rel),
                display_name: rel.rsplit('/').next().unwrap().to_string(),
                byte_size: 10,
                mtime_ms: 1_000,
                mtime_ns: 1_000_000_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        repos.link_file(file.id, asset_id).unwrap();
    }

    fn png_text_row(parser: &str, prompt: Option<&str>) -> AssetMetadataParsed {
        AssetMetadataParsed {
            parser_name: parser.to_string(),
            parser_version: "1".to_string(),
            columns: MetadataColumns {
                positive_prompt: prompt.map(|p| p.to_string()),
                ..MetadataColumns::default()
            },
            raw_png_metadata: Some(r#"{"Title":"x"}"#.to_string()),
            ..AssetMetadataParsed::default()
        }
    }

    #[test]
    fn save_writes_rows_and_embedded_prompt_caption() {
        with_repos("save", |repos| {
            let root = repos
                .add_root(std::path::Path::new("/tmp/gento-wp9-a"))
                .unwrap();
            let asset = make_asset(repos, "blake3:aa");
            link_present_file(repos, root.id, "img.png", asset);

            let saved = repos
                .save_asset_metadata(
                    asset,
                    &[
                        png_text_row("png-text", None),
                        png_text_row("sd-params", Some("1girl, sunset")),
                    ],
                )
                .unwrap();
            assert!(saved);
            let rows = repos.list_asset_metadata(asset).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].parsed.parser_name, "png-text"); // 昇順
            assert_eq!(rows[1].parsed.parser_name, "sd-params");

            // embedded_promptキャプション (source=embedded_metadata)
            let caption: Option<(String, String, i64)> = repos
                .conn()
                .query_row(
                    "SELECT body, source, manually_edited FROM captions \
                     WHERE asset_id = ?1 AND kind = 'embedded_prompt' AND current = 1",
                    [asset],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .unwrap();
            let (body, source, edited) = caption.expect("embedded_prompt行");
            assert_eq!(body, "1girl, sunset");
            assert_eq!(source, "embedded_metadata");
            assert_eq!(edited, 0);

            // FTS文書にpositive promptが入る
            let doc = crate::search::build_asset_doc(repos.conn(), asset)
                .unwrap()
                .unwrap();
            assert!(doc.captions.contains("1girl, sunset"), "{}", doc.captions);

            // 再保存 (同version) は冪等スキップ
            let saved_again = repos
                .save_asset_metadata(
                    asset,
                    &[
                        png_text_row("png-text", None),
                        png_text_row("sd-params", Some("1girl, sunset")),
                    ],
                )
                .unwrap();
            assert!(!saved_again, "同versionは書き換えない");

            // 存在しないassetはNotFound
            assert!(matches!(
                repos.save_asset_metadata(9999, &[png_text_row("x", None)]),
                Err(RepoError::NotFound)
            ));
        });
    }

    #[test]
    fn manual_edit_protects_embedded_prompt() {
        with_repos("manual-protect", |repos| {
            let root = repos
                .add_root(std::path::Path::new("/tmp/gento-wp9-b"))
                .unwrap();
            let asset = make_asset(repos, "blake3:bb");
            link_present_file(repos, root.id, "img.png", asset);
            repos
                .save_asset_metadata(asset, &[png_text_row("sd-params", Some("orig"))])
                .unwrap();
            // 手動編集相当: source=manual・manually_edited=1
            repos
                .conn()
                .execute(
                    "UPDATE captions SET source = 'manual', manually_edited = 1 \
                     WHERE asset_id = ?1 AND kind = 'embedded_prompt'",
                    [asset],
                )
                .unwrap();
            // 再解析 (version繰り上げ) でも手動編集行は上書きされない
            let mut row = png_text_row("sd-params", Some("regenerated"));
            row.parser_version = "2".to_string();
            repos.save_asset_metadata(asset, &[row]).unwrap();
            let body: String = repos
                .conn()
                .query_row(
                    "SELECT body FROM captions \
                     WHERE asset_id = ?1 AND kind = 'embedded_prompt' AND current = 1",
                    [asset],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(body, "orig", "手動編集を保護");
            // 行自体はversion 2へ置換される
            let rows = repos.list_asset_metadata(asset).unwrap();
            assert_eq!(rows[0].parsed.parser_version, "2");
        });
    }

    #[test]
    fn reparse_candidates_by_current_parsers() {
        with_repos("reparse", |repos| {
            let root = repos
                .add_root(std::path::Path::new("/tmp/gento-wp9-c"))
                .unwrap();
            let current: Vec<(String, String)> = vec![
                ("png-text".to_string(), "1".to_string()),
                ("exif".to_string(), "1".to_string()),
            ];
            // 行なしasset → 候補
            let a1 = make_asset(repos, "blake3:c1");
            link_present_file(repos, root.id, "a1.png", a1);
            // 現行versionの行を持つasset (exif行なしでもPNG形式なら問題ない —
            // 形式ごとの行構成差は「欠落」とみなさない) → 非候補
            let a2 = make_asset(repos, "blake3:c2");
            link_present_file(repos, root.id, "a2.png", a2);
            repos
                .save_asset_metadata(a2, &[png_text_row("png-text", None)])
                .unwrap();
            // 古いversion行のみ → 候補
            let a3 = make_asset(repos, "blake3:c3");
            link_present_file(repos, root.id, "a3.png", a3);
            let mut old = png_text_row("png-text", None);
            old.parser_version = "0".to_string();
            repos.save_asset_metadata(a3, &[old]).unwrap();
            // 現行に存在しないparser名の行のみ (parser廃止) → 候補
            let a5 = make_asset(repos, "blake3:c5");
            link_present_file(repos, root.id, "a5.png", a5);
            repos
                .save_asset_metadata(a5, &[png_text_row("invoke-ai", None)])
                .unwrap();
            // presentファイルなし → 非候補 (fd対象が無いため)
            let a4 = make_asset(repos, "blake3:c4");

            let targets = repos.assets_needing_reparse(&current, 100).unwrap();
            let ids: Vec<i64> = targets.iter().map(|t| t.asset_id).collect();
            assert!(ids.contains(&a1), "行なし: {ids:?}");
            assert!(!ids.contains(&a2), "現行version充足: {ids:?}");
            assert!(ids.contains(&a3), "古version: {ids:?}");
            assert!(ids.contains(&a5), "廃止parser行のみ: {ids:?}");
            assert!(!ids.contains(&a4), "presentファイルなし: {ids:?}");
            for t in &targets {
                assert_eq!(t.file.scan_state, ScanState::Present);
            }
            // limit
            let limited = repos.assets_needing_reparse(&current, 1).unwrap();
            assert_eq!(limited.len(), 1);
        });
    }
}
