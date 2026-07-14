//! Serialized factual ingestion, aggregation, policy, and query services.
//!
//! The signed macOS app will be the sole owner of the live engine handle.

pub mod chunker;
pub mod coverage;
pub mod duration;
pub mod health;
pub mod ingest;
pub mod policy;
pub mod reconcile;
pub mod runtime;
pub mod service;
pub mod study;

pub use chunker::*;
pub use coverage::*;
pub use duration::*;
pub use health::*;
pub use ingest::*;
pub use policy::*;
pub use reconcile::*;
pub use runtime::*;
pub use service::*;
pub use study::*;
