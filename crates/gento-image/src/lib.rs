//! GEN-TO 画像パイプラインcrate。
//!
//! フェーズ2/3でディレクトリ走査・画像プローブ・サムネイル生成・キャッシュ・
//! ワーカープールを実装する。Tauriに依存してはならない
//! (docs/implementation-plan.md §5.1)。

pub mod cache;
pub mod metrics;
pub mod pool;
pub mod probe;
pub mod scan;
pub mod stats;
pub mod thumbnail;
