//! USearch HNSW索引ラッパ (MVP WP-12)。
//!
//! - ベクトル正本はSQLite embeddings表 (WP-11)。本moduleの索引は
//!   **派生データ** — 破損・喪失はSQLiteから全再構築で復旧する。
//! - モデルidentity (name+revision+dims) ごとに索引を分離する
//!   (設計§10: 異なるモデル/revision/次元の混在禁止)。保存は
//!   `data_dir/indexes/{model-id}/` (呼び出し側が決める) へ:
//!   - `index.usearch` — HNSW索引本体 (usearch save/load)
//!   - `identity.json` — model/revision/dims/countのsidecar (混在防止の正本)
//! - metric = Cos (ベクトルはL2正規化済みのため distance = 1 - cosine)。
//!   スカラーはF32 (量子化なし — 品質優先。メモリ実測は
//!   docs/dev/measurements/wp12-usearch-50k.md)。
//! - 連結度・expansionは控えめ既定 (§7)。詳細設定余地として定数を公開。
//!
//! このmoduleはDBに依存しない (ベクトルの供給は呼び出し側。crateの
//! 「ワーカーを軽く保つ」規約を維持)。

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// HNSW連結度 (1ノードあたりの辺数)。控えめ既定。
pub const DEFAULT_CONNECTIVITY: usize = 16;
/// 構築時の候補展開幅。大きいほど再現率↑・構築時間↑。
pub const DEFAULT_EXPANSION_ADD: usize = 96;
/// 検索時の候補展開幅。5万件計測 (measurements/wp12-usearch-50k.md) で
/// recall@10 >= 0.95 を担保する水準 (24だと0.92まで落ちる)。
pub const DEFAULT_EXPANSION_SEARCH: usize = 64;

/// 索引のモデルidentity (§10混在防止の検証単位)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexIdentity {
    pub model_name: String,
    pub model_revision: String,
    pub dimensions: usize,
}

/// identity.json (索引sidecar)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IdentityFile {
    #[serde(flatten)]
    identity: IndexIdentity,
    /// 保存時点のベクトル数 (SQLiteとの件数照合に使う。呼び出し側判断)。
    count: usize,
    /// sidecar書式version。
    version: u32,
}

const IDENTITY_VERSION: u32 = 1;
const INDEX_FILE: &str = "index.usearch";
const IDENTITY_FILE: &str = "identity.json";

/// USearch索引の操作エラー。
#[derive(Debug)]
pub enum VectorIndexError {
    /// usearch本体のエラー (cxx例外の文字列化)。
    Usearch(String),
    /// 索引ファイルと期待identityが一致しない (§10混入防止)。
    IdentityMismatch {
        found: IndexIdentity,
        expected: IndexIdentity,
    },
    Io(String),
}

impl std::fmt::Display for VectorIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorIndexError::Usearch(message) => write!(f, "索引エラー: {message}"),
            VectorIndexError::IdentityMismatch { found, expected } => write!(
                f,
                "索引のモデルidentityが一致しません: 索引={}:{} ({}次元) / 期待={}:{} ({}次元)",
                found.model_name,
                found.model_revision,
                found.dimensions,
                expected.model_name,
                expected.model_revision,
                expected.dimensions
            ),
            VectorIndexError::Io(message) => write!(f, "索引ファイルエラー: {message}"),
        }
    }
}

impl std::error::Error for VectorIndexError {}

impl From<cxx::Exception> for VectorIndexError {
    fn from(exception: cxx::Exception) -> Self {
        VectorIndexError::Usearch(exception.to_string())
    }
}

fn io_error(error: std::io::Error) -> VectorIndexError {
    VectorIndexError::Io(error.to_string())
}

fn options_for(identity: &IndexIdentity) -> usearch::ffi::IndexOptions {
    usearch::ffi::IndexOptions {
        dimensions: identity.dimensions,
        metric: usearch::ffi::MetricKind::Cos,
        quantization: usearch::ffi::ScalarKind::F32,
        connectivity: DEFAULT_CONNECTIVITY,
        expansion_add: DEFAULT_EXPANSION_ADD,
        expansion_search: DEFAULT_EXPANSION_SEARCH,
        multi: false,
    }
}

/// 検索結果1件 (key = asset id。similarity = cosine類似度 1-distance)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbor {
    pub key: i64,
    pub similarity: f32,
}

/// USearch索引 (モデル単位)。
pub struct VectorIndex {
    index: usearch::Index,
    identity: IndexIdentity,
}

impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("identity", &self.identity)
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl VectorIndex {
    /// 空の索引を作成する (全再構築用)。
    pub fn new(identity: IndexIdentity, capacity: usize) -> Result<Self, VectorIndexError> {
        let index = usearch::Index::new(&options_for(&identity))?;
        if capacity > 0 {
            index.reserve(capacity)?;
        }
        Ok(Self { index, identity })
    }

    /// 保存済み索引を開く。identity (sidecar + 索引ヘッダ両方) が期待と
    /// 一致しない場合はErr (混在防止)。
    pub fn open(dir: &Path, expected: &IndexIdentity) -> Result<Self, VectorIndexError> {
        let identity_path = dir.join(IDENTITY_FILE);
        let index_path = dir.join(INDEX_FILE);
        let sidecar_bytes = std::fs::read(&identity_path).map_err(io_error)?;
        let sidecar: IdentityFile = serde_json::from_slice(&sidecar_bytes).map_err(|e| {
            VectorIndexError::Io(format!(
                "identity.jsonの解析に失敗しました ({}): {e}",
                identity_path.display()
            ))
        })?;
        if sidecar.identity != *expected {
            return Err(VectorIndexError::IdentityMismatch {
                found: sidecar.identity,
                expected: expected.clone(),
            });
        }
        // ヘッダも検証 (sidecarだけ信用しない — 破損検出の二重化)
        let metadata = usearch::Index::metadata(&index_path.to_string_lossy())?;
        if metadata.dimensions as usize != expected.dimensions {
            return Err(VectorIndexError::IdentityMismatch {
                found: IndexIdentity {
                    model_name: expected.model_name.clone(),
                    model_revision: expected.model_revision.clone(),
                    dimensions: metadata.dimensions as usize,
                },
                expected: expected.clone(),
            });
        }
        let index = usearch::Index::new(&options_for(expected))?;
        index.load(&index_path.to_string_lossy())?;
        Ok(Self {
            index,
            identity: expected.clone(),
        })
    }

    pub fn identity(&self) -> &IndexIdentity {
        &self.identity
    }

    /// 索引内のベクトル数。
    pub fn len(&self) -> usize {
        self.index.size()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 索引の概算メモリ使用量 (バイト)。
    pub fn memory_usage(&self) -> usize {
        self.index.memory_usage()
    }

    pub fn contains(&self, asset_id: i64) -> bool {
        self.index.contains(asset_id as u64)
    }

    /// 容量を追加予定件数分確保する (構築前のreserve)。
    pub fn reserve(&self, capacity: usize) -> Result<(), VectorIndexError> {
        Ok(self.index.reserve(capacity)?)
    }

    /// ベクトルを1件追加する。次元不一致はErr。容量を超える場合は
    /// 自動で拡張する (usearchはreserve先行必須 — 増分追加のたびに
    /// 呼び出し側で容量管理を強いる設計になっているため、ここで倍加
    /// 方針を吸収する)。
    pub fn add(&self, asset_id: i64, vector: &[f32]) -> Result<(), VectorIndexError> {
        self.check_dims(vector)?;
        if self.len() >= self.index.capacity() {
            let grown = self.index.capacity().max(1024).saturating_mul(2);
            self.index.reserve(grown)?;
        }
        Ok(self.index.add(asset_id as u64, vector)?)
    }

    /// 近傍検索 (類似度降順。similarity = 1 - cos距離)。
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<Neighbor>, VectorIndexError> {
        self.check_dims(query)?;
        // usearchは容量0の索引への検索をErrにする (reserve先行要求)。
        // 空索引 = 空結果として扱う方が呼び出し側の自然な期待に合う。
        if self.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let matches = self.index.search(query, k)?;
        Ok(Self::neighbors_of(matches))
    }

    /// 構造フィルタ適用済みの近傍検索 (allowed内のkeyのみ。§ハイブリッド
    /// 検索の構造フィルタ)。5万規模のHashSet照合で十分高速 (実測は
    /// measurements参照)。
    pub fn filtered_search(
        &self,
        query: &[f32],
        k: usize,
        allowed: &HashSet<i64>,
    ) -> Result<Vec<Neighbor>, VectorIndexError> {
        self.check_dims(query)?;
        if self.is_empty() || k == 0 || allowed.is_empty() {
            return Ok(Vec::new());
        }
        let matches = self
            .index
            .filtered_search(query, k, |key| allowed.contains(&(key as i64)))?;
        Ok(Self::neighbors_of(matches))
    }

    /// 索引を空にする (再構築の下準備。ディスク上のファイルは呼び出し側で
    /// 扱う)。
    pub fn reset(&self) -> Result<(), VectorIndexError> {
        Ok(self.index.reset()?)
    }

    /// ディスクへ保存する (tmp書き込み → rename のアトミック切替。
    /// crashしても旧ファイルか新ファイルのいずれかが完全な形で残る)。
    pub fn save(&self, dir: &Path) -> Result<(), VectorIndexError> {
        std::fs::create_dir_all(dir).map_err(io_error)?;
        let index_tmp = dir.join(format!("{INDEX_FILE}.tmp"));
        let identity_tmp = dir.join(format!("{IDENTITY_FILE}.tmp"));
        let identity_final = dir.join(IDENTITY_FILE);
        let index_final = dir.join(INDEX_FILE);
        self.index.save(&index_tmp.to_string_lossy())?;
        let sidecar = IdentityFile {
            identity: self.identity.clone(),
            count: self.len(),
            version: IDENTITY_VERSION,
        };
        let bytes = serde_json::to_vec_pretty(&sidecar)
            .map_err(|e| VectorIndexError::Io(format!("identity.json直列化失敗: {e}")))?;
        std::fs::write(&identity_tmp, bytes).map_err(io_error)?;
        // identityを先に確定 (索引本体が新でもsidecarが旧なら件数照合で
        // 再構築に落ちる — 過渡状態は常に安全側)
        std::fs::rename(&identity_tmp, &identity_final).map_err(io_error)?;
        std::fs::rename(&index_tmp, &index_final).map_err(io_error)?;
        Ok(())
    }

    fn check_dims(&self, vector: &[f32]) -> Result<(), VectorIndexError> {
        if vector.len() != self.identity.dimensions {
            return Err(VectorIndexError::Usearch(format!(
                "ベクトル次元が不一致です: 期待={} 実際={}",
                self.identity.dimensions,
                vector.len()
            )));
        }
        Ok(())
    }

    fn neighbors_of(matches: usearch::ffi::Matches) -> Vec<Neighbor> {
        matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(key, distance)| Neighbor {
                key: *key as i64,
                // Cos metric = 1 - cosine → similarity = 1 - distance
                similarity: 1.0 - distance,
            })
            .collect()
    }
}

/// 索引ディレクトリ配下のファイルを削除する (索引破棄・モデル削除用。
/// ディレクトリ自体は残す)。
pub fn remove_index_files(dir: &Path) -> Result<(), VectorIndexError> {
    for name in [INDEX_FILE, IDENTITY_FILE] {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(io_error)?;
        }
    }
    Ok(())
}

/// 索引ファイルが存在するか (ロード可否は別)。
pub fn index_files_exist(dir: &Path) -> bool {
    dir.join(INDEX_FILE).exists() && dir.join(IDENTITY_FILE).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "gento-vidx-{tag}-{}-{}",
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

    fn identity(dimensions: usize) -> IndexIdentity {
        IndexIdentity {
            model_name: "ruri-v3-70m".to_string(),
            model_revision: "rev-test".to_string(),
            dimensions,
        }
    }

    /// クラスタ番号方向に立つ単位ベクトル + 小さな摂動 (直交クラスタ)。
    fn cluster_vector(dimensions: usize, cluster: usize, noise: f32) -> Vec<f32> {
        let mut vector = vec![noise; dimensions];
        vector[cluster % dimensions] += 1.0;
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        vector.iter().map(|v| v / norm).collect()
    }

    #[test]
    fn search_ranks_same_cluster_top() {
        let identity = identity(8);
        let index = VectorIndex::new(identity.clone(), 16).unwrap();
        for id in 0..20i64 {
            let cluster = (id % 2) as usize;
            let noise = (id as f32) * 0.001;
            index.add(id, &cluster_vector(8, cluster, noise)).unwrap();
        }
        let query = cluster_vector(8, 0, 0.0005);
        let hits = index.search(&query, 10).unwrap();
        assert_eq!(hits.len(), 10);
        // 同クラスタ (id偶数) が上位を占める
        assert!(
            hits.iter().take(10).filter(|h| h.key % 2 == 0).count() >= 9,
            "{hits:?}"
        );
        // 類似度は降順
        for pair in hits.windows(2) {
            assert!(pair[0].similarity >= pair[1].similarity);
        }
        // 類似度は [0, 1] に収まる (正規化済みcosine)
        for hit in &hits {
            assert!((-0.001..=1.001).contains(&hit.similarity), "{hits:?}");
        }
    }

    #[test]
    fn identical_vector_similarity_is_near_one() {
        let identity = identity(4);
        let index = VectorIndex::new(identity, 4).unwrap();
        let vector = cluster_vector(4, 0, 0.0);
        index.add(1, &vector).unwrap();
        let hits = index.search(&vector, 1).unwrap();
        assert_eq!(hits[0].key, 1);
        assert!(hits[0].similarity > 0.999, "{hits:?}");
    }

    #[test]
    fn save_load_round_trip_preserves_search() {
        let tmp = TempDir::new("roundtrip");
        let identity = identity(8);
        {
            let index = VectorIndex::new(identity.clone(), 32).unwrap();
            for id in 0..32i64 {
                let cluster = (id % 4) as usize;
                index
                    .add(id, &cluster_vector(8, cluster, id as f32 * 0.0005))
                    .unwrap();
            }
            index.save(tmp.path()).unwrap();
        }
        let reopened = VectorIndex::open(tmp.path(), &identity).unwrap();
        assert_eq!(reopened.len(), 32);
        assert_eq!(reopened.identity(), &identity);
        let query = cluster_vector(8, 2, 0.001);
        let hits = reopened.search(&query, 4).unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| h.key % 4 == 2),
            "再読込後もクラスタ一致: {hits:?}"
        );
        assert!(reopened.contains(17));
        assert!(!reopened.contains(999));
    }

    #[test]
    fn open_rejects_model_mismatch() {
        let tmp = TempDir::new("mismatch");
        let identity = identity(8);
        let index = VectorIndex::new(identity.clone(), 2).unwrap();
        index.add(1, &cluster_vector(8, 0, 0.0)).unwrap();
        index.save(tmp.path()).unwrap();
        // 別モデル (次元同一でもname違いは拒否)
        let other = IndexIdentity {
            model_name: "ruri-v3-30m".to_string(),
            model_revision: "rev-test".to_string(),
            dimensions: 8,
        };
        let error = VectorIndex::open(tmp.path(), &other).unwrap_err();
        assert!(
            matches!(error, VectorIndexError::IdentityMismatch { .. }),
            "{error}"
        );
        // 別次元も拒否 (§10)
        let other_dims = IndexIdentity {
            model_name: "ruri-v3-70m".to_string(),
            model_revision: "rev-test".to_string(),
            dimensions: 4,
        };
        assert!(matches!(
            VectorIndex::open(tmp.path(), &other_dims).unwrap_err(),
            VectorIndexError::IdentityMismatch { .. }
        ));
        // 別revisionも拒否
        let other_rev = IndexIdentity {
            model_name: "ruri-v3-70m".to_string(),
            model_revision: "rev-other".to_string(),
            dimensions: 8,
        };
        assert!(matches!(
            VectorIndex::open(tmp.path(), &other_rev).unwrap_err(),
            VectorIndexError::IdentityMismatch { .. }
        ));
    }

    #[test]
    fn open_rejects_corrupt_index_file() {
        let tmp = TempDir::new("corrupt");
        let identity = identity(8);
        let index = VectorIndex::new(identity.clone(), 2).unwrap();
        index.add(1, &cluster_vector(8, 0, 0.0)).unwrap();
        index.save(tmp.path()).unwrap();
        // 索引本体を破損させる (中途切断)
        let path = tmp.path().join(INDEX_FILE);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let error = VectorIndex::open(tmp.path(), &identity).unwrap_err();
        assert!(
            !matches!(error, VectorIndexError::IdentityMismatch { .. }),
            "破損は混在と区別される (どちらでも再構築要因): {error}"
        );
    }

    #[test]
    fn add_rejects_dimension_mismatch() {
        let index = VectorIndex::new(identity(8), 2).unwrap();
        let error = index.add(1, &[0.5f32; 4]).unwrap_err();
        assert!(error.to_string().contains("次元"), "{error}");
        let error = index.search(&[0.5f32; 4], 1).unwrap_err();
        assert!(error.to_string().contains("次元"), "{error}");
    }

    #[test]
    fn filtered_search_restricts_to_allowed_keys() {
        let identity = identity(8);
        let index = VectorIndex::new(identity, 16).unwrap();
        for id in 0..16i64 {
            let cluster = (id % 2) as usize;
            index
                .add(id, &cluster_vector(8, cluster, id as f32 * 0.001))
                .unwrap();
        }
        let allowed: HashSet<i64> = [1i64, 3, 5, 7].into_iter().collect();
        let query = cluster_vector(8, 0, 0.001);
        let hits = index.filtered_search(&query, 10, &allowed).unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| allowed.contains(&h.key)),
            "許可集合外は返らない: {hits:?}"
        );
        // 全件不許可なら空
        let none: HashSet<i64> = HashSet::new();
        assert!(index.filtered_search(&query, 5, &none).unwrap().is_empty());
    }

    #[test]
    fn remove_index_files_and_existence_check() {
        let tmp = TempDir::new("remove");
        let identity = identity(4);
        let index = VectorIndex::new(identity, 1).unwrap();
        index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert!(!index_files_exist(tmp.path()));
        index.save(tmp.path()).unwrap();
        assert!(index_files_exist(tmp.path()));
        remove_index_files(tmp.path()).unwrap();
        assert!(!index_files_exist(tmp.path()));
        // 存在しないファイルの削除も成功 (冪等)
        remove_index_files(tmp.path()).unwrap();
    }

    #[test]
    fn reset_clears_index() {
        let index = VectorIndex::new(identity(4), 8).unwrap();
        index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(index.len(), 1);
        index.reset().unwrap();
        assert_eq!(index.len(), 0);
        assert!(index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap().is_empty());
    }
}
