//! 永続ジョブキュー (jobs表) のrepository実装 (MVP WP-10)。
//!
//! 状態機械 (spec §11.2。CHECK制約が正本):
//!
//! ```text
//!  queued ──claim──▶ running ──成功──▶ succeeded
//!    ▲                 │
//!    │                 ├─失敗 (attempts < max)──▶ retry_waiting
//!    │                 │        ▲ scheduled_at = バックオフ期限
//!    │                 │        └──期限到着 (claim)──▶ running
//!    │                 └─失敗 (attempts >= max)──▶ failed
//!    │                          failed ──retry_failed──▶ queued (手動)
//!  queued/retry_waiting ──cancel──▶ cancelled
//!  running (前回終了時) ──起動回復──▶ queued
//! ```
//!
//! - claimはトランザクション内で SELECT → UPDATE することで
//!   複数ワーカーが同一ジョブを二重実行しない (単一writer接続の
//!   Mutex直列化に加えた防衛。接続を分けた実装に変えても安全)。
//! - pending中の同一 (job_type, target_id) 重複は部分UNIQUE index
//!   (migration #8) が拒否する。`enqueue_job` はON CONFLICT DO NOTHING
//!   で「既に待機中なら何もしない」を冪等にする。
//! - バックオフ期限の計算は gento-core::jobs::backoff_delay_ms
//!   (純粋関数)。この層は期限 (RFC3339) の保存のみ。

use gento_core::repo::RepoError;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ids::{new_uuid_v7, now_utc_rfc3339};
use crate::repos::{map_err, SqliteRepos};

/// jobs行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    pub id: i64,
    pub uuid: String,
    /// "ai_caption_tags" | "ai_reparse" (CHECK制約が正本)。
    pub job_type: String,
    pub target_type: String,
    pub target_id: i64,
    /// "queued" | "running" | "succeeded" | "failed" | "cancelled" | "retry_waiting"。
    pub status: String,
    pub priority: i64,
    pub attempts: u32,
    pub max_attempts: u32,
    pub payload: String,
    pub error: Option<String>,
    /// retry_waitingのバックオフ期限 (RFC3339)。queued/running等ではNULL。
    pub scheduled_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const JOB_COLUMNS: &str = "id, uuid, job_type, target_type, target_id, status, priority, \
     attempts, max_attempts, payload, error, scheduled_at, started_at, finished_at, \
     created_at, updated_at";

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: row.get(0)?,
        uuid: row.get(1)?,
        job_type: row.get(2)?,
        target_type: row.get(3)?,
        target_id: row.get(4)?,
        status: row.get(5)?,
        priority: row.get(6)?,
        attempts: row.get::<_, i64>(7)?.max(0) as u32,
        max_attempts: row.get::<_, i64>(8)?.max(1) as u32,
        payload: row.get(9)?,
        error: row.get(10)?,
        scheduled_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn get_job_by_id(conn: &Connection, id: i64) -> Result<Option<JobRecord>, RepoError> {
    let sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1");
    conn.query_row(&sql, [id], map_job)
        .optional()
        .map_err(map_err)
}

/// ジョブ種別 (IPC文字列)。
pub const JOB_TYPE_AI_CAPTION_TAGS: &str = "ai_caption_tags";
pub const JOB_TYPE_AI_REPARSE: &str = "ai_reparse";

/// 現在時刻 + `delay_ms` をRFC3339 (UTC) で返す (バックオフ期限の計算)。
pub fn rfc3339_after_now(delay_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_millis(delay_ms))
        .unwrap_or(std::time::SystemTime::now());
    crate::ids::format_epoch_rfc3339_pub(now)
}

const ELIGIBLE_CONDITION: &str = "status = 'queued' \
     OR (status = 'retry_waiting' AND scheduled_at IS NOT NULL AND scheduled_at <= ?1)";

impl SqliteRepos<'_> {
    /// ジョブを投入する。同一 (job_type, target_id) のpendingジョブが
    /// 既にある場合は投入せず `None` を返す (冪等)。
    pub fn enqueue_job(
        &self,
        job_type: &str,
        target_type: &str,
        target_id: i64,
        priority: i64,
        payload: &str,
    ) -> Result<Option<JobRecord>, RepoError> {
        let now = now_utc_rfc3339();
        self.conn()
            .execute(
                "INSERT INTO jobs (uuid, job_type, target_type, target_id, status, priority, \
                 attempts, max_attempts, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'queued', ?5, 0, ?6, ?7, ?8, ?8)
                 ON CONFLICT (job_type, target_id) WHERE status IN \
                 ('queued', 'running', 'retry_waiting') DO NOTHING",
                params![
                    new_uuid_v7(),
                    job_type,
                    target_type,
                    target_id,
                    priority,
                    gento_core::jobs::DEFAULT_MAX_ATTEMPTS as i64,
                    payload,
                    now
                ],
            )
            .map_err(map_err)?;
        let id = self.conn().last_insert_rowid();
        if id == 0 {
            // ON CONFLICT DO NOTHINGでもlast_insert_rowidは直前の成功INSERTを
            // 返し得るため、changes()で実際の投入有無を判定する。
        }
        let changed = self.conn().changes() > 0;
        if !changed {
            return Ok(None);
        }
        get_job_by_id(self.conn(), id)
    }

    /// 起動回復: 前回終了時 `running` のジョブを `queued` へ戻す
    /// (クリーンシャットダウン・クラッシュ両方。attemptsは据え置き —
    /// 実行中だった試行は失敗扱いにせず、同一試行として再実行する)。
    pub fn recover_running_jobs(&self) -> Result<u64, RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'queued', updated_at = ?1 \
                 WHERE status = 'running'",
                [now],
            )
            .map_err(map_err)?;
        Ok(changed as u64)
    }

    /// 実行可能なジョブ候補を覗き見する (状態遷移なし。workerが
    /// プロファイル飽和を考慮して対象を選ぶために使う)。
    pub fn list_eligible_jobs(
        &self,
        now_rfc3339: &str,
        limit: u32,
    ) -> Result<Vec<JobRecord>, RepoError> {
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE {ELIGIBLE_CONDITION} \
             ORDER BY priority ASC, id ASC LIMIT ?2"
        );
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map(params![now_rfc3339, limit], map_job)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// 指定IDのジョブをatomically claimする (queued/retry_waitingかつ
    /// backoff期限到着の場合のみ成功)。他ワーカーとの競合は0変更で敗退。
    pub fn claim_job_by_id(
        &self,
        id: i64,
        now_rfc3339: &str,
    ) -> Result<Option<JobRecord>, RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let now = now_utc_rfc3339();
        // ?3 = 判定時刻 (ELIGIBLE_CONDITION相当をinline。?1=idと衝突しない)
        let sql = "UPDATE jobs SET status = 'running', attempts = attempts + 1, \
             started_at = ?2, updated_at = ?2, error = NULL \
             WHERE id = ?1 AND (status = 'queued' \
                OR (status = 'retry_waiting' AND scheduled_at IS NOT NULL \
                    AND scheduled_at <= ?3))";
        let changed = tx
            .execute(sql, params![id, now, now_rfc3339])
            .map_err(map_err)?;
        if changed == 0 {
            return Ok(None);
        }
        let record = get_job_by_id(&tx, id)?.ok_or(RepoError::NotFound)?;
        tx.commit().map_err(map_err)?;
        Ok(Some(record))
    }

    /// 実行可能なジョブを1件atomically claimする (`running`へ遷移)。
    ///
    /// eligible = `queued`、または `retry_waiting` かつバックオフ期限
    /// (`scheduled_at`) 到着。優先度昇順・同優先度はID昇順。
    /// `now_rfc3339` は呼び出し側の時刻 (テストで固定値を注入可能)。
    pub fn claim_next_job(&self, now_rfc3339: &str) -> Result<Option<JobRecord>, RepoError> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let candidate: Option<i64> = tx
            .query_row(
                "SELECT id FROM jobs \
                 WHERE status = 'queued' \
                    OR (status = 'retry_waiting' AND scheduled_at IS NOT NULL \
                        AND scheduled_at <= ?1) \
                 ORDER BY priority ASC, id ASC LIMIT 1",
                [now_rfc3339],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_err)?;
        let Some(id) = candidate else {
            return Ok(None);
        };
        let now = now_utc_rfc3339();
        tx.execute(
            "UPDATE jobs SET status = 'running', attempts = attempts + 1, \
             started_at = ?2, updated_at = ?2, error = NULL WHERE id = ?1 AND \
             status IN ('queued', 'retry_waiting')",
            params![id, now],
        )
        .map_err(map_err)?;
        let record = get_job_by_id(&tx, id)?.ok_or(RepoError::NotFound)?;
        tx.commit().map_err(map_err)?;
        Ok(Some(record))
    }

    /// 実行成功で確定する (running → succeeded)。
    pub fn complete_job(&self, id: i64) -> Result<(), RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'succeeded', finished_at = ?2, \
                 updated_at = ?2 WHERE id = ?1 AND status = 'running'",
                params![id, now],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// 実行失敗。attempts < max_attempts なら `retry_waiting` へ
    /// (バックオフ期限 `retry_at_rfc3339`)、上限到達なら `failed` へ確定。
    /// 戻り値は遷移後のstatus。
    pub fn fail_job(
        &self,
        id: i64,
        error: &str,
        retry_at_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let now = now_utc_rfc3339();
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| RepoError::Db(e.to_string()))?;
        let record = get_job_by_id(&tx, id)?.ok_or(RepoError::NotFound)?;
        if record.status != "running" {
            return Err(RepoError::Constraint(format!(
                "job {id} is {} (not running); cannot fail",
                record.status
            )));
        }
        let status = if record.attempts >= record.max_attempts {
            "failed"
        } else {
            "retry_waiting"
        };
        tx.execute(
            "UPDATE jobs SET status = ?2, error = ?3, scheduled_at = ?4, \
             finished_at = ?5, updated_at = ?6 WHERE id = ?1",
            params![
                id,
                status,
                error,
                if status == "retry_waiting" {
                    Some(retry_at_rfc3339)
                } else {
                    None
                },
                if status == "failed" {
                    Some(now.as_str())
                } else {
                    None
                },
                now
            ],
        )
        .map_err(map_err)?;
        tx.commit().map_err(map_err)?;
        Ok(status.to_string())
    }

    /// 再試行不能な失敗。attempts上限とは無関係に即座 `failed` へ確定する
    /// (認証失敗・不正リクエスト等、反復しても同じ結果になる失敗の再送を
    /// 構造的に防ぐ — retry_waitingへ入らない)。
    pub fn fail_job_permanent(&self, id: i64, error: &str) -> Result<String, RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'failed', error = ?2, scheduled_at = NULL, \
                 finished_at = ?3, updated_at = ?3 WHERE id = ?1 AND status = 'running'",
                params![id, error, now],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::Constraint(format!(
                "job {id} is not running; cannot fail permanently"
            )));
        }
        Ok("failed".to_string())
    }

    /// queued/retry_waitingのジョブをキャンセルする。
    /// 実行中 (running) はHTTP呼び出しを中断できないため拒否する。
    pub fn cancel_job(&self, id: i64) -> Result<JobRecord, RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'cancelled', finished_at = ?2, \
                 scheduled_at = NULL, updated_at = ?2 \
                 WHERE id = ?1 AND status IN ('queued', 'retry_waiting')",
                params![id, now],
            )
            .map_err(map_err)?;
        if changed == 0 {
            return Err(RepoError::Constraint(format!(
                "job {id} is not cancellable (running/succeeded/failed/absent)"
            )));
        }
        get_job_by_id(self.conn(), id)?.ok_or(RepoError::NotFound)
    }

    /// 失敗済みジョブを再試行待ちへ戻す (手動再試行。attemptsをリセット)。
    /// `ids`が空なら全failedが対象。戻り値は戻した件数。
    pub fn retry_failed_jobs(&self, ids: &[i64]) -> Result<u64, RepoError> {
        let now = now_utc_rfc3339();
        let changed = if ids.is_empty() {
            self.conn()
                .execute(
                    "UPDATE jobs SET status = 'queued', attempts = 0, error = NULL, \
                     scheduled_at = NULL, finished_at = NULL, updated_at = ?1 \
                     WHERE status = 'failed'",
                    [now],
                )
                .map_err(map_err)?
        } else {
            let mut total = 0;
            for id in ids {
                total += self
                    .conn()
                    .execute(
                        "UPDATE jobs SET status = 'queued', attempts = 0, error = NULL, \
                         scheduled_at = NULL, finished_at = NULL, updated_at = ?1 \
                         WHERE id = ?2 AND status = 'failed'",
                        params![now, id],
                    )
                    .map_err(map_err)?;
            }
            total
        };
        Ok(changed as u64)
    }

    /// ジョブ一覧 (新しい順)。
    pub fn list_jobs(&self, limit: u32) -> Result<Vec<JobRecord>, RepoError> {
        let sql = format!("SELECT {JOB_COLUMNS} FROM jobs ORDER BY id DESC LIMIT ?1");
        let mut stmt = self.conn().prepare(&sql).map_err(map_err)?;
        let rows = stmt
            .query_map([limit], map_job)
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// 状態ごとの件数 (一覧UIのサマリ)。
    pub fn job_status_counts(&self) -> Result<Vec<(String, u64)>, RepoError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT status, count(*) FROM jobs GROUP BY status ORDER BY status")
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })
            .map_err(map_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_err)?;
        Ok(rows)
    }

    /// ペイロードJSONの `profile_id` が一致するpendingジョブをキャンセル
    /// (プロファイル削除時の後始末。JSON1関数はbundled SQLiteに組み込み)。
    pub fn cancel_jobs_for_ai_profile(&self, profile_id: i64) -> Result<u64, RepoError> {
        let now = now_utc_rfc3339();
        let changed = self
            .conn()
            .execute(
                "UPDATE jobs SET status = 'cancelled', finished_at = ?2, updated_at = ?2 \
                 WHERE status IN ('queued', 'retry_waiting') \
                   AND job_type = ?3 \
                   AND json_extract(payload, '$.profile_id') = ?1",
                params![profile_id, now, JOB_TYPE_AI_CAPTION_TAGS],
            )
            .map_err(map_err)?;
        Ok(changed as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{run_migrations, MigrationOpts};
    use crate::schema::MIGRATIONS;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-db-jobs-{tag}-{}-{}",
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

    fn asset_id(repos: &SqliteRepos<'_>, hash: &str) -> i64 {
        use gento_core::repo::{AssetRepo, NewAsset};
        repos
            .create_asset(NewAsset {
                content_hash: hash.to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 1,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap()
            .id
    }

    fn enqueue(repos: &SqliteRepos<'_>, target: i64) -> Option<JobRecord> {
        repos
            .enqueue_job(
                JOB_TYPE_AI_CAPTION_TAGS,
                "asset",
                target,
                5,
                &format!("{{\"profile_id\":1,\"asset_id\":{target}}}"),
            )
            .unwrap()
    }

    const NOW: &str = "2026-09-02T00:00:00.000Z";

    #[test]
    fn enqueue_dedupes_pending_same_target() {
        with_repos("dedup", |repos| {
            let a = enqueue(repos, 1).expect("first insert");
            assert_eq!(a.status, "queued");
            assert_eq!(a.max_attempts, gento_core::jobs::DEFAULT_MAX_ATTEMPTS);
            // 同一targetのpending中投入は無視
            assert!(enqueue(repos, 1).is_none());
            // 完了後は再投入可能
            let claimed = repos.claim_next_job(NOW).unwrap().expect("claim");
            assert_eq!(claimed.id, a.id);
            assert_eq!(claimed.status, "running");
            assert_eq!(claimed.attempts, 1);
            repos.complete_job(claimed.id).unwrap();
            let again = enqueue(repos, 1).expect("re-enqueue after finish");
            assert_ne!(again.id, a.id);
        });
    }

    #[test]
    fn claim_respects_priority_and_backoff_deadline() {
        with_repos("claimorder", |repos| {
            let low = enqueue(repos, 1).unwrap();
            // priority 0 (高) を後から投入
            repos
                .enqueue_job(JOB_TYPE_AI_REPARSE, "asset", 2, 0, "{}")
                .unwrap();
            let first = repos.claim_next_job(NOW).unwrap().unwrap();
            assert_eq!(first.id, low.id + 1, "priority 0が先");
            // 残りもclaimして両方fail → 全ジョブがretry_waitingへ
            let second = repos.claim_next_job(NOW).unwrap().unwrap();
            assert_eq!(second.id, low.id);
            repos
                .fail_job(first.id, "endpoint down", "2026-09-02T09:00:00.000Z")
                .unwrap();
            repos
                .fail_job(second.id, "endpoint down", "2026-09-02T09:00:00.000Z")
                .unwrap();
            assert!(
                repos
                    .claim_next_job("2026-09-02T00:00:05.000Z")
                    .unwrap()
                    .is_none(),
                "期限前のretry_waitingはclaim不可"
            );
            // 期限到着でclaim可能 (priority 0が先)
            let retry = repos
                .claim_next_job("2026-09-02T09:00:00.000Z")
                .unwrap()
                .expect("期限到着でclaim");
            assert_eq!(retry.id, first.id);
            assert_eq!(retry.attempts, 2, "試行回数は加算");
        });
    }

    #[test]
    fn fail_transitions_retry_then_failed_at_attempts_limit() {
        with_repos("failflow", |repos| {
            let job = enqueue(repos, 1).unwrap();
            let id = job.id;
            let _ = repos.claim_next_job(NOW).unwrap().unwrap();
            assert_eq!(
                repos
                    .fail_job(id, "err1", "2026-09-02T00:00:30.000Z")
                    .unwrap(),
                "retry_waiting"
            );
            let j = repos.list_jobs(10).unwrap().remove(0);
            assert_eq!(j.status, "retry_waiting");
            assert_eq!(j.scheduled_at.as_deref(), Some("2026-09-02T00:00:30.000Z"));
            assert_eq!(j.error.as_deref(), Some("err1"));
            assert!(j.finished_at.is_none(), "retry_waitingは未完了");
            // 2回目
            let _ = repos
                .claim_next_job("2026-09-02T00:01:00.000Z")
                .unwrap()
                .unwrap();
            assert_eq!(
                repos
                    .fail_job(id, "err2", "2026-09-02T00:02:00.000Z")
                    .unwrap(),
                "retry_waiting"
            );
            // 3回目 (max_attempts=3到達) → failed
            let _ = repos
                .claim_next_job("2026-09-02T00:03:00.000Z")
                .unwrap()
                .unwrap();
            assert_eq!(
                repos
                    .fail_job(id, "err3", "2026-09-02T00:04:00.000Z")
                    .unwrap(),
                "failed"
            );
            let j = repos.list_jobs(10).unwrap().remove(0);
            assert_eq!(j.status, "failed");
            assert!(j.finished_at.is_some());
            assert!(j.scheduled_at.is_none());
            assert_eq!(j.attempts, 3);
            // failedはclaimされない
            assert!(repos
                .claim_next_job("2026-09-02T09:00:00.000Z")
                .unwrap()
                .is_none());
        });
    }

    #[test]
    fn permanent_fail_terminates_immediately_regardless_of_attempts() {
        with_repos("permfail", |repos| {
            let job = enqueue(repos, 1).unwrap();
            let claimed = repos.claim_next_job(NOW).unwrap().unwrap();
            assert_eq!(claimed.attempts, 1);
            // 1回目の失敗でpermanent → retry_waitingを経由せず即failed
            assert_eq!(
                repos
                    .fail_job_permanent(job.id, "認証に失敗しました (401)")
                    .unwrap(),
                "failed"
            );
            let j = repos.list_jobs(10).unwrap().remove(0);
            assert_eq!(j.status, "failed");
            assert_eq!(j.attempts, 1, "attemptsはclaim分のみ");
            assert!(j.scheduled_at.is_none(), "backoff期限は設定されない");
            assert!(j.finished_at.is_some());
            assert_eq!(j.error.as_deref(), Some("認証に失敗しました (401)"));
            // failedはclaimされない (自動再試行なし)
            assert!(repos
                .claim_next_job("2026-09-02T09:00:00.000Z")
                .unwrap()
                .is_none());
            // running以外への適用は拒否
            assert!(matches!(
                repos.fail_job_permanent(job.id, "again"),
                Err(RepoError::Constraint(_))
            ));
        });
    }

    #[test]
    fn cancel_only_pending_and_retry_failed_resets() {
        with_repos("cancel", |repos| {
            let a = enqueue(repos, 1).unwrap();
            let _ = enqueue(repos, 2).unwrap();
            let cancelled = repos.cancel_job(a.id).unwrap();
            assert_eq!(cancelled.status, "cancelled");
            // cancelled済みの再キャンセルは拒否
            assert!(matches!(
                repos.cancel_job(a.id),
                Err(RepoError::Constraint(_))
            ));
            // runningのキャンセルは拒否
            let running = repos.claim_next_job(NOW).unwrap().unwrap();
            assert!(matches!(
                repos.cancel_job(running.id),
                Err(RepoError::Constraint(_))
            ));
            // 3試行すべて失敗 → failed確定
            repos
                .fail_job(running.id, "x", "2026-09-02T00:00:30.000Z")
                .unwrap();
            for minute in 1..=2 {
                let again = repos
                    .claim_next_job(&format!("2026-09-02T00:0{minute}:00.000Z"))
                    .unwrap()
                    .expect("backoff期限到着でclaim");
                assert_eq!(again.id, running.id);
                repos
                    .fail_job(running.id, "x", &format!("2026-09-02T00:0{minute}:30.000Z"))
                    .unwrap();
            }
            let j = repos.list_jobs(10).unwrap().remove(0);
            assert_eq!(j.status, "failed");
            let restored = repos.retry_failed_jobs(&[]).unwrap();
            assert_eq!(restored, 1);
            let j = repos.list_jobs(10).unwrap().remove(0);
            assert_eq!(j.status, "queued");
            assert_eq!(j.attempts, 0);
        });
    }

    #[test]
    fn recover_running_to_queued_on_startup() {
        with_repos("recover", |repos| {
            let first = enqueue(repos, 1).unwrap();
            let claimed = repos.claim_next_job(NOW).unwrap().unwrap(); // running
            assert_eq!(claimed.id, first.id);
            let _ = enqueue(repos, 2).unwrap(); // queued
            let recovered = repos.recover_running_jobs().unwrap();
            assert_eq!(recovered, 1);
            let jobs = repos.list_jobs(10).unwrap();
            let running_after: Vec<_> = jobs.iter().filter(|j| j.status == "running").collect();
            assert!(running_after.is_empty(), "runningは残らない");
            let queued: Vec<_> = jobs.iter().filter(|j| j.status == "queued").collect();
            assert_eq!(queued.len(), 2);
            // 回復されたジョブのclaimは試行回数を1から進める
            // (attempts据え置きのためclaimで +1 → 2)
            let claimed = repos.claim_next_job(NOW).unwrap().unwrap();
            assert!(claimed.attempts >= 1);
        });
    }

    #[test]
    fn status_counts_and_list_roundtrip() {
        with_repos("counts", |repos| {
            let a = enqueue(repos, 1).unwrap();
            let _ = repos.claim_next_job(NOW).unwrap().unwrap();
            repos.complete_job(a.id).unwrap();
            let _ = enqueue(repos, 2).unwrap();
            let counts = repos.job_status_counts().unwrap();
            assert!(counts.contains(&("queued".to_string(), 1)));
            assert!(counts.contains(&("succeeded".to_string(), 1)));
        });
    }

    #[test]
    fn cancel_jobs_for_ai_profile_targets_payload_only() {
        with_repos("cancelprofile", |repos| {
            let asset = asset_id(repos, "blake3:jp1");
            repos
                .enqueue_job(
                    JOB_TYPE_AI_CAPTION_TAGS,
                    "asset",
                    asset,
                    5,
                    "{\"profile_id\":7}",
                )
                .unwrap();
            let asset2 = asset_id(repos, "blake3:jp2");
            repos
                .enqueue_job(
                    JOB_TYPE_AI_CAPTION_TAGS,
                    "asset",
                    asset2,
                    5,
                    "{\"profile_id\":9}",
                )
                .unwrap();
            let cancelled = repos.cancel_jobs_for_ai_profile(7).unwrap();
            assert_eq!(cancelled, 1);
            let jobs = repos.list_jobs(10).unwrap();
            let j7 = jobs
                .iter()
                .find(|j| j.payload.contains("\"profile_id\":7"))
                .unwrap();
            assert_eq!(j7.status, "cancelled");
            let j9 = jobs
                .iter()
                .find(|j| j.payload.contains("\"profile_id\":9"))
                .unwrap();
            assert_eq!(j9.status, "queued");
        });
    }
}
