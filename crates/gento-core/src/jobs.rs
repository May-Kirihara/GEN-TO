//! ジョブ優先度とサムネイルサイズクラスの基本型 (docs/implementation-plan.md §5.1)。
//!
//! サムネイル生成ワーカープール (gento-image::pool) とIPC (gento-tauri) の
//! 両方から参照されるため、Tauri非依存のこのcrateに置く。
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
}
