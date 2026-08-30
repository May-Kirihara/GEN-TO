//! カタログ: レコード保持・安定順序・ページング・仮想カタログ
//! (docs/implementation-plan.md §4.2, §9 / WP-2)。
//!
//! Tauriに依存しない。IPC DTOの直列化形状は docs/dev/api-contract.md が契約。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::AssetId;

/// 1ページあたりの最大件数。サーバ側で強制する
/// (5万件をフロントエンドへ一括転送しない、本体計画 §9)。
pub const MAX_PAGE_LIMIT: u32 = 500;

/// 仮想5万件カタログ (UI負荷試験用) の構築。
/// devtools機能: debug buildまたは `--features devtools` でのみコンパイルされる
/// (WP-0。通常releaseビルドに計測用コードは含めない)。
#[cfg(any(debug_assertions, feature = "devtools"))]
pub mod virtual_catalog {
    use super::ImageRecord;

    /// 仮想カタログの既定規模 (docs/implementation-plan.md §2)。
    pub const VIRTUAL_CATALOG_COUNT: usize = 50_000;

    /// [`build_virtual`] のエラー: 実カタログが空 (未走査)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EmptyBaseCatalogError;

    impl std::fmt::Display for EmptyBaseCatalogError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "real catalog is empty; run a scan before building the virtual catalog"
            )
        }
    }

    impl std::error::Error for EmptyBaseCatalogError {}

    /// 仮想カタログ生成 (docs/implementation-plan.md §4.2)。
    ///
    /// - 実レコードを循環参照 (source_path共有)
    /// - 一意IDは base 最後のID + 1 から連番
    /// - 表示名は `virtual-{index:05}-{元表示名}`
    /// - rating = index % 6、triage は index % 3 で決定的に変化
    /// - 順序は生成順 (= ID昇順) で安定
    pub fn build_virtual(
        base: &[ImageRecord],
        count: usize,
    ) -> Result<Vec<ImageRecord>, EmptyBaseCatalogError> {
        let last_id = base.last().ok_or(EmptyBaseCatalogError)?.id.value();
        // IDは走査スレッドが1から採番するため+1が桁あふれする前提は不存在。
        let first_id = last_id
            .checked_add(1)
            .expect("scan-assigned asset ids are far below u64::MAX");

        let out = (0..count)
            .map(|index| {
                let src = &base[index % base.len()];
                ImageRecord {
                    id: super::AssetId::new(first_id + index as u64),
                    source_path: src.source_path.clone(),
                    display_name: format!("virtual-{index:05}-{}", src.display_name),
                    width: src.width,
                    height: src.height,
                    byte_size: src.byte_size,
                    mtime_ms: src.mtime_ms,
                    state: src.state,
                    thumb_state: super::ThumbnailState::Missing,
                    rating: (index % 6) as u8,
                    triage: match index % 3 {
                        0 => super::Triage::Undecided,
                        1 => super::Triage::Keep,
                        _ => super::Triage::Reject,
                    },
                }
            })
            .collect();
        Ok(out)
    }
}

#[cfg(any(debug_assertions, feature = "devtools"))]
pub use virtual_catalog::{build_virtual, EmptyBaseCatalogError, VIRTUAL_CATALOG_COUNT};

/// 画像レコードの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageState {
    Ready,
    Unreadable,
    Unsupported,
    Corrupt,
}

/// サムネイル状態。WP-3で使用し、Phase 2では Missing 固定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbnailState {
    Missing,
    Queued,
    Ready,
    Failed,
}

/// 仮のトリアージ判定 (仕様草案の採用・除外状態のスパイク用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Triage {
    Undecided,
    Keep,
    Reject,
}

/// カタログの稼働モード。Real = 実走査結果、Virtual = UI負荷試験用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogMode {
    Real,
    Virtual,
}

/// カタログ内の1画像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRecord {
    pub id: AssetId,
    /// 実ファイルパス (内部用。IPC DTOには含めない)。
    pub source_path: PathBuf,
    pub display_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_size: u64,
    /// キャッシュキー用の更新日時 (Unixエポックミリ秒、内部用)。
    pub mtime_ms: u64,
    pub state: ImageState,
    pub thumb_state: ThumbnailState,
    /// 仮の評価 (0..=5)。
    pub rating: u8,
    pub triage: Triage,
}

/// IPC用リスト項目DTO (docs/implementation-plan.md §9 DTO候補)。
/// camelCaseは本体計画§9の型定義に合わせる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageListItem {
    pub id: AssetId,
    pub display_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_size: u64,
    pub state: ImageState,
    pub thumbnail_state: ThumbnailState,
}

impl ImageRecord {
    /// IPC DTOへの変換。内部フィールド (source_path, mtime_ms) は出さない。
    pub fn to_list_item(&self) -> ImageListItem {
        ImageListItem {
            id: self.id,
            display_name: self.display_name.clone(),
            width: self.width,
            height: self.height,
            byte_size: self.byte_size,
            state: self.state,
            thumbnail_state: self.thumb_state,
        }
    }
}

/// ページ取得結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResult {
    pub generation: u64,
    pub total: u32,
    pub offset: u32,
    pub items: Vec<ImageListItem>,
    pub has_more: bool,
}

/// ページ取得エラー。IPCでは `{kind: "staleGeneration"|...}` 形状で直列化する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PageError {
    StaleGeneration { requested: u64, current: u64 },
    InvalidRange { message: String },
}

/// カタログのスナップショット状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogState {
    /// 稼働モードに応じた「表示中」のレコード列 (常にソート済み)。
    pub records: Vec<ImageRecord>,
    pub generation: u64,
    pub mode: CatalogMode,
    pub scanning: bool,
}

impl CatalogState {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            generation: 1,
            mode: CatalogMode::Real,
            scanning: false,
        }
    }

    /// 安定順序: パス文字列 (UTF-8バイト) 昇順。
    /// 非UTF-8パスはto_string_lossyの置換後で比較する。
    pub fn sort_records(&mut self) {
        self.records.sort_by(|a, b| {
            a.source_path
                .to_string_lossy()
                .cmp(&b.source_path.to_string_lossy())
        });
    }

    pub fn total(&self) -> u32 {
        self.records.len().min(u32::MAX as usize) as u32
    }

    /// ページ取得。limit は 1..=MAX_PAGE_LIMIT を強制する。
    /// offset が total を超える場合は空ページ (エラーにしない)。
    pub fn page(&self, generation: u64, offset: u32, limit: u32) -> Result<PageResult, PageError> {
        if generation != self.generation {
            return Err(PageError::StaleGeneration {
                requested: generation,
                current: self.generation,
            });
        }
        if limit == 0 {
            return Err(PageError::InvalidRange {
                message: "limit must be >= 1".to_string(),
            });
        }
        if limit > MAX_PAGE_LIMIT {
            return Err(PageError::InvalidRange {
                message: format!("limit must be <= {MAX_PAGE_LIMIT}"),
            });
        }

        let total = self.total();
        if offset >= total {
            return Ok(PageResult {
                generation,
                total,
                offset,
                items: Vec::new(),
                has_more: false,
            });
        }

        let start = offset as usize;
        let end = start.saturating_add(limit as usize).min(self.records.len());
        let items = self.records[start..end]
            .iter()
            .map(|r| r.to_list_item())
            .collect();
        Ok(PageResult {
            generation,
            total,
            offset,
            items,
            has_more: end < self.records.len(),
        })
    }
}

impl Default for CatalogState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, id: u64) -> ImageRecord {
        ImageRecord {
            id: AssetId::new(id),
            source_path: PathBuf::from(path),
            display_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            width: Some(100),
            height: Some(200),
            byte_size: 1234,
            mtime_ms: 1_700_000_000_000,
            state: ImageState::Ready,
            thumb_state: ThumbnailState::Missing,
            rating: 0,
            triage: Triage::Undecided,
        }
    }

    fn catalog_with(paths: &[&str]) -> CatalogState {
        let mut state = CatalogState::new();
        for (i, p) in paths.iter().enumerate() {
            state.records.push(record(p, (i + 1) as u64));
        }
        state.sort_records();
        state
    }

    #[test]
    fn page_basic_paging() {
        let state = catalog_with(&[
            "a.png", "b.png", "c.png", "d.png", "e.png", "f.png", "g.png", "h.png", "i.png",
            "j.png",
        ]);
        let gen = state.generation;

        let first = state.page(gen, 0, 5).unwrap();
        assert_eq!(first.items.len(), 5);
        assert!(first.has_more);
        assert_eq!(first.offset, 0);
        assert_eq!(first.total, 10);

        // 倍数ちょうどの末尾ページ
        let last = state.page(gen, 5, 5).unwrap();
        assert_eq!(last.items.len(), 5);
        assert!(!last.has_more);

        // 末尾半端
        let tail = state.page(gen, 8, 5).unwrap();
        assert_eq!(tail.items.len(), 2);
        assert!(!tail.has_more);
        assert_eq!(tail.items[0].display_name, "i.png");
        assert_eq!(tail.items[1].display_name, "j.png");
    }

    #[test]
    fn page_offset_beyond_total_returns_empty_page() {
        let state = catalog_with(&["a.png", "b.png"]);
        let gen = state.generation;

        let at_end = state.page(gen, 2, 100).unwrap();
        assert_eq!(at_end.items.len(), 0);
        assert!(!at_end.has_more);
        assert_eq!(at_end.total, 2);

        let beyond = state.page(gen, 999, 100).unwrap();
        assert_eq!(beyond.items.len(), 0);
        assert!(!beyond.has_more);
    }

    #[test]
    fn page_rejects_limit_zero() {
        let state = catalog_with(&["a.png"]);
        assert!(matches!(
            state.page(state.generation, 0, 0),
            Err(PageError::InvalidRange { .. })
        ));
    }

    #[test]
    fn page_rejects_limit_over_500() {
        let state = catalog_with(&["a.png"]);
        assert!(matches!(
            state.page(state.generation, 0, 501),
            Err(PageError::InvalidRange { .. })
        ));
        // 境界値ちょうどは許可
        assert!(state.page(state.generation, 0, MAX_PAGE_LIMIT).is_ok());
    }

    #[test]
    fn page_rejects_stale_generation() {
        let state = catalog_with(&["a.png"]);
        let err = state.page(state.generation + 7, 0, 10).unwrap_err();
        assert_eq!(
            err,
            PageError::StaleGeneration {
                requested: state.generation + 7,
                current: state.generation,
            }
        );
    }

    #[test]
    fn records_sorted_by_utf8_path_bytes() {
        // 挿入順はソート後と無関係な順序で与える
        let mut state = CatalogState::new();
        for (i, p) in ["b/c.png", "a.png", "b/a.png", "B.png", "あ.png", "a++.png"]
            .iter()
            .enumerate()
        {
            state.records.push(record(p, (i + 1) as u64));
        }
        state.sort_records();
        let paths: Vec<String> = state
            .records
            .iter()
            .map(|r| r.source_path.to_string_lossy().into_owned())
            .collect();
        // UTF-8バイト昇順: 'B'(0x42) < 'a'(0x61) < 'a+' < 'b' < 'あ'(0xE3...)
        assert_eq!(
            paths,
            vec!["B.png", "a++.png", "a.png", "b/a.png", "b/c.png", "あ.png"]
        );
    }

    #[test]
    fn stable_order_across_rebuilds() {
        let set = ["x1.png", "x2.png", "x10.png", "m.png", "z.png"];
        let mut a = CatalogState::new();
        let mut b = CatalogState::new();
        // 異なる挿入順で構築 (IDは採番順依存で変わるが、順序は同一になるべき)
        for (i, p) in set.iter().enumerate() {
            a.records.push(record(p, (i + 1) as u64));
        }
        for (i, p) in set.iter().rev().enumerate() {
            b.records.push(record(p, (i + 1) as u64));
        }
        a.sort_records();
        b.sort_records();
        let names = |s: &CatalogState| -> Vec<String> {
            s.page(s.generation, 0, 100)
                .unwrap()
                .items
                .iter()
                .map(|i| i.display_name.clone())
                .collect()
        };
        assert_eq!(names(&a), names(&b));
    }

    #[test]
    fn batch_insert_keeps_sorted_view() {
        let mut state = catalog_with(&["m.png", "z.png"]);
        // 走査バッチ相当: 末尾へ追加して再ソート
        state.records.push(record("a.png", 99));
        state.records.push(record("n.png", 100));
        state.sort_records();
        let page = state.page(state.generation, 0, 100).unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.display_name.as_str()).collect();
        assert_eq!(names, vec!["a.png", "m.png", "n.png", "z.png"]);
    }

    #[test]
    fn dto_serializes_camel_case_with_string_id() {
        let item = record("dir/img.png", 42).to_list_item();
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "42",
                "displayName": "img.png",
                "width": 100,
                "height": 200,
                "byteSize": 1234,
                "state": "ready",
                "thumbnailState": "missing",
            })
        );
        let back: ImageListItem = serde_json::from_value(json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn page_error_serialization_shapes() {
        let stale = PageError::StaleGeneration {
            requested: 3,
            current: 4,
        };
        assert_eq!(
            serde_json::to_value(&stale).unwrap(),
            serde_json::json!({"kind": "staleGeneration", "requested": 3, "current": 4})
        );
        let invalid = PageError::InvalidRange {
            message: "limit must be <= 500".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&invalid).unwrap(),
            serde_json::json!({"kind": "invalidRange", "message": "limit must be <= 500"})
        );
    }

    // ===== 仮想カタログ (devtools機能。debug/devtoolsビルドのみ) =====

    #[cfg(any(debug_assertions, feature = "devtools"))]
    mod virtual_tests {
        use super::super::virtual_catalog::*;
        use super::*;

        #[test]
        fn virtual_catalog_count_ids_and_paths() {
            let base = [
                record("b.png", 10),
                record("c.png", 11),
                record("d.png", 12),
            ];
            let v = build_virtual(&base, 10).unwrap();
            assert_eq!(v.len(), 10);

            // ID一意、base最後のID+1から連番
            let ids: Vec<u64> = v.iter().map(|r| r.id.value()).collect();
            let expected: Vec<u64> = (13..=22).collect();
            assert_eq!(ids, expected);

            // source_pathは循環参照
            for (index, rec) in v.iter().enumerate() {
                assert_eq!(rec.source_path, base[index % base.len()].source_path);
                assert_eq!(rec.width, Some(100));
                assert_eq!(rec.byte_size, 1234);
            }
        }

        #[test]
        fn virtual_catalog_display_name_rating_triage() {
            let base = [record("b.png", 10), record("c.png", 11)];
            let v = build_virtual(&base, 8).unwrap();
            assert_eq!(v[0].display_name, "virtual-00000-b.png");
            assert_eq!(v[1].display_name, "virtual-00001-c.png");
            assert_eq!(v[2].display_name, "virtual-00002-b.png");
            assert_eq!(v[7].display_name, "virtual-00007-c.png");

            for (index, rec) in v.iter().enumerate() {
                assert_eq!(rec.rating as usize, index % 6);
                let expected_triage = match index % 3 {
                    0 => Triage::Undecided,
                    1 => Triage::Keep,
                    _ => Triage::Reject,
                };
                assert_eq!(rec.triage, expected_triage);
                assert_eq!(rec.thumb_state, ThumbnailState::Missing);
            }
        }

        #[test]
        fn virtual_catalog_requires_non_empty_base() {
            let err = build_virtual(&[], 5).unwrap_err();
            assert_eq!(err, EmptyBaseCatalogError);
            assert!(!err.to_string().is_empty());
        }

        #[test]
        fn virtual_catalog_50k_last_page_and_uniqueness() {
            let base = [record("b.png", 1), record("c.png", 2)];
            let v = build_virtual(&base, VIRTUAL_CATALOG_COUNT).unwrap();
            assert_eq!(v.len(), VIRTUAL_CATALOG_COUNT);

            let mut state = CatalogState::new();
            state.records = v;
            state.mode = CatalogMode::Virtual;
            assert_eq!(state.total(), 50_000);

            // ID一意性 (全件)
            let mut ids: Vec<u64> = state.records.iter().map(|r| r.id.value()).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), VIRTUAL_CATALOG_COUNT);
            assert_eq!(ids[0], 3);
            assert_eq!(*ids.last().unwrap(), 2 + VIRTUAL_CATALOG_COUNT as u64);

            // 末尾ページ
            let page = state.page(state.generation, 49_900, 200).unwrap();
            assert_eq!(page.total, 50_000);
            assert_eq!(page.items.len(), 100);
            assert!(!page.has_more);
            assert_eq!(page.items[99].display_name, "virtual-49999-c.png");

            // 先頭ページはID昇順 (生成順)
            let first = state.page(state.generation, 0, 3).unwrap();
            let first_ids: Vec<String> = first.items.iter().map(|i| i.id.to_string()).collect();
            assert_eq!(first_ids, vec!["3", "4", "5"]);
        }
    }
}
