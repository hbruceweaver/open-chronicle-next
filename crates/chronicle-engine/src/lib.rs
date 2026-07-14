//! Serialized factual ingestion, aggregation, policy, and query services.
//!
//! The signed macOS app will be the sole owner of the live engine handle.

pub mod chunker;
pub mod coverage;
pub mod duration;
pub mod health;
pub mod ingest;
pub mod reconcile;

pub use chunker::*;
pub use coverage::*;
pub use duration::*;
pub use health::*;
pub use ingest::*;
pub use reconcile::*;
