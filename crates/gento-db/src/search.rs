//! FTS5検索文書 (asset_docs) の構築・整合性・検索実行 (MVP WP-8)。
//!
//! # 設計 (docs/dev/api-contract.md §42・data-model §9)
//!
//! - 検索文書の正本は**Rust駆動**: タグ/キャプション/ファイル名・パスを
//!   書き込むrepo操作と同一トランザクションで対象assetのみ
//!   DELETE+INSERT再構築する (トリガー不使用)。
//! - 文書構成: ファイル名 (全File。重複排除) / パス (同) / タグ (正規化名) /
//!   キャプション (description + sd_tags + **embedded_prompt (positive
//!   prompt。WP-9接続)** ) / user_note。negative prompt・workflow全体は
//!   含めない (data-model §9)。
//! - 検索実行はAST ([`gento_search::SearchQuery`]) から**固定SQL断片 +
//!   パラメータバインドのみ**を構成する (ユーザー入力のSQL連結禁止 —
//!   WP-6 smart_whereと同じ規約)。
//! - trigramトークナイザは3文字未満を索引化しないため、1〜2文字を含む
//!   語句は検索文書テーブルへのLIKEフォールバックで一致させる。

use gento_core::repo::RepoError;
use gento_search::{
    escape_like, CmpOp, FieldCondition, FieldKey, FieldValue, SearchQuery, TextPlan,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection};

use crate::repos::map_err;

/// 一括再構築のコミット単位 (fts_rebuild・診断修復用)。
const REBUILD_BATCH: usize = 500;

/// IN句1チャンクの上限 (証拠収集のasset_id批量)。
const EVIDENCE_CHUNK: usize = 500;

/// 1 asset分の検索文書。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetDoc {
    pub asset_id: i64,
    pub filename: String,
    pub path: String,
    pub tags: String,
    pub captions: String,
    pub note: String,
}

/// 検索文書のフィールド (一致根拠表示用。FTS列順と一致)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocField {
    Filename,
    Path,
    Tags,
    Captions,
    Note,
}

impl DocField {
    /// FTS5テーブルの列インデックス (asset_id=0を除く本文列)。
    pub const fn column_index(self) -> usize {
        match self {
            DocField::Filename => 1,
            DocField::Path => 2,
            DocField::Tags => 3,
            DocField::Captions => 4,
            DocField::Note => 5,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            DocField::Filename => "filename",
            DocField::Path => "path",
            DocField::Tags => "tags",
            DocField::Captions => "captions",
            DocField::Note => "note",
        }
    }
}

/// 一致根拠 (フィールド + 前後文snippet)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEvidence {
    pub field: DocField,
    pub snippet: String,
}

/// 検索スコープ (WP-8: library全体 / 現在フォルダ=ルート / コレクション)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchScope {
    pub root_id: Option<i64>,
    pub collection_id: Option<i64>,
    /// true (既定) = presentなFileを持つassetに限定する (グリッド表示用途。
    /// 表示できないassetは検索結果に出さない)。
    /// false = 全assetが対象 (WP-6互換のスマートコレクション評価)。
    pub require_present_files: bool,
}

impl SearchScope {
    /// グリッド表示用途の既定スコープ (library全体・present限定)。
    pub fn for_display() -> Self {
        Self {
            root_id: None,
            collection_id: None,
            require_present_files: true,
        }
    }

    /// スマートコレクション評価用 (present限定なし)。
    pub fn for_smart_collection() -> Self {
        Self {
            root_id: None,
            collection_id: None,
            require_present_files: false,
        }
    }
}

// ===== 文書構築 =====

/// 1 assetの検索文書をDBから組み立てる。presentなFileを持たないasset
/// (または存在しないasset) はNone (文書なし = 検索対象外)。
pub fn build_asset_doc(conn: &Connection, asset_id: i64) -> Result<Option<AssetDoc>, RepoError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM assets WHERE id = ?1)",
            [asset_id],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    if !exists {
        return Ok(None);
    }
    // ファイル名・パス (presentのみ。重複排除・安定順)
    let mut names: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT display_name, rel_path, path_encoding FROM files \
                 WHERE asset_id = ?1 AND scan_state = 'present' ORDER BY id",
            )
            .map_err(map_err)?;
        let mut rows = stmt.query([asset_id]).map_err(map_err)?;
        while let Some(row) = rows.next().map_err(map_err)? {
            let name: String = row.get(0).map_err(map_err)?;
            let rel_bytes: Vec<u8> = row.get(1).map_err(map_err)?;
            if !names.contains(&name) {
                names.push(name);
            }
            let display = crate::paths::decode_path(&rel_bytes)
                .to_string_lossy()
                .into_owned();
            if !paths.contains(&display) {
                paths.push(display);
            }
        }
    }
    if names.is_empty() {
        // presentなFileが無いassetは索引しない (表示もできない)
        return Ok(None);
    }
    // タグ (正規化名・安定順)
    let mut tags: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT t.normalized_name FROM asset_tags at \
                 JOIN tags t ON t.id = at.tag_id \
                 WHERE at.asset_id = ?1 ORDER BY t.normalized_name",
            )
            .map_err(map_err)?;
        let mut rows = stmt.query([asset_id]).map_err(map_err)?;
        while let Some(row) = rows.next().map_err(map_err)? {
            tags.push(row.get(0).map_err(map_err)?);
        }
    }
    // キャプション: description + sd_tags + embedded_prompt (現行のみ)。
    // WP-9でembedded_prompt (positive prompt) を接続 — 検索対象はpositiveのみ。
    // embedded_negative_prompt・workflow全体は意味文書に含めない
    // (data-model §9。明示的全文検索は将来課題)。
    let mut captions: Vec<String> = Vec::new();
    let mut note = String::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT kind, body FROM captions \
                 WHERE asset_id = ?1 AND current = 1 \
                   AND kind IN ('description', 'sd_tags', 'user_note', 'embedded_prompt') \
                 ORDER BY kind",
            )
            .map_err(map_err)?;
        let mut rows = stmt.query([asset_id]).map_err(map_err)?;
        while let Some(row) = rows.next().map_err(map_err)? {
            let kind: String = row.get(0).map_err(map_err)?;
            let body: String = row.get(1).map_err(map_err)?;
            if body.is_empty() {
                continue;
            }
            match kind.as_str() {
                "user_note" => note = body,
                _ => captions.push(body),
            }
        }
    }
    Ok(Some(AssetDoc {
        asset_id,
        filename: names.join("\n"),
        path: paths.join("\n"),
        tags: tags.join(" "),
        captions: captions.join("\n"),
        note,
    }))
}

/// 対象assetの文書を再構築する (DELETE+INSERT。同一tx呼び出し前提。
/// 呼び出し側txの一部として実行されるため、ここではtxを張らない)。
/// presentなFileを持たないassetの文書は削除される。戻り値は文書を
/// 書き込んだ (または削除した) asset数。
pub fn rebuild_docs_for_assets(conn: &Connection, asset_ids: &[i64]) -> Result<usize, RepoError> {
    let mut changed = 0usize;
    // 同一assetの重複を除去 (順序保持)
    let mut seen = std::collections::HashSet::new();
    for asset_id in asset_ids.iter().copied() {
        if !seen.insert(asset_id) {
            continue;
        }
        // rowid = asset_id の1:1対応で管理する (asset_idはFTS列ではないため
        // WHERE asset_id は全文走査になる。rowid経由なら索引削除がO(log n))
        conn.execute("DELETE FROM asset_docs WHERE rowid = ?1", [asset_id])
            .map_err(map_err)?;
        match build_asset_doc(conn, asset_id)? {
            Some(doc) => {
                conn.execute(
                    "INSERT INTO asset_docs (rowid, asset_id, filename, path, tags, captions, note) \
                     VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        doc.asset_id,
                        doc.filename,
                        doc.path,
                        doc.tags,
                        doc.captions,
                        doc.note
                    ],
                )
                .map_err(map_err)?;
            }
            None => {
                // 文書なし (presentファイル不在・asset不在)。削除済み。
            }
        }
        changed += 1;
    }
    Ok(changed)
}

/// assets本体に存在しないasset_idの文書を削除する (修復用)。
pub fn prune_orphan_docs(conn: &Connection) -> Result<u64, RepoError> {
    let changed = conn
        .execute(
            "DELETE FROM asset_docs WHERE asset_id NOT IN (SELECT id FROM assets)",
            [],
        )
        .map_err(map_err)?;
    Ok(changed as u64)
}

/// 軽量な件数照合 (expected, actual)。走査後の保守フック用で、
/// 全文書再構築による内容比較は行わない (fts_integrityを使う)。
pub fn fts_counts(conn: &Connection) -> Result<(u64, u64), RepoError> {
    let expected: i64 = conn
        .query_row(
            "SELECT count(DISTINCT asset_id) FROM files \
             WHERE scan_state = 'present' AND asset_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let actual: i64 = conn
        .query_row("SELECT count(*) FROM asset_docs", [], |r| r.get(0))
        .map_err(map_err)?;
    Ok((expected.max(0) as u64, actual.max(0) as u64))
}

/// FTS索引の整合性レポート。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct FtsIntegrityReport {
    /// presentファイルを持つassets数 (文書があるべき数)。
    pub expected_docs: u64,
    /// 実際のasset_docs行数。
    pub actual_docs: u64,
    /// 文書が欠けているasset数。
    pub missing_docs: u64,
    /// 正本のassetが存在しない文書の数。
    pub orphan_docs: u64,
    /// 同一asset_idの文書が複数行ある数 (FTSテーブルはUNIQUE制約がない)。
    pub duplicate_docs: u64,
    /// 文書内容が現DB状態と一致しないasset数。
    pub stale_contents: u64,
    /// 欠落・孤児・内容不一致がすべて0。
    pub consistent: bool,
}

/// 整合性解析の詳細 (修復の対象id付き)。
struct IntegrityDetail {
    report: FtsIntegrityReport,
    missing: Vec<i64>,
    stale: Vec<i64>,
    orphans: Vec<i64>,
    duplicates: Vec<i64>,
}

/// asset_docs と assets正本の整合性を検査する。
/// 全assetの文書を再構築して内容比較する (5万件で数秒以内の想定。
/// 診断・修復はオンデマンド実行)。
pub fn fts_integrity(conn: &Connection) -> Result<FtsIntegrityReport, RepoError> {
    analyze_integrity(conn).map(|detail| detail.report)
}

fn analyze_integrity(conn: &Connection) -> Result<IntegrityDetail, RepoError> {
    // 対象: presentなFileを持つasset (文書があるべき集合)
    let expected_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT asset_id FROM files \
                 WHERE scan_state = 'present' AND asset_id IS NOT NULL ORDER BY asset_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(map_err)?;
        rows
    };
    let doc_rows: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT asset_id FROM asset_docs ORDER BY asset_id")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(map_err)?;
        rows
    };
    let expected_set: std::collections::HashSet<i64> = expected_ids.iter().copied().collect();
    let doc_set: std::collections::HashSet<i64> = doc_rows.iter().copied().collect();
    let missing_ids: Vec<i64> = expected_set.difference(&doc_set).copied().collect();
    let orphan_ids: Vec<i64> = doc_set.difference(&expected_set).copied().collect();
    // 重複: 期待集合∩文書集合のうち複数行を持つasset
    let mut row_counts: std::collections::HashMap<i64, u32> = std::collections::HashMap::new();
    for id in &doc_rows {
        *row_counts.entry(*id).or_default() += 1;
    }
    let duplicate_ids: Vec<i64> = row_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(id, _)| *id)
        .collect();

    // 内容比較 (共通集合のみ。欠落は上で計上済み)
    let mut stale_ids: Vec<i64> = Vec::new();
    for asset_id in expected_set.intersection(&doc_set) {
        let stored = read_doc(conn, *asset_id)?;
        let rebuilt = build_asset_doc(conn, *asset_id)?;
        let matches = match (&stored, &rebuilt) {
            (Some(stored), Some(rebuilt)) => {
                stored.filename == rebuilt.filename
                    && stored.path == rebuilt.path
                    && stored.tags == rebuilt.tags
                    && stored.captions == rebuilt.captions
                    && stored.note == rebuilt.note
            }
            _ => false,
        };
        if !matches {
            stale_ids.push(*asset_id);
        }
    }
    let report = FtsIntegrityReport {
        expected_docs: expected_ids.len() as u64,
        actual_docs: doc_rows.len() as u64,
        missing_docs: missing_ids.len() as u64,
        orphan_docs: orphan_ids.len() as u64,
        duplicate_docs: duplicate_ids.len() as u64,
        stale_contents: stale_ids.len() as u64,
        consistent: missing_ids.is_empty()
            && orphan_ids.is_empty()
            && duplicate_ids.is_empty()
            && stale_ids.is_empty(),
    };
    Ok(IntegrityDetail {
        report,
        missing: missing_ids,
        stale: stale_ids,
        orphans: orphan_ids,
        duplicates: duplicate_ids,
    })
}

/// 不整合のみを修復する (fts_rebuildより軽い対象限定修復)。
/// 戻り値は (再構築したasset数, 削除した孤立/重複文書数)。
pub fn fts_repair(conn: &Connection) -> Result<(u64, u64), RepoError> {
    let detail = analyze_integrity(conn)?;
    let mut removed = 0u64;
    for orphan in &detail.orphans {
        conn.execute("DELETE FROM asset_docs WHERE rowid = ?1", [orphan])
            .map_err(map_err)?;
        removed += 1;
    }
    for duplicate in &detail.duplicates {
        conn.execute("DELETE FROM asset_docs WHERE rowid = ?1", [duplicate])
            .map_err(map_err)?;
        removed += 1;
    }
    let mut rebuild_ids = detail.missing.clone();
    rebuild_ids.extend(detail.stale.iter().copied());
    let rebuilt = rebuild_docs_for_assets(conn, &rebuild_ids)? as u64;
    Ok((rebuilt, removed))
}

/// 保存済み文書行の読み出し。
fn read_doc(conn: &Connection, asset_id: i64) -> Result<Option<AssetDoc>, RepoError> {
    match conn.query_row(
        "SELECT asset_id, filename, path, tags, captions, note FROM asset_docs \
         WHERE rowid = ?1",
        [asset_id],
        |row| {
            Ok(AssetDoc {
                asset_id: row.get(0)?,
                filename: row.get(1)?,
                path: row.get(2)?,
                tags: row.get(3)?,
                captions: row.get(4)?,
                note: row.get(5)?,
            })
        },
    ) {
        Ok(doc) => Ok(Some(doc)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(other) => Err(map_err(other)),
    }
}

/// 全文書を削除して再構築する (fts_rebuildコマンドの本体)。
/// 進捗は `progress(done, total)` へ通知。戻り値は再構築した文書数。
pub fn fts_rebuild(
    conn: &Connection,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, RepoError> {
    conn.execute("DELETE FROM asset_docs", [])
        .map_err(map_err)?;
    let expected_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT asset_id FROM files \
                 WHERE scan_state = 'present' AND asset_id IS NOT NULL ORDER BY asset_id",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(map_err)?;
        rows
    };
    let total = expected_ids.len() as u64;
    let mut written = 0u64;
    for chunk in expected_ids.chunks(REBUILD_BATCH) {
        // 呼び出し側 (writer) のtx境界を尊重しつつ、バッチごとの巨大
        // トランザクションを避けるため逐次commitする
        conn.execute_batch("BEGIN")
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let result = rebuild_docs_for_assets(conn, chunk);
        match result {
            Ok(count) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| RepoError::Db(e.to_string()))?;
                written += count as u64;
                progress(written, total);
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
    Ok(written)
}

// ===== 検索実行 =====

/// AST → SQL WHERE断片 + バインド値 (固定断片のみ。連結禁止規約)。
struct QuerySql {
    where_clause: String,
    values: Vec<SqlValue>,
}

/// フィールド条件 → SQL断片。`values`へバインド値を追記する。
fn field_fragment(condition: &FieldCondition, values: &mut Vec<SqlValue>) -> String {
    let FieldCondition { key, op, value } = condition;
    match key {
        FieldKey::Rating => {
            let FieldValue::Number(number) = value else {
                return "0".to_string(); // parserが保証 (防御)
            };
            values.push(SqlValue::Integer(*number));
            let symbol = match op {
                CmpOp::Eq => "=",
                CmpOp::Ne => "<>",
                CmpOp::Ge => ">=",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Lt => "<",
            };
            format!("a.rating {symbol} ?")
        }
        FieldKey::Cull => {
            let FieldValue::Text(state) = value else {
                return "0".to_string();
            };
            values.push(SqlValue::Text(state.clone()));
            let symbol = match op {
                CmpOp::Eq => "=",
                _ => "<>",
            };
            format!("a.triage {symbol} ?")
        }
        FieldKey::Fav => {
            let FieldValue::Bool(flag) = value else {
                return "0".to_string();
            };
            values.push(SqlValue::Integer(i64::from(*flag)));
            let symbol = match op {
                CmpOp::Eq => "=",
                _ => "<>",
            };
            format!("a.favorite {symbol} ?")
        }
        FieldKey::Type => {
            let FieldValue::Text(input) = value else {
                return "0".to_string();
            };
            let symbol = match op {
                CmpOp::Eq => "=",
                _ => "<>",
            };
            if input.contains('/') {
                // 完全なMIMEでの指定
                values.push(SqlValue::Text(input.to_ascii_lowercase()));
                format!("a.mime_type {symbol} ?")
            } else {
                // 拡張子指定 (png等): MIME一致 or present Fileの拡張子一致
                let ext = input.to_ascii_lowercase();
                values.push(SqlValue::Text(mime_of_ext(&ext)));
                values.push(SqlValue::Text(ext));
                format!(
                    "(a.mime_type {symbol} ? OR EXISTS (\
                      SELECT 1 FROM files tf \
                      WHERE tf.asset_id = a.id AND tf.scan_state = 'present' \
                        AND tf.extension {symbol} ?))"
                )
            }
        }
        FieldKey::Path | FieldKey::Name | FieldKey::Note => {
            let FieldValue::Text(text) = value else {
                return "0".to_string();
            };
            let column = match key {
                FieldKey::Path => "path",
                FieldKey::Name => "filename",
                _ => "note",
            };
            let pattern = format!("%{}%", escape_like(text));
            values.push(SqlValue::Text(pattern));
            // 検索文書の該当列に対する部分一致 (BLOBパスでなく文書の
            // 表示文字列に対して一致させる)。asset_idはFTSのUNINDEXED列で
            // 索引がないため、相関EXISTSではなく一度に絞り込むIN句にする
            // (50k行×全走査になるのを避ける)
            format!("a.id IN (SELECT asset_id FROM asset_docs WHERE {column} LIKE ? ESCAPE '\\')")
        }
        FieldKey::Tag => {
            let FieldValue::Text(name) = value else {
                return "0".to_string();
            };
            let normalized = gento_core::metadata::normalize_tag_name(name);
            values.push(SqlValue::Text(normalized));
            match op {
                CmpOp::Eq => {
                    "EXISTS (SELECT 1 FROM asset_tags at2 JOIN tags t2 ON t2.id = at2.tag_id \
                     WHERE at2.asset_id = a.id AND t2.normalized_name = ?)"
                        .to_string()
                }
                _ => "NOT EXISTS (SELECT 1 FROM asset_tags at2 JOIN tags t2 ON t2.id = at2.tag_id \
                     WHERE at2.asset_id = a.id AND t2.normalized_name = ?)"
                    .to_string(),
            }
        }
        FieldKey::Added => {
            let FieldValue::DateRange { from, to } = value else {
                return "0".to_string();
            };
            let mut fragments = Vec::new();
            if let Some(from) = from {
                values.push(SqlValue::Text(from.lower_bound_rfc3339()));
                fragments.push("a.created_at >= ?".to_string());
            }
            if let Some(to) = to {
                values.push(SqlValue::Text(to.next_day_lower_bound_rfc3339()));
                fragments.push("a.created_at < ?".to_string());
            }
            if fragments.is_empty() {
                return "1".to_string();
            }
            fragments.join(" AND ")
        }
        // WP-9: 埋め込み画像メタデータ (asset_metadata)。行はasset複数
        // (parserごと) のためEXISTSで判定する。
        FieldKey::Model | FieldKey::Sampler | FieldKey::Vae => {
            let FieldValue::Text(text) = value else {
                return "0".to_string();
            };
            let column = match key {
                FieldKey::Model => "generation_model",
                FieldKey::Sampler => "sampler",
                _ => "vae",
            };
            let pattern = format!("%{}%", escape_like(text));
            values.push(SqlValue::Text(pattern));
            // Ne = 「どのparser行にも一致しない」(NOT EXISTS)
            let exists = if matches!(op, CmpOp::Ne) {
                "NOT EXISTS"
            } else {
                "EXISTS"
            };
            format!(
                "{exists} (SELECT 1 FROM asset_metadata am WHERE am.asset_id = a.id \
                  AND am.{column} LIKE ? ESCAPE '\\')"
            )
        }
        FieldKey::Steps => {
            let FieldValue::Number(number) = value else {
                return "0".to_string();
            };
            values.push(SqlValue::Integer(*number));
            let symbol = match op {
                CmpOp::Eq => "=",
                CmpOp::Ne => "<>",
                CmpOp::Ge => ">=",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Lt => "<",
            };
            format!(
                "EXISTS (SELECT 1 FROM asset_metadata am \
                  WHERE am.asset_id = a.id AND am.steps IS NOT NULL AND am.steps {symbol} ?)"
            )
        }
        FieldKey::Has => {
            let FieldValue::Text(what) = value else {
                return "0".to_string();
            };
            if what != "workflow" {
                return "0".to_string(); // parserが保証 (防御)
            }
            let exists = if matches!(op, CmpOp::Ne) {
                "NOT EXISTS"
            } else {
                "EXISTS"
            };
            format!(
                "{exists} (SELECT 1 FROM asset_metadata am WHERE am.asset_id = a.id \
                  AND am.raw_workflow IS NOT NULL)"
            )
        }
        FieldKey::Lora => {
            let FieldValue::Text(text) = value else {
                return "0".to_string();
            };
            // generation_extra JSONの loras 配列 (SD: {name,...} / Comfy: 同形式)
            // への部分一致。JSON1 (bundled SQLite) のjson_eachで走査。
            let pattern = format!("%{}%", escape_like(text));
            values.push(SqlValue::Text(pattern));
            "EXISTS (SELECT 1 FROM asset_metadata am, \
                  json_each(json_extract(am.generation_extra, '$.loras')) je \
                  WHERE am.asset_id = a.id AND je.value LIKE ? ESCAPE '\\')"
                .to_string()
        }
    }
}

/// 既知の拡張子 → MIME。未知はそのまま (一致しないだけ)。
fn mime_of_ext(ext: &str) -> String {
    let mime = match ext {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        other => other,
    };
    mime.to_string()
}

/// AST全体 → WHERE句 (先頭に " WHERE " 付き。空条件は空文字)。
fn compile_query(query: &SearchQuery, scope: &SearchScope) -> QuerySql {
    let mut fragments: Vec<String> = Vec::new();
    let mut values: Vec<SqlValue> = Vec::new();

    // 表示可能対象 = presentなFileを持つasset (ライブラリ一覧と同一集合)。
    // スマートコレクション評価 (WP-6互換) ではこの制約を外す。
    if scope.require_present_files {
        fragments.push(
            "EXISTS (SELECT 1 FROM files f WHERE f.asset_id = a.id AND f.scan_state = 'present')"
                .to_string(),
        );
    }

    // フリーテキスト → FTS MATCH / LIKEフォールバック
    let text_terms = query.text_terms();
    let plan = TextPlan::compile(&text_terms);
    if let Some(match_expr) = &plan.match_expr {
        values.push(SqlValue::Text(match_expr.clone()));
        fragments
            .push("a.id IN (SELECT asset_id FROM asset_docs WHERE asset_docs MATCH ?)".to_string());
    }
    if !plan.like_terms.is_empty() {
        // LIKEフォールバック: 全列ORのサブクエリでasset集合を先に絞る
        // (非相関 — asset_docsのasset_id列は索引を持たないため)
        let mut patterns: Vec<String> = Vec::new();
        for term in &plan.like_terms {
            patterns.push(format!("%{}%", escape_like(term)));
        }
        let conjunction = patterns
            .iter()
            .map(|pattern| {
                for _ in 0..5 {
                    values.push(SqlValue::Text(pattern.clone()));
                }
                "(filename LIKE ? ESCAPE '\\' OR path LIKE ? ESCAPE '\\' \
                   OR tags LIKE ? ESCAPE '\\' OR captions LIKE ? ESCAPE '\\' \
                   OR note LIKE ? ESCAPE '\\')"
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        fragments.push(format!(
            "a.id IN (SELECT asset_id FROM asset_docs WHERE {conjunction})"
        ));
    }

    for condition in query.field_conditions() {
        fragments.push(field_fragment(condition, &mut values));
    }

    // スコープ
    if let Some(root_id) = scope.root_id {
        values.push(SqlValue::Integer(root_id));
        fragments.push(
            "EXISTS (SELECT 1 FROM files fr WHERE fr.asset_id = a.id \
             AND fr.root_id = ? AND fr.scan_state = 'present')"
                .to_string(),
        );
    }
    if let Some(collection_id) = scope.collection_id {
        values.push(SqlValue::Integer(collection_id));
        fragments.push(
            "a.id IN (SELECT asset_id FROM collection_items WHERE collection_id = ?)".to_string(),
        );
    }

    if fragments.is_empty() {
        QuerySql {
            where_clause: String::new(),
            values,
        }
    } else {
        QuerySql {
            where_clause: format!(" WHERE {}", fragments.join(" AND ")),
            values,
        }
    }
}

/// AST検索を実行してasset id一覧 (id昇順・ページング) を返す。
pub fn search_asset_ids(
    conn: &Connection,
    query: &SearchQuery,
    scope: &SearchScope,
    offset: u32,
    limit: u32,
) -> Result<(Vec<i64>, u64), RepoError> {
    let sql_parts = compile_query(query, scope);
    // ページ取得と全件数を1文で得る (window関数。重いFTS/LIKEサブクエリの
    // 二重実行を避ける — 5万件計測で約半分に短縮)。
    let mut page_values = sql_parts.values.clone();
    page_values.push(SqlValue::Integer(limit as i64));
    page_values.push(SqlValue::Integer(offset as i64));
    let mut stmt = conn
        .prepare(&format!(
            "SELECT a.id, count(*) OVER () FROM assets a{} ORDER BY a.id ASC LIMIT ? OFFSET ?",
            sql_parts.where_clause
        ))
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params_from_iter(page_values.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(map_err)?
        .collect::<rusqlite::Result<Vec<(i64, i64)>>>()
        .map_err(map_err)?;
    let total = match (rows.last(), offset) {
        // 窓関数のtotalは行が無いと取れないため、offset>0で空ページの
        // 場合はcountへフォールバックする (ページ末尾超過の判定用)。
        // 失敗は伝播する (unwrap_or(0)だと実在する一致があるのに
        // total=0の虚偽表示になる — QA-WP8)。
        (None, 0) => 0,
        (None, _) => conn
            .query_row(
                &format!("SELECT count(*) FROM assets a{}", sql_parts.where_clause),
                params_from_iter(sql_parts.values.iter()),
                |r| r.get(0),
            )
            .map_err(map_err)?,
        (Some((_, total)), _) => *total,
    };
    Ok((
        rows.into_iter().map(|(id, _)| id).collect(),
        total.max(0) as u64,
    ))
}

/// 検索結果の一致根拠を組み立てる (表示中ページ分)。
///
/// - MATCH語: 各FTS列に列フィルタMATCH (`filename:"語"`) を発行し
///   `snippet()` で前後文を得る (日本語の文字境界で途切れない)。
/// - LIKE語: 文書列を直接読み、Rust側で位置特定 + 文字境界safeな切断。
pub fn search_evidence(
    conn: &Connection,
    asset_ids: &[i64],
    query: &SearchQuery,
) -> Result<Vec<(i64, Vec<FieldEvidence>)>, RepoError> {
    let mut out: Vec<(i64, Vec<FieldEvidence>)> = Vec::new();
    if asset_ids.is_empty() {
        return Ok(out);
    }
    let text_terms = query.text_terms();
    let plan = TextPlan::compile(&text_terms);
    if plan.is_empty() {
        return Ok(out);
    }
    let mut by_asset: std::collections::HashMap<i64, Vec<FieldEvidence>> =
        std::collections::HashMap::new();
    // 列フィルタMATCH式 (語ごとに `col:"語"` をAND結続 — FTS5の
    // `col: a b` 形は先頭語にのみフィルタが掛かるため)
    let column_match_expr = |field: DocField| -> String {
        // OR結合: 検索全体は全語ANDだが、根拠は「この列にどの語が
        // 一致したか」を列ごとに報告する
        plan.match_terms
            .iter()
            .map(|term| format!("{}:{}", field.as_str(), term))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    for chunk in asset_ids.chunks(EVIDENCE_CHUNK) {
        // MATCH語: 列別にsnippet取得
        if !plan.match_terms.is_empty() {
            for field in [
                DocField::Filename,
                DocField::Path,
                DocField::Tags,
                DocField::Captions,
                DocField::Note,
            ] {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT asset_id, snippet(asset_docs, {index}, '[', ']', '…', 12) \
                     FROM asset_docs WHERE asset_docs = '{colmatch}' \
                     AND asset_id IN ({placeholders})",
                    index = field.column_index(),
                    colmatch = column_match_expr(field).replace('\'', "''"),
                    placeholders = placeholders
                );
                let mut stmt = conn.prepare(&sql).map_err(map_err)?;
                let rows = stmt
                    .query_map(params_from_iter(chunk.iter()), |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(map_err)?
                    .collect::<rusqlite::Result<Vec<(i64, String)>>>()
                    .map_err(map_err)?;
                for (asset_id, snippet) in rows {
                    by_asset
                        .entry(asset_id)
                        .or_default()
                        .push(FieldEvidence { field, snippet });
                }
            }
        }
        // LIKE語: 文書列を読んでRust側でsnip
        if !plan.like_terms.is_empty() {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT asset_id, filename, path, tags, captions, note FROM asset_docs \
                 WHERE asset_id IN ({placeholders})",
                placeholders = placeholders
            );
            let mut stmt = conn.prepare(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<(i64, String, String, String, String, String)>>>()
                .map_err(map_err)?;
            for (asset_id, filename, path, tags, captions, note) in rows {
                for term in &plan.like_terms {
                    for (field, text) in [
                        (DocField::Filename, &filename),
                        (DocField::Path, &path),
                        (DocField::Tags, &tags),
                        (DocField::Captions, &captions),
                        (DocField::Note, &note),
                    ] {
                        if let Some(snippet) = make_snippet(text, term) {
                            by_asset
                                .entry(asset_id)
                                .or_default()
                                .push(FieldEvidence { field, snippet });
                        }
                    }
                }
            }
        }
    }
    for asset_id in asset_ids {
        if let Some(evidence) = by_asset.remove(asset_id) {
            out.push((*asset_id, evidence));
        }
    }
    Ok(out)
}

/// LIKE一致箇所の前後文 (半角12文字幅。文字境界safe)。非一致はNone。
/// ASCIIは大文字小文字を区別しない (SQLite LIKEの既定に合わせる)。
fn make_snippet(haystack: &str, needle: &str) -> Option<String> {
    let lower_hay = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let at = lower_hay.find(&lower_needle)?;
    let radius = 12usize;
    let chars: Vec<char> = haystack.chars().collect();
    // バイト位置 → 文字位置
    let char_at = |byte: usize| haystack[..byte].chars().count();
    let start_char = char_at(at).saturating_sub(radius);
    let end_char = (char_at(at) + lower_needle.chars().count() + radius).min(chars.len());
    let mut snippet: String = chars[start_char..end_char].iter().collect();
    if start_char > 0 {
        snippet.insert(0, '…');
    }
    if end_char < chars.len() {
        snippet.push('…');
    }
    Some(snippet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use crate::SqliteRepos;
    use gento_core::collections::CollectionRepo;
    use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo};
    use gento_core::repo::{
        AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, ScanState, TriageState,
    };
    use gento_search::parse;
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-search-{tag}-{}-{}",
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

    fn with_repos<T>(tag: &str, f: impl FnOnce(&SqliteRepos<'_>) -> T) -> T {
        let tmp = TempDir::new(tag);
        let mut conn = Connection::open(tmp.path().join("db.sqlite3")).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        run_migrations(&mut conn, MIGRATIONS, &MigrationOpts::default()).unwrap();
        let repos = SqliteRepos::new(&conn);
        f(&repos)
    }

    fn make_asset(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        repos
            .create_asset(NewAsset {
                content_hash: format!("blake3:{hash}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: Some("image/png".to_string()),
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap()
            .id
    }

    fn link_file(repos: &SqliteRepos<'_>, root_id: i64, rel: &str, asset_id: i64) {
        let file = repos
            .upsert_file(FileUpsert {
                root_id,
                rel_path: PathBuf::from(rel),
                display_name: rel.rsplit('/').next().unwrap().to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        repos.link_file(file.id, asset_id).unwrap();
    }

    fn search_all(repos: &SqliteRepos<'_>, query_text: &str) -> Vec<i64> {
        let query = parse(query_text).unwrap().query;
        search_asset_ids(repos.conn(), &query, &SearchScope::default(), 0, 1000)
            .unwrap()
            .0
    }

    // ===== WP-9: 埋め込み画像メタデータ (asset_metadata) の検索 =====

    /// テスト用の生成系メタデータ行ビルダ。
    #[derive(Default)]
    struct SdRow<'a> {
        model: Option<&'a str>,
        steps: Option<i64>,
        sampler: Option<&'a str>,
        vae: Option<&'a str>,
        loras_extra: Option<&'a str>,
        prompt: Option<&'a str>,
        negative: Option<&'a str>,
        workflow: Option<&'a str>,
    }

    impl SdRow<'_> {
        fn build(&self) -> gento_core::assetmeta::AssetMetadataParsed {
            use gento_core::assetmeta::{AssetMetadataParsed, MetadataColumns};
            AssetMetadataParsed {
                parser_name: if self.workflow.is_some() {
                    "comfy".to_string()
                } else {
                    "sd-params".to_string()
                },
                parser_version: "1".to_string(),
                columns: MetadataColumns {
                    generation_model: self.model.map(str::to_string),
                    steps: self.steps,
                    sampler: self.sampler.map(str::to_string),
                    vae: self.vae.map(str::to_string),
                    generation_extra: self.loras_extra.map(str::to_string),
                    positive_prompt: self.prompt.map(str::to_string),
                    negative_prompt: self.negative.map(str::to_string),
                    ..MetadataColumns::default()
                },
                raw_workflow: self.workflow.map(str::to_string),
                ..AssetMetadataParsed::default()
            }
        }
    }

    #[test]
    fn wp9_embedded_prompt_search_and_negative_exclusion() {
        with_repos("wp9-prompt", |repos| {
            let root = repos.add_root(Path::new("/data/wp9p")).unwrap();
            let sd = make_asset(repos, "sd1");
            link_file(repos, root.id, "sd.png", sd);
            // positiveにだけ入る語 (silver) とnegativeにだけ入る語 (watermark)
            use gento_core::assetmeta::AssetMetadataRepo as _;
            repos
                .save_asset_metadata(
                    sd,
                    &[SdRow {
                        model: Some("animagine xl"),
                        steps: Some(28),
                        sampler: Some("Euler a"),
                        prompt: Some("1girl, silver hair, masterpiece"),
                        negative: Some("watermark, lowres"),
                        ..SdRow::default()
                    }
                    .build()],
                )
                .unwrap();

            // positive promptの語でヒット (FTS・LIKE両経路)
            assert_eq!(search_all(repos, "silver hair"), vec![sd]);
            assert_eq!(
                search_all(repos, "ma"),
                vec![sd],
                "1-2文字はLIKEフォールバック"
            );
            // negative promptの語ではヒットしない (data-model §9)
            assert!(search_all(repos, "watermark").is_empty());
            assert!(search_all(repos, "lowres").is_empty());
        });
    }

    #[test]
    fn wp9_metadata_field_keys() {
        with_repos("wp9-keys", |repos| {
            use gento_core::assetmeta::AssetMetadataRepo as _;
            let root = repos.add_root(Path::new("/data/wp9k")).unwrap();
            let sd = make_asset(repos, "k1");
            link_file(repos, root.id, "sd.png", sd);
            repos
                .save_asset_metadata(
                    sd,
                    &[SdRow {
                        model: Some("animagine xl 3.1"),
                        steps: Some(28),
                        sampler: Some("Euler a"),
                        vae: Some("sdxl_vae.safetensors"),
                        loras_extra: Some(r#"{"loras":[{"name":"add-detail-xl"}]}"#),
                        prompt: Some("p"),
                        ..SdRow::default()
                    }
                    .build()],
                )
                .unwrap();
            let comfy = make_asset(repos, "k2");
            link_file(repos, root.id, "comfy.png", comfy);
            repos
                .save_asset_metadata(
                    comfy,
                    &[SdRow {
                        model: Some("flux1-dev"),
                        steps: Some(30),
                        sampler: Some("euler"),
                        prompt: Some("p"),
                        workflow: Some(r#"{"nodes":[]}"#),
                        ..SdRow::default()
                    }
                    .build()],
                )
                .unwrap();
            let plain = make_asset(repos, "k3");
            link_file(repos, root.id, "plain.png", plain);

            // model: 部分一致
            assert_eq!(search_all(repos, "model:animagine"), vec![sd]);
            assert_eq!(search_all(repos, "model:\"flux1-dev\""), vec![comfy]);
            assert!(search_all(repos, "model:nonexistent").is_empty());
            // Ne (構文上は `key:!=` が無いためAST直接構築 — GUIフィルタ経路)
            {
                use gento_search::{FieldValue, SearchQuery, SearchTerm};
                let query = SearchQuery {
                    terms: vec![SearchTerm::Field(gento_search::FieldCondition {
                        key: gento_search::FieldKey::Model,
                        op: gento_search::CmpOp::Ne,
                        value: FieldValue::Text("animagine".to_string()),
                    })],
                };
                let hits = search_asset_ids(repos.conn(), &query, &SearchScope::default(), 0, 1000)
                    .unwrap()
                    .0;
                assert_eq!(hits, vec![comfy, plain], "Ne = 一致しないasset (id昇順)");
            }

            // steps: 数値比較
            assert_eq!(search_all(repos, "steps:>=30"), vec![comfy]);
            assert_eq!(search_all(repos, "steps:>28"), vec![comfy]);
            assert_eq!(search_all(repos, "steps:<=28"), vec![sd]);
            assert_eq!(search_all(repos, "steps:28"), vec![sd]);
            // メタデータなしassetはsteps条件に掛からない
            assert!(!search_all(repos, "steps:>=1").contains(&plain));

            // has:workflow (ComfyUIのみ)
            assert_eq!(search_all(repos, "has:workflow"), vec![comfy]);
            {
                use gento_search::{FieldValue, SearchQuery, SearchTerm};
                let query = SearchQuery {
                    terms: vec![SearchTerm::Field(gento_search::FieldCondition {
                        key: gento_search::FieldKey::Has,
                        op: gento_search::CmpOp::Ne,
                        value: FieldValue::Text("workflow".to_string()),
                    })],
                };
                let hits = search_asset_ids(repos.conn(), &query, &SearchScope::default(), 0, 1000)
                    .unwrap()
                    .0;
                assert_eq!(hits, vec![sd, plain], "workflow非保持 (AST直接構築)");
            }

            // sampler / vae
            assert_eq!(
                search_all(repos, "sampler:euler"),
                vec![sd, comfy],
                "部分一致で両方"
            );
            // フリーテキストではvae名はヒットしない (文書化されるのはprompt等のみ)
            assert!(search_all(repos, "sdxl_vae").is_empty());
            assert_eq!(search_all(repos, "vae:sdxl"), vec![sd]);

            // lora: JSON配列内の名前照合
            assert_eq!(search_all(repos, "lora:add-detail"), vec![sd]);
            assert!(search_all(repos, "lora:unknown-lora").is_empty());

            // 複合
            assert_eq!(search_all(repos, "has:workflow steps:>=30"), vec![comfy]);
        });
    }

    // ===== 文書構築 (トリガーなきRust駆動・同一tx再構築) =====

    #[test]
    fn doc_composition_from_files_tags_captions() {
        with_repos("compose", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "c1");
            link_file(repos, root.id, "dir/青い髪の少女.png", asset);
            link_file(repos, root.id, "dir/copy2.png", asset);
            repos.add_tags_batch(&[asset], &["Night", "空"]).unwrap();
            repos
                .set_caption(asset, CaptionKind::Description, "夕暮れの描写")
                .unwrap();
            repos
                .set_caption(asset, CaptionKind::SdTags, "1girl, sunset")
                .unwrap();
            repos
                .set_caption(asset, CaptionKind::UserNote, "買った本の表紙")
                .unwrap();
            // 文書は書き込みと同一txで再構築されている (hook)
            let doc = build_asset_doc(repos.conn(), asset).unwrap().unwrap();
            assert!(doc.filename.contains("青い髪の少女.png"));
            assert!(doc.filename.contains("copy2.png"));
            assert!(doc.path.contains("dir/青い髪の少女.png"));
            assert!(doc.tags.contains("night") && doc.tags.contains("空"));
            assert!(doc.captions.contains("夕暮れの描写"));
            assert!(doc.captions.contains("1girl, sunset"));
            assert_eq!(doc.note, "買った本の表紙");
        });
    }

    #[test]
    fn doc_rebuild_follows_same_transaction_writes() {
        with_repos("rebuild", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "r1");
            link_file(repos, root.id, "a.png", asset);
            // タグ変更 → 文書即反映
            repos.add_tags_batch(&[asset], &["sky"]).unwrap();
            let hits = search_all(repos, "tag:sky");
            assert_eq!(hits, vec![asset]);
            repos
                .remove_tag_from_asset(asset, repos.list_asset_tags(asset).unwrap()[0].tag.id)
                .unwrap();
            assert!(search_all(repos, "tag:sky").is_empty());
            // キャプション変更 → 即反映
            repos
                .set_caption(asset, CaptionKind::Description, "vegetable garden")
                .unwrap();
            assert_eq!(search_all(repos, "garden"), vec![asset]);
            // 検索語は文書から消える
            repos
                .set_caption(asset, CaptionKind::Description, "changed")
                .unwrap();
            assert!(search_all(repos, "garden").is_empty());
        });
    }

    #[test]
    fn asset_without_present_files_has_no_doc() {
        with_repos("nofile", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "n1");
            assert!(build_asset_doc(repos.conn(), asset).unwrap().is_none());
            link_file(repos, root.id, "x.png", asset);
            assert!(build_asset_doc(repos.conn(), asset).unwrap().is_some());
            // 不在化 → 文書削除
            let file = repos
                .find_by_path(root.id, &PathBuf::from("x.png"))
                .unwrap()
                .unwrap();
            repos.set_scan_state(file.id, ScanState::Missing).unwrap();
            assert!(build_asset_doc(repos.conn(), asset).unwrap().is_none());
        });
    }

    // ===== 検索実行 (日本語・英数字・構文) =====

    #[test]
    fn search_japanese_and_ascii_keywords() {
        with_repos("keywords", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "k1");
            let a2 = make_asset(repos, "k2");
            let a3 = make_asset(repos, "k3");
            link_file(repos, root.id, "sunset_beach.png", a1);
            link_file(repos, root.id, "夕焼け.png", a2);
            link_file(repos, root.id, "other.png", a3);
            repos
                .set_caption(a3, CaptionKind::Description, "sunset over 双子ビル")
                .unwrap();

            // 英数字 (trigram MATCH)
            assert_eq!(search_all(repos, "sunset"), vec![a1, a3]);
            // 日本語substring (trigram)
            assert_eq!(search_all(repos, "夕焼け"), vec![a2]);
            assert_eq!(search_all(repos, "双子ビル"), vec![a3]);
            // 複数語はAND
            assert_eq!(search_all(repos, "sunset beach"), vec![a1]);
            assert!(search_all(repos, "sunset 花火").is_empty());
            // 引用句
            assert_eq!(search_all(repos, "\"sunset over\""), vec![a3]);
            // 空クエリ = 全件
            assert_eq!(search_all(repos, "").len(), 3);
        });
    }

    #[test]
    fn search_short_queries_fall_back_to_like() {
        with_repos("likefb", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "s1");
            let a2 = make_asset(repos, "s2");
            link_file(repos, root.id, "ab.png", a1);
            link_file(repos, root.id, "zz.png", a2);
            repos.add_tags_batch(&[a2], &["bd"]).unwrap();

            // 1〜2文字クエリ (trigram不可 → LIKE)
            assert_eq!(search_all(repos, "ab"), vec![a1]);
            assert_eq!(search_all(repos, "bd"), vec![a2]);
            assert_eq!(search_all(repos, "z"), vec![a2]);
            // 3文字はtrigram MATCH経由
            repos.add_tags_batch(&[a1], &["abc"]).unwrap();
            assert_eq!(search_all(repos, "abc"), vec![a1]);
        });
    }

    #[test]
    fn search_field_conditions() {
        with_repos("fields", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let rated = make_asset(repos, "f1");
            let picked = make_asset(repos, "f2");
            let faved = make_asset(repos, "f3");
            let tagged = make_asset(repos, "f4");
            let jpeg = repos
                .create_asset(NewAsset {
                    content_hash: "blake3:f5".to_string(),
                    hash_algorithm: "blake3".to_string(),
                    byte_size: 10,
                    mime_type: Some("image/jpeg".to_string()),
                    width: None,
                    height: None,
                    has_alpha: None,
                    is_animated: None,
                    frame_count: None,
                })
                .unwrap()
                .id;
            for (i, asset) in [rated, picked, faved, tagged].iter().enumerate() {
                link_file(repos, root.id, &format!("f{i}.png"), *asset);
            }
            link_file(repos, root.id, "photo.jpg", jpeg);
            repos.set_rating(rated, 4).unwrap();
            repos.set_cull_state(picked, TriageState::Picked).unwrap();
            repos.set_favorite(faved, true).unwrap();
            repos.add_tags_batch(&[tagged], &["Night"]).unwrap();

            assert_eq!(search_all(repos, "rating:>=4"), vec![rated]);
            assert_eq!(search_all(repos, "rating:4"), vec![rated]);
            assert_eq!(search_all(repos, "cull:picked"), vec![picked]);
            assert_eq!(search_all(repos, "fav:true"), vec![faved]);
            // タグ検索は正規化一致
            assert_eq!(search_all(repos, "tag:night"), vec![tagged]);
            assert_eq!(search_all(repos, "tag:NIGHT"), vec![tagged]);
            // type: 拡張子と完全MIME両対応
            assert_eq!(search_all(repos, "type:jpeg"), vec![jpeg]);
            assert_eq!(search_all(repos, "type:image/jpeg"), vec![jpeg]);
            assert_eq!(search_all(repos, "type:png").len(), 4);
            // path / name 部分一致
            assert_eq!(search_all(repos, "name:f1"), vec![picked]);
            assert_eq!(search_all(repos, "path:photo"), vec![jpeg]);
            // 組み合わせ (フリーテキスト×構造化のAND)
            assert_eq!(search_all(repos, "tag:night name:f3"), vec![tagged]);
            assert_eq!(
                search_all(repos, "rating:>=4 fav:true cull:picked"),
                Vec::<i64>::new()
            );
            assert!(search_all(repos, "tag:night fav:true").is_empty());
        });
    }

    #[test]
    fn search_added_date_range() {
        with_repos("added", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "d1");
            let a2 = make_asset(repos, "d2");
            link_file(repos, root.id, "a.png", a1);
            link_file(repos, root.id, "b.png", a2);
            // 作成日時を操作 (直接SQL — テスト専用)
            repos
                .conn()
                .execute(
                    "UPDATE assets SET created_at = '2026-03-15T10:00:00.000Z' WHERE id = ?1",
                    [a1],
                )
                .unwrap();
            repos
                .conn()
                .execute(
                    "UPDATE assets SET created_at = '2026-07-01T10:00:00.000Z' WHERE id = ?1",
                    [a2],
                )
                .unwrap();
            assert_eq!(search_all(repos, "added:2026-01-01..2026-06-30"), vec![a1]);
            assert_eq!(search_all(repos, "added:2026-07-01"), vec![a2]);
            assert_eq!(search_all(repos, "added:..2026-06-30"), vec![a1]);
            assert_eq!(search_all(repos, "added:2026-07-01.."), vec![a2]);
        });
    }

    #[test]
    fn search_scopes_root_and_collection() {
        with_repos("scope", |repos| {
            let root1 = repos.add_root(Path::new("/data/one")).unwrap();
            let root2 = repos.add_root(Path::new("/data/two")).unwrap();
            let a1 = make_asset(repos, "sc1");
            let a2 = make_asset(repos, "sc2");
            link_file(repos, root1.id, "one.png", a1);
            link_file(repos, root2.id, "two.png", a2);
            let collection = repos.create_manual_collection("c").unwrap();
            repos.add_collection_items(collection.id, &[a1]).unwrap();

            let query = parse("").unwrap().query;
            let (ids, total) = search_asset_ids(
                repos.conn(),
                &query,
                &SearchScope {
                    root_id: Some(root1.id),
                    collection_id: None,
                    require_present_files: true,
                },
                0,
                10,
            )
            .unwrap();
            assert_eq!((ids, total), (vec![a1], 1));

            let (ids, _) = search_asset_ids(
                repos.conn(),
                &query,
                &SearchScope {
                    root_id: None,
                    collection_id: Some(collection.id),
                    require_present_files: true,
                },
                0,
                10,
            )
            .unwrap();
            assert_eq!(ids, vec![a1]);
        });
    }

    #[test]
    fn search_paging() {
        with_repos("paging", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let mut ids = Vec::new();
            for i in 0..5 {
                let asset = make_asset(repos, &format!("p{i}"));
                link_file(repos, root.id, &format!("p{i}.png"), asset);
                ids.push(asset);
            }
            let query = parse("p").unwrap().query;
            let (page1, total) =
                search_asset_ids(repos.conn(), &query, &SearchScope::default(), 0, 2).unwrap();
            let (page2, _) =
                search_asset_ids(repos.conn(), &query, &SearchScope::default(), 2, 2).unwrap();
            let (page3, _) =
                search_asset_ids(repos.conn(), &query, &SearchScope::default(), 4, 2).unwrap();
            assert_eq!(total, 5);
            assert_eq!(page1.len(), 2);
            assert_eq!(page2.len(), 2);
            assert_eq!(page3.len(), 1);
            let mut all = page1;
            all.extend(page2);
            all.extend(page3);
            assert_eq!(all, ids);
        });
    }

    // ===== 一致根拠 =====

    #[test]
    fn evidence_fields_and_snippets() {
        with_repos("evidence", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "e1");
            link_file(repos, root.id, "sunset_photo.png", asset);
            repos.add_tags_batch(&[asset], &["sunsettag"]).unwrap();
            repos
                .set_caption(asset, CaptionKind::Description, "美しい夕焼けの海辺の描写")
                .unwrap();
            let query = parse("sunset 夕焼け").unwrap().query;
            let evidence = search_evidence(repos.conn(), &[asset], &query).unwrap();
            assert_eq!(evidence.len(), 1);
            let (_, fields) = &evidence[0];
            let field_names: Vec<&str> = fields.iter().map(|e| e.field.as_str()).collect();
            assert!(field_names.contains(&"filename"), "{field_names:?}");
            assert!(field_names.contains(&"tags"), "{field_names:?}");
            assert!(field_names.contains(&"captions"), "{field_names:?}");
            for evidence in fields {
                assert!(
                    evidence.snippet.contains('[') && evidence.snippet.contains(']'),
                    "snippetは一致箇所を[]で示す: {}",
                    evidence.snippet
                );
            }
            // 非一致assetは結果に出ない
            let other = make_asset(repos, "e2");
            link_file(repos, root.id, "zzz.png", other);
            let evidence = search_evidence(repos.conn(), &[other], &query).unwrap();
            assert!(evidence.is_empty());
        });
    }

    #[test]
    fn evidence_like_short_terms() {
        with_repos("elikeshort", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let asset = make_asset(repos, "el1");
            link_file(repos, root.id, "ab.png", asset);
            let query = parse("ab").unwrap().query;
            let evidence = search_evidence(repos.conn(), &[asset], &query).unwrap();
            assert_eq!(evidence.len(), 1);
            assert_eq!(evidence[0].1[0].field, DocField::Filename);
            assert!(evidence[0].1[0].snippet.contains("ab"));
        });
    }

    // ===== 整合性診断・再構築 =====

    #[test]
    fn integrity_detects_and_rebuild_repairs() {
        with_repos("integrity", |repos| {
            let root = repos.add_root(Path::new("/data/p")).unwrap();
            let a1 = make_asset(repos, "i1");
            let a2 = make_asset(repos, "i2");
            link_file(repos, root.id, "i1.png", a1);
            link_file(repos, root.id, "i2.png", a2);
            let report = fts_integrity(repos.conn()).unwrap();
            assert!(report.consistent, "{report:?}");
            assert_eq!(report.expected_docs, 2);

            // 索引破壊を模擬 (直接SQL)
            repos
                .conn()
                .execute("DELETE FROM asset_docs WHERE asset_id = ?1", [a1])
                .unwrap();
            repos
                .conn()
                .execute(
                    "UPDATE asset_docs SET filename = 'stale', path = 'stale' WHERE asset_id = ?1",
                    [a2],
                )
                .unwrap();
            let broken = fts_integrity(repos.conn()).unwrap();
            assert!(!broken.consistent);
            assert_eq!(broken.missing_docs, 1);
            assert_eq!(broken.stale_contents, 1);

            // 全再構築で修復
            let mut progress_calls = 0;
            let written = fts_rebuild(repos.conn(), &mut |done, total| {
                progress_calls += 1;
                assert!(done <= total);
            })
            .unwrap();
            assert_eq!(written, 2);
            assert!(progress_calls >= 1, "進捗が通知される");
            let repaired = fts_integrity(repos.conn()).unwrap();
            assert!(repaired.consistent, "{repaired:?}");
            assert_eq!(search_all(repos, "i1"), vec![a1]);
        });
    }

    #[test]
    fn prune_orphan_docs_removes_dangling() {
        with_repos("prune", |repos| {
            // assets本体に無いasset_idの文書を直接注入
            repos
                .conn()
                .execute(
                    "INSERT INTO asset_docs (asset_id, filename, path, tags, captions, note) \
                     VALUES (999999, 'x', 'x', '', '', '')",
                    [],
                )
                .unwrap();
            let removed = prune_orphan_docs(repos.conn()).unwrap();
            assert_eq!(removed, 1);
        });
    }

    // ===== 5万件性能 (合成行を直接DBへ投入して計測) =====

    #[test]
    fn search_performance_50k_synthetic() {
        with_repos("perf50k", |repos| {
            let conn = repos.conn();
            let root = repos.add_root(Path::new("/data/big")).unwrap();
            let total = 50_000i64;
            conn.execute_batch("BEGIN").unwrap();
            // assets + files (実ファイル不要の合成行)
            for i in 0..total {
                let hash = format!("blake3:perf{i:08x}");
                let mime = match i % 3 {
                    0 => "image/png",
                    1 => "image/jpeg",
                    _ => "image/webp",
                };
                conn.execute(
                    "INSERT INTO assets (uuid, content_hash, hash_algorithm, byte_size, \
                     mime_type, rating, created_at, updated_at) \
                     VALUES (?1, ?2, 'blake3', 100, ?3, ?4, ?5, ?5)",
                    params![
                        format!("00000000-0000-7000-8000-{i:012}", i = i + 1),
                        hash,
                        mime,
                        i % 6,
                        format!("2026-{:02}-15T10:00:00.000Z", (i % 12) + 1)
                    ],
                )
                .unwrap();
                let asset_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO files (uuid, root_id, asset_id, rel_path, path_encoding, \
                     display_name, extension, byte_size, mtime_ms, last_seen_at, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, CAST(?4 AS BLOB), 'utf8', ?5, ?6, 100, 1, \
                     '2026-08-31T00:00:00.000Z', '2026-08-31T00:00:00.000Z', '2026-08-31T00:00:00.000Z')",
                    params![
                        format!("00000000-0000-7000-8000-{i:012}", i = total + i + 1),
                        root.id,
                        asset_id,
                        format!("dir{}/画像{:06}.png", i % 100, i),
                        format!("画像{:06}.png", i),
                        "png"
                    ],
                )
                .unwrap();
                if i % 10 == 0 {
                    conn.execute(
                        "INSERT INTO tags (uuid, display_name, normalized_name, created_at, updated_at) \
                         VALUES (?1, ?2, ?2, '2026-08-31T00:00:00.000Z', '2026-08-31T00:00:00.000Z') \
                         ON CONFLICT (normalized_name) DO NOTHING",
                        params![
                            format!("00000000-0000-7000-8000-{i:012}", i = 2 * total + i + 1),
                            format!("tag{}", i % 50)
                        ],
                    )
                    .unwrap();
                    conn.execute(
                        "INSERT INTO asset_tags (asset_id, tag_id, source, manually_edited, created_at) \
                         SELECT ?1, id, 'manual', 1, '2026-08-31T00:00:00.000Z' FROM tags \
                         WHERE normalized_name = ?2",
                        params![asset_id, format!("tag{}", i % 50)],
                    )
                    .unwrap();
                }
            }
            conn.execute_batch("COMMIT").unwrap();
            eprintln!("[perf] synthetic rows inserted");

            // 全文書構築 (fts_rebuild経路)
            let started = std::time::Instant::now();
            let written = fts_rebuild(conn, &mut |_, _| {}).unwrap();
            assert_eq!(written, total as u64);
            let rebuild_secs = started.elapsed().as_secs_f64();
            eprintln!("[perf] rebuild done in {rebuild_secs:.1}s");
            assert!(
                rebuild_secs < 120.0,
                "5万件の再構築が遅すぎ: {rebuild_secs}s"
            );

            // クエリ計測: 代表シナリオ×繰り返しでp95
            let scenarios = [
                "画像",                         // 日本語substring (高頻度)
                "画像012345",                   // 日本語 (狭い)
                "tag:tag0 rating:>=4",          // 構造化
                "tag:tag0 画像",                // 複合
                "added:2026-01-01..2026-03-31", // 日付範囲
                "ab",                           // LIKEフォールバック
            ];
            let mut timings: Vec<f64> = Vec::new();
            for scenario in scenarios {
                for run in 0..10 {
                    let query = parse(scenario).unwrap().query;
                    let started = std::time::Instant::now();
                    let (ids, total_hits) =
                        search_asset_ids(conn, &query, &SearchScope::default(), 0, 200).unwrap();
                    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                    eprintln!("[perf] {scenario} run{run}: {elapsed:.2}ms");
                    timings.push(elapsed);
                    // 結果が空でないシナリオの健全性 (LIKE 'ab' は空もあり得る)
                    if run == 0 {
                        println!("scenario {scenario:?}: {total_hits} hits, {elapsed:.2}ms");
                        assert!(
                            total_hits > 0 || scenario == "ab",
                            "{scenario} は少なくとも1件ヒットする前提"
                        );
                        let _ = ids;
                    }
                }
            }
            timings.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let p95 = timings[(timings.len() as f64 * 0.95) as usize];
            println!(
                "50k search: n={} p50={:.2}ms p95={:.2}ms max={:.2}ms rebuild={:.1}s",
                timings.len(),
                timings[timings.len() / 2],
                p95,
                timings[timings.len() - 1],
                rebuild_secs
            );
            // 目安 (p95 < 100ms) はrelease測定の基準。debugビルドはFTS/LIKEの
            // 解釈が遅いため緩い上限で健全性のみ担保する (release値は
            // handoff・measurementsに記録)。
            if cfg!(debug_assertions) {
                assert!(
                    p95 < 500.0,
                    "5万件規模のクエリp95が異常: {p95:.2}ms (debug測定)"
                );
            } else {
                assert!(
                    p95 < 100.0,
                    "5万件規模のクエリp95が100ms超: {p95:.2}ms (release測定)"
                );
            }

            // 実経路計測 (search_run相当 — QA-WP8): 一致全件を5000件/ページ
            // で全収集する (apps/desktopのcollect_search_idsと同じ呼び方)。
            // 広域クエリ「画像」= 5万一致のフル収集。
            let wide = parse("画像").unwrap().query;
            let started = std::time::Instant::now();
            let mut collected: u64 = 0;
            let mut offset: u32 = 0;
            let total_hits = loop {
                let (page, total) =
                    search_asset_ids(conn, &wide, &SearchScope::default(), offset, 5_000).unwrap();
                let page_empty = page.is_empty();
                collected += page.len() as u64;
                if collected >= total.min(100_000) || page_empty {
                    break total;
                }
                offset += 5_000;
            };
            let collect_ms = started.elapsed().as_secs_f64() * 1000.0;
            println!(
                "50k full collect (search_run相当): total={total_hits} collected={collected} pages={} {collect_ms:.0}ms",
                offset / 5_000 + 1
            );
            assert_eq!(total_hits, 50_000, "広域クエリは全件一致が前提");
            assert_eq!(collected, 50_000);
            // 健全性の上限のみ (release実測値は measurements/wp8-search-50k.md)。
            if cfg!(debug_assertions) {
                assert!(
                    collect_ms < 20_000.0,
                    "5万件の全収集が遅すぎ (debug): {collect_ms:.0}ms"
                );
            } else {
                assert!(
                    collect_ms < 5_000.0,
                    "5万件の全収集が遅すぎ (release): {collect_ms:.0}ms"
                );
            }
        });
    }
}
