use std::collections::BTreeMap;

use chronicle_domain::{
    EventEnvelope, EventId, EventKind, EventPayload, EvidenceSource, ImageArtifactId,
    ImageReference, ObservationContent, ScreenshotDeletionCause, ScreenshotLifecycle,
    ScreenshotLifecycleAction, ScreenshotProjectedState,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::checksum::{canonical_json, checksum_bytes};
use crate::{FaultInjector, JournalFamily, Result, ScreenshotStore, StoreError, StoreGeneration};

const RETENTION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPreview {
    pub schema_version: u32,
    pub store_generation: u64,
    pub cutoff: DateTime<Utc>,
    pub inventory_digest: String,
    pub candidate_artifact_ids: Vec<ImageArtifactId>,
    pub candidate_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionConfirmation {
    preview: RetentionPreview,
    confirmed: bool,
}

impl RetentionConfirmation {
    pub const fn confirmed(preview: RetentionPreview) -> Self {
        Self {
            preview,
            confirmed: true,
        }
    }

    pub const fn unconfirmed(preview: RetentionPreview) -> Self {
        Self {
            preview,
            confirmed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionApplyResult {
    pub schema_version: u32,
    pub store_generation: u64,
    pub cutoff: DateTime<Utc>,
    pub deleted_artifact_ids: Vec<ImageArtifactId>,
    pub deleted_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct InventoryDigestEntry {
    artifact_id: ImageArtifactId,
    source_event_id: EventId,
    expires_at: DateTime<Utc>,
    projected_state: Option<ScreenshotProjectedState>,
    file_present: bool,
    file_bytes: u64,
}

#[derive(Clone, Debug)]
struct InventoryEntry {
    source: EventEnvelope,
    image: ImageReference,
    projected_state: Option<ScreenshotProjectedState>,
    file_present: bool,
    file_bytes: u64,
}

#[derive(Clone, Debug)]
struct Inventory {
    entries: BTreeMap<ImageArtifactId, InventoryEntry>,
    digest: String,
}

impl ScreenshotStore {
    pub fn preview_retention(&self, cutoff: DateTime<Utc>) -> Result<RetentionPreview> {
        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let _screenshots = shared.screenshots()?;
        let inventory = self.retention_inventory()?;
        let candidates = eligible_candidates(&inventory, cutoff);
        Ok(RetentionPreview {
            schema_version: RETENTION_SCHEMA_VERSION,
            store_generation: self.generation.generation,
            cutoff,
            inventory_digest: inventory.digest,
            candidate_bytes: candidates.iter().try_fold(0_u64, |total, entry| {
                total.checked_add(entry.file_bytes).ok_or_else(|| {
                    StoreError::InvalidPath("retention byte count overflow".to_owned())
                })
            })?,
            candidate_artifact_ids: candidates
                .into_iter()
                .map(|entry| entry.image.artifact_id.clone())
                .collect(),
        })
    }

    pub fn apply_retention(
        &self,
        confirmation: RetentionConfirmation,
        applied_at: DateTime<Utc>,
        faults: FaultInjector,
    ) -> Result<RetentionApplyResult> {
        if !confirmation.confirmed {
            return Err(StoreError::RetentionNotConfirmed);
        }
        let preview = confirmation.preview;
        if preview.schema_version != RETENTION_SCHEMA_VERSION {
            return Err(StoreError::RetentionPreviewStale);
        }
        let current_generation = StoreGeneration::load(&self.root)?;
        if preview.store_generation != current_generation.generation {
            return Err(StoreError::StaleGeneration {
                expected: preview.store_generation,
                actual: current_generation.generation,
            });
        }
        if applied_at < preview.cutoff {
            return Err(StoreError::InvalidPath(
                "retention cannot apply before its cutoff".to_owned(),
            ));
        }

        let shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        // This exclusive cross-process + in-process guard remains held from the
        // inventory recheck through every request/unlink/completion record.
        let _screenshots = shared.screenshots()?;
        let inventory = self.retention_inventory()?;
        let candidates = eligible_candidates(&inventory, preview.cutoff);
        let candidate_ids = candidates
            .iter()
            .map(|entry| entry.image.artifact_id.clone())
            .collect::<Vec<_>>();
        let candidate_bytes = candidates.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(entry.file_bytes)
                .ok_or_else(|| StoreError::InvalidPath("retention byte count overflow".to_owned()))
        })?;
        if inventory.digest != preview.inventory_digest
            || candidate_ids != preview.candidate_artifact_ids
            || candidate_bytes != preview.candidate_bytes
        {
            return Err(StoreError::RetentionPreviewStale);
        }

        let mut deleted = Vec::with_capacity(candidates.len());
        let mut deleted_bytes = 0_u64;
        for (index, entry) in candidates.into_iter().enumerate() {
            // Keep the projection critical section per image so a large
            // retention batch cannot block unrelated factual ingestion for the
            // duration of every filesystem unlink.
            let _snapshot = self.locks.query_snapshot()?;
            let (request, completion) = retention_events(&entry.source, &entry.image, applied_at)?;
            self.delete_locked_at_occurrence(&request, &completion, faults, index)?;
            deleted_bytes = deleted_bytes.checked_add(entry.file_bytes).ok_or_else(|| {
                StoreError::InvalidPath("retention byte count overflow".to_owned())
            })?;
            deleted.push(entry.image.artifact_id);
        }
        Ok(RetentionApplyResult {
            schema_version: RETENTION_SCHEMA_VERSION,
            store_generation: self.generation.generation,
            cutoff: preview.cutoff,
            deleted_artifact_ids: deleted,
            deleted_bytes,
        })
    }

    fn retention_inventory(&self) -> Result<Inventory> {
        let records = self.journal.scan_all(JournalFamily::Events, false)?;
        let mut entries = BTreeMap::<ImageArtifactId, InventoryEntry>::new();
        let events = records
            .records
            .into_iter()
            .map(|record| {
                EventEnvelope::parse(
                    std::str::from_utf8(record.body_bytes())
                        .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
                )
                .map_err(StoreError::from)
            })
            .collect::<Result<Vec<_>>>()?;
        for event in &events {
            match &event.payload {
                EventPayload::ObservationAttempt(attempt) => {
                    if let ObservationContent::Captured(content) = &attempt.content
                        && let Some(image) = &content.image
                    {
                        let expected = crate::artifacts::derived_screenshot_path(
                            event,
                            image.artifact_id.as_str(),
                        );
                        if image.managed_relative_path.as_str() != expected {
                            return Err(StoreError::InvalidPath(
                                "canonical image path violates screenshot derivation".to_owned(),
                            ));
                        }
                        if entries.contains_key(&image.artifact_id) {
                            return Err(StoreError::StableIdConflict {
                                id: image.artifact_id.to_string(),
                            });
                        }
                        let (file_present, file_bytes) =
                            match self.root.open_file(&expected, false, false, false) {
                                Ok(file) => (true, file.metadata()?.len()),
                                Err(StoreError::Io(error))
                                    if error.kind() == std::io::ErrorKind::NotFound =>
                                {
                                    (false, 0)
                                }
                                Err(error) => return Err(error),
                            };
                        entries.insert(
                            image.artifact_id.clone(),
                            InventoryEntry {
                                source: event.clone(),
                                image: image.clone(),
                                projected_state: None,
                                file_present,
                                file_bytes,
                            },
                        );
                    }
                }
                EventPayload::ScreenshotLifecycle(_) | EventPayload::RecordingGap(_) => {}
            }
        }
        // Lifecycle records may be in later UTC shards, or appear before their
        // source during a wall-clock correction. Apply them only after the full
        // image-intent inventory is known.
        for event in &events {
            if let EventPayload::ScreenshotLifecycle(lifecycle) = &event.payload
                && let Some(entry) = entries.get_mut(&lifecycle.artifact_id)
            {
                if entry.source.event_id != lifecycle.source_event_id {
                    return Err(StoreError::InvalidPath(
                        "screenshot lifecycle source provenance changed".to_owned(),
                    ));
                }
                entry.projected_state = Some(lifecycle.projected_state);
            }
        }
        let digest_entries = entries
            .values()
            .map(|entry| InventoryDigestEntry {
                artifact_id: entry.image.artifact_id.clone(),
                source_event_id: entry.source.event_id.clone(),
                expires_at: entry.image.expires_at,
                projected_state: entry.projected_state,
                file_present: entry.file_present,
                file_bytes: entry.file_bytes,
            })
            .collect::<Vec<_>>();
        let digest = checksum_bytes(&canonical_json(&digest_entries)?);
        Ok(Inventory { entries, digest })
    }
}

fn eligible_candidates(inventory: &Inventory, cutoff: DateTime<Utc>) -> Vec<InventoryEntry> {
    inventory
        .entries
        .values()
        .filter(|entry| {
            entry.projected_state == Some(ScreenshotProjectedState::Retained)
                && entry.file_present
                && entry.image.expires_at <= cutoff
        })
        .cloned()
        .collect()
}

fn retention_events(
    source: &EventEnvelope,
    image: &ImageReference,
    applied_at: DateTime<Utc>,
) -> Result<(EventEnvelope, EventEnvelope)> {
    let request = lifecycle_event(
        source,
        image,
        ScreenshotLifecycleAction::DeleteRequested,
        ScreenshotProjectedState::DeletePending,
        applied_at,
        false,
    )?;
    let completion = lifecycle_event(
        source,
        image,
        ScreenshotLifecycleAction::DeleteCompleted,
        ScreenshotProjectedState::Expired,
        applied_at,
        true,
    )?;
    Ok((request, completion))
}

fn lifecycle_event(
    source: &EventEnvelope,
    image: &ImageReference,
    action: ScreenshotLifecycleAction,
    state: ScreenshotProjectedState,
    applied_at: DateTime<Utc>,
    completed: bool,
) -> Result<EventEnvelope> {
    let id_material = format!(
        "retention:{action:?}:{}:{}",
        image.artifact_id,
        applied_at.to_rfc3339()
    );
    let digest = checksum_bytes(id_material.as_bytes());
    let event = EventEnvelope {
        schema_version: chronicle_domain::CONTRACT_VERSION.to_owned(),
        event_id: EventId::new(format!("retention-image-{}", &digest[..32]))
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
        device_id: source.device_id.clone(),
        scheduled_at: None,
        observed_at: applied_at,
        recorded_at: applied_at,
        display_timezone: source.display_timezone.clone(),
        source: EvidenceSource {
            adapter: "screenshot-retention".to_owned(),
            version: "1".to_owned(),
        },
        kind: EventKind::ScreenshotLifecycle,
        payload: EventPayload::ScreenshotLifecycle(ScreenshotLifecycle {
            artifact_id: image.artifact_id.clone(),
            action,
            deletion_cause: Some(ScreenshotDeletionCause::RetentionExpired),
            projected_state: state,
            requested_at: Some(applied_at),
            completed_at: completed.then_some(applied_at),
            source_event_id: source.event_id.clone(),
        }),
    };
    event.validate().map_err(|reason| {
        StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
    })?;
    Ok(event)
}
