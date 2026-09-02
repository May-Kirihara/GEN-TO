//! UUIDv7生成とUTC RFC3339時刻整形 (MVP WP-1)。
//!
//! - UUID: v7 (時系列おおよそ整列)。36文字小文字ハイフン区切りTEXT。
//!   PM計画§6-4の確定事項として、**一意性のみ保証し単調性は保証しない**
//!   (クロック巻き戻り時の順序保証はしない)。整列が必要な場面は
//!   DBの `created_at` 等の明示的時刻列でソートすること。
//! - 日時: UTC RFC3339 TEXT (`YYYY-MM-DDTHH:MM:SS.mmmZ`。常にミリ秒3桁)。
//!   chrono等の依存を避け、Hinnantのcivil-from-days算法で自前変換する。

/// UUIDv7を生成する (uuid crate。rng = getrandom)。
pub(crate) fn new_uuid_v7() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// 現在時刻をRFC3339 (UTC・ミリ秒) で返す。
pub(crate) fn now_utc_rfc3339() -> String {
    format_epoch_rfc3339(std::time::SystemTime::now())
}

/// 任意時刻のRFC3339整形 (jobs.rsのバックオフ期限計算から使用)。
pub(crate) fn format_epoch_rfc3339_pub(t: std::time::SystemTime) -> String {
    format_epoch_rfc3339(t)
}

/// backupファイル名などに使うコンパクト形式 (`YYYYMMDDTHHMMSSmmmZ`)。
pub(crate) fn now_utc_compact() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_epoch_compact(d.as_secs() as i64, d.subsec_millis())
}

/// エポック秒・ミリ秒 → `YYYY-MM-DDTHH:MM:SS.mmmZ`。
fn format_epoch_rfc3339(t: std::time::SystemTime) -> String {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format_epoch(d.as_secs() as i64, d.subsec_millis(), "-", "T", ":", ".")
}

/// 汎用整形。区切り文字を差し替える (compact形式は全区切り空文字)。
fn format_epoch(secs: i64, millis: u32, dsep: &str, tsep: &str, csep: &str, msep: &str) -> String {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (sod / 3_600, (sod % 3_600) / 60, sod % 60);
    format!("{y:04}{dsep}{m:02}{dsep}{d:02}{tsep}{h:02}{csep}{mi:02}{csep}{s:02}{msep}{millis:03}Z")
}

fn format_epoch_compact(secs: i64, millis: u32) -> String {
    format_epoch(secs, millis, "", "T", "", "")
}

/// days (1970-01-01 = 0) → (year, month, day)。Howard Hinnantの算法。
/// 参考: https://howardhinnant.github.io/date_algorithms.html#civil_from_days
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn uuid_v7_format() {
        for _ in 0..200 {
            let u = new_uuid_v7();
            let parts: Vec<&str> = u.split('-').collect();
            let lens: [usize; 5] = [8, 4, 4, 4, 12];
            assert_eq!(parts.len(), 5, "{u}");
            for (p, len) in parts.iter().zip(lens) {
                assert_eq!(p.len(), len, "{u}");
                assert!(
                    p.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                    "hex小文字のみ: {u}"
                );
            }
            assert_eq!(u.len(), 36);
            // version nibble = 7 (13バイト目の上位4bit)
            assert_eq!(u.as_bytes()[14], b'7', "{u}");
            // variant: 17バイト目の上位2bit = 10 → hexは 8/9/a/b
            let variant = u.as_bytes()[19];
            assert!(matches!(variant, b'8' | b'9' | b'a' | b'b'), "{u}");
        }
    }

    #[test]
    fn uuid_v7_unique() {
        let mut seen = HashSet::new();
        for _ in 0..2_000 {
            assert!(seen.insert(new_uuid_v7()));
        }
    }

    #[test]
    fn uuid_v7_timestamp_prefix_is_recent() {
        // v7上位48bit = Unixミリ秒。現在時刻と±1分の範囲に収まること。
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let u = new_uuid_v7();
        // 48bit = 12 hex文字。ハイフン (位置8) を避けて連結する
        let hex = format!("{}{}", &u[0..8], &u[9..13]);
        let ms = i64::from_str_radix(&hex, 16).unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(
            ms >= before.saturating_sub(1_000) && ms <= after + 1_000,
            "ms={ms} before={before} after={after}"
        );
    }

    #[test]
    fn rfc3339_known_epochs() {
        let f = |secs: i64, ms: u32| format_epoch(secs, ms, "-", "T", ":", ".");
        assert_eq!(f(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(f(86_400, 1), "1970-01-02T00:00:00.001Z");
        assert_eq!(f(1_759_000_000, 123), "2025-09-27T19:06:40.123Z");
        // うるう年境界 (2024-02-29 → 03-01)
        assert_eq!(f(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
        assert_eq!(f(1_709_251_200, 0), "2024-03-01T00:00:00.000Z");
        // 年末年始
        assert_eq!(f(1_735_689_599, 999), "2024-12-31T23:59:59.999Z");
        assert_eq!(f(1_735_689_600, 0), "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn rfc3339_shape_and_monotonic() {
        let a = now_utc_rfc3339();
        assert_eq!(a.len(), 24, "{a}");
        assert!(a.ends_with('Z'));
        assert_eq!(a.as_bytes()[10], b'T');
        assert_eq!(a.as_bytes()[23], b'Z');
        // 2回呼び出しで減少しない (同一ミリ秒は許容)
        let b = now_utc_rfc3339();
        assert!(a <= b, "{a} > {b}");
    }

    #[test]
    fn compact_timestamp_shape() {
        let c = now_utc_compact();
        // YYYYMMDD (8) + T (1) + HHMMSS (6) + mmm (3) + Z (1) = 19
        assert_eq!(c.len(), 19, "{c}");
        assert!(c.ends_with('Z'));
        assert!(c
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'T' || b == b'Z'));
        // 辞書順 = 時系列 (世代保持のソートで利用)
        let d = now_utc_compact();
        assert!(c <= d);
    }
}
