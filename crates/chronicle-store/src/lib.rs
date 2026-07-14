//! Durable canonical storage and rebuildable SQLite projection.

pub mod artifacts;
pub mod checksum;
pub mod export;
pub mod generation;
pub mod health;
pub mod journal;
pub mod layout;
pub mod locks;
pub mod maintenance;
pub mod permissions;
pub mod projection;
pub mod queries;
pub mod receipts;
pub mod recovery;
pub mod retention;
pub mod search;
pub mod sqlite;
pub mod statistics;
pub mod storage;

use std::io;

use chronicle_domain::{
    DurableAcknowledgement, HealthCode, HealthSeverity, HealthSnapshot, ProjectionHealth,
};
use chrono::{DateTime, Utc};
use thiserror::Error;

pub use artifacts::*;
pub use export::*;
pub use generation::*;
pub use health::*;
pub use journal::*;
pub use layout::*;
pub use locks::*;
pub use maintenance::*;
pub use projection::*;
pub use queries::*;
pub use receipts::*;
pub use recovery::*;
pub use retention::*;
pub use search::*;
pub use sqlite::*;
pub use statistics::*;
pub use storage::*;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("managed path is invalid: {0}")]
    InvalidPath(String),
    #[error("managed root must be owned by the current non-root user")]
    WrongOwner,
    #[error("managed object has unsafe permissions: {0}")]
    UnsafePermissions(String),
    #[error("I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("JSON failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQLite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("contract failure: {0}")]
    Contract(#[from] chronicle_domain::ContractError),
    #[error("complete canonical record is corrupt in {shard} at byte {offset}: {reason}")]
    CorruptRecord {
        shard: String,
        offset: u64,
        reason: String,
    },
    #[error("lock timed out: {0}")]
    LockTimeout(String),
    #[error("another Open Chronicle application process owns capture")]
    CaptureOwnerActive,
    #[error("an evidence deletion is committed and must be resumed before normal store access")]
    MaintenanceInProgress,
    #[error(
        "screenshot transaction requires {required_bytes} available bytes but found {available_bytes}"
    )]
    ScreenshotFreeSpace {
        available_bytes: u64,
        required_bytes: u64,
    },
    #[error(
        "screenshot transaction would add {candidate_bytes} bytes to {managed_image_bytes} managed image bytes above quota {quota_bytes}"
    )]
    ScreenshotImageQuota {
        managed_image_bytes: u64,
        candidate_bytes: u64,
        quota_bytes: u64,
    },
    #[error("stable ID {id} was replayed with different canonical bytes")]
    StableIdConflict { id: String },
    #[error("artifact expected prior revision conflict")]
    ArtifactConflict,
    #[error("derived artifact status or identity transition is invalid")]
    InvalidArtifactTransition,
    #[error("derived artifact evidence reference is invalid")]
    InvalidEvidenceReference,
    #[error("disclosure grant already exists")]
    GrantAlreadyExists,
    #[error("disclosure grant was not found")]
    GrantNotFound,
    #[error("disclosure grant belongs to another client")]
    GrantClientMismatch,
    #[error("disclosure grant is inactive")]
    GrantInactive,
    #[error("disclosure cursor was not found or expired")]
    CursorNotFound,
    #[error("disclosure cursor does not match this query scope")]
    CursorScopeMismatch,
    #[error("disclosure response exceeds its byte budget")]
    DisclosureByteLimit,
    #[error("store handle is stale; expected generation {expected}, found {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("runtime SQLite identity mismatch: {0}")]
    SqliteIdentity(String),
    #[error("journal repair requires explicit confirmation")]
    RepairNotConfirmed,
    #[error("journal repair is incomplete: {0}")]
    RepairIncomplete(String),
    #[error("screenshot retention requires explicit confirmation")]
    RetentionNotConfirmed,
    #[error("screenshot retention preview is stale")]
    RetentionPreviewStale,
    #[error("injected crash boundary: {0:?}")]
    InjectedFault(FaultPoint),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub fn critical_storage_health(observed_at: DateTime<Utc>) -> HealthSnapshot {
    HealthSnapshot {
        observed_at,
        severity: HealthSeverity::Critical,
        code: HealthCode::StorageUnavailable,
        projection: ProjectionHealth::Blocked,
        acknowledgement: Some(DurableAcknowledgement::NotDurable),
        factual_message: "Canonical storage is unavailable; recording must pause until recovery."
            .to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    AfterJournalAppend,
    AfterJournalSync,
    BeforeJournalManifestUpdate,
    AfterRowInsert,
    AfterCursorUpdate,
    AfterCurrentPointerUpdate,
    AfterWatermarkUpdate,
    BeforeTransactionCommit,
    AfterTransactionCommit,
    AfterArtifactRename,
    AfterArtifactDirectorySync,
    AfterProvisionalImageSync,
    AfterObservationAppend,
    AfterImagePromotion,
    AfterImagePromotionDirectorySync,
    AfterLifecycleCompletion,
    AfterDeleteRequest,
    AfterImageUnlink,
    AfterImageUnlinkDirectorySync,
    AfterDeleteCompletion,
    AfterRepairArchive,
    AfterRepairSuccessor,
    AfterRepairOriginalUnlink,
    AfterRepairMarker,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FaultInjector {
    point: Option<FaultPoint>,
    abort_process: bool,
    occurrence: usize,
}

impl FaultInjector {
    pub const fn at(point: FaultPoint) -> Self {
        Self {
            point: Some(point),
            abort_process: false,
            occurrence: 0,
        }
    }

    pub const fn at_occurrence(point: FaultPoint, occurrence: usize) -> Self {
        Self {
            point: Some(point),
            abort_process: false,
            occurrence,
        }
    }

    pub const fn abort_at(point: FaultPoint) -> Self {
        Self {
            point: Some(point),
            abort_process: true,
            occurrence: 0,
        }
    }

    pub const fn none() -> Self {
        Self {
            point: None,
            abort_process: false,
            occurrence: 0,
        }
    }

    pub fn check(self, point: FaultPoint) -> Result<()> {
        self.check_occurrence(point, 0)
    }

    pub fn check_occurrence(self, point: FaultPoint, occurrence: usize) -> Result<()> {
        if self.point == Some(point) && self.occurrence == occurrence {
            if self.abort_process {
                std::process::abort();
            }
            return Err(StoreError::InjectedFault(point));
        }
        Ok(())
    }
}
