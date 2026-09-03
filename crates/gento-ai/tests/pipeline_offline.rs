//! testserver非依存のAIパイプラインテスト (FG-WP16-3 N5)。
//!
//! WP-0規約で `gento_ai::testserver` (mock) は `debug_assertions ||
//! feature = "devtools"` でのみコンパイルされる。mockを必要としない
//! テスト (endpoint停止時の分類・DB並行読取など) はこの対象ファイルに
//! 置き、**release `--all-targets` でも実行される** (mock依存の
//! `pipeline_e2e.rs` だけがゲートを維持する)。

use std::path::PathBuf;
use std::sync::Arc;

use gento_ai::pipeline::{run_caption_tags_job, AiRunOutcome, PipelineOptions};
use gento_ai::prompt::ProfileExtra;
use gento_core::assetmeta::{AssetMetadataParsed, AssetMetadataRepo, MetadataColumns};
use gento_core::repo::{
    AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, RootSettings, ScanState,
};
use gento_db::{AiProfileRecord, Db, JobRecord, NewAiProfile, JOB_TYPE_AI_CAPTION_TAGS};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gento-ai-offline-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
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

/// テスト用の小さな実PNG。
fn test_png(path: &std::path::Path) {
    use image::{Rgb, RgbImage};
    let img = RgbImage::from_fn(300, 200, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 0x40])
    });
    img.write_with_encoder(image::codecs::png::PngEncoder::new(
        std::fs::File::create(path).unwrap(),
    ))
    .unwrap();
}

/// DB + ルート + asset + 代表ファイルを用意し、(db, root_id, asset_id) を返す。
/// (pipeline_e2e.rs の fixture と同一構成 — testserverへの接続は行わない)
fn fixture(
    tag: &str,
    ai_enabled: bool,
    send_exif: bool,
    send_gps: bool,
) -> (TempDir, Arc<Db>, i64, i64) {
    let tmp = TempDir::new(tag);
    let root_dir = tmp.0.join("photos");
    std::fs::create_dir_all(&root_dir).unwrap();
    test_png(&root_dir.join("a.png"));
    let db = Arc::new(Db::open(tmp.0.join("db.sqlite3")).unwrap());
    let (root_id, asset_id) = db
        .with_writer(|repos| {
            let root = repos.add_root(&root_dir)?;
            repos.set_root_settings(
                root.id,
                RootSettings {
                    watch_enabled: true,
                    ai_enabled,
                    send_exif,
                    send_gps,
                },
            )?;
            let file = repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("a.png"),
                display_name: "a.png".to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })?;
            let asset = repos.create_asset(NewAsset {
                content_hash: format!("blake3:{tag}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: Some("image/png".to_string()),
                width: Some(300),
                height: Some(200),
                has_alpha: Some(false),
                is_animated: Some(false),
                frame_count: None,
            })?;
            repos.link_file(file.id, asset.id)?;
            repos.save_asset_metadata(
                asset.id,
                &[AssetMetadataParsed {
                    parser_name: "exif".to_string(),
                    parser_version: "1".to_string(),
                    columns: MetadataColumns {
                        taken_at: Some("2026-08-01T10:00:00Z".to_string()),
                        camera_make: Some("TestCam".to_string()),
                        camera_model: Some("X-100".to_string()),
                        lens: Some("50mm".to_string()),
                        gps_latitude: Some(35.681236),
                        gps_longitude: Some(139.767125),
                        ..MetadataColumns::default()
                    },
                    raw_exif: None,
                    raw_iptc: None,
                    raw_png_metadata: None,
                    raw_workflow: None,
                }],
            )?;
            Ok((root.id, asset.id))
        })
        .unwrap();
    (tmp, db, root_id, asset_id)
}

fn profile_for(db: &Db, base_url: &str, extra: ProfileExtra) -> AiProfileRecord {
    db.with_writer(|repos| {
        repos.insert_ai_profile(&NewAiProfile {
            display_name: "unreachable".to_string(),
            base_url: base_url.to_string(),
            model: "mock-vision-model".to_string(),
            vision_capable: true,
            enabled: true,
            timeout_ms: 10_000,
            concurrency: 2,
            extra_json: Some(extra.to_json_string()),
        })
    })
    .unwrap()
}

fn enqueue(db: &Db, asset_id: i64, profile_id: i64) -> JobRecord {
    db.with_writer(|repos| {
        repos.enqueue_job(
            JOB_TYPE_AI_CAPTION_TAGS,
            "asset",
            asset_id,
            5,
            &format!("{{\"profile_id\":{profile_id},\"asset_id\":{asset_id}}}"),
        )
    })
    .unwrap()
    .expect("enqueue")
}

// ===== 接続失敗の分離 (endpoint停止 → 再試行可能。testserver不要) =====

#[test]
fn e2e_connection_failure_is_retryable_and_local_reads_unaffected() {
    // 使用しないportを開けて即閉じる (接続拒否を安定させる)
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let base_url = format!("http://127.0.0.1:{dead_port}/v1");
    let (_tmp, db, root, asset_id) = fixture("down", false, false, false);
    let profile = profile_for(&db, &base_url, ProfileExtra::default());
    let job = enqueue(&db, asset_id, profile.id);
    let claimed = db
        .with_writer(|repos| repos.claim_job_by_id(job.id, &gento_db::rfc3339_after_now(0)))
        .unwrap()
        .expect("claim");

    let outcome = run_caption_tags_job(&db, &claimed, &profile, None, &PipelineOptions::default());
    match &outcome {
        AiRunOutcome::Failed {
            retryable, error, ..
        } => {
            assert!(retryable, "接続失敗は再試行可能: {error}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    // ai_runsに失敗が記録される
    let runs = db
        .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 10))
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "failed");
    assert!(runs[0].error.is_some());
    // ファイラー機能 (DB読み取り) は阻害されない — 同一DBへの並行読み書き
    let count = db
        .with_reader(|repos| {
            Ok(repos
                .conn()
                .query_row(
                    "SELECT count(*) FROM files WHERE root_id = ?1",
                    [root],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap())
        })
        .unwrap();
    assert_eq!(count, 1);
}
