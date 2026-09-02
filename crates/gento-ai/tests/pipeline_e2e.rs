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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gento_ai::apikey::{ApiKey, SecretStore};
use gento_ai::client::{
    test_connection, test_structured, test_text, test_vision, AiClient, AiEndpoint,
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
        AiRunOutcome::Failed { error, .. } => {
            let retry_at = gento_db::rfc3339_after_now(0);
            let _ = db.with_writer(|repos| repos.fail_job(claimed.id, error, &retry_at));
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

    // プロファイルexif=OFFならEXIFも送られない (AND)
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
        !body3.contains("35.68"),
        "EXIFがOFFならGPS節ごと無い: {body3}"
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

// ===== 画像縮小 (acceptance: 送信前の最大辺) =====

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
