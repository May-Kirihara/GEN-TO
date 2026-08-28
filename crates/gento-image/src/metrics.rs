//! 走査・プローブの計測とメモリ記録 (Phase 2 / WP-2)。
//!
//! acceptance「走査時間とメモリを記録できる」への対応:
//! elapsed はイベント・ログ両方へ、RSS はログへ出力する。

use std::path::Path;

/// 走査1回分の計測値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanMetrics<'a> {
    pub root: &'a Path,
    pub found: u64,
    pub errors: u64,
    pub scan_elapsed_ms: u64,
    /// プローブ (寸法取得) の累積時間。
    pub probe_elapsed_ms: u64,
    pub rss_kb: Option<u64>,
}

/// /proc/self/status の VmRSS をKB単位で返す。Linux以外・解析失敗は None。
pub fn read_self_rss_kb() -> Option<u64> {
    parse_vm_rss_kb(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// /proc/self/status 形式のテキストから VmRSS (KB) を解析する。
pub fn parse_vm_rss_kb(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let value = line.strip_prefix("VmRSS:")?.trim();
    let kb = value.strip_suffix("kB").unwrap_or(value).trim();
    kb.parse().ok()
}

/// 走査完了時の計測ログ (tracing info)。
pub fn log_scan_metrics(metrics: &ScanMetrics<'_>) {
    tracing::info!(
        root = %metrics.root.display(),
        found = metrics.found,
        errors = metrics.errors,
        scan_elapsed_ms = metrics.scan_elapsed_ms,
        probe_elapsed_ms = metrics.probe_elapsed_ms,
        rss_kb = metrics.rss_kb,
        "scan metrics"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vm_rss_from_status_text() {
        let status = "Name:\tgento\nVmPeak:\t  1000 kB\nVmRSS:\t   432 kB\n";
        assert_eq!(parse_vm_rss_kb(status), Some(432));
        assert_eq!(parse_vm_rss_kb("VmRSS:\t  1024 kB"), Some(1024));
        assert_eq!(parse_vm_rss_kb("nothing here"), None);
        assert_eq!(parse_vm_rss_kb("VmRSS:\tnotanumber kB"), None);
    }

    #[test]
    fn reads_own_rss_on_linux() {
        // 開発環境はLinux。プロセスは生きているので必ず正の値が取れるはず。
        let rss = read_self_rss_kb();
        assert!(rss.is_some());
        assert!(rss.unwrap() > 0);
    }
}
