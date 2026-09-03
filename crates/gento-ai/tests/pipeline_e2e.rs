//! WP-10 パイプラインE2E (mock OpenAI互換endpoint相手)。
//!
//! 証明対象:
//! - mock (llama.cpp互換の `/v1/models` + `/v1/chat/completions`) へ接続し
//!   タグ/キャプションが適用される (acceptance 1の自動証明分)
//! - 手動編集保護 (acceptance 6)
//! - EXIF/GPS送信設定がプロンプトへ反映される (acceptance 5)
//! - APIキーがDBへ平文保存されない (acceptance 4のDB側)
//! - Bearer認証: 鍵がAuthorizationヘッダで送られる (mockが検証)
//! - 接続失敗は再試行可能エラーとして扱われる
//!
//! 依存するtestserver (mock) はWP-0規約でコンパイル隔離された開発専用
//! 機能のため、本ファイル全体も同ゲートで囲む (通常releaseビルドでは空)。

#![cfg(any(debug_assertions, feature = "devtools"))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gento_ai::apikey::{ApiKey, SecretStore};
use gento_ai::client::{
    test_connection, test_structured, test_text, test_vision, AiClient, AiEndpoint, AiError,
};
use gento_ai::pipeline::{run_caption_tags_job, AiRunOutcome, PipelineOptions};
use gento_ai::prompt::ProfileExtra;
use gento_ai::testserver::{MockBehavior, MockServer};
use gento_core::assetmeta::{AssetMetadataParsed, AssetMetadataRepo, MetadataColumns};
use gento_core::metadata::{AssetTagRepo, CaptionKind, CaptionRepo, CaptionSource};
use gento_core::repo::{
    AssetRepo, FileRepo, FileUpsert, LibraryRootRepo, NewAsset, RootSettings, ScanState,
};
use gento_db::{AiProfileRecord, Db, JobRecord, NewAiProfile, JOB_TYPE_AI_CAPTION_TAGS};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gento-ai-e2e-{tag}-{}-{}",
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

/// テスト用の小さな実PNG (300x200 — 縮小されない寸法)。
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
            // EXIF/GPSメタデータ行 (photo parser相当)
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
            display_name: "mock".to_string(),
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

fn job_of(db: &Db, job_id: i64) -> JobRecord {
    db.with_reader(|repos| {
        Ok(repos
            .list_jobs(50)?
            .into_iter()
            .find(|j| j.id == job_id)
            .expect("job"))
    })
    .unwrap()
}

/// ワーカーと同じ遷移で1ジョブを確定まで進める (claim→実行→完了/失敗)。
/// 失敗はretryable分類に従い、worker (tauri ai.rs) と同じ遷移を辿る:
/// permanent → 即failed / retryable → backoff付きretry_waiting。
fn drive(db: &Db, job: &JobRecord, profile: &AiProfileRecord) -> AiRunOutcome {
    let claimed = db
        .with_writer(|repos| repos.claim_job_by_id(job.id, &gento_db::rfc3339_after_now(0)))
        .unwrap()
        .expect("claim");
    let outcome = run_caption_tags_job(db, &claimed, profile, None, &PipelineOptions::default());
    match &outcome {
        AiRunOutcome::Succeeded { .. } => {
            db.with_writer(|repos| repos.complete_job(claimed.id))
                .unwrap();
        }
        AiRunOutcome::Failed {
            error, retryable, ..
        } => {
            if *retryable {
                let retry_at = gento_db::rfc3339_after_now(30_000);
                let _ = db.with_writer(|repos| repos.fail_job(claimed.id, error, &retry_at));
            } else {
                let _ = db.with_writer(|repos| repos.fail_job_permanent(claimed.id, error));
            }
        }
    }
    outcome
}

// ===== acceptance 1 (mock自動証明分): 接続してタグ/キャプション適用 =====

#[test]
fn e2e_pipeline_applies_tags_and_caption_from_mock() {
    let server = MockServer::start(MockBehavior::default());
    let (_tmp, db, _root, asset_id) = fixture("apply", false, false, false);
    let profile = profile_for(&db, &server.base_url, ProfileExtra::default());
    let job = enqueue(&db, asset_id, profile.id);

    let outcome = drive(&db, &job, &profile);
    match &outcome {
        AiRunOutcome::Succeeded {
            result,
            applied_tags,
            ..
        } => {
            assert!(result.tags.contains(&"青空".to_string()), "{result:?}");
            assert!(result.caption.is_some());
            assert!(*applied_tags >= 1);
        }
        other => panic!("expected success, got {other:?}"),
    }

    // 適用結果: source=ai・ai_run_id付き
    let (tags, caption) = db
        .with_reader(|repos| {
            let tags = repos.list_asset_tags(asset_id)?;
            let caption = repos.get_current_caption(asset_id, CaptionKind::Description)?;
            Ok((tags, caption))
        })
        .unwrap();
    assert!(tags.iter().any(|t| t.source.as_str() == "ai"));
    let caption = caption.expect("current caption");
    assert_eq!(caption.source, CaptionSource::Ai);
    assert!(db
        .with_reader(|repos| {
            Ok(repos
                .conn()
                .query_row(
                    "SELECT ai_run_id FROM captions WHERE id = ?1",
                    [caption.id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .unwrap()
                .is_some())
        })
        .unwrap());

    // ジョブ・ai_runsの状態
    let job_after = job_of(&db, job.id);
    assert_eq!(job_after.status, "succeeded");
    let runs = db
        .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 10))
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
    assert_eq!(runs[0].prompt_version, gento_ai::PROMPT_VERSION);
    assert!(runs[0].raw_response.is_some());

    // 4種テスト機能も通る (接続/テキスト/Vision/構造化JSON)
    let client = AiClient::new(
        AiEndpoint {
            base_url: server.base_url.clone(),
            model: "mock-vision-model".to_string(),
            timeout: Duration::from_secs(5),
        },
        None,
        false,
    );
    assert!(test_connection(&client).ok, "接続");
    assert!(test_text(&client).ok, "テキスト");
    assert!(test_vision(&client).ok, "Vision");
    assert!(test_structured(&client).ok, "構造化JSON");
    assert_eq!(
        server
            .log
            .models_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

// ===== acceptance 6: 手動編集保護 =====

#[test]
fn e2e_manual_edits_are_protected_on_regeneration() {
    let server = MockServer::start(MockBehavior {
        chat_content: r#"{"tags":["空","雲"],"caption":"AI説明v1"}"#.to_string(),
        ..MockBehavior::default()
    });
    let (_tmp, db, _root, asset_id) = fixture("protect", false, false, false);
    let profile = profile_for(&db, &server.base_url, ProfileExtra::default());

    // 1回目: AI適用
    let job = enqueue(&db, asset_id, profile.id);
    let outcome = drive(&db, &job, &profile);
    assert!(matches!(outcome, AiRunOutcome::Succeeded { .. }));

    // 手動タグ「手動タグ」+ 手動キャプション
    db.with_writer(|repos| repos.add_tags_batch(&[asset_id], &["手動タグ"]))
        .unwrap();
    db.with_writer(|repos| {
        repos.set_caption(asset_id, CaptionKind::Description, "手動で直した説明")
    })
    .unwrap();

    // 2回目 (再生成・force): AIは別タグを返す
    let server2 = MockServer::start(MockBehavior {
        chat_content: r#"{"tags":["空","新しいタグ","手動タグ"],"caption":"AI説明v2"}"#.to_string(),
        ..MockBehavior::default()
    });
    let profile2 = profile_for(&db, &server2.base_url, ProfileExtra::default());
    let job2 = enqueue(&db, asset_id, profile2.id);
    let outcome = drive(&db, &job2, &profile2);
    assert!(matches!(outcome, AiRunOutcome::Succeeded { .. }));

    // 手動キャプションが現行のまま
    let current = db
        .with_reader(|repos| repos.get_current_caption(asset_id, CaptionKind::Description))
        .unwrap()
        .expect("current");
    assert_eq!(current.body, "手動で直した説明");
    assert_eq!(current.source, CaptionSource::Manual);
    assert!(current.manually_edited);
    // AI v2は履歴に非現行で存在
    let history = db
        .with_reader(|repos| repos.list_caption_history(asset_id, CaptionKind::Description, 10))
        .unwrap();
    let v2 = history
        .iter()
        .find(|c| c.body == "AI説明v2")
        .expect("v2 in history");
    assert!(!v2.is_current);

    // 手動タグ行は変化しない (source=manual・manually_edited=1のまま)
    let tags = db
        .with_reader(|repos| repos.list_asset_tags(asset_id))
        .unwrap();
    let manual_tag = tags
        .iter()
        .find(|t| t.tag.display_name == "手動タグ")
        .expect("manual tag");
    assert_eq!(manual_tag.source.as_str(), "manual");
    assert!(manual_tag.manually_edited);
    // 新しいAIタグは追加されている
    assert!(tags.iter().any(|t| t.tag.display_name == "新しいタグ"));
    // 1回目のAIタグも残る
    assert!(tags.iter().any(|t| t.tag.display_name == "雲"));
}

// ===== acceptance 5: EXIF/GPS送信制御 (リクエスト本文の実検証) =====

#[test]
fn e2e_privacy_gates_exif_and_gps_in_request_body() {
    let server = MockServer::start(MockBehavior::default());
    // ルート設定: exif=ON, gps=OFF
    let (_tmp, db, _root, asset_id) = fixture("privacy", false, true, false);
    // プロファイル設定: exif=ON, gps=ON → 実効: exif=ON, gps=OFF
    let profile = profile_for(
        &db,
        &server.base_url,
        ProfileExtra {
            send_exif: true,
            send_gps: true,
            ..ProfileExtra::default()
        },
    );
    let job = enqueue(&db, asset_id, profile.id);
    let outcome = drive(&db, &job, &profile);
    assert!(matches!(outcome, AiRunOutcome::Succeeded { .. }));

    let body = server.log.last_body.lock().unwrap().clone();
    // EXIF (カメラ・レンズ) は含まれる
    assert!(body.contains("TestCam"), "EXIFは送信される: {body}");
    assert!(body.contains("50mm"));
    // GPS座標は含まれない (ルートがgps=OFF)
    assert!(!body.contains("35.68"), "GPS座標は送信されない: {body}");
    assert!(!body.contains("139.76"));

    // ルートgps=ONにすると座標が含まれる (両方ON)
    let server2 = MockServer::start(MockBehavior::default());
    let (_tmp2, db2, _r2, asset2) = fixture("privacy2", false, true, true);
    let profile2 = profile_for(
        &db2,
        &server2.base_url,
        ProfileExtra {
            send_exif: true,
            send_gps: true,
            ..ProfileExtra::default()
        },
    );
    let job2 = enqueue(&db2, asset2, profile2.id);
    let outcome2 = drive(&db2, &job2, &profile2);
    assert!(matches!(outcome2, AiRunOutcome::Succeeded { .. }));
    let body2 = server2.log.last_body.lock().unwrap().clone();
    assert!(
        body2.contains("35.681236"),
        "両方ONならGPS座標が含まれる: {body2}"
    );

    // プロファイルexif=OFFならEXIFは送られない (AND)。GPSはEXIFとは
    // 独立したopt-inのため、この構成 (ルートgps=ON×プロファイルgps=ON)
    // では座標のみ含まれる (FG-WP10: GPS独立性)。
    let server3 = MockServer::start(MockBehavior::default());
    let (_tmp3, db3, _r3, asset3) = fixture("privacy3", false, true, true);
    let profile3 = profile_for(
        &db3,
        &server3.base_url,
        ProfileExtra {
            send_exif: false,
            send_gps: true,
            ..ProfileExtra::default()
        },
    );
    let job3 = enqueue(&db3, asset3, profile3.id);
    let outcome3 = drive(&db3, &job3, &profile3);
    assert!(matches!(outcome3, AiRunOutcome::Succeeded { .. }));
    let body3 = server3.log.last_body.lock().unwrap().clone();
    assert!(
        !body3.contains("TestCam"),
        "プロファイル拒否でEXIF無し: {body3}"
    );
    assert!(
        !body3.contains("50mm") && !body3.contains("2026-08-01"),
        "EXIF撮影情報は含まれない: {body3}"
    );
    assert!(
        body3.contains("35.681236"),
        "EXIFがOFFでもGPS許可なら座標は含まれる: {body3}"
    );
}

// ===== acceptance 4: APIキーはDBに平文保存されない + Bearer検証 =====

#[test]
fn e2e_api_key_sent_as_bearer_and_never_in_db() {
    let secret_value = "sk-wp10-e2e-secret-12345".to_string();
    let server = MockServer::start(MockBehavior {
        expected_bearer: Some(secret_value.clone()),
        ..MockBehavior::default()
    });
    let tmp = TempDir::new("bearer");
    let db = Arc::new(Db::open(tmp.0.join("db.sqlite3")).unwrap());
    // 鍵を保存 (ヘッドレスならfileフォールバック。keyring環境でも参照形式は同じ)
    let secrets = SecretStore::new(&tmp.0);
    let stored = secrets
        .save(
            "01900000-0000-7000-8000-000000000001",
            &ApiKey::new(secret_value.clone()),
        )
        .unwrap();
    let _profile = db
        .with_writer(|repos| {
            let profile = repos.insert_ai_profile(&NewAiProfile {
                display_name: "auth-mock".to_string(),
                base_url: server.base_url.clone(),
                model: "mock-vision-model".to_string(),
                vision_capable: true,
                enabled: true,
                timeout_ms: 10_000,
                concurrency: 2,
                extra_json: None,
            })?;
            repos.set_ai_profile_api_key_ref(profile.id, Some(&stored.as_ref_string()))?;
            Ok(profile)
        })
        .unwrap();

    // テスト機能: 正しい鍵で全通過 (mockがBearer検証)
    let client = AiClient::new(
        AiEndpoint {
            base_url: server.base_url.clone(),
            model: "mock-vision-model".to_string(),
            timeout: Duration::from_secs(5),
        },
        secrets.load(&stored.as_ref_string()),
        false,
    );
    assert!(test_connection(&client).ok, "Bearer検証を通る接続");
    assert!(test_text(&client).ok);
    assert_eq!(
        server
            .log
            .unauthorized
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    // 誤った鍵では401 (鍵がヘッダで送られることの裏付け)
    let server_bad = MockServer::start(MockBehavior {
        expected_bearer: Some("sk-different".to_string()),
        ..MockBehavior::default()
    });
    let bad_client = AiClient::new(
        AiEndpoint {
            base_url: server_bad.base_url.clone(),
            model: "m".to_string(),
            timeout: Duration::from_secs(5),
        },
        Some(ApiKey::new("sk-wrong".to_string())),
        false,
    );
    let report = test_connection(&bad_client);
    assert!(!report.ok);
    assert_eq!(report.status, Some(401));
    assert_eq!(
        server_bad
            .log
            .unauthorized
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // DB全文検索: 鍵文字列がどこにも存在しない (WAL含む全ファイルのバイト列)
    let db_path = tmp.0.join("db.sqlite3");
    let mut found = false;
    for entry in std::fs::read_dir(&tmp.0).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            if bytes
                .windows(secret_value.len())
                .any(|w| w == secret_value.as_bytes())
            {
                found = true;
            }
        }
    }
    assert!(!found, "DB/WALファイルに鍵平文が存在します: {db_path:?}");

    // ai_profiles.api_key_ref は参照形式 (keyring:/file:)
    let ref_value = db
        .with_reader(|repos| {
            Ok(repos
                .list_ai_profiles()?
                .first()
                .unwrap()
                .api_key_ref
                .clone()
                .unwrap())
        })
        .unwrap();
    assert!(
        ref_value.starts_with("keyring:") || ref_value.starts_with("file:"),
        "参照形式: {ref_value}"
    );
    assert!(!ref_value.contains(&secret_value));
}

// ===== 接続失敗の分離 (endpoint停止 → 再試行可能) =====

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
        other => panic!("expected failure: {other:?}"),
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

// ===== acceptance: 送信前の最大辺 =====

#[test]
fn e2e_large_image_is_downscaled_before_send() {
    let server = MockServer::start(MockBehavior::default());
    let tmp = TempDir::new("downscale");
    // 2200x1200のPNG (長辺2200 > 1024)
    use image::{Rgb, RgbImage};
    let img = RgbImage::from_fn(2200, 1200, |x, _| Rgb([(x % 256) as u8, 0x80, 0x40]));
    img.write_with_encoder(image::codecs::png::PngEncoder::new(
        std::fs::File::create(tmp.0.join("big.png")).unwrap(),
    ))
    .unwrap();
    let db = Arc::new(Db::open(tmp.0.join("db.sqlite3")).unwrap());
    let asset_id = db
        .with_writer(|repos| {
            let root = repos.add_root(&tmp.0)?;
            let file = repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("big.png"),
                display_name: "big.png".to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })?;
            let asset = repos.create_asset(NewAsset {
                content_hash: "blake3:big".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })?;
            repos.link_file(file.id, asset.id)?;
            Ok(asset.id)
        })
        .unwrap();
    let profile = profile_for(&db, &server.base_url, ProfileExtra::default());
    let job = enqueue(&db, asset_id, profile.id);
    let outcome = drive(&db, &job, &profile);
    assert!(matches!(outcome, AiRunOutcome::Succeeded { .. }));
    // リクエスト本文のdata URLが最大辺1024の再エンコードであることを
    // サーバー記録から検証: 実寸は応答しないので、ai_runsの概要で検証
    let runs = db
        .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 5))
        .unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(&runs[0].request_summary.clone().unwrap()).unwrap();
    assert_eq!(summary["image_width"], 1024, "長辺1024へ縮小: {summary}");
    let height = summary["image_height"].as_i64().unwrap();
    assert!((555..=565).contains(&height), "アスペクト保持: {summary}");
}

// ===== FG-WP10 blocker 1: プロファイル保存値 (8192等) も実行時に強制 =====

#[test]
fn e2e_saved_out_of_range_edge_is_enforced_at_execution() {
    let server = MockServer::start(MockBehavior::default());
    let tmp = TempDir::new("edge8192");
    use image::{Rgb, RgbImage};
    // 4000x2000 — 旧「0=縮小なし」や8192設定では原寸/4000pxが出る寸法
    let img = RgbImage::from_fn(4000, 2000, |x, _| Rgb([(x % 256) as u8, 0x80, 0x40]));
    img.write_with_encoder(image::codecs::png::PngEncoder::new(
        std::fs::File::create(tmp.0.join("huge.png")).unwrap(),
    ))
    .unwrap();
    let db = Arc::new(Db::open(tmp.0.join("db.sqlite3")).unwrap());
    let asset_id = db
        .with_writer(|repos| {
            let root = repos.add_root(&tmp.0)?;
            let file = repos.upsert_file(FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from("huge.png"),
                display_name: "huge.png".to_string(),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1_000,
                scan_state: ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })?;
            let asset = repos.create_asset(NewAsset {
                content_hash: "blake3:huge".to_string(),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })?;
            repos.link_file(file.id, asset.id)?;
            Ok(asset.id)
        })
        .unwrap();
    // 保存済みextraに8192 (範囲外) を直接埋め込む (IPCでは拒否される値の
    // 旧プロファイル残留を模倣)
    let profile = profile_for(
        &db,
        &server.base_url,
        ProfileExtra {
            max_image_edge: 8192,
            ..ProfileExtra::default()
        },
    );
    let job = enqueue(&db, asset_id, profile.id);
    let outcome = drive(&db, &job, &profile);
    assert!(matches!(outcome, AiRunOutcome::Succeeded { .. }));
    let runs = db
        .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 5))
        .unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(&runs[0].request_summary.clone().unwrap()).unwrap();
    assert_eq!(
        summary["image_width"], 2048,
        "8192設定でも送信は最大辺2048へ強制: {summary}"
    );
    assert_eq!(summary["image_height"], 1024);
}

// ===== FG-WP10 blocker 4: HTTP応答の境界とリダイレクト =====

fn plain_client(base_url: &str) -> AiClient {
    AiClient::new(
        AiEndpoint {
            base_url: base_url.to_string(),
            model: "mock-vision-model".to_string(),
            timeout: Duration::from_secs(10),
        },
        None,
        false,
    )
}

#[test]
fn client_rejects_huge_response_with_content_length() {
    let server = MockServer::start(MockBehavior {
        huge_body: Some(gento_ai::testserver::HugeBodyMode::ContentLength),
        ..MockBehavior::default()
    });
    let client = plain_client(&server.base_url);
    let started = std::time::Instant::now();
    let err = client
        .chat(&[gento_ai::client::text_message("hi")], None)
        .unwrap_err();
    assert!(
        matches!(err, AiError::Oversized { .. }),
        "巨大応答はエラー: {err}"
    );
    assert!(!err.is_retryable(), "応答過大は非リトライ: {err}");
    // Content-Length検査で即座に拒否 (3MiB読み切らない)
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "即時拒否: {:?}",
        started.elapsed()
    );
    // エラーで完了 (タイムアウト・パニックせず)。巨大応答はchat経路のみで
    // models (通常サイズ) は引き続き正常 — 障害の隔離
    let report = test_connection(&client);
    assert!(report.ok, "models応答は通常どおり: {report:?}");
}

#[test]
fn client_rejects_huge_chunked_response_via_streaming_cap() {
    let server = MockServer::start(MockBehavior {
        huge_body: Some(gento_ai::testserver::HugeBodyMode::Chunked),
        ..MockBehavior::default()
    });
    let client = plain_client(&server.base_url);
    // Content-Lengthが無いためストリーミング読み取り上限で抑える
    let err = client
        .chat(&[gento_ai::client::text_message("hi")], None)
        .unwrap_err();
    assert!(
        matches!(err, AiError::Oversized { .. }),
        "chunked巨大応答もエラー: {err}"
    );
    assert!(!err.is_retryable());
}

#[test]
fn client_does_not_follow_redirects() {
    // リダイレクト先 (本来の応答を返す正常なmock)
    let target = MockServer::start(MockBehavior::default());
    let redirector = MockServer::start(MockBehavior {
        redirect_to: Some(format!("{}/chat/completions", target.base_url)),
        ..MockBehavior::default()
    });
    let client = plain_client(&redirector.base_url);
    let err = client
        .chat(&[gento_ai::client::text_message("hi")], None)
        .unwrap_err();
    match &err {
        AiError::Api { status, .. } => assert_eq!(*status, 302, "3xxはそのまま返る: {err}"),
        other => panic!("expected Api(302), got {other:?}"),
    }
    assert!(!err.is_retryable());
    // リダイレクト先へは転送されない (リクエストが届かない)
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(target.chat_calls(), 0, "転送先へのリクエスト無し");
    assert_eq!(redirector.chat_calls(), 1);
}

// ===== FG-WP10 blocker 2 (pipeline層): 4xxは非リトライ・5xxはリトライ =====

#[test]
fn e2e_http_401_is_permanent_and_500_is_retryable() {
    // 401 (認証失敗): 画像を再送しても同じ結果 — permanent
    let server_401 = MockServer::start(MockBehavior {
        chat_status: 401,
        ..MockBehavior::default()
    });
    let (_tmp, db, _root, asset_id) = fixture("auth401", false, false, false);
    let profile = profile_for(&db, &server_401.base_url, ProfileExtra::default());
    let job = enqueue(&db, asset_id, profile.id);
    let outcome = drive(&db, &job, &profile);
    match &outcome {
        AiRunOutcome::Failed {
            retryable, error, ..
        } => {
            assert!(!retryable, "401は非リトライ: {error}");
            assert!(error.contains("401"), "エラーにstatus: {error}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    assert_eq!(job_of(&db, job.id).status, "failed", "即時failed確定");
    assert_eq!(server_401.chat_calls(), 1, "1回のみのリクエスト");

    // 500 (サーバー側の一時障害): backoffで再試行し得る — retry_waiting
    let server_500 = MockServer::start(MockBehavior {
        chat_status: 500,
        ..MockBehavior::default()
    });
    let (_tmp2, db2, _r2, asset2) = fixture("err500", false, false, false);
    let profile2 = profile_for(&db2, &server_500.base_url, ProfileExtra::default());
    let job2 = enqueue(&db2, asset2, profile2.id);
    let outcome2 = drive(&db2, &job2, &profile2);
    match &outcome2 {
        AiRunOutcome::Failed {
            retryable, error, ..
        } => {
            assert!(retryable, "500はリトライ可能: {error}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    let after = job_of(&db2, job2.id);
    assert_eq!(after.status, "retry_waiting", "backoff待機へ");
    assert!(after.scheduled_at.is_some(), "再試行期限あり");
}

// ===== FG-WP10 非ブロッカー9: 適用エラーはジョブ/ランを失敗に =====

#[test]
fn e2e_apply_failure_fails_run_and_job_not_succeeded() {
    let server = MockServer::start(MockBehavior::default());
    let (_tmp, db, _root, asset_id) = fixture("applyfail", false, false, false);
    let profile = profile_for(&db, &server.base_url, ProfileExtra::default());
    let job = enqueue(&db, asset_id, profile.id);

    // captions INSERTを必ず失敗させるtriggerを注入 (repo書き込み障害の模倣)
    db.with_writer(|repos| -> Result<(), gento_core::repo::RepoError> {
        repos
            .conn()
            .execute(
                "CREATE TRIGGER fail_caption_insert BEFORE INSERT ON captions \
                 BEGIN SELECT RAISE(ABORT, 'injected caption failure'); END",
                [],
            )
            .map_err(|e| gento_core::repo::RepoError::Db(e.to_string()))?;
        Ok(())
    })
    .unwrap();

    let outcome = drive(&db, &job, &profile);
    match &outcome {
        AiRunOutcome::Failed {
            error, retryable, ..
        } => {
            assert!(error.contains("キャプションの適用に失敗"), "{error}");
            assert!(!retryable);
        }
        other => panic!("expected failure, got {other:?}"),
    }
    let after = job_of(&db, job.id);
    assert_eq!(after.status, "failed", "ジョブはsucceededにならない");
    let runs = db
        .with_reader(|repos| repos.list_ai_runs_for_asset(asset_id, 5))
        .unwrap();
    assert_eq!(runs[0].status, "failed", "ai_runsも失敗確定");
}
