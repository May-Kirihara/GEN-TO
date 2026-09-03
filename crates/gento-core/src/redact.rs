//! ログ・診断bundle出力への秘密redaction (MVP WP-15)。
//!
//! 設計契約:
//! - **鍵素材**: 先頭4字を残してマスクする (`sk-1234abcd…` → `sk-1****`)。
//!   プレフィックス保持は「どの鍵か」の勘案を可能にしつつ素材を露出しない
//!   ための最小情報 (api-contract §49)。
//! - **自由文scrub** ([`scrub_text`]): 出力前に通す最終防護。既知の
//!   秘密パターン (`Bearer …`・`Authorization: …`・`sk-`系長トークン・
//!   `api_key=` 系クエリ/JSON値) をマスクへ置換する。構造的排除
//!   (ApiKeyのDebug伏せ字・request_summaryの鍵不含) が第一線であり、
//!   本関数は迂回経路 (panic payload・外部文字列の混入) の保険。
//! - **GPS**: 座標は構造的に出力しない (ログ・bundleの対象フィールドに
//!   GPS列を含めない)。GPSを保持する構造体 ([`crate::assetmeta`]
//!   のMetadataColumns) のDebugは座標値を `<gps>` へ伏せる — 将来
//!   `{:?}` でログ混入した場合の構造的防護。
//!
//! このmoduleは純関数のみ (DB・IO依存なし。gento-ai/tauri層から利用)。

/// 秘密値 (APIキー等) の表示用マスク。先頭4字 + `****`。
/// 4字に満たない場合は先頭を返さず全面マスク (短い値は全体が素材)。
pub fn redact_key(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 8 {
        return "****".to_string();
    }
    let prefix: String = chars[..4].iter().collect();
    format!("{prefix}****")
}

/// scrub対象のBearer系トークン最小長 (「Bearer x」のような短い一致の
/// 誤爆を避ける。実APIキーは十分長い)。
const TOKEN_MIN_LEN: usize = 12;

/// 座標として扱う小数桁数 (5桁 ≒ 1m精度。GPSの通常表現は6桁)。
/// 4桁以下の小数 (比率・ヒット率・サイズ等) は対象外。
const COORD_MIN_DECIMALS: usize = 5;

/// 自由文の秘密パターンをマスクへ置換する (破壊的ではなく新しいString)。
///
/// 対象:
/// - `Bearer <token>` / `bearer <token>` (HTTP Authorization形式)
/// - `Authorization: <token>` (ヘッダ行形式。値全体)
/// - `sk-`/`rk-`/`ghp_`/`gho_` 始まりの長いトークン
/// - `api_key`/`apikey`/`api-key` の `=`/`:` 形式の値 (クエリ・JSON・
///   環境変数ダンプを問わず)
/// - GPS座標らしい高精度小数 (`-?数字.小数5桁以上` → `<coord>`。
///   PM決定「GPS=座標完全除外」。通常ログに出る頻度は低く、誤爆して
///   ても診断価値を損なわない)
pub fn scrub_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // マルチバイト境界で始まる場合は対象外のためそのまま流す
        match next_secret(bytes, i) {
            Some((start, end)) => {
                // startまでの間は秘密無し (next_secretは先頭候補を返す)
                out.push_str(&input[i..start]);
                let matched = &input[start..end];
                out.push_str(&mask_matched(matched));
                i = end;
            }
            None => {
                out.push_str(&input[i..]);
                break;
            }
        }
    }
    out
}

/// `input[i..]` における最初の秘密っぽい区間 `(start, end)` を返す。
/// 見つからなければNone (呼び出し側は残り全部をそのまま出す)。
fn next_secret(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut consider = |c: Option<(usize, usize)>| {
        if let Some(candidate) = c {
            if best.map(|b| candidate.0 < b.0).unwrap_or(true) {
                best = Some(candidate);
            }
        }
    };
    consider(find_word_prefixed(bytes, from, "bearer", true));
    consider(find_word_prefixed(bytes, from, "authorization", false));
    consider(find_token_prefixed(
        bytes,
        from,
        &["sk-", "rk-", "ghp_", "gho_"],
    ));
    consider(find_assignment_value(
        bytes,
        from,
        &["api_key", "apikey", "api-key"],
    ));
    consider(find_coordinate(bytes, from));
    best
}

/// GPS座標らしい高精度小数 (`-?\d{1,3}\.\d{5,}`) を検出する。
fn find_coordinate(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        let is_digit_or_dot = b.is_ascii_digit() || b == b'.' || b == b'-';
        if !is_digit_or_dot {
            i += 1;
            continue;
        }
        // 数字/`.`/`-`の連続 区間を取り出して判定
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'-')
        {
            i += 1;
        }
        let candidate = &bytes[start..i];
        if let Some(dot) = candidate.iter().position(|b| *b == b'.') {
            let decimals = candidate.len() - dot - 1;
            let int_len = dot
                - candidate
                    .iter()
                    .position(|b| *b == b'-')
                    .map(|p| p + 1)
                    .unwrap_or(0);
            // 整数部1〜3桁 + 小数5桁以上 (緯度経度の十進度表現)
            if (1..=3).contains(&int_len) && decimals >= COORD_MIN_DECIMALS {
                return Some((start, i));
            }
        }
    }
    None
}

/// 単語 `word` の直後に続くトークンを秘密区間として返す。
/// `word` 自体は区間に含める (マスク表示に「Bearer ****」と出すため —
/// 何が伏せられたか分かる方がトラブルシュートに有用)。
fn find_word_prefixed(
    bytes: &[u8],
    from: usize,
    word: &str,
    require_space: bool,
) -> Option<(usize, usize)> {
    let lower = bytes
        .iter()
        .map(|b| b.to_ascii_lowercase())
        .collect::<Vec<u8>>();
    let needle: Vec<u8> = word.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let mut search = from;
    while let Some(pos) = find_sub(&lower[search..], &needle) {
        let start = search + pos;
        let after_word = start + needle.len();
        let is_word_boundary =
            start == 0 || !(lower[start - 1].is_ascii_alphanumeric() || lower[start - 1] == b'_');
        let mut token_start = after_word;
        // bearerは空白区切り、authorizationは「:」区切りを要求
        let sep_ok = if require_space {
            matches!(lower.get(token_start), Some(b' ') | Some(b'\t'))
        } else {
            matches!(lower.get(token_start), Some(b':'))
        };
        if is_word_boundary && sep_ok {
            while matches!(
                lower.get(token_start),
                Some(b' ') | Some(b'\t') | Some(b':')
            ) {
                token_start += 1;
            }
            if let Some(end) = token_end(bytes, token_start, TOKEN_MIN_LEN) {
                return Some((start, end));
            }
        }
        search = start + 1;
    }
    None
}

/// `sk-` 等の接頭辞を持つトークンそのものを秘密区間として返す。
fn find_token_prefixed(bytes: &[u8], from: usize, prefixes: &[&str]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for prefix in prefixes {
        let needle = prefix.as_bytes();
        let mut search = from;
        while let Some(pos) = find_sub(&bytes[search..], needle) {
            let start = search + pos;
            let is_word_boundary = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric()
                    || bytes[start - 1] == b'_'
                    || bytes[start - 1] == b'-');
            if is_word_boundary {
                if let Some(end) = token_end(bytes, start, TOKEN_MIN_LEN) {
                    let candidate = (start, end);
                    if best.map(|b| candidate.0 < b.0).unwrap_or(true) {
                        best = Some(candidate);
                    }
                    break;
                }
            }
            search = start + 1;
        }
    }
    best
}

/// `api_key=<value>` 系の値部分を秘密区間として返す (キー名は残す)。
/// JSONの `"api_key": "value"` 形式 (閉じ引用符→コロン) にも対応する。
fn find_assignment_value(bytes: &[u8], from: usize, names: &[&str]) -> Option<(usize, usize)> {
    let lower = bytes
        .iter()
        .map(|b| b.to_ascii_lowercase())
        .collect::<Vec<u8>>();
    let mut best: Option<(usize, usize)> = None;
    for name in names {
        let needle: Vec<u8> = name.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let mut search = from;
        while let Some(pos) = find_sub(&lower[search..], &needle) {
            let start = search + pos;
            let is_word_boundary = start == 0
                || !(lower[start - 1].is_ascii_alphanumeric()
                    || lower[start - 1] == b'_'
                    || lower[start - 1] == b'-');
            let after_name = start + needle.len();
            if is_word_boundary {
                // JSON形式の閉じ引用符を読み飛ばす ("api_key": value)
                let mut cursor = after_name;
                if bytes.get(cursor) == Some(&b'"') {
                    cursor += 1;
                }
                if matches!(bytes.get(cursor), Some(b'=') | Some(b':')) {
                    let value_start = cursor + 1;
                    // 値までの区切り空白を読み飛ばす (値区間には含めない)
                    let mut vs = value_start;
                    while matches!(bytes.get(vs), Some(b' ') | Some(b'\t')) {
                        vs += 1;
                    }
                    let candidate = if bytes.get(vs) == Some(&b'"') {
                        // 引用符付き値: 開き・閉じ引用符は残して内側を伏せる
                        find_sub(&bytes[vs + 1..], b"\"")
                            .map(|close| (vs + 1, vs + 1 + close))
                            .filter(|(start, end)| end - start >= 4)
                    } else {
                        token_end(bytes, vs, 4).map(|end| (vs, end))
                    };
                    if let Some(candidate) = candidate {
                        if best.map(|b| candidate.0 < b.0).unwrap_or(true) {
                            best = Some(candidate);
                        }
                    }
                }
            }
            search = start + 1;
        }
    }
    best
}

/// `start` から始まるトークン (区切り文字まで) の終端を返す。
/// トークン長が `min_len` 未満ならNone (誤爆防止)。
fn token_end(bytes: &[u8], start: usize, min_len: usize) -> Option<usize> {
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        let is_token_char = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'-' | b'.' | b'~' | b'+' | b'/' | b'=' | b'"' | b'\''
            );
        if !is_token_char {
            break;
        }
        end += 1;
    }
    (end - start >= min_len).then_some(end)
}

/// マッチした区間のマスク表現。区間内に区切り (空白・`:`) を含む場合は
/// プレフィックス単語と区切り文字を残してトークン部を伏せる
/// (`Bearer ****` / `Authorization: ****`)。座標らしい数値は `<coord>`。
fn mask_matched(matched: &str) -> String {
    if let Some(split) = matched.find([' ', '\t', ':']) {
        let (head, _) = matched.split_at(split);
        let keep_sep = if matched[split..].starts_with(':') {
            ":"
        } else {
            ""
        };
        // headは「bearer」等の単語。大小は元のまま残す
        format!("{head}{keep_sep} ****")
    } else if matched.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        "<coord>".to_string()
    } else {
        "****".to_string()
    }
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 絶対パスらしい部分を `<path>` へ置換する
/// (診断bundleの実パス除外・WP-15。FG-WP16-3 B4でWindows対応・空白混入
/// パスの丸ごとsculpt化)。
///
/// 開始条件 (いずれも語境界 — 英数字・`/`・`\`・`:`・`.` の直後はURL等の
/// 続きのため対象外):
/// - unix絶対: `/` で始まる区間
/// - UNC: `\\` で始まる区間
/// - Windowsドライブ: `[A-Za-z]:[/\\]` (直前が英数字/`_`でないこと —
///   `https://` 等のURL部を誤検出しない)
///
/// 区間はパス構成文字 (`A-Za-z0-9._+-`) と区切り (`/`・`\`) の連続。
/// **空白混入パス**: 区間の直後に空白があり、その後の (パス文字|区切り|空白)
/// の連続の中にさらに区切りが現れる場合はパスが続いているとみなし、
/// 空白ごと丸ごと対象にする (`/home/u/My Photos/x.png` 等。過剰scrubの
/// 誤爆 < 漏れ、の方針 — URLは `:` が連続を打ち切るため保持される)。
pub fn scrub_paths(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(end) = path_candidate_end(bytes, i) {
            // 末尾の区切り/空白は除外済み。実質1文字しか無ければ置換しない
            if end > i + 1 {
                out.push_str("<path>");
                i = end;
                continue;
            }
        }
        // マルチバイト安全: 次の文字境界まで進む
        let mut next = i + 1;
        while next < bytes.len() && (bytes[next] & 0xC0) == 0x80 {
            next += 1;
        }
        let ch_len = next - i;
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// パス構成文字 (`A-Za-z0-9._+-`)。
fn is_path_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'+')
}

/// パス区切り (`/` と `\` — Windows両用)。
fn is_path_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// `i` が絶対パス候補の開始ならその終端 (末尾の区切り・空白を除く) を
/// 返す。候補でなければNone。
fn path_candidate_end(bytes: &[u8], i: usize) -> Option<usize> {
    let boundary = |pos: usize| -> bool {
        pos == 0
            || !(bytes[pos - 1].is_ascii_alphanumeric()
                || bytes[pos - 1] == b'/'
                || bytes[pos - 1] == b'\\'
                || bytes[pos - 1] == b':'
                || bytes[pos - 1] == b'.')
    };
    let is_drive_start = bytes[i].is_ascii_alphabetic()
        && bytes.get(i + 1) == Some(&b':')
        && bytes.get(i + 2).is_some_and(|&b| is_path_sep(b))
        // ドライブ文字の直前は英数字/`_`でないこと (URLスキーム末尾の
        // "s:" 等を誤検出しない)
        && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'));
    let starts = bytes[i] == b'/' && boundary(i)
        || bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'\\') && boundary(i)
        || is_drive_start;
    if !starts {
        return None;
    }
    // ドライブ文字の場合は "C:" の分を読み飛ばしてからrunを消費する
    // (区切りから始まる)
    let mut end = if is_drive_start { i + 2 } else { i };
    loop {
        while end < bytes.len() && (is_path_char(bytes[end]) || is_path_sep(bytes[end])) {
            end += 1;
        }
        // 空白継続判定: 続く (パス文字|区切り|空白) 連続の中に区切りが
        // さらに現れれば、パスが空白を挟んで続いているとみなす
        if end < bytes.len() && bytes[end] == b' ' {
            let mut j = end;
            while j < bytes.len()
                && (is_path_char(bytes[j]) || is_path_sep(bytes[j]) || bytes[j] == b' ')
            {
                j += 1;
            }
            if bytes[end..j].iter().any(|&b| is_path_sep(b)) {
                // 最後の区切りに続く構成要素までをパスに含める
                // (末尾の空白区切りの通常語はパスの外 — 例:
                // "/home/u/My Photos/x.png ignored" の "ignored")
                let last_sep = bytes[end..j].iter().rposition(|&b| is_path_sep(b)).unwrap() + end;
                let mut k = last_sep + 1;
                while k < bytes.len() && (is_path_char(bytes[k]) || is_path_sep(bytes[k])) {
                    k += 1;
                }
                end = k;
                continue;
            }
        }
        break;
    }
    // 末尾の区切り・空白は区切りとして除外
    while end > i && (is_path_sep(bytes[end - 1]) || bytes[end - 1] == b' ') {
        end -= 1;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assetmeta::MetadataColumns;

    #[test]
    fn redact_key_keeps_short_prefix_and_masks_rest() {
        assert_eq!(redact_key("sk-1234567890abcdef"), "sk-1****");
        assert_eq!(redact_key("ghp_16charactersecret"), "ghp_****");
    }

    #[test]
    fn redact_key_masks_short_values_entirely() {
        assert_eq!(redact_key("abc"), "****");
        assert_eq!(redact_key("1234567"), "****");
        assert_eq!(redact_key(""), "****");
    }

    #[test]
    fn scrub_masks_bearer_tokens() {
        let raw = "request failed: Authorization header Bearer sk-live-abcdef123456 rejected";
        let scrubbed = scrub_text(raw);
        assert!(scrubbed.contains("Bearer ****"), "{scrubbed}");
        assert!(!scrubbed.contains("sk-live-abcdef123456"), "{scrubbed}");
    }

    #[test]
    fn scrub_masks_authorization_header_lines() {
        // 実際の形式: "Authorization: <token>"
        let raw = "Authorization: ghp_0123456789abcdefghijklmnop";
        let scrubbed = scrub_text(raw);
        assert!(scrubbed.contains("Authorization: ****"), "{scrubbed}");
        assert!(!scrubbed.contains("ghp_0123456789"), "{scrubbed}");
    }

    #[test]
    fn scrub_masks_bare_key_like_tokens() {
        let raw = "saved key sk-abcdefghijklmnopqrstuvwxyz then done";
        let scrubbed = scrub_text(raw);
        assert!(
            !scrubbed.contains("abcdefghijklmnopqrstuvwxyz"),
            "{scrubbed}"
        );
        assert!(scrubbed.contains("****"), "{scrubbed}");
        // 前後の通常文は保持
        assert!(scrubbed.contains("saved key"), "{scrubbed}");
        assert!(scrubbed.contains("then done"), "{scrubbed}");
    }

    #[test]
    fn scrub_masks_api_key_assignment_values() {
        let raw = "GET /v1/models?api_key=AIzaSyD-1234567890 HTTP/1.1";
        let scrubbed = scrub_text(raw);
        assert!(!scrubbed.contains("AIzaSyD-1234567890"), "{scrubbed}");
        assert!(scrubbed.contains("api_key=****"), "{scrubbed}");
        // JSON形式
        let raw_json = r#"{"api_key": "super-secret-value-123", "model": "x"}"#;
        let scrubbed_json = scrub_text(raw_json);
        assert!(
            !scrubbed_json.contains("super-secret-value-123"),
            "{scrubbed_json}"
        );
        assert!(
            scrubbed_json.contains(r#""api_key": "****""#),
            "{scrubbed_json}"
        );
        assert!(scrubbed_json.contains(r#""model": "x""#), "{scrubbed_json}");
    }

    #[test]
    fn scrub_leaves_ordinary_text_alone() {
        let raw = "scan finished: 100 files, 3 dirs (ok)";
        assert_eq!(scrub_text(raw), raw);
        // 短い「sk-xx」は誤爆しない
        let casual = "task sk-12 queued";
        assert_eq!(scrub_text(casual), casual);
        // 低精度の小数 (比率・ヒット率等) は座標扱いしない
        let ratio = "hit_rate=0.3333 p95=12.5ms";
        assert_eq!(scrub_text(ratio), ratio);
    }

    #[test]
    fn scrub_masks_gps_like_coordinates() {
        // GPS十進度表現 (6桁) は完全除外 (PM決定「GPS=座標完全除外」)
        let raw = "probe coords lat=35.681236 lon=139.767125 done";
        let scrubbed = scrub_text(raw);
        assert!(!scrubbed.contains("35.681236"), "{scrubbed}");
        assert!(!scrubbed.contains("139.767125"), "{scrubbed}");
        assert!(scrubbed.contains("<coord>"), "{scrubbed}");
        assert!(scrubbed.contains("lat=<coord>"), "{scrubbed}");
        // 負の座標 (南緯・西経) も対象
        let south = "position -33.865143, 151.209900 sydney";
        let scrubbed = scrub_text(south);
        assert!(!scrubbed.contains("-33.865143"), "{scrubbed}");
        assert!(!scrubbed.contains("151.209900"), "{scrubbed}");
        // 5桁境界は対象、4桁は対象外
        assert!(!scrub_text("v=1.23456").contains("1.23456"));
        assert!(scrub_text("v=1.2345").contains("1.2345"));
    }

    #[test]
    fn scrub_handles_multiple_secrets_and_multibyte() {
        let raw = "日本語の説明: Bearer abcdef1234567890 と sk-abcdefgh12345678 が混在";
        let scrubbed = scrub_text(raw);
        assert!(!scrubbed.contains("abcdef1234567890"), "{scrubbed}");
        assert!(!scrubbed.contains("sk-abcdefgh12345678"), "{scrubbed}");
        assert!(scrubbed.contains("日本語の説明"), "{scrubbed}");
        assert!(scrubbed.contains("が混在"), "{scrubbed}");
    }

    #[test]
    fn metadata_columns_debug_redacts_gps() {
        let columns = MetadataColumns {
            gps_latitude: Some(35.659722),
            gps_longitude: Some(139.745556),
            camera_make: Some("TestCam".to_string()),
            ..MetadataColumns::default()
        };
        let text = format!("{columns:?}");
        assert!(text.contains("<gps>"), "{text}");
        assert!(!text.contains("35.659"), "{text}");
        assert!(!text.contains("139.745"), "{text}");
        // 非GPS列は通常どおり見える
        assert!(text.contains("TestCam"), "{text}");
    }

    #[test]
    fn metadata_columns_debug_without_gps_is_unchanged_shape() {
        let columns = MetadataColumns::default();
        let text = format!("{columns:?}");
        assert!(!text.contains("Some("), "全列None: {text}");
    }

    #[test]
    fn scrub_paths_replaces_absolute_paths_and_keeps_urls() {
        let raw = "scan /data/photos/sub done; tmp=/tmp/gento-x-1 cache hit";
        let scrubbed = scrub_paths(raw);
        assert!(!scrubbed.contains("/data/photos"), "{scrubbed}");
        assert!(!scrubbed.contains("/tmp/gento-x-1"), "{scrubbed}");
        assert!(scrubbed.contains("scan <path> done"), "{scrubbed}");
        assert!(scrubbed.contains("tmp=<path>"), "{scrubbed}");
        // URLは保持 (スキーム・ホスト部の/は対象外)
        let url = "endpoint https://api.example.com/v1 responded";
        assert_eq!(scrub_paths(url), url);
        // 相対パス・日本語は無傷
        let casual = "file a.png in dir/sub, 件数 3";
        assert_eq!(scrub_paths(casual), casual);
    }

    /// FG-WP16-3 B4: Windows絶対パス (ドライブ文字・UNC) と空白混入unixパスを
    /// 丸ごscrubする (誤爆 < 漏れの方針。URLは保持)。
    #[test]
    fn scrub_paths_covers_windows_drive_unc_and_space_containing_paths() {
        // ドライブ文字 (区切り\\と/両方)
        let drive = "failed to read C:\\Users\\name\\work\\img.png (denied)";
        let scrubbed = scrub_paths(drive);
        assert!(!scrubbed.contains("Users"), "{scrubbed}");
        assert!(!scrubbed.contains("img.png"), "{scrubbed}");
        assert!(scrubbed.contains("<path>"), "{scrubbed}");
        assert!(scrubbed.contains("(denied)"), "{scrubbed}");

        let fwd = "open C:/Users/name/work/x.png ok";
        let scrubbed = scrub_paths(fwd);
        assert!(!scrubbed.contains("Users"), "{scrubbed}");
        assert!(scrubbed.contains("ok"), "{scrubbed}");

        // UNC (\\\\server\\share\\…)
        let unc = "load \\\\fileserver\\photos\\2026\\img.png failed";
        let scrubbed = scrub_paths(unc);
        assert!(!scrubbed.contains("fileserver"), "{scrubbed}");
        assert!(!scrubbed.contains("photos"), "{scrubbed}");
        assert!(scrubbed.contains("failed"), "{scrubbed}");

        // 空白混入unixパス (`/` を含む続きは丸ごと1パス候補)
        let spaced = "cannot stat /home/u/My Photos/x.png ignored";
        let scrubbed = scrub_paths(spaced);
        assert!(!scrubbed.contains("My Photos"), "{scrubbed}");
        assert!(!scrubbed.contains("x.png"), "{scrubbed}");
        assert!(
            scrubbed.contains("cannot stat <path> ignored"),
            "{scrubbed}"
        );

        // URLは相変わらず保持 (ドライブ文字誤検出しない)
        let url = "endpoint https://api.example.com/v1 responded";
        assert_eq!(scrub_paths(url), url);
        // 時刻表現・区切り付き本文はドライブ扱いしない
        let casual = "at 10:00:00 done, ratio 3:4 ok";
        assert_eq!(scrub_paths(casual), casual);
    }
}
