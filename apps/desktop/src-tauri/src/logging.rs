//! ロギング基盤 (MVP WP-15)。
//!
//! - **stdout tee + メモリring**: 従来のstdout出力 (計測ドライバが
//!   grepする形式) を維持しつつ、直近ログ行をbounded ringへ複写する。
//!   診断bundleの「直近ログ末尾」はこのring (＝redaction済みログの出力
//!   そのもの) からのみ取り出す。
//! - **実行時ログレベル変更**: `general.log_level` 設定から
//!   `tracing_subscriber::reload` handle経由でEnvFilterを切り替える。
//!   起動時は `GENTO_LOG` envが優先 (env指定時は設定を適用しない)。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use gento_db::{Db, LogLevel};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

/// ringの行数上限 (bundleには末尾100行程度を使う。余裕を持って512)。
const RING_MAX_LINES: usize = 512;

/// 直近ログ行のbounded ring。
pub struct LogRing {
    lines: Mutex<VecDeque<String>>,
}

impl LogRing {
    fn new() -> Self {
        LogRing {
            lines: Mutex::new(VecDeque::new()),
        }
    }

    fn push_line(&self, line: &str) {
        let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        if lines.len() >= RING_MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(line.trim_end_matches('\r').to_string());
    }

    /// 末尾最大 `max` 行を (古い順に) 返す。
    pub fn tail(&self, max: usize) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        let skip = lines.len().saturating_sub(max);
        lines.iter().skip(skip).cloned().collect()
    }

    /// テスト用: 空のringを作る (診断bundleのredaction検証等)。
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        LogRing::new()
    }

    /// テスト用: 危険な行の混入シミュレーション (scrub検証)。
    #[cfg(test)]
    pub(crate) fn push_line_for_test(&self, line: &str) {
        self.push_line(line);
    }
}

/// fmt layerへ渡すMakeWriter (stdout + ringへtee)。
#[derive(Clone)]
struct RingTee {
    ring: Arc<LogRing>,
}

/// 1イベント分の出力を組み立てるwriter。行完了 (改行) ごとにringへ渡す。
struct LineTeeWriter {
    ring: Arc<LogRing>,
    buf: Vec<u8>,
}

impl std::io::Write for LineTeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        {
            // stdoutは従来形式を維持 (計測ドライバがgrepするため)
            let mut out = std::io::stdout().lock();
            out.write_all(buf)?;
        }
        self.buf.extend_from_slice(buf);
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            self.ring.push_line(&text);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

impl<'a> MakeWriter<'a> for RingTee {
    type Writer = LineTeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LineTeeWriter {
            ring: Arc::clone(&self.ring),
            buf: Vec::new(),
        }
    }
}

/// set_default (thread-local subscriber) を使うテストの直列化。
/// tracingのcallsite interestキャッシュは subscriber 変更時に再構築
/// されるため、並行する別テストのset_defaultが完了前に走ると
/// interestが「無効」と誤キャッシュされ得る (単独では通るが全体実行で
/// 空captureになるのはこの競合)。set_default利用テストは全てこの
/// ロックを取ること。
#[cfg(test)]
pub(crate) static SUBSCRIBER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ===== テスト用: グローバルcapture subscriber =====
// thread-localなset_defaultは、並行テストが同じbinary内でsubscriberを
// 積み替えるとcallsite interestの再構築と競合し、イベントが静かに落る
// (tracingの既知の制限)。テストからログを検証する場合はこちらの
// プロセス-global subscriberを使い、出力をthread idで仕分ける。

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
pub(crate) struct TestCapture {
    lines: std::sync::Mutex<Vec<(String, String)>>,
}

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
impl TestCapture {
    fn push(&self, thread: String, line: String) {
        let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        if lines.len() >= 20_000 {
            lines.drain(..10_000);
        }
        lines.push((thread, line));
    }

    /// 呼び出し元スレッドの行のみを返す。
    pub(crate) fn my_lines(&self) -> Vec<String> {
        let me = format!("{:?}", std::thread::current().id());
        let lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        lines
            .iter()
            .filter(|(thread, _)| *thread == me)
            .map(|(_, line)| line.clone())
            .collect()
    }
}

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
#[derive(Clone, Default)]
pub(crate) struct TestCaptureWriter {
    sink: Option<std::sync::Arc<TestCapture>>,
    buf: Vec<u8>,
}

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
impl std::io::Write for TestCaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(sink) = &self.sink {
            let thread = format!("{:?}", std::thread::current().id());
            self.buf.extend_from_slice(buf);
            while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                let text = String::from_utf8_lossy(&line[..line.len() - 1]);
                sink.push(thread.clone(), text.into_owned());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
impl<'a> MakeWriter<'a> for TestCaptureWriter {
    type Writer = TestCaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
static TEST_CAPTURE: std::sync::OnceLock<std::sync::Arc<TestCapture>> = std::sync::OnceLock::new();

/// テストbinary全体で1つのcapture subscriberをinstallする
/// (二度目以降の呼び出しは既存のcaptureを返す)。
#[cfg(all(test, any(debug_assertions, feature = "devtools")))]
pub(crate) fn test_capture() -> std::sync::Arc<TestCapture> {
    let existing = TEST_CAPTURE.get();
    if let Some(cap) = existing {
        return std::sync::Arc::clone(cap);
    }
    // set_default利用テスト (reload検証) とのsubscriber切替え競合を避ける
    let _lock = SUBSCRIBER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let cap = std::sync::Arc::new(TestCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let subscriber = tracing_subscriber::Registry::default()
        .with(tracing_subscriber::EnvFilter::new("debug"))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(TestCaptureWriter {
                    sink: Some(std::sync::Arc::clone(&cap)),
                    buf: Vec::new(),
                })
                .without_time(),
        );
    // 先勝ち: 既に誰かがglobal defaultを設定していたら諦めて
    // capture-onlyで返す (イベントは流れないため呼び出し側の注意書き参照)
    let _ = subscriber.try_init();
    let _ = TEST_CAPTURE.set(std::sync::Arc::clone(&cap));
    cap
}

/// グローバル状態 (init後に不変)。init前のテストではNoneのまま。
struct Globals {
    ring: Arc<LogRing>,
    handle: Mutex<reload::Handle<EnvFilter, Registry>>,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

/// ロギングを初期化する (lib.rsのsetupから1回)。
///
/// 初期フィルタ: `GENTO_LOG` env (従来のEnvFilter書式・フル指定可) >
/// `info`。設定値 (`general.log_level`) の適用はDB open後に
/// [`apply_startup_setting`] で行う (env指定時は適用しない = env優先)。
pub fn init() -> Arc<LogRing> {
    let ring = Arc::new(LogRing::new());
    let filter = EnvFilter::try_from_env("GENTO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, handle) = reload::Layer::new(filter);
    let subscriber =
        Registry::default()
            .with(filter_layer)
            .with(tracing_subscriber::fmt::layer().with_writer(RingTee {
                ring: Arc::clone(&ring),
            }));
    if subscriber.try_init().is_err() {
        // 二重init (主にテスト実行)。既存のstderr出力が継続するため、
        // アプリ動作には影響しない (ringのteeは失われる)。
        eprintln!("wp15 logging: tracing subscriber already initialized; ring tee not installed");
    }
    let globals = Globals {
        ring: Arc::clone(&ring),
        handle: Mutex::new(handle),
    };
    let _ = GLOBALS.set(globals);
    ring
}

/// グローバルringの参照 (診断bundle用)。未initならNone。
pub fn ring() -> Option<Arc<LogRing>> {
    GLOBALS.get().map(|g| Arc::clone(&g.ring))
}

/// ログレベルを実行時変更する (`settings_set_log_level` から)。
pub fn apply_level(level: LogLevel) -> Result<(), String> {
    let Some(globals) = GLOBALS.get() else {
        return Err("ロギングが未初期化です".to_string());
    };
    let handle = globals
        .handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    handle
        .reload(EnvFilter::new(level.as_str()))
        .map_err(|e| format!("ログレベルの変更に失敗しました: {e}"))
}

/// 起動時設定適用: `GENTO_LOG` envが未指定の場合のみ
/// `general.log_level` 設定をフィルタへ反映する。
pub fn apply_startup_setting(db: &Db) {
    if std::env::var_os("GENTO_LOG").is_some() {
        tracing::debug!("GENTO_LOG env set; skipping general.log_level setting");
        return;
    }
    let snapshot = gento_db::load_app_settings(db);
    let level = snapshot.general.log_level;
    if level != LogLevel::Info {
        if let Err(e) = apply_level(level) {
            tracing::warn!(error = %e, "failed to apply general.log_level at startup");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn ring_keeps_bounded_tail_in_order() {
        let ring = LogRing::new();
        for i in 0..(RING_MAX_LINES + 100) {
            ring.push_line(&format!("line-{i}"));
        }
        let tail = ring.tail(10);
        assert_eq!(tail.len(), 10);
        // tailは古い順: 612行投入しringは末尾512行 (line-100..611) を保持
        assert_eq!(tail[0], "line-602");
        assert_eq!(tail[9], "line-611");
        // 上限を超えて保持しない
        let all = ring.tail(RING_MAX_LINES * 2);
        assert_eq!(all.len(), RING_MAX_LINES);
        assert_eq!(all[0], "line-100");
    }

    #[test]
    fn line_tee_writer_records_complete_lines_only() {
        let ring = Arc::new(LogRing::new());
        let mut writer = LineTeeWriter {
            ring: Arc::clone(&ring),
            buf: Vec::new(),
        };
        writer.write_all(b"partial without newline").unwrap();
        assert!(ring.tail(10).is_empty(), "改行前はringへ入らない");
        writer.write_all(b" and done\nsecond line\n").unwrap();
        let tail = ring.tail(10);
        assert_eq!(tail.len(), 2);
        assert!(tail[0].starts_with("partial without newline and done"));
        assert_eq!(tail[1], "second line");
    }

    #[test]
    fn reload_handle_changes_visible_level() {
        let _subscriber_lock = super::SUBSCRIBER_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let ring = Arc::new(LogRing::new());
        let (filter, handle) = reload::Layer::new(EnvFilter::new("warn"));
        let subscriber = Registry::default().with(filter).with(
            tracing_subscriber::fmt::layer()
                .with_writer(RingTee {
                    ring: Arc::clone(&ring),
                })
                .without_time(),
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::warn!("wp15 warn probe");
        tracing::debug!("wp15 hidden-before-reload");
        handle.reload(EnvFilter::new("debug")).unwrap();
        tracing::debug!("wp15 debug probe visible");
        let text = ring.tail(64).join("\n");
        assert!(text.contains("wp15 warn probe"), "{text}");
        assert!(!text.contains("hidden-before-reload"), "{text}");
        assert!(text.contains("wp15 debug probe visible"), "{text}");
    }
}
