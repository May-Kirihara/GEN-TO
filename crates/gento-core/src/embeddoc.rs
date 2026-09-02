//! 埋め込み用検索文書の決定的構成 (MVP WP-11)。
//!
//! docs/embedding-runtime.md §6「過度に長い画像メタデータをそのまま入力
//! しない。最大トークン数と各情報源の採用上限を設定し、キャプション、
//! タグ、Positive prompt、メモ等を決定的な規則で検索文書へ構成する」
//! の純ドメイン実装。DBアクセスを含まない (gento-dbが
//! `SearchDocInput` を組み立てる)。
//!
//! FTS検索文書 (WP-8/9。gento-db::search) と同じ意味論:
//! - 含めるもの: ファイル名 / キャプション (description + sd_tags) /
//!   Positive prompt (embedded_prompt) / タグ / メモ (user_note)。
//! - 含めないもの: negative prompt・workflow全体 (data-model §9)。
//!   パスも意味検索の対象外とする (WP-11仕様の情報源一覧に含まれない)。
//!
//! 構成規則 (決定的 — 同一入力は常に同一文書):
//! 1. 各情報源を文字数上限 (Unicode scalar value単位) で切り詰め
//!    (タグは個数上限を先に適用してから総文字数上限)。
//! 2. 空でない行のみを改行区切りで連結する。
//! 3. トークン数上限 (manifest最大入力長) はこの層では扱わない —
//!    providerのtokenizer側 `enable_truncation` が正しく適用する
//!    (特殊トークン位置のため。CONV-1教訓)。

/// 埋め込み用検索文書の入力 (1 asset分。gento-dbが組み立てる)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchDocInput {
    /// 表示ファイル名 (presentなFile。呼び出し側で重複排除済み想定)。
    pub filenames: Vec<String>,
    /// キャプション本文 (description + sd_tags。現行のみ)。
    pub captions: Vec<String>,
    /// Positive prompt (embedded_prompt)。negative promptは含めない。
    pub prompt: Option<String>,
    /// タグ (正規化名・安定順)。
    pub tags: Vec<String>,
    /// メモ (user_note)。
    pub note: Option<String>,
}

/// 情報源別の採用上限 (WP-11仕様値)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchDocCaps {
    /// キャプション採用の総文字数上限。
    pub caption_chars: usize,
    /// Positive prompt採用の文字数上限。
    pub prompt_chars: usize,
    /// タグ採用の個数上限。
    pub tag_count: usize,
    /// タグ列採用の総文字数上限 (個数適用後)。
    pub tag_chars: usize,
    /// メモ採用の文字数上限。
    pub note_chars: usize,
    /// ファイル名採用の総文字数上限 (複数名の連結後)。
    pub filename_chars: usize,
}

impl Default for SearchDocCaps {
    fn default() -> Self {
        // WP-11仕様: caption 512字・prompt 512字・タグ30個・note 256字。
        // ファイル名は仕様上上限未指定のため運用上十分な512字。
        Self {
            caption_chars: 512,
            prompt_chars: 512,
            tag_count: 30,
            tag_chars: 512,
            note_chars: 256,
            filename_chars: 512,
        }
    }
}

/// 文字列をUnicode scalar value単位で先頭から `max_chars` に切り詰める。
/// char_indicesでバイト境界を安全に跨ぐ (日本語の途中バイト切断防止)。
fn truncate_chars(text: &str, max_chars: usize) -> &str {
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut end = text.len();
    for (index, (offset, _)) in text.char_indices().enumerate() {
        if index == max_chars {
            end = offset;
            break;
        }
    }
    &text[..end]
}

/// キャプション群を上限内で先頭から連結する。1件目だけで上限に
/// 達する場合は1件目の切り詰めのみ (以降を足さない — 決定的)。
fn join_captions(captions: &[String], max_chars: usize) -> String {
    let mut used = 0usize;
    let mut parts: Vec<&str> = Vec::new();
    for caption in captions {
        if used >= max_chars {
            break;
        }
        let remaining = max_chars - used;
        let part = truncate_chars(caption, remaining);
        if part.is_empty() {
            break;
        }
        parts.push(part);
        used += part.chars().count();
        // 複数キャプション間の区切り改行1字も上限に数える。
        used += 1;
    }
    parts.join("\n")
}

/// 1 assetの検索文書を構成する。入力の同一性に対して決定的
/// (順序は呼び出し側の安定順に従う)。
pub fn compose_search_document(input: &SearchDocInput, caps: &SearchDocCaps) -> String {
    let mut lines: Vec<String> = Vec::new();

    let filenames_joined = input.filenames.join("\n");
    let filename_line = truncate_chars(&filenames_joined, caps.filename_chars);
    if !filename_line.is_empty() {
        lines.push(filename_line.to_string());
    }

    let caption_line = join_captions(&input.captions, caps.caption_chars);
    if !caption_line.is_empty() {
        lines.push(caption_line);
    }

    if let Some(prompt) = &input.prompt {
        let prompt_line = truncate_chars(prompt, caps.prompt_chars);
        if !prompt_line.is_empty() {
            lines.push(prompt_line.to_string());
        }
    }

    let tags_joined = input
        .tags
        .iter()
        .take(caps.tag_count)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let tag_line = truncate_chars(&tags_joined, caps.tag_chars);
    if !tag_line.is_empty() {
        lines.push(tag_line.to_string());
    }

    if let Some(note) = &input.note {
        let note_line = truncate_chars(note, caps.note_chars);
        if !note_line.is_empty() {
            lines.push(note_line.to_string());
        }
    }

    lines.join("\n")
}

/// 既定上限で構成する (呼び出し側の簡便ラッパ)。
pub fn compose_search_document_default(input: &SearchDocInput) -> String {
    compose_search_document(input, &SearchDocCaps::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        filenames: &[&str],
        captions: &[&str],
        prompt: Option<&str>,
        tags: &[&str],
        note: Option<&str>,
    ) -> SearchDocInput {
        SearchDocInput {
            filenames: filenames.iter().map(|s| s.to_string()).collect(),
            captions: captions.iter().map(|s| s.to_string()).collect(),
            prompt: prompt.map(|s| s.to_string()),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            note: note.map(|s| s.to_string()),
        }
    }

    #[test]
    fn composes_all_sources_in_fixed_order() {
        let doc = compose_search_document_default(&input(
            &["photo.png"],
            &["夕暮れの海岸"],
            Some("sunset, beach, 1girl"),
            &["風景", "夕方"],
            Some("夏休みの一枚"),
        ));
        assert_eq!(
            doc,
            "photo.png\n夕暮れの海岸\nsunset, beach, 1girl\n風景 夕方\n夏休みの一枚"
        );
    }

    #[test]
    fn empty_sources_produce_empty_document() {
        let doc = compose_search_document_default(&input(&[], &[], None, &[], None));
        assert_eq!(doc, "");
        // 空文字列のみの情報源も空文書
        let doc = compose_search_document_default(&input(&[""], &[""], Some(""), &[""], Some("")));
        assert_eq!(doc, "");
    }

    #[test]
    fn omits_absent_sources_without_blank_lines() {
        let doc = compose_search_document_default(&input(&["a.png"], &[], None, &["tag1"], None));
        assert_eq!(doc, "a.png\ntag1");
    }

    #[test]
    fn caption_char_cap_truncates_on_char_boundary() {
        let long_caption = "あ".repeat(600);
        let doc = compose_search_document_default(&input(&[], &[&long_caption], None, &[], None));
        assert_eq!(doc.chars().count(), 512, "512字で切り詰め");
        assert!(doc.chars().all(|c| c == 'あ'));
        // ASCIIでも文字数基準
        let ascii = "a".repeat(600);
        let doc = compose_search_document_default(&input(&[], &[&ascii], None, &[], None));
        assert_eq!(doc.chars().count(), 512);
    }

    #[test]
    fn multiple_captions_fill_cap_together() {
        let caption_a = "あ".repeat(300);
        let caption_i = "い".repeat(300);
        let doc = compose_search_document_default(&input(
            &[],
            &[&caption_a, &caption_i],
            None,
            &[],
            None,
        ));
        // 1件目300字 + 区切り1字 + 2件目の先頭211字 = 512
        assert_eq!(doc.chars().count(), 512);
        assert!(doc.contains('あ'));
        assert!(doc.contains('い'));
    }

    #[test]
    fn prompt_and_note_caps() {
        let prompt = "p".repeat(700);
        let note = "n".repeat(300);
        let doc =
            compose_search_document_default(&input(&[], &[], Some(&prompt), &[], Some(&note)));
        let lines: Vec<&str> = doc.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars().count(), 512, "prompt 512字");
        assert_eq!(lines[1].chars().count(), 256, "note 256字");
    }

    #[test]
    fn tag_count_cap_takes_first_thirty() {
        let tags: Vec<String> = (0..50).map(|i| format!("tag{i:02}")).collect();
        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
        let doc = compose_search_document_default(&input(&[], &[], None, &tag_refs, None));
        let tag_line = doc;
        let taken: Vec<&str> = tag_line.split(' ').collect();
        assert_eq!(taken.len(), 30, "タグは30個まで");
        assert_eq!(taken[0], "tag00");
        assert_eq!(taken[29], "tag29");
        assert!(!tag_line.contains("tag30"));
    }

    #[test]
    fn tag_char_cap_applies_after_count_cap() {
        let tags: Vec<String> = (0..30).map(|_| "あ".repeat(30)).collect();
        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
        let doc = compose_search_document_default(&input(&[], &[], None, &tag_refs, None));
        // 30字×30個=900字 > 512 → 総文字数上限 (空白区切り込み)
        assert!(doc.chars().count() <= 512, "{}", doc.chars().count());
        assert!(doc.chars().count() >= 500, "上限いっぱいまで採用");
    }

    #[test]
    fn filename_cap_applies_to_joined_names() {
        let names: Vec<String> = (0..40).map(|i| format!("file{i:03}.png")).collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let doc = compose_search_document_default(&input(&name_refs, &[], None, &[], None));
        assert!(doc.chars().count() <= 512, "{}", doc.chars().count());
        assert!(doc.starts_with("file000.png"));
    }

    #[test]
    fn composition_is_deterministic() {
        let inp = input(
            &["x.png", "x (1).png"],
            &["説明A", "sd, tags, here"],
            Some("masterpiece, best quality"),
            &["夜", "街", "neo"],
            Some("覚書"),
        );
        let first = compose_search_document_default(&inp);
        let second = compose_search_document_default(&inp);
        assert_eq!(first, second);
        // source_hash (BLAKE3) の入力として安定していることを擬似検証
        assert!(first.contains("x.png"));
    }

    #[test]
    fn custom_caps_are_honored() {
        let caps = SearchDocCaps {
            caption_chars: 10,
            prompt_chars: 5,
            tag_count: 2,
            tag_chars: 100,
            note_chars: 3,
            filename_chars: 4,
        };
        let doc = compose_search_document(
            &input(
                &["abcdef.png"],
                &["01234567890123"],
                Some("prompt!"),
                &["t1", "t2", "t3"],
                Some("note!"),
            ),
            &caps,
        );
        assert_eq!(
            doc, "abcd\n0123456789\npromp\nt1 t2\nnot",
            "各上限が文字数・個数単位で適用される"
        );
    }

    #[test]
    fn default_caps_match_wp11_spec() {
        let caps = SearchDocCaps::default();
        assert_eq!(caps.caption_chars, 512);
        assert_eq!(caps.prompt_chars, 512);
        assert_eq!(caps.tag_count, 30);
        assert_eq!(caps.note_chars, 256);
    }
}
