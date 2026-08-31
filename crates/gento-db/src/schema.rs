//! スキーマ定義とmigration一覧 (MVP WP-1)。
//!
//! migration #1 は計画書§9の12テーブル全部 (DDLはdocs/data-model.md
//! §2〜§12の属性を反映)。AiProfile/AiRunテーブルはWP-10で追加migration
//! (#2以降) とする — `asset_tags.ai_run_id` / `captions.ai_run_id` 列も
//! この時点では作らない。SQLiteは既存列へのFK付与ができず、後付けには
//! テーブル再構築が必要になるため、WP-10で `ai_runs` テーブル作成と
//! 同時に `ALTER TABLE … ADD COLUMN ai_run_id INTEGER REFERENCES
//! ai_runs(id)` で追加する (NULL既定のADD COLUMNならFK参照を付与可能。
//! テーブル再構築を避ける意図)。
//!
//! 方針:
//! - 1 migration = 1トランザクション (migrate.rs)。順方向のみ。
//! - 破壊的migration追加時は事前backupが必ず走る (migrate.rs)。
//! - インデックスは検索頻度の高いものを初期migrationに含める。

/// migration #1 で作るテーブル (diagnoseの行数計上にも使う)。
pub(crate) const TABLES: [&str; 12] = [
    "library_roots",
    "assets",
    "files",
    "tags",
    "asset_tags",
    "captions",
    "collections",
    "collection_items",
    "asset_metadata",
    "jobs",
    "file_operations",
    "embeddings",
];

/// 個別のmigration定義。
#[derive(Debug, Clone)]
pub(crate) struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 順方向migration一覧。versionは1開始・連続であること
/// (run_migrationsが検証する)。
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_12_tables",
        sql: MIGRATION_V1_SQL,
    },
    Migration {
        version: 2,
        name: "wp2_files_scan_indexes",
        sql: MIGRATION_V2_SQL,
    },
    Migration {
        version: 3,
        name: "wp2_root_os_file_id_index",
        sql: MIGRATION_V3_SQL,
    },
    Migration {
        version: 4,
        name: "fgwp22_files_mtime_ns",
        sql: MIGRATION_V4_SQL,
    },
    Migration {
        version: 5,
        name: "wp7_file_operations_status",
        sql: MIGRATION_V5_SQL,
    },
];

/// 現在の最新schema version (`PRAGMA user_version` の目標値)。
/// このbuildの [`crate::Db::open`] は既存DBを必ずこの値へ移行する。
pub const LATEST_VERSION: i32 = 5;

/// WP-2 (登録ルート・差分スキャン) のindex追加。
/// 列追加は不要 — 設定列は#1に含まれ、os_file_idも#1でNULL可として定義済み。
///
/// - `(root_id, scan_state)`: 差分スキャンの既存File列挙とmissing一覧の
///   複合条件。既存の `idx_files_scan_state` は全ルート横断のため
///   ルート単位の絞り込みに効かない。
/// - `os_file_id` 部分index: 外部移動・改名の再発見照合
///   (`WHERE os_file_id IS NOT NULL` でNULL行を索引に載せない)。
const MIGRATION_V2_SQL: &str = r"
CREATE INDEX idx_files_root_scan_state ON files(root_id, scan_state);
CREATE INDEX idx_files_os_file_id ON files(os_file_id) WHERE os_file_id IS NOT NULL;
";

/// FG-WP2 (Codex正確性レビュー対応) のindex追加 (indexのみ。列追加なし)。
///
/// - `(root_id, os_file_id)` 部分index: reconciliation の移動検出照合は
///   「同一ルート内」条件付きルックアップ (`find_by_os_file_id`) のため、
///   グローバル `idx_files_os_file_id` (#2) よりこの複合indexが実査形に
///   適合する。旧indexは後方互換で残す (削除migrationは不要)。
const MIGRATION_V3_SQL: &str = r"
CREATE INDEX idx_files_root_os_file_id
    ON files(root_id, os_file_id) WHERE os_file_id IS NOT NULL;
";

/// FG-WP2-2 (レビューラウンド2): 不変世代トークン `(os_file_id, size,
/// mtime_ns)` のns成分列。既存 `mtime_ms` 列は後方互換で残し、走査は
/// 両方を更新する。NULL可 — migration前の行は次の走査でnsが埋まり、
/// それまでの間トークン照合は不一致 (Stale) 扱いで安全側に倒れる。
/// migration #1は凍結済みのため、greenfieldでも列追加は#4とする。
const MIGRATION_V4_SQL: &str = r"
ALTER TABLE files ADD COLUMN mtime_ns INTEGER NULL;
";

/// WP-7: `file_operations.status` のCHECKを拡張する (計画書§15の実行
/// モデル: planned / executing / done / partial / failed / undone)。
/// #1の初期CHECK ('pending','succeeded',…) はWP-7の状態機構と名前が
/// 合わないため、SQLiteのCHECK変更に必要なテーブル再構築で置き換える。
/// 旧値は「未完了→executing」「成功→done」へマップ (WP-7以前に行が書か
/// れる経路は無かったが、安全のため恒等でない変換を明示)。
const MIGRATION_V5_SQL: &str = r"
CREATE TABLE file_operations_v5 (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    operation_type TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('planned', 'executing', 'done', 'partial', 'failed', 'undone')),
    undoable INTEGER NOT NULL DEFAULT 0 CHECK (undoable IN (0, 1)),
    -- 操作要求 (複数ファイルの一括操作は1論理操作として記録)。
    request_json TEXT NOT NULL,
    result_json TEXT,
    created_at TEXT NOT NULL,
    undone_at TEXT
);
INSERT INTO file_operations_v5 (id, uuid, operation_type, status, undoable,
                                request_json, result_json, created_at, undone_at)
    SELECT id, uuid, operation_type,
           CASE status
               WHEN 'pending' THEN 'executing'
               WHEN 'succeeded' THEN 'done'
               ELSE status
           END,
           undoable, request_json, result_json, created_at, undone_at
      FROM file_operations;
DROP TABLE file_operations;
ALTER TABLE file_operations_v5 RENAME TO file_operations;
";

const MIGRATION_V1_SQL: &str = r#"
CREATE TABLE library_roots (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    -- OSネイティブパスの生バイト (Unix。非UTF-8を含み得る)。
    path BLOB NOT NULL,
    path_encoding TEXT NOT NULL
        CHECK (path_encoding IN ('unix-bytes', 'utf8')),
    -- 表示用パス (lossy変換。非UTF-8部分は置換文字)。
    display_path TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- ルートごとの拡張設定 (data-model §2.3。WP-2のwatcherが使用)。
    watch_enabled INTEGER NOT NULL DEFAULT 1 CHECK (watch_enabled IN (0, 1)),
    ai_enabled INTEGER NOT NULL DEFAULT 0 CHECK (ai_enabled IN (0, 1)),
    send_exif INTEGER NOT NULL DEFAULT 0 CHECK (send_exif IN (0, 1)),
    send_gps INTEGER NOT NULL DEFAULT 0 CHECK (send_gps IN (0, 1)),
    -- 将来の詳細設定用JSON (NULL可。列追加を避ける逃がし)。
    settings_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (path_encoding, path)
);

CREATE TABLE assets (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    -- '<algorithm>:<hex>'。同一内容 = 同一Asset (WP-2でBLAKE3採用)。
    content_hash TEXT NOT NULL UNIQUE,
    hash_algorithm TEXT NOT NULL CHECK (hash_algorithm IN ('blake3')),
    hash_state TEXT NOT NULL DEFAULT 'done'
        CHECK (hash_state IN ('pending', 'done')),
    mime_type TEXT,
    byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
    width INTEGER,
    height INTEGER,
    bit_depth INTEGER,
    has_alpha INTEGER CHECK (has_alpha IN (0, 1)),
    is_animated INTEGER CHECK (is_animated IN (0, 1)),
    frame_count INTEGER,
    duration_ms INTEGER,
    rating INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
    triage TEXT NOT NULL DEFAULT 'unselected'
        CHECK (triage IN ('unselected', 'picked', 'rejected')),
    favorite INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    root_id INTEGER NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    -- Asset作成前の走査中Fileは NULL (WP-1受入「Asset作成前でも
    -- 走査中Fileを管理できる」)。RESTRICT: Fileが残る限りAsset削除を
    -- 拒否し、評価・タグ等の共有メタデータ喪失を防ぐ。
    asset_id INTEGER REFERENCES assets(id) ON DELETE RESTRICT,
    -- 登録ルートからの相対パス (生バイト)。UNIQUEと合わせ差分走査の
    -- キーとして使う。
    rel_path BLOB NOT NULL,
    path_encoding TEXT NOT NULL
        CHECK (path_encoding IN ('unix-bytes', 'utf8')),
    display_name TEXT NOT NULL,
    extension TEXT,
    byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
    -- ファイル由来の時刻はepoch ms整数 (RFC3339 TEXTはDBレコードの
    -- ライフサイクル時刻のみに使う規約)。
    mtime_ms INTEGER NOT NULL,
    ctime_ms INTEGER,
    -- OSファイルID (Linux inode等。利用可能な場合のみWP-2で設定)。
    os_file_id BLOB,
    scan_state TEXT NOT NULL DEFAULT 'present'
        CHECK (scan_state IN ('present', 'missing', 'unreadable', 'unsupported', 'corrupt')),
    -- 最後にファイルの存在を確認した時刻。状態変更 (set_scan_state) では
    -- 更新しない。存在確認を反映するのはupsert (発見) のみ。
    last_seen_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (root_id, path_encoding, rel_path)
);
CREATE INDEX idx_files_asset_id ON files(asset_id);
CREATE INDEX idx_files_scan_state ON files(scan_state);

CREATE TABLE tags (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    display_name TEXT NOT NULL,
    -- 正規化名 (前後空白除去等。規則の詳細はWP-5で確定)。
    normalized_name TEXT NOT NULL UNIQUE,
    category TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE asset_tags (
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    source TEXT NOT NULL
        CHECK (source IN ('manual', 'ai', 'import', 'embedded_metadata')),
    ai_confidence REAL,
    -- ai_run_id は WP-10 でai_runsテーブル作成と同時にADD COLUMNで追加
    -- (crate doc参照。この時点では列自体を持たない)。
    manually_edited INTEGER NOT NULL DEFAULT 0 CHECK (manually_edited IN (0, 1)),
    created_at TEXT NOT NULL,
    PRIMARY KEY (asset_id, tag_id)
);
CREATE INDEX idx_asset_tags_tag_id ON asset_tags(tag_id);

CREATE TABLE captions (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    kind TEXT NOT NULL
        CHECK (kind IN ('description', 'sd_tags', 'user_note', 'ocr',
                        'embedded_prompt', 'embedded_negative_prompt')),
    body TEXT NOT NULL,
    source TEXT NOT NULL
        CHECK (source IN ('manual', 'ai', 'import', 'embedded_metadata')),
    language TEXT,
    -- ai_run_id は WP-10 でai_runsテーブル作成と同時にADD COLUMNで追加
    -- (crate doc参照。この時点では列自体を持たない)。
    -- 再生成時は旧レコードを削除せず非現行 (0) として履歴を残す。
    current INTEGER NOT NULL DEFAULT 1 CHECK (current IN (0, 1)),
    manually_edited INTEGER NOT NULL DEFAULT 0 CHECK (manually_edited IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_captions_asset_kind ON captions(asset_id, kind);
-- 現行レコードは asset+kind で一意。履歴 (current=0) は複数許容。
CREATE UNIQUE INDEX idx_captions_current_unique
    ON captions(asset_id, kind) WHERE current = 1;

CREATE TABLE collections (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('manual', 'smart')),
    -- smart: 構造化検索条件のJSON (SQL文そのものは保存しない。WP-6)。
    query_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (kind = 'manual' OR query_json IS NOT NULL)
);

CREATE TABLE collection_items (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    -- 手動コレクション内の任意並び順 (WP-6で使用)。
    position INTEGER NOT NULL,
    note TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (collection_id, asset_id)
);
CREATE INDEX idx_collection_items_asset_id ON collection_items(asset_id);

CREATE TABLE asset_metadata (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    -- 正規化済み主要項目 (NULL = 未知/不存在)。data-model §6。
    taken_at TEXT,
    camera_make TEXT,
    camera_model TEXT,
    lens TEXT,
    gps_latitude REAL,
    gps_longitude REAL,
    orientation INTEGER,
    generator TEXT,
    generation_model TEXT,
    vae TEXT,
    sampler TEXT,
    scheduler TEXT,
    steps INTEGER,
    cfg_scale REAL,
    -- 生成ツールによる表現差と数値範囲のため文字列。
    seed TEXT,
    positive_prompt TEXT,
    negative_prompt TEXT,
    -- LoRA等の追加生成情報 (JSON)。
    generation_extra TEXT,
    -- 生データ (JSON文字列)。
    raw_exif TEXT,
    raw_iptc TEXT,
    raw_png_metadata TEXT,
    raw_workflow TEXT,
    -- parser更新時の再解析判定に使用。
    parser_name TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (asset_id, parser_name)
);

CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    job_type TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    priority INTEGER NOT NULL DEFAULT 5 CHECK (priority BETWEEN 0 AND 9),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 3 CHECK (max_attempts >= 1),
    payload TEXT NOT NULL DEFAULT '{}',
    error TEXT,
    scheduled_at TEXT,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_jobs_status_priority ON jobs(status, priority);

CREATE TABLE file_operations (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    operation_type TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('pending', 'succeeded', 'partial', 'failed', 'undone')),
    undoable INTEGER NOT NULL DEFAULT 0 CHECK (undoable IN (0, 1)),
    -- 操作要求 (複数ファイルの一括操作は1論理操作として記録)。
    request_json TEXT NOT NULL,
    result_json TEXT,
    created_at TEXT NOT NULL,
    undone_at TEXT
);

CREATE TABLE embeddings (
    id INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE
        CHECK (length(uuid) = 36 AND uuid = lower(uuid)),
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL,
    model_name TEXT NOT NULL,
    model_revision TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    dtype TEXT NOT NULL CHECK (dtype IN ('f32', 'f16')),
    -- 元検索文書のハッシュ (期限切れ検出に使用。WP-12)。
    source_hash TEXT NOT NULL,
    vector BLOB NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (asset_id, purpose, model_name, model_revision)
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_versions_are_contiguous_from_1() {
        for (index, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                m.version,
                index as i32 + 1,
                "migration番号は1開始連続であること"
            );
        }
        assert_eq!(LATEST_VERSION, MIGRATIONS.last().expect("空でない").version);
    }

    #[test]
    fn tables_const_lists_12_tables() {
        assert_eq!(TABLES.len(), 12);
        // 計画書§9の12テーブルと一致
        let expected = [
            "library_roots",
            "assets",
            "files",
            "tags",
            "asset_tags",
            "captions",
            "collections",
            "collection_items",
            "asset_metadata",
            "jobs",
            "file_operations",
            "embeddings",
        ];
        for t in expected {
            assert!(TABLES.contains(&t), "missing table: {t}");
        }
    }
}
