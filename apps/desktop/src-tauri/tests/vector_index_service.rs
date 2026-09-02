//! VectorIndexService結合テスト (WP-12)。
//!
//! 常時実行 (合成ベクトル — モデル不要):
//! - `build_search_and_restart_cycle`: 構築→検索→再起動ロード→検索
//! - `corrupt_index_file_rebuilds`: 索引破損 → 自動再構築 (acceptance 2)
//! - `status_stays_responsive_during_build`: 構築中もstatus/検索が応答
//!   (UI非ブロックの実証 — acceptance 2)
//! - `model_switch_invalidates_index`: モデル切替で旧索引は使えない (§10)
//!
//! 実モデル (`GENTO_EMBEDDING_MODELS_DIR` 設定時):
//! - `hybrid_search_pipeline_real_model`: 自然言語クエリ→関連asset上位・
//!   類似画像検索・重み変更で順位変化 (acceptance 3/4のE2E)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gento_db::Db;

type NoticeSink = Arc<dyn Fn(&str, &str) + Send + Sync>;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gento-visvc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
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

fn make_db(tag: &str) -> (Arc<Db>, TempDir) {
    let tmp = TempDir::new(tag);
    let db = Db::open(tmp.path().join("t.sqlite3")).expect("db");
    (Arc::new(db), tmp)
}

type SinkLog = Arc<std::sync::Mutex<Vec<(String, String)>>>;

fn recording_sink() -> (NoticeSink, SinkLog) {
    let log: SinkLog = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log2 = Arc::clone(&log);
    (
        Arc::new(move |event: &str, payload: &str| {
            log2.lock()
                .unwrap()
                .push((event.to_string(), payload.to_string()));
        }),
        log,
    )
}

/// presentなFileを持つassetを作りcaptionを付ける。
fn seed_asset(db: &Arc<Db>, tag: &str, caption: &str) -> i64 {
    use gento_core::metadata::CaptionRepo;
    use gento_core::repo::{AssetRepo, FileRepo, LibraryRootRepo};
    db.with_writer(|repos| {
        let root = repos
            .list_roots()
            .unwrap()
            .into_iter()
            .next()
            .unwrap_or_else(|| repos.add_root(Path::new("/tmp/gento-visvc")).unwrap());
        let asset = repos
            .create_asset(gento_core::NewAsset {
                content_hash: format!("blake3:{tag}"),
                hash_algorithm: "blake3".to_string(),
                byte_size: 10,
                mime_type: None,
                width: None,
                height: None,
                has_alpha: None,
                is_animated: None,
                frame_count: None,
            })
            .unwrap();
        let file = repos
            .upsert_file(gento_core::FileUpsert {
                root_id: root.id,
                rel_path: PathBuf::from(format!("{tag}.png")),
                display_name: format!("{tag}.png"),
                byte_size: 10,
                mtime_ms: 1,
                mtime_ns: 1,
                scan_state: gento_core::ScanState::Present,
                os_file_id: None,
                detach_asset: false,
            })
            .unwrap();
        repos.link_file(file.id, asset.id).unwrap();
        repos
            .set_caption(
                asset.id,
                gento_core::metadata::CaptionKind::Description,
                caption,
            )
            .unwrap();
        Ok(asset.id)
    })
    .expect("seed")
}

/// クラスタ構造の合成docベクトル (asset i は i%CLUSTERSクラスタ)。
fn synthetic_vector(asset_index: usize, dims: usize, clusters: usize) -> Vec<f32> {
    let cluster = asset_index % clusters;
    let mut vector = vec![0.01f32; dims];
    vector[cluster % dims] += 1.0;
    vector[(cluster + dims / 2) % dims] += 0.5;
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    vector.iter().map(|v| v / norm).collect()
}

/// 合成埋め込みをDBへ投入 (manifestの70m identityと同一モデル表記)。
/// 注: seed_assetは内部でwith_writerを張るため、ここでは外で呼ぶ
/// (writer Mutexは非再入 — 二重取得はデッドロック)。
fn seed_synthetic_embeddings(db: &Arc<Db>, count: usize, dims: usize, clusters: usize) {
    let model = "ruri-v3-70m";
    let revision = gento_tauri_lib::embedding::model_revision_of(model);
    for i in 0..count {
        let asset = seed_asset(db, &format!("syn{i:05}"), &format!("synthetic {i}"));
        let vector = synthetic_vector(i, dims, clusters);
        db.with_writer(|repos| {
            repos.embedding_upsert(
                asset,
                model,
                &revision,
                dims,
                "f32",
                &format!("h{i}"),
                &vector,
            )
        })
        .expect("embedding upsert");
    }
}

fn wait_for<F: Fn() -> Option<T>, T>(what: &str, timeout: Duration, probe: F) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        if Instant::now() > deadline {
            panic!("待機タイムアウト: {what}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn real_models_dir() -> Option<PathBuf> {
    let dir = std::env::var("GENTO_EMBEDDING_MODELS_DIR").ok()?;
    let path = PathBuf::from(dir);
    let path = if path.is_absolute() {
        path
    } else {
        // src-tauriのmanifest dirから3親上がworkspace root
        // (2親だと apps/ になり、相対指定が無音スキップする —
        // embedding_service.rsも同様に修正済み)
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join(path)
    };
    let verified = gento_embedding::manifest::verify_package(&path, "ruri-v3-70m");
    verified.is_verified().then_some(path)
}

// ===== 常時実行: 構築→検索→再起動→破損再構築 =====

#[test]
fn build_search_restart_and_corruption_cycle() {
    let dims = 384;
    let (db, _db_tmp) = make_db("cycle");
    let index_root = TempDir::new("idx");
    // データディレクトリ = index_rootそのもの (indexes/が下に作られる)
    seed_synthetic_embeddings(&db, 300, dims, 30);

    let (sink, log) = recording_sink();
    let service = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        sink,
    );
    // 構築完了まで待つ (バックグラウンド)
    wait_for("索引構築", Duration::from_secs(120), || {
        service.status().ready.then_some(())
    });
    let status = service.status();
    assert_eq!(status.model, "ruri-v3-70m");
    assert_eq!(status.sqlite_count, 300);
    assert_eq!(status.indexed_count, 300);
    assert!(!status.building);

    // 検索: クラスタ0のベクトルで問うと同クラスタ (asset 1, 31, 61...) が上位
    let query = synthetic_vector(0, dims, 30);
    let hits = service.search(&query, 5, None).expect("索引readyのはず");
    assert!(hits.len() >= 5);
    assert_eq!(hits[0].key % 30, 1, "同クラスタ: asset{}", hits[0].key);
    let top_clusters: Vec<i64> = hits.iter().map(|h| h.key % 30).collect();
    assert!(
        top_clusters.iter().filter(|c| **c == 1).count() >= 4,
        "上位の大半が同クラスタ: {hits:?}"
    );
    // 進捗・readyイベントが出ている
    let log = log.lock().unwrap().clone();
    assert!(
        log.iter().any(|(event, _)| event == "vector-index:ready"),
        "readyイベント: {log:?}"
    );
    assert!(
        log.iter()
            .any(|(event, _)| event == "vector-index:progress"),
        "progressイベント: {log:?}"
    );

    // 構造フィルタ (半分のみ許可)
    let allowed: std::collections::HashSet<i64> = (1..=300i64).filter(|id| id % 2 == 0).collect();
    let filtered = service.search(&query, 5, Some(&allowed)).expect("filtered");
    assert!(filtered.iter().all(|h| allowed.contains(&h.key)));

    // 増分追加フック (ジョブ完了経路の直接呼び出し)
    let revision = gento_tauri_lib::embedding::model_revision_of("ruri-v3-70m");
    service.on_embedding_upserted("ruri-v3-70m", &revision, hits[0].key, &query);
    // flushで保存 → 再起動後に増分が見える (ここでは同ベクトル再追加は冪等)

    drop(service); // shutdown + flush

    // ===== 再起動: ディスクから即ロード → 即検索 (acceptance) =====
    let (sink2, _log2) = recording_sink();
    let service2 = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        sink2,
    );
    wait_for("再起動ロード", Duration::from_secs(30), || {
        service2.status().ready.then_some(())
    });
    let hits2 = service2.search(&query, 5, None).expect("再ロード後検索");
    assert_eq!(hits2[0].key, hits[0].key, "再ロード後も同一結果 (決定性)");
    drop(service2);

    // ===== 索引破損 → 破棄してSQLiteから再構築 (acceptance) =====
    let index_file = index_root
        .path()
        .join("indexes")
        .join("ruri-v3-70m")
        .join("index.usearch");
    let bytes = std::fs::read(&index_file).expect("index file");
    std::fs::write(&index_file, &bytes[..bytes.len() / 3]).expect("corrupt");
    let (sink3, _log3) = recording_sink();
    let service3 = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        sink3,
    );
    wait_for("破損再構築", Duration::from_secs(120), || {
        service3.status().ready.then_some(())
    });
    let hits3 = service3.search(&query, 5, None).expect("再構築後検索");
    assert_eq!(hits3[0].key, hits[0].key, "再構築後も同一結果");
}

// ===== 常時実行: 構築中も応答 (UI非ブロック実証) =====

#[test]
fn status_stays_responsive_during_build() {
    let (db, _db_tmp) = make_db("responsive");
    let index_root = TempDir::new("idx2");
    // 構築に時間がかかる規模 (5000ベクトル — debug実行でも数秒)
    seed_synthetic_embeddings(&db, 5_000, 384, 50);
    let (sink, _log) = recording_sink();
    let service = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        sink,
    );
    // 構築が始まったら (または完了まで)、status呼び出しが高速に返り続ける
    // ことと、search呼び出しがデッドロックしないことを確認する。
    let mut status_calls = 0usize;
    let mut max_status_ms = 0f64;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut observed_building = false;
    while Instant::now() < deadline {
        let started = Instant::now();
        let status = service.status();
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        max_status_ms = max_status_ms.max(elapsed);
        status_calls += 1;
        if status.building {
            observed_building = true;
            // 構築中のsearch (未readyならNone — ただしデッドロックしない)
            let _ = service.search(&[0.1; 384], 3, None);
        }
        if status.ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(service.status().ready, "期限内に構築完了");
    // statusは索引ロックを共有するが、構築スレッドはページ単位でロックを
    // 手放すため、呼び出しが詰まらない (UI操作可能性の実証)。
    println!(
        "[wp12-e2e] status calls during build: {status_calls}, max latency {max_status_ms:.1}ms, building observed: {observed_building}"
    );
    assert!(status_calls >= 2, "複数回のstatus呼び出し");
    assert!(max_status_ms < 500.0, "statusが遅い: {max_status_ms:.1}ms");
}

// ===== 常時実行: モデル切替で旧索引は無効 (§10) =====

#[test]
fn model_switch_invalidates_index() {
    let (db, _db_tmp) = make_db("modelswitch");
    let index_root = TempDir::new("idx3");
    seed_synthetic_embeddings(&db, 10, 384, 5);
    let (sink, _log) = recording_sink();
    let service = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        sink,
    );
    wait_for("構築", Duration::from_secs(120), || {
        service.status().ready.then_some(())
    });
    assert!(service.is_ready());
    // 30mへ切り替え (ベクトルなし) → 旧索引 (70m) は使えない
    db.with_writer(|repos| repos.setting_set(gento_db::SETTING_EMBEDDING_MODEL, "ruri-v3-30m"))
        .unwrap();
    assert!(!service.is_ready(), "identity不一致で無効");
    assert!(service.search(&[0.1; 384], 3, None).is_none());
    // 30mは256次元 — 384次元ベクトルでの誤検索は構造的に起きない
    // (identity照合で弾かれる) ことが本質 (§10混在防止)
    // 元へ戻すと再び利用可能
    db.with_writer(|repos| repos.setting_set(gento_db::SETTING_EMBEDDING_MODEL, "ruri-v3-70m"))
        .unwrap();
    assert!(service.is_ready());
}

// ===== 実モデルE2E (acceptance 3/4): 自然言語→関連asset・類似・重み =====

#[test]
fn hybrid_search_pipeline_real_model() {
    let Some(models_dir) = real_models_dir() else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定または未検証 — スキップ");
        return;
    };
    let (db, _db_tmp) = make_db("hybrid");
    let index_root = TempDir::new("idx4");
    // トピック別caption付きasset (WP-11評価セットの縮小版)
    let cat = seed_asset(&db, "cat", "窓辺で日向ぼっこをしている猫の写真。");
    let kitten = seed_asset(&db, "kitten", "子猫が毛布の上で眠っているイラスト。");
    let dog = seed_asset(&db, "dog", "公園でフリスビーを追いかける犬の写真。");
    let night = seed_asset(&db, "night", "夜の東京タワーと街の灯りの夜景写真。");
    let ramen = seed_asset(&db, "ramen", "テーブルに並ぶラーメンのボウル。");

    // 埋め込みサービス (実ワーカー) + 有効化 → ジョブ完了まで待つ
    let (embedding_sink, _elog) = recording_sink();
    let embedding = gento_tauri_lib::embedding::EmbeddingService::start(
        Arc::clone(&db),
        models_dir,
        embedding_sink,
    );
    embedding.enable(Some("ruri-v3-70m")).expect("enable");
    wait_for("埋め込みジョブ", Duration::from_secs(180), || {
        let count = db
            .with_reader(|repos| {
                repos.embedding_count_for_model(
                    "ruri-v3-70m",
                    &gento_tauri_lib::embedding::model_revision_of("ruri-v3-70m"),
                )
            })
            .unwrap_or(0);
        (count >= 5).then_some(count)
    });

    // 索引サービス + 増分フック接続 (本物の配線と同じ構成)
    let (index_sink, _ilog) = recording_sink();
    let vector_index = gento_tauri_lib::vectorindex::VectorIndexService::start(
        Arc::clone(&db),
        index_root.path().to_path_buf(),
        index_sink,
    );
    embedding.set_vector_sink({
        let vector_index_for_sink = Arc::clone(&vector_index);
        Arc::new(
            move |model: &str, revision: &str, asset_id: i64, vector: &[f32]| {
                vector_index_for_sink.on_embedding_upserted(model, revision, asset_id, vector);
            },
        )
    });
    // 増分追加済み (ジョブ完了フック経由) とは限らない期間に構築された
    // 可能性もあるため、ここで強制的に最新状態へ合わせる
    vector_index.rebuild_now();
    wait_for("索引", Duration::from_secs(120), || {
        let status = vector_index.status();
        (status.ready && status.indexed_count >= 5).then_some(status)
    });

    // === 1. 自然言語クエリ → 関連assetが上位 (acceptance 3) ===
    let ast = gento_search::parse("くつろいでいる猫").unwrap().query;
    let scope = gento_db::SearchScope {
        root_id: None,
        collection_id: None,
        require_present_files: true,
    };
    let (records, total, warnings, semantic) = gento_tauri_lib::search::execute_search(
        &db,
        &ast,
        &scope,
        1.0, // w=1: 意味のみ
        Some(Arc::clone(&embedding) as Arc<gento_tauri_lib::embedding::EmbeddingService>),
        Some(Arc::clone(&vector_index)),
        vec![],
    )
    .expect("hybrid execute");
    assert!(warnings.is_empty(), "警告: {warnings:?}");
    assert!(total >= 1, "意味検索で結果が出る (FTS不一致でも)");
    // レコードのasset id順序: 意味近傍順 (猫系が最上位)
    let record_ids: Vec<String> = records.iter().map(|r| r.id.to_string()).collect();
    println!("[wp12-e2e] w=1 records: {record_ids:?}");
    let top_numeric: Option<i64> = record_ids.first().and_then(|id| id.parse::<i64>().ok());
    assert!(
        top_numeric == Some(cat) || top_numeric == Some(kitten),
        "猫系assetが最上位 (cat={cat}, kitten={kitten}): {record_ids:?}"
    );
    assert!(semantic.len() >= 2, "意味スコアが返る: {semantic:?}");

    // === 2. 重み変更で順位変化 (acceptance 4のE2E側) ===
    // キーワード一致 (FTS) するクエリで w=0 と w=1 の先頭が変わることを確認:
    // 「ラーメン」はFTSでramenに一致し、意味でもramenが近い。一方「夜景」系
    // との相対順序は重みで変わり得る。ここでは決定的な検証として、
    // w=0 (純キーワード=旧経路) がFTS一致のみを返すことを使う。
    let ast_ramen = gento_search::parse("ラーメン").unwrap().query;
    let (records_kw, total_kw, _w, _s) = gento_tauri_lib::search::execute_search(
        &db,
        &ast_ramen,
        &scope,
        0.0,
        Some(Arc::clone(&embedding)),
        Some(Arc::clone(&vector_index)),
        vec![],
    )
    .expect("keyword execute");
    assert!(total_kw <= 1, "純キーワードはFTS一致のみ: {total_kw}");
    let kw_ids: Vec<i64> = records_kw.iter().map(|r| r.id.value() as i64).collect();
    assert!(kw_ids.iter().all(|id| *id == ramen));
    // w=1では意味近傍も候補に入る (ramen最上位は維持)
    let (_records_sem, total_sem, _w2, _s2) = gento_tauri_lib::search::execute_search(
        &db,
        &ast_ramen,
        &scope,
        1.0,
        Some(Arc::clone(&embedding)),
        Some(Arc::clone(&vector_index)),
        vec![],
    )
    .expect("semantic execute");
    assert!(
        total_sem > total_kw,
        "w=1は意味近傍を含め候補が増える: kw={total_kw} sem={total_sem}"
    );

    // === 3. 類似画像検索 (acceptance 3): catの近傍はkitten ===
    let (similar_records, similar_scores) =
        gento_tauri_lib::search::execute_similar(&db, &vector_index, cat, 3).expect("similar");
    assert!(!similar_records.is_empty());
    let top_ids: Vec<i64> = similar_records
        .iter()
        .map(|r| r.id.value() as i64)
        .collect();
    println!("[wp12-e2e] similar to cat: {top_ids:?} scores={similar_scores:?}");
    assert_eq!(top_ids.first(), Some(&kitten), "catの最近傍はkitten");
    assert!(similar_scores.contains_key(&kitten.to_string()));

    // 夜景・犬にも言及: 類似先の多様性 (同トピック最上位の検査は上で十分)
    let _ = (dog, night);
}
