//! AI実行パイプライン (MVP WP-10)。
//!
//! 1ジョブ (ai_caption_tags) の実行を担う:
//!
//! ```text
//! ai_runs開始(running) → 代表ファイル解決 → 画像縮小 (最大辺設定) →
//! プロンプト構築 (EXIF/GPS送信設定のAND適用) → chat/completions →
//! 応答パース → タグ/キャプション適用 (手動編集保護) → ai_runs確定
//! ```
//!
//! - 各ステップの失敗は [`AiRunOutcome::Failed`] として返り、
//!   ai_runsへエラーを記録する (ジョブの再試行判定は呼び出し側)。
//! - APIキーは Authorization ヘッダへ出るのみ。DB・ログへ書く経路は
//!   持たない (`request_summary` には鍵を含めない)。

use std::time::Duration;

use gento_core::metadata::CaptionKind;
use gento_core::AssetMetadataRepo;
use gento_db::{AiProfileRecord, AssetSource, Db, JobRecord};

use crate::apikey::ApiKey;
use crate::client::{AiClient, AiEndpoint, AiError};
use crate::imaging::{encode_for_send, EncodedImage};
pub use crate::prompt::CaptionTagsResult;
use crate::prompt::{
    build_caption_tags_messages, effective_privacy, parse_caption_tags_response, AiMetadataHints,
    ProfileExtra, PROMPT_VERSION,
};

/// ジョブ実行の完結結果。
#[derive(Debug, Clone, PartialEq)]
pub enum AiRunOutcome {
    /// 成功。適用結果の内訳を返す。
    Succeeded {
        run_id: i64,
        result: CaptionTagsResult,
        /// 現行キャプションになったか (手動現行保持時はfalse)。
        caption_became_current: bool,
        /// 新規付与したタグ数 (既存同名スキップ後)。
        applied_tags: u64,
    },
    /// 失敗 (ai_runsへ記録済み)。再試行可否は呼び出し側で判定。
    Failed {
        run_id: i64,
        error: String,
        retryable: bool,
    },
}

/// 実行オプション (テストで差し替え可能な部分)。
#[derive(Debug, Clone, Default)]
pub struct PipelineOptions {
    /// 画像縮小の最大辺の上書き (None = プロファイル設定)。
    pub max_image_edge_override: Option<u32>,
}

/// asset_metadata行 (全parser) から送信ヒントを構築する
/// (列ごとに最初の非NULL値。gpsはsend_gps制御の対象)。
fn metadata_hints(db: &Db, asset_id: i64) -> AiMetadataHints {
    let records = db
        .with_reader(|repos| repos.list_asset_metadata(asset_id))
        .unwrap_or_default();
    let mut hints = AiMetadataHints::default();
    for record in &records {
        let columns = &record.parsed.columns;
        if hints.taken_at.is_none() {
            hints.taken_at = columns.taken_at.clone();
        }
        if hints.camera_make.is_none() {
            hints.camera_make = columns.camera_make.clone();
        }
        if hints.camera_model.is_none() {
            hints.camera_model = columns.camera_model.clone();
        }
        if hints.lens.is_none() {
            hints.lens = columns.lens.clone();
        }
        if hints.gps_latitude.is_none() {
            hints.gps_latitude = columns.gps_latitude;
        }
        if hints.gps_longitude.is_none() {
            hints.gps_longitude = columns.gps_longitude;
        }
    }
    hints
}

/// 1ジョブ分のcaption+tags生成を実行する。
///
/// `job` は処理中のジョブ。APIキーは呼び出し側 (worker) が
/// SecretStoreから解決したものを渡す (ここでは保存を扱わない)。
pub fn run_caption_tags_job(
    db: &Db,
    job: &JobRecord,
    profile: &AiProfileRecord,
    api_key: Option<ApiKey>,
    options: &PipelineOptions,
) -> AiRunOutcome {
    let asset_id = job.target_id;
    let extra = ProfileExtra::parse(profile.extra_json.as_deref());
    let privacy_input = {
        // ルート設定は代表ファイルの所属ルートから (asset単位で確定)。
        let source: Option<AssetSource> = db
            .with_reader(|repos| repos.representative_present_file(asset_id))
            .unwrap_or(None);
        source
    };
    let Some(source) = privacy_input else {
        return start_and_fail(
            db,
            asset_id,
            profile,
            "送信対象ファイルがありません (missing/unsupported)",
            false,
            &serde_json::json!({"model": profile.model}),
        );
    };
    let privacy = effective_privacy(source.root.send_exif, source.root.send_gps, &extra);

    // ① 画像読み→縮小→data URL
    let image_bytes = std::fs::read(source.absolute_path());
    let image: Option<EncodedImage> = match &image_bytes {
        Ok(bytes) => {
            let max_edge = options
                .max_image_edge_override
                .unwrap_or(extra.max_image_edge);
            match encode_for_send(bytes, max_edge) {
                Ok(encoded) => Some(encoded),
                Err(err) => {
                    return start_and_fail(
                        db,
                        asset_id,
                        profile,
                        &format!("画像の処理に失敗しました: {err}"),
                        false,
                        &serde_json::json!({"model": profile.model}),
                    );
                }
            }
        }
        Err(err) => {
            return start_and_fail(
                db,
                asset_id,
                profile,
                &format!("ファイル読み取りに失敗しました: {err}"),
                false,
                &serde_json::json!({"model": profile.model}),
            );
        }
    };
    let image = image.expect("Ok branch always Some");

    // ② ai_runs開始 + リクエスト概要 (鍵は含まない)
    let request_summary = serde_json::json!({
        "model": profile.model,
        "prompt_version": PROMPT_VERSION,
        "image_jpeg_bytes": image.jpeg_bytes,
        "image_width": image.width,
        "image_height": image.height,
        "send_exif": privacy.send_exif,
        "send_gps": privacy.send_gps,
        "endpoint_kind": if profile.api_key_ref.is_some() { "auth" } else { "local" },
    });
    let summary_str = request_summary.to_string();
    let run = db.with_writer(|repos| {
        repos.insert_ai_run(
            asset_id,
            Some(profile.id),
            "caption_and_tags",
            PROMPT_VERSION,
            Some(&summary_str),
        )
    });
    let run = match run {
        Ok(run) => run,
        Err(err) => {
            return AiRunOutcome::Failed {
                run_id: 0,
                error: format!("ai_runs記録失敗: {err}"),
                retryable: true,
            };
        }
    };

    // ③ プロンプト構築 + 呼び出し
    let hints = metadata_hints(db, asset_id);
    let messages = build_caption_tags_messages(&image.data_url, &hints, privacy);
    let client = AiClient::new(
        AiEndpoint {
            base_url: profile.base_url.clone(),
            model: profile.model.clone(),
            timeout: Duration::from_millis(profile.timeout_ms as u64),
        },
        api_key,
        extra.send_response_format,
    );
    match client.chat(&messages, None) {
        Ok(response) => {
            // ④ パース → 適用 (手動保護はrepo側で強制)
            match parse_caption_tags_response(&response.content) {
                Ok(result) => {
                    let applied_tags = db
                        .with_writer(|repos| repos.add_ai_tags(asset_id, &result.tags, run.id))
                        .unwrap_or(0);
                    let caption_became_current = result.caption.as_ref().is_some_and(|body| {
                        db.with_writer(|repos| {
                            repos.apply_ai_caption(asset_id, CaptionKind::Description, body, run.id)
                        })
                        .unwrap_or(false)
                    });
                    let _ = db.with_writer(|repos| {
                        repos.finish_ai_run(run.id, "succeeded", Some(&response.content), None)
                    });
                    AiRunOutcome::Succeeded {
                        run_id: run.id,
                        result,
                        caption_became_current,
                        applied_tags,
                    }
                }
                Err(err) => finish_failed(db, run.id, &err, false),
            }
        }
        Err(err) => finish_failed(db, run.id, &err.to_string(), err.is_retryable()),
    }
}

/// ai_runs開始前の失敗 (ファイル解決・画像処理エラー)。
fn start_and_fail(
    db: &Db,
    asset_id: i64,
    profile: &AiProfileRecord,
    error: &str,
    retryable: bool,
    summary: &serde_json::Value,
) -> AiRunOutcome {
    let run = db.with_writer(|repos| {
        repos.insert_ai_run(
            asset_id,
            Some(profile.id),
            "caption_and_tags",
            PROMPT_VERSION,
            Some(&summary.to_string()),
        )
    });
    match run {
        Ok(run) => finish_failed(db, run.id, error, retryable),
        Err(err) => AiRunOutcome::Failed {
            run_id: 0,
            error: format!("ai_runs記録失敗: {err}"),
            retryable,
        },
    }
}

fn finish_failed(db: &Db, run_id: i64, error: &str, retryable: bool) -> AiRunOutcome {
    let err = error.to_string();
    let _ = db.with_writer(|repos| repos.finish_ai_run(run_id, "failed", None, Some(&err)));
    AiRunOutcome::Failed {
        run_id,
        error: err,
        retryable,
    }
}

/// [`AiError`] から再試行可否を取り出す (worker向けの再エクスポート)。
pub fn error_is_retryable(error: &AiError) -> bool {
    error.is_retryable()
}
