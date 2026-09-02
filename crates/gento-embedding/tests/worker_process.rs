//! ワーカーバイナリの結合テスト (stdioプロトコル実機駆動)。
//!
//! - `worker_protocol_without_model`: モデル不要の経路 (status・未ロード
//!   embed拒否・検証失敗load・shutdown・終了コード)。常時実行。
//! - `worker_full_cycle_with_real_model`: 実モデル (.local-models)。
//!   `GENTO_EMBEDDING_MODELS_DIR` 設定時のみ。

use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use gento_embedding::worker_proto::{
    read_frame, write_frame, WorkerRequest, WorkerResponse, WorkerResult,
};

struct WorkerProcess {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl WorkerProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gento-embedding-worker"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn worker");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 1,
        }
    }

    fn request(&mut self, request: WorkerRequest) -> WorkerResponse {
        let stdin = self.stdin.as_mut().expect("stdin closed");
        write_frame(stdin, &request).expect("write request");
        read_frame(&mut self.stdout)
            .expect("read response")
            .expect("response frame")
    }

    fn call(&mut self, request: WorkerRequest) -> WorkerResponse {
        let id = self.next_id;
        self.next_id += 1;
        let with_id = match request {
            WorkerRequest::Load {
                models_dir,
                model,
                threads,
                ..
            } => WorkerRequest::Load {
                id,
                model,
                models_dir,
                threads,
            },
            WorkerRequest::Unload { .. } => WorkerRequest::Unload { id },
            WorkerRequest::EmbedQuery { text, .. } => WorkerRequest::EmbedQuery { id, text },
            WorkerRequest::EmbedDocuments { texts, .. } => {
                WorkerRequest::EmbedDocuments { id, texts }
            }
            WorkerRequest::Status { .. } => WorkerRequest::Status { id },
            WorkerRequest::Shutdown { .. } => WorkerRequest::Shutdown { id },
        };
        self.request(with_id)
    }

    fn wait_exit(&mut self) -> std::process::ExitStatus {
        self.child.wait().expect("wait worker")
    }
}

fn temp_models_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gento-worker-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("models dir");
    dir
}

#[test]
fn worker_protocol_without_model() {
    let mut worker = WorkerProcess::spawn();
    // status: 未ロード
    let response = worker.call(WorkerRequest::Status { id: 0 });
    assert!(response.ok, "{response:?}");
    match response.result {
        Some(WorkerResult::Status { status }) => {
            assert_eq!(status.loaded_model, None);
            assert_eq!(status.queries_processed, 0);
            assert_eq!(status.documents_processed, 0);
        }
        other => panic!("status結果が期待と違う: {other:?}"),
    }
    // embed_query: 未ロード拒否
    let response = worker.call(WorkerRequest::EmbedQuery {
        id: 0,
        text: "テスト".to_string(),
    });
    assert!(!response.ok);
    assert!(
        response
            .error
            .clone()
            .unwrap()
            .contains("ロードされていません"),
        "{response:?}"
    );
    // load: 存在しないmodels_dir → 検証失敗
    let bogus_dir = temp_models_dir("bogus");
    let response = worker.call(WorkerRequest::Load {
        id: 0,
        model: "ruri-v3-70m".to_string(),
        models_dir: bogus_dir.to_string_lossy().to_string(),
        threads: None,
    });
    assert!(!response.ok, "{response:?}");
    assert!(
        response.error.clone().unwrap().contains("検証失敗"),
        "検証失敗のエラーであること: {response:?}"
    );
    // load: 不正モデルID
    let response = worker.call(WorkerRequest::Load {
        id: 0,
        model: "bogus-model".to_string(),
        models_dir: bogus_dir.to_string_lossy().to_string(),
        threads: Some(1),
    });
    assert!(!response.ok);
    let _ = std::fs::remove_dir_all(&bogus_dir);
    // shutdown: 正常終了
    let response = worker.call(WorkerRequest::Shutdown { id: 0 });
    assert!(response.ok);
    assert!(matches!(response.result, Some(WorkerResult::Shutdown)));
    let status = worker.wait_exit();
    assert!(status.success(), "終了コード0: {status}");
}

#[test]
fn worker_exits_when_stdin_closes() {
    let mut worker = WorkerProcess::spawn();
    worker.stdin = None; // stdinを閉じる (worker側でEOF観測)
    let status = worker.wait_exit();
    assert!(status.success(), "stdin閉鎖で終了: {status}");
}

#[test]
fn worker_full_cycle_with_real_model() {
    let Ok(_env) = std::env::var("GENTO_EMBEDDING_MODELS_DIR") else {
        eprintln!("GENTO_EMBEDDING_MODELS_DIR 未設定 — スキップ");
        return;
    };
    let models_dir = resolve_models_dir();
    let mut worker = WorkerProcess::spawn();
    let response = worker.call(WorkerRequest::Load {
        id: 0,
        model: "ruri-v3-70m".to_string(),
        models_dir: models_dir.to_string_lossy().to_string(),
        threads: Some(2),
    });
    assert!(response.ok, "{response:?}");
    match response.result {
        Some(WorkerResult::Load { model_info }) => {
            assert_eq!(model_info.model_id, "ruri-v3-70m");
            assert_eq!(model_info.embedding_dim, 384);
        }
        other => panic!("load結果: {other:?}"),
    }
    let response = worker.call(WorkerRequest::EmbedQuery {
        id: 0,
        text: "夜の街のネオン".to_string(),
    });
    assert!(response.ok);
    match response.result {
        Some(WorkerResult::EmbedQuery { vector }) => {
            assert_eq!(vector.len(), 384);
            let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "norm={norm}");
        }
        other => panic!("embed_query結果: {other:?}"),
    }
    let response = worker.call(WorkerRequest::EmbedDocuments {
        id: 0,
        texts: vec!["昼のビーチ".to_string(), "夜の街".to_string()],
    });
    assert!(response.ok);
    match response.result {
        Some(WorkerResult::EmbedDocuments { vectors }) => {
            assert_eq!(vectors.len(), 2);
            assert!(vectors.iter().all(|v| v.len() == 384));
        }
        other => panic!("embed_documents結果: {other:?}"),
    }
    let response = worker.call(WorkerRequest::Status { id: 0 });
    assert!(response.ok);
    match response.result {
        Some(WorkerResult::Status { status }) => {
            assert_eq!(status.loaded_model.as_deref(), Some("ruri-v3-70m"));
            assert_eq!(status.queries_processed, 1);
            assert_eq!(status.documents_processed, 2);
        }
        other => panic!("status結果: {other:?}"),
    }
    // unload → status未ロード
    let response = worker.call(WorkerRequest::Unload { id: 0 });
    assert!(response.ok);
    let response = worker.call(WorkerRequest::Status { id: 0 });
    match response.result {
        Some(WorkerResult::Status { status }) => assert_eq!(status.loaded_model, None),
        other => panic!("status結果: {other:?}"),
    }
    let response = worker.call(WorkerRequest::Shutdown { id: 0 });
    assert!(response.ok);
    assert!(worker.wait_exit().success());
}

fn resolve_models_dir() -> PathBuf {
    let path = PathBuf::from(
        std::env::var("GENTO_EMBEDDING_MODELS_DIR").expect("GENTO_EMBEDDING_MODELS_DIR"),
    );
    if path.is_absolute() {
        path
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join(path)
    }
}
