//! ジョブ優先度とサムネイルサイズクラスの基本型 (docs/implementation-plan.md §5.1)。
//!
//! サムネイル生成ワーカープール (gento-image::pool) とIPC (gento-tauri) の
//! 両方から参照されるため、Tauri非依存のこのcrateに置く。
//!
//! WP-10で永続ジョブキュー (jobs表) の指数バックオフ計算
//! ([`backoff_delay_ms`]) を追加した。純粋関数としてこのcrateに置き、
//! 実行側 (gento-tauriのAIワーカー) は計算のみを委譲する。
//!
//! IPC文字列 (docs/dev/api-contract.md 契約):
//! - Priority: `interactive` | `viewport` | `prefetch` | `overscan` | `background`
//! - SizeClass: `s256` | `s512` | `preview2048`

use std::fmt;
use std::str::FromStr;

/// サムネイル生成ジョブの優先度。数値が小さいほど優先される
/// (technical-spike.md §10 の初期案に対応)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// 単一ビューア・比較表示中の画像 (最優先)。
    Interactive = 0,
    /// 一覧の表示範囲。
    Viewport = 1,
    /// 閲覧方向の先読み。
    Prefetch = 2,
    /// overscan範囲。
    Overscan = 3,
    /// 現在フォルダの残り・一括生成 (最低優先)。
    Background = 4,
}

impl Priority {
    /// 全優先度。昇順。
    pub const ALL: [Priority; 5] = [
        Priority::Interactive,
        Priority::Viewport,
        Priority::Prefetch,
        Priority::Overscan,
        Priority::Background,
    ];

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Priority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Priority::Interactive => "interactive",
            Priority::Viewport => "viewport",
            Priority::Prefetch => "prefetch",
            Priority::Overscan => "overscan",
            Priority::Background => "background",
        }
    }
}

/// 不明な優先度文字列のパース失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePriorityError;

impl fmt::Display for ParsePriorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown priority: expected one of interactive|viewport|prefetch|overscan|background"
        )
    }
}

impl std::error::Error for ParsePriorityError {}

impl FromStr for Priority {
    type Err = ParsePriorityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "interactive" => Ok(Priority::Interactive),
            "viewport" => Ok(Priority::Viewport),
            "prefetch" => Ok(Priority::Prefetch),
            "overscan" => Ok(Priority::Overscan),
            "background" => Ok(Priority::Background),
            _ => Err(ParsePriorityError),
        }
    }
}

/// サムネイル・previewのサイズクラス (長辺ピクセル基準)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeClass {
    /// 長辺256pxサムネイル。
    S256,
    /// 長辺512pxサムネイル。
    S512,
    /// 長辺2,048pxプレビュー。
    Preview2048,
}

impl SizeClass {
    pub const ALL: [SizeClass; 3] = [SizeClass::S256, SizeClass::S512, SizeClass::Preview2048];

    /// このクラスの長辺上限 (px)。元画像の長辺がこの値以下なら拡大しない。
    pub const fn long_edge(self) -> u32 {
        match self {
            SizeClass::S256 => 256,
            SizeClass::S512 => 512,
            SizeClass::Preview2048 => 2048,
        }
    }

    /// IPC文字列 (`as_str` と同一)。
    pub const fn ipc_name(self) -> &'static str {
        self.as_str()
    }

    /// キャッシュファイル名へ埋め込む短い識別子。
    /// ファイル名長さ上限 (48文字) 内に収めるため短縮形を使う。
    pub const fn file_id(self) -> &'static str {
        match self {
            SizeClass::S256 => "256",
            SizeClass::S512 => "512",
            SizeClass::Preview2048 => "p2048",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            SizeClass::S256 => "s256",
            SizeClass::S512 => "s512",
            SizeClass::Preview2048 => "preview2048",
        }
    }
}

impl fmt::Display for SizeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 不明なサイズクラス文字列のパース失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseSizeClassError;

// ===== WP-10: 永続ジョブの指数バックオフ =====

/// バックオフの基準遅延 (ms)。既定30秒。
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 30_000;
/// バックオフの倍率。30s → 2m → 8m の列 (§18「指数バックオフ」)。
pub const DEFAULT_BACKOFF_FACTOR: u64 = 4;
/// ジョブの既定最大試行回数 (初回 + 再試行2回)。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// バックオフの上限 (ms)。試行を重ねても1時間を超えて待たせない。
pub const BACKOFF_CAP_MS: u64 = 60 * 60 * 1000;

/// 永続ジョブの再試行バックオフ遅延 (ms) を計算する (WP-10)。
///
/// - `attempts`: 終了した試行回数 (1起算。0は1として扱う)。
/// - `jitter01`: ジッター乱数 [0.0, 1.0)。範囲外はクランプする。
///
/// 遅延は `base * factor^(attempts-1)` を基準とし、ジッターで
/// 0.5倍以上1.5倍未満に散らす (同時失敗したジョブ群の再試行が
/// 同一時刻に殺到するのを防ぐ)。基準値は [`BACKOFF_CAP_MS`] で打ち切る。
/// 決定的テストのため純粋関数 (乱数は呼び出し側が供給する)。
pub fn backoff_delay_ms(attempts: u32, base_ms: u64, factor: u64, jitter01: f64) -> u64 {
    let attempts = attempts.max(1);
    // factor^(attempts-1)。 saturating: オーバーフロー時はcapへ到達させる。
    let mut scale: u64 = 1;
    for _ in 1..attempts {
        scale = match scale.checked_mul(factor) {
            Some(next) => next,
            None => return BACKOFF_CAP_MS,
        };
        if scale.saturating_mul(base_ms) >= BACKOFF_CAP_MS {
            return BACKOFF_CAP_MS;
        }
    }
    let nominal = base_ms.saturating_mul(scale).min(BACKOFF_CAP_MS);
    let jitter = jitter01.clamp(0.0, 1.0);
    // 0.5 + jitter ∈ [0.5, 1.5]。jitter=0.5で基準値と一致する。
    let multiplier = 0.5 + jitter;
    nomimal_mul(nominal, multiplier).clamp(1, BACKOFF_CAP_MS)
}

/// f64乗算を飽和付き整数へ (丸め誤差でcap超過しないようminで抑える)。
fn nomimal_mul(nominal: u64, multiplier: f64) -> u64 {
    let v = (nominal as f64 * multiplier).round();
    if v >= BACKOFF_CAP_MS as f64 {
        BACKOFF_CAP_MS
    } else if v <= 0.0 {
        1
    } else {
        v as u64
    }
}

impl fmt::Display for ParseSizeClassError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown size class: expected one of s256|s512|preview2048"
        )
    }
}

impl std::error::Error for ParseSizeClassError {}

impl FromStr for SizeClass {
    type Err = ParseSizeClassError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "s256" => Ok(SizeClass::S256),
            "s512" => Ok(SizeClass::S512),
            "preview2048" => Ok(SizeClass::Preview2048),
            _ => Err(ParseSizeClassError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_order_interactive_is_highest() {
        // 数値が小さいほど優先。ソート昇順 = 优先度順。
        let mut v = vec![
            Priority::Background,
            Priority::Interactive,
            Priority::Overscan,
            Priority::Viewport,
            Priority::Prefetch,
        ];
        v.sort();
        assert_eq!(
            v,
            vec![
                Priority::Interactive,
                Priority::Viewport,
                Priority::Prefetch,
                Priority::Overscan,
                Priority::Background,
            ]
        );
        assert!(Priority::Interactive < Priority::Background);
    }

    #[test]
    fn priority_str_round_trip() {
        for p in Priority::ALL {
            let s = p.to_string();
            assert_eq!(s.parse::<Priority>().unwrap(), p);
        }
        assert_eq!(Priority::Background.to_string(), "background");
    }

    #[test]
    fn priority_rejects_unknown() {
        for bad in ["", "INTERACTIVE", "ui", "bg", "0"] {
            assert!(bad.parse::<Priority>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn size_class_mapping() {
        assert_eq!(SizeClass::S256.long_edge(), 256);
        assert_eq!(SizeClass::S512.long_edge(), 512);
        assert_eq!(SizeClass::Preview2048.long_edge(), 2048);
        assert_eq!(SizeClass::S256.file_id(), "256");
        assert_eq!(SizeClass::Preview2048.file_id(), "p2048");
        assert_eq!(SizeClass::S512.ipc_name(), "s512");
    }

    #[test]
    fn size_class_str_round_trip() {
        for c in SizeClass::ALL {
            assert_eq!(c.to_string().parse::<SizeClass>().unwrap(), c);
        }
        assert!("S256".parse::<SizeClass>().is_err());
        assert!("2048".parse::<SizeClass>().is_err());
        assert!("".parse::<SizeClass>().is_err());
    }

    // ===== WP-10: backoff_delay_ms =====

    #[test]
    fn backoff_sequence_is_30s_2m_8m_at_midpoint_jitter() {
        // ジッター0.5 → 倍率1.0 (基準値そのもの)
        let base = DEFAULT_BACKOFF_BASE_MS;
        assert_eq!(
            backoff_delay_ms(1, base, DEFAULT_BACKOFF_FACTOR, 0.5),
            30_000
        );
        assert_eq!(
            backoff_delay_ms(2, base, DEFAULT_BACKOFF_FACTOR, 0.5),
            120_000
        );
        assert_eq!(
            backoff_delay_ms(3, base, DEFAULT_BACKOFF_FACTOR, 0.5),
            480_000
        );
    }

    #[test]
    fn backoff_jitter_stays_within_half_to_one_and_a_half() {
        for attempts in 1..=5 {
            let lo = backoff_delay_ms(attempts, 30_000, 4, 0.0);
            let hi = backoff_delay_ms(attempts, 30_000, 4, 0.999);
            // midpoint = 基準値 (jitter 0.5)
            let mid = backoff_delay_ms(attempts, 30_000, 4, 0.5);
            assert!(lo >= mid / 2, "lo {lo} < half of {mid}");
            assert!(hi < mid * 2, "hi {hi} >= 1.5x of {mid}");
            assert!(lo >= 1);
        }
    }

    #[test]
    fn backoff_jitter_input_is_clamped() {
        // 範囲外のジッター入力はクランプされ、パニックしない
        let below = backoff_delay_ms(1, 30_000, 4, -0.7);
        let above = backoff_delay_ms(1, 30_000, 4, 3.0);
        assert_eq!(below, backoff_delay_ms(1, 30_000, 4, 0.0));
        // jitter=1.0相当 → 1.5倍ちょうど
        assert_eq!(above, 45_000);
        assert!(above >= 1);
    }

    #[test]
    fn backoff_is_capped_at_one_hour() {
        // 試行回数が大きくても1時間を超えない
        for attempts in [6u32, 10, 40, u32::MAX] {
            assert_eq!(
                backoff_delay_ms(attempts, 30_000, 4, 0.5),
                BACKOFF_CAP_MS,
                "attempts={attempts}"
            );
        }
    }

    #[test]
    fn backoff_attempts_zero_is_first_retry() {
        assert_eq!(
            backoff_delay_ms(0, 30_000, 4, 0.5),
            backoff_delay_ms(1, 30_000, 4, 0.5)
        );
    }
}
