use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use chronicle_domain::{
    ChunkRevision, ExportChecksum, ExportCounts, JournalCutoff, QueryArtifact, QueryEvent, UtcRange,
};
use chronicle_store::checksum::{canonical_json, checksum_bytes};
use chronicle_store::{
    CanonicalJournal, EvidenceDeletionConfirmation, EvidenceDeletionOptions,
    EvidenceDeletionPreview, EvidenceDeletionReceipt, EvidenceDeletionResult,
    FactoryResetInventory, FaultInjector, LockManager, MaintenanceFaultInjector,
    MaintenanceInventory, MaintenanceStore, ManagedRoot, Projector, RetentionApplyResult,
    RetentionConfirmation, RetentionPreview, ScreenshotStore, SqliteStore, StableExportBuilder,
    StableSnapshotSelection, StoreGeneration, StoreQueries, scan_artifact_revisions,
    store_health_metrics,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{EngineError, Result};

const APP_MAINTENANCE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Stable, path-free export material for the signed application's own export UI.
///
/// This type is intentionally not routed through [`crate::SharedService`]. Agent
/// and MCP exports remain disclosure-grant scoped; only the app-owned boundary
/// may call this ungranted maintenance API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppExportSnapshot {
    pub schema_version: u32,
    pub stable_cutoff: DateTime<Utc>,
    pub store_generation: u64,
    pub range: UtcRange,
    pub included_counts: ExportCounts,
    pub available_counts: ExportCounts,
    pub included_content_classes: Vec<String>,
    pub excluded_content_classes: Vec<String>,
    pub journal_cutoffs: Vec<JournalCutoff>,
    pub checksums: Vec<ExportChecksum>,
    pub truncated: bool,
    pub events: Vec<QueryEvent>,
    pub chunks: Vec<ChunkRevision>,
    pub artifacts: Vec<QueryArtifact>,
}

/// App-owned maintenance composition over the authoritative store primitives.
///
/// The façade retains no projection handle, which is important because evidence
/// deletion advances the store generation and makes all prior handles stale.
#[derive(Clone, Debug)]
pub struct AppMaintenance {
    root: ManagedRoot,
    lock_timeout: Duration,
    clock: fn() -> DateTime<Utc>,
}

impl AppMaintenance {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(ManagedRoot::initialize(path)?, DEFAULT_LOCK_TIMEOUT)
    }

    pub fn open(root: ManagedRoot, lock_timeout: Duration) -> Result<Self> {
        StoreGeneration::initialize(&root)?;
        Ok(Self {
            root,
            lock_timeout,
            clock: Utc::now,
        })
    }

    /// Replaces the wall clock for deterministic boundary testing.
    #[doc(hidden)]
    pub fn with_clock(mut self, clock: fn() -> DateTime<Utc>) -> Self {
        self.clock = clock;
        self
    }

    pub fn preview_retention(&self, cutoff: DateTime<Utc>) -> Result<RetentionPreview> {
        Ok(self.screenshot_store()?.preview_retention(cutoff)?)
    }

    pub fn apply_retention(
        &self,
        confirmation: RetentionConfirmation,
        applied_at: DateTime<Utc>,
        faults: FaultInjector,
    ) -> Result<RetentionApplyResult> {
        Ok(self
            .screenshot_store()?
            .apply_retention(confirmation, applied_at, faults)?)
    }

    pub fn export_snapshot(
        &self,
        range: UtcRange,
        include_ocr: bool,
        include_derived: bool,
        max_bytes: u64,
    ) -> Result<AppExportSnapshot> {
        range.validate().map_err(EngineError::Configuration)?;
        let locks = LockManager::new(self.root.clone(), self.lock_timeout);
        // Export completeness is a canonical-store property, not merely a
        // SQLite property. Freeze trusted writers while checking projection
        // currency and selecting the pinned projection snapshot.
        let _exclusive = locks.exclusive_maintenance()?;
        // Projection writers take this same guard. Capture the clock while it
        // is held and retain it through the pinned snapshot so a caller cannot
        // claim an arbitrary historical cutoff for newer projected rows.
        let _query_snapshot = locks.query_snapshot()?;
        let stable_cutoff = (self.clock)();
        if stable_cutoff < range.end {
            return Err(EngineError::Configuration(
                "export stable cutoff cannot precede the requested range".to_owned(),
            ));
        }
        let generation = StoreGeneration::load(&self.root)?;
        let sqlite = SqliteStore::open(self.root.clone())?;
        generation.ensure_current(&self.root)?;
        if store_health_metrics(&self.root, &sqlite, stable_cutoff)?.projection_pending_records > 0
        {
            return Err(EngineError::Configuration(
                "export requires the canonical journal to be fully projected".to_owned(),
            ));
        }
        if include_derived {
            reconcile_and_verify_artifact_projection(&self.root, &sqlite)?;
        }
        let selection = StableExportBuilder::new(StoreQueries::new(sqlite))?.full_export(
            &range,
            include_ocr,
            include_derived,
            max_bytes,
        )?;
        generation.ensure_current(&self.root)?;
        app_export_snapshot(
            range,
            stable_cutoff,
            generation.generation,
            include_ocr,
            include_derived,
            selection,
        )
    }

    pub fn evidence_inventory(
        &self,
        options: EvidenceDeletionOptions,
    ) -> Result<MaintenanceInventory> {
        Ok(self.maintenance_store()?.evidence_inventory(options)?)
    }

    pub fn prepare_evidence_deletion(
        &self,
        options: EvidenceDeletionOptions,
        prepared_at: DateTime<Utc>,
    ) -> Result<EvidenceDeletionPreview> {
        Ok(self
            .maintenance_store()?
            .prepare_evidence_deletion(options, prepared_at)?)
    }

    pub fn evidence_deletion_receipt(&self) -> Result<Option<EvidenceDeletionReceipt>> {
        Ok(self.maintenance_store()?.evidence_deletion_receipt()?)
    }

    pub fn finalize_evidence_deletion(
        &self,
        confirmation: EvidenceDeletionConfirmation,
        completed_at: DateTime<Utc>,
        faults: MaintenanceFaultInjector,
    ) -> Result<EvidenceDeletionResult> {
        Ok(self.maintenance_store()?.finalize_evidence_deletion(
            confirmation,
            completed_at,
            faults,
        )?)
    }

    pub fn resume_evidence_deletion(
        &self,
        completed_at: DateTime<Utc>,
        faults: MaintenanceFaultInjector,
    ) -> Result<EvidenceDeletionResult> {
        Ok(self
            .maintenance_store()?
            .resume_evidence_deletion(completed_at, faults)?)
    }

    pub fn factory_reset_inventory(
        &self,
        prepared_at: DateTime<Utc>,
    ) -> Result<FactoryResetInventory> {
        Ok(self
            .maintenance_store()?
            .factory_reset_inventory(prepared_at)?)
    }

    fn maintenance_store(&self) -> Result<MaintenanceStore> {
        Ok(MaintenanceStore::open(
            self.root.clone(),
            self.lock_timeout,
        )?)
    }

    fn screenshot_store(&self) -> Result<ScreenshotStore> {
        let sqlite = SqliteStore::open(self.root.clone())?;
        Ok(ScreenshotStore::new(
            self.root.clone(),
            CanonicalJournal::new(self.root.clone()),
            Projector::new(sqlite),
        )?)
    }
}

fn reconcile_and_verify_artifact_projection(
    root: &ManagedRoot,
    sqlite: &SqliteStore,
) -> Result<()> {
    let canonical = scan_artifact_revisions(root)?;
    let projector = Projector::new(sqlite.clone());
    for revision in &canonical {
        projector.project_artifact(revision, FaultInjector::none())?;
    }
    let canonical_ids = canonical
        .iter()
        .map(|revision| revision.revision_id.to_string())
        .collect::<BTreeSet<_>>();
    let projected_ids = sqlite
        .snapshot_ids()?
        .artifact_revision_ids
        .into_iter()
        .collect::<BTreeSet<_>>();
    if canonical_ids != projected_ids {
        return Err(chronicle_store::StoreError::SqliteIdentity(
            "derived artifact projection does not exactly match canonical revisions".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn app_export_snapshot(
    range: UtcRange,
    stable_cutoff: DateTime<Utc>,
    store_generation: u64,
    include_ocr: bool,
    include_derived: bool,
    selection: StableSnapshotSelection,
) -> Result<AppExportSnapshot> {
    let included_counts = ExportCounts {
        events: selection.events.len() as u64,
        chunks: selection.chunks.len() as u64,
        artifacts: selection.artifacts.len() as u64,
    };
    let (included_content_classes, excluded_content_classes) =
        content_inventory(include_ocr, include_derived);
    let checksums = vec![
        ExportChecksum {
            component: "events".to_owned(),
            sha256: checksum_bytes(&canonical_json(&selection.events)?),
        },
        ExportChecksum {
            component: "chunks".to_owned(),
            sha256: checksum_bytes(&canonical_json(&selection.chunks)?),
        },
        ExportChecksum {
            component: "derived-artifacts".to_owned(),
            sha256: checksum_bytes(&canonical_json(&selection.artifacts)?),
        },
    ];
    Ok(AppExportSnapshot {
        schema_version: APP_MAINTENANCE_SCHEMA_VERSION,
        stable_cutoff,
        store_generation,
        range,
        included_counts,
        available_counts: selection.available_counts,
        included_content_classes,
        excluded_content_classes,
        journal_cutoffs: selection.journal_cutoffs,
        checksums,
        truncated: selection.truncated,
        events: selection.events,
        chunks: selection.chunks,
        artifacts: selection.artifacts,
    })
}

fn content_inventory(include_ocr: bool, include_derived: bool) -> (Vec<String>, Vec<String>) {
    let mut included = vec!["metadata".to_owned()];
    let mut excluded = vec!["screenshots".to_owned()];
    if include_ocr {
        included.push("ocr".to_owned());
    } else {
        excluded.push("ocr".to_owned());
    }
    if include_derived {
        included.push("derived".to_owned());
    } else {
        excluded.push("derived".to_owned());
    }
    (included, excluded)
}
