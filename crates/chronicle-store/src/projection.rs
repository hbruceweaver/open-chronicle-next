use chronicle_domain::{
    ChunkRevision, DerivedArtifactRevision, EventEnvelope, EventPayload, ObservationContent,
    ScreenshotLifecycle, ScreenshotLifecycleAction, ScreenshotProjectedState,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::checksum::{canonical_json, checksum_bytes};
use crate::{
    FaultInjector, FaultPoint, JournalFamily, Result, SqliteStore, StoreError, VerifiedRecord,
};

#[derive(Clone, Debug)]
pub struct Projector {
    sqlite: SqliteStore,
}

impl Projector {
    pub const fn new(sqlite: SqliteStore) -> Self {
        Self { sqlite }
    }

    pub fn project_record(&self, record: &VerifiedRecord, faults: FaultInjector) -> Result<()> {
        let mut connection = self.sqlite.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cursor = current_cursor(&transaction, record)?;
        if cursor != record.start_offset() {
            if cursor >= record.end_offset() {
                verify_already_projected(&transaction, record)?;
                return Ok(());
            }
            return Err(StoreError::SqliteIdentity(format!(
                "projection cursor {cursor} cannot advance across unprojected bytes {}..{} in {}",
                record.start_offset(),
                record.end_offset(),
                record.shard()
            )));
        }
        match record.family() {
            JournalFamily::Events => project_event(&transaction, record, faults)?,
            JournalFamily::Chunks => project_chunk(&transaction, record, faults)?,
        }
        update_cursor(&transaction, record)?;
        faults.check(FaultPoint::AfterCursorUpdate)?;
        faults.check(FaultPoint::BeforeTransactionCommit)?;
        transaction.commit()?;
        faults.check(FaultPoint::AfterTransactionCommit)?;
        Ok(())
    }

    pub fn project_artifact(
        &self,
        artifact: &DerivedArtifactRevision,
        faults: FaultInjector,
    ) -> Result<()> {
        artifact.validate().map_err(|reason| {
            StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
        })?;
        let body_bytes = canonical_json(artifact)?;
        let checksum = checksum_bytes(&body_bytes);
        let body_json = String::from_utf8(body_bytes)
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        let mut connection = self.sqlite.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT checksum FROM artifact_revisions WHERE revision_id = ?1",
                [artifact.revision_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != checksum {
                return Err(StoreError::StableIdConflict {
                    id: artifact.revision_id.to_string(),
                });
            }
        } else {
            let current: Option<String> = transaction
                .query_row(
                    "SELECT revision_id FROM current_artifacts WHERE artifact_id = ?1",
                    [artifact.artifact_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if current.as_deref()
                != artifact
                    .expected_prior_revision_id
                    .as_ref()
                    .map(|revision| revision.as_str())
            {
                return Err(StoreError::ArtifactConflict);
            }
            transaction.execute(
                "INSERT INTO artifact_revisions(revision_id, artifact_id, prior_revision_id, created_at, status, checksum, body_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    artifact.revision_id.as_str(),
                    artifact.artifact_id.as_str(),
                    artifact.prior_revision_id.as_ref().map(|id| id.as_str()),
                    artifact.created_at.to_rfc3339(),
                    enum_value(&artifact.status)?,
                    checksum,
                    body_json,
                ],
            )?;
            for event_id in &artifact.evidence.event_ids {
                transaction.execute(
                    "INSERT INTO artifact_evidence_refs(revision_id, evidence_kind, evidence_id) VALUES(?1, 'event', ?2)",
                    params![artifact.revision_id.as_str(), event_id.as_str()],
                )?;
            }
            for chunk_id in &artifact.evidence.chunk_ids {
                transaction.execute(
                    "INSERT INTO artifact_evidence_refs(revision_id, evidence_kind, evidence_id) VALUES(?1, 'chunk', ?2)",
                    params![artifact.revision_id.as_str(), chunk_id.as_str()],
                )?;
            }
            transaction.execute(
                "INSERT INTO current_artifacts(artifact_id, revision_id) VALUES(?1, ?2) ON CONFLICT(artifact_id) DO UPDATE SET revision_id=excluded.revision_id",
                params![artifact.artifact_id.as_str(), artifact.revision_id.as_str()],
            )?;
            faults.check(FaultPoint::AfterCurrentPointerUpdate)?;
        }
        faults.check(FaultPoint::BeforeTransactionCommit)?;
        transaction.commit()?;
        faults.check(FaultPoint::AfterTransactionCommit)?;
        Ok(())
    }
}

fn project_event(
    transaction: &Transaction<'_>,
    record: &VerifiedRecord,
    faults: FaultInjector,
) -> Result<()> {
    let body = std::str::from_utf8(record.body_bytes())
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    let event = EventEnvelope::parse(body)?;
    let existing: Option<String> = transaction
        .query_row(
            "SELECT checksum FROM events WHERE event_id = ?1",
            [event.event_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != record.checksum() {
            return Err(StoreError::StableIdConflict {
                id: event.event_id.to_string(),
            });
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO events(event_id, checksum, kind, recorded_at, body_json) VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            event.event_id.as_str(),
            record.checksum(),
            enum_value(&event.kind)?,
            event.recorded_at.to_rfc3339(),
            body,
        ],
    )?;
    faults.check(FaultPoint::AfterRowInsert)?;
    match &event.payload {
        EventPayload::ObservationAttempt(attempt) => {
            let scheduled_at = event.scheduled_at.ok_or_else(|| {
                StoreError::SqliteIdentity(
                    "projected observation attempt has no scheduled_at".to_owned(),
                )
            })?;
            mark_pending_bucket(
                transaction,
                &event.device_id,
                &event.event_id,
                scheduled_at,
                attempt.cadence_seconds,
            )?;
            let (application, process, title, domain, hash, ocr) = match &attempt.content {
                ObservationContent::Captured(content) => (
                    Some(content.context.application_bundle_id.as_str()),
                    Some(content.context.process_name.as_str()),
                    content.context.window_title.as_deref(),
                    content
                        .context
                        .authorized_domain
                        .as_ref()
                        .map(|context| context.domain.as_str()),
                    Some(content.content_hash.as_str()),
                    content.ocr.as_ref().map(|evidence| evidence.text.as_str()),
                ),
                ObservationContent::Unchanged(content) => (
                    Some(content.context.application_bundle_id.as_str()),
                    Some(content.context.process_name.as_str()),
                    content.context.window_title.as_deref(),
                    content
                        .context
                        .authorized_domain
                        .as_ref()
                        .map(|context| context.domain.as_str()),
                    Some(content.content_hash.as_str()),
                    None,
                ),
                ObservationContent::Protected(_) | ObservationContent::NoEvidence(_) => {
                    (None, None, None, None, None, None)
                }
            };
            transaction.execute(
                "INSERT INTO observations(event_id, attempt_status, evidence_state, presence_state, ocr_state, application_bundle_id, process_name, window_title, authorized_domain, content_hash, ocr_text) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    event.event_id.as_str(),
                    enum_value(&attempt.attempt_status)?,
                    enum_value(&attempt.evidence_state)?,
                    enum_value(&attempt.presence_state)?,
                    enum_value(&attempt.ocr_state)?,
                    application,
                    process,
                    title,
                    domain,
                    hash,
                    ocr,
                ],
            )?;
            if let Some(text) = ocr
                && !text.is_empty()
            {
                transaction.execute(
                    "INSERT INTO ocr_fts(event_id, text) VALUES(?1, ?2)",
                    params![event.event_id.as_str(), text],
                )?;
            }
        }
        EventPayload::ScreenshotLifecycle(lifecycle) => {
            validate_lifecycle_projection(transaction, lifecycle)?;
            transaction.execute(
                "INSERT INTO screenshot_lifecycle(artifact_id, source_event_id, last_event_id, state, deletion_cause, requested_at, completed_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(artifact_id) DO UPDATE SET last_event_id=excluded.last_event_id, state=excluded.state, deletion_cause=excluded.deletion_cause, requested_at=excluded.requested_at, completed_at=excluded.completed_at",
                params![
                    lifecycle.artifact_id.as_str(),
                    lifecycle.source_event_id.as_str(),
                    event.event_id.as_str(),
                    enum_value(&lifecycle.projected_state)?,
                    lifecycle.deletion_cause.as_ref().map(enum_value).transpose()?,
                    lifecycle.requested_at.map(|value| value.to_rfc3339()),
                    lifecycle.completed_at.map(|value| value.to_rfc3339()),
                ],
            )?;
        }
        EventPayload::RecordingGap(gap) => {
            let mut bucket = utc_bucket_start(gap.start)?;
            while bucket < gap.end {
                mark_pending_bucket(transaction, &event.device_id, &event.event_id, bucket, 30)?;
                bucket += Duration::seconds(300);
            }
        }
    }
    Ok(())
}

fn utc_bucket_start(at: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let seconds = at.timestamp().div_euclid(300) * 300;
    Utc.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| StoreError::InvalidPath("aggregation bucket timestamp overflow".to_owned()))
}

fn mark_pending_bucket(
    transaction: &Transaction<'_>,
    device_id: &chronicle_domain::DeviceId,
    event_id: &chronicle_domain::EventId,
    at: DateTime<Utc>,
    cadence_seconds: u32,
) -> Result<()> {
    let bucket = utc_bucket_start(at)?;
    transaction.execute(
        "INSERT INTO aggregation_pending_buckets(
             device_id, bucket_start, bucket_start_epoch,
             finalization_cadence_seconds, generation_at)
         VALUES(?1, ?2, ?3, ?4, NULL)
         ON CONFLICT(device_id, bucket_start) DO UPDATE SET
           finalization_cadence_seconds=max(
             aggregation_pending_buckets.finalization_cadence_seconds,
             excluded.finalization_cadence_seconds)",
        params![
            device_id.as_str(),
            bucket.to_rfc3339(),
            bucket.timestamp(),
            cadence_seconds,
        ],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO aggregation_bucket_events(device_id, bucket_start, event_id)
         VALUES(?1, ?2, ?3)",
        params![device_id.as_str(), bucket.to_rfc3339(), event_id.as_str()],
    )?;
    Ok(())
}

fn validate_lifecycle_projection(
    transaction: &Transaction<'_>,
    lifecycle: &ScreenshotLifecycle,
) -> Result<()> {
    let source_body: Option<String> = transaction
        .query_row(
            "SELECT body_json FROM events WHERE event_id=?1",
            [lifecycle.source_event_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let source = source_body
        .as_deref()
        .ok_or_else(|| {
            StoreError::SqliteIdentity(
                "screenshot lifecycle source observation is not projected".to_owned(),
            )
        })
        .and_then(|body| EventEnvelope::parse(body).map_err(StoreError::from))?;
    let attempt = match &source.payload {
        EventPayload::ObservationAttempt(attempt) => attempt,
        _ => {
            return Err(StoreError::SqliteIdentity(
                "screenshot lifecycle source is not an observation".to_owned(),
            ));
        }
    };
    let source_artifact = match &attempt.content {
        ObservationContent::Captured(content) => content.image.as_ref(),
        _ => None,
    }
    .map(|image| image.artifact_id.as_str());
    if source_artifact.is_some_and(|artifact| artifact != lifecycle.artifact_id.as_str()) {
        return Err(StoreError::SqliteIdentity(
            "screenshot lifecycle artifact does not match its observation".to_owned(),
        ));
    }
    if source_artifact.is_none()
        && matches!(
            lifecycle.action,
            ScreenshotLifecycleAction::WriteCompleted | ScreenshotLifecycleAction::WriteFailed
        )
    {
        return Err(StoreError::SqliteIdentity(
            "screenshot write lifecycle has no observation image intent".to_owned(),
        ));
    }
    let existing: Option<(String, String, Option<String>, Option<String>)> = transaction
        .query_row(
            "SELECT source_event_id, state, deletion_cause, requested_at FROM screenshot_lifecycle WHERE artifact_id=?1",
            [lifecycle.artifact_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if existing
        .as_ref()
        .is_some_and(|(source_id, _, _, _)| source_id != lifecycle.source_event_id.as_str())
    {
        return Err(StoreError::SqliteIdentity(
            "screenshot lifecycle source provenance changed".to_owned(),
        ));
    }
    match lifecycle.action {
        ScreenshotLifecycleAction::WriteCompleted | ScreenshotLifecycleAction::WriteFailed => {
            if existing.is_some() {
                return Err(StoreError::SqliteIdentity(
                    "screenshot has multiple initial write outcomes".to_owned(),
                ));
            }
        }
        ScreenshotLifecycleAction::Missing => {
            if existing
                .as_ref()
                .is_some_and(|(_, state, _, _)| state != "retained")
            {
                return Err(StoreError::SqliteIdentity(
                    "screenshot has an invalid missing transition".to_owned(),
                ));
            }
        }
        ScreenshotLifecycleAction::DeleteRequested => {
            if existing.as_ref().is_some_and(|(_, state, _, _)| {
                state == "delete-pending" || state == "expired" || state == "user-deleted"
            }) {
                return Err(StoreError::SqliteIdentity(
                    "screenshot has an invalid repeated delete request".to_owned(),
                ));
            }
        }
        ScreenshotLifecycleAction::DeleteCompleted => {
            let expected_state = enum_value(&ScreenshotProjectedState::DeletePending)?;
            let expected_cause = lifecycle
                .deletion_cause
                .as_ref()
                .map(enum_value)
                .transpose()?;
            let expected_requested = lifecycle.requested_at.map(|value| value.to_rfc3339());
            if !existing
                .as_ref()
                .is_some_and(|(_, state, deletion_cause, requested_at)| {
                    state == &expected_state
                        && deletion_cause == &expected_cause
                        && requested_at == &expected_requested
                })
            {
                return Err(StoreError::SqliteIdentity(
                    "screenshot delete completion does not match its request".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn project_chunk(
    transaction: &Transaction<'_>,
    record: &VerifiedRecord,
    faults: FaultInjector,
) -> Result<()> {
    let body = std::str::from_utf8(record.body_bytes())
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    let chunk = ChunkRevision::parse(body)?;
    let existing: Option<String> = transaction
        .query_row(
            "SELECT checksum FROM chunk_revisions WHERE revision_id = ?1",
            [chunk.revision_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != record.checksum() {
            return Err(StoreError::StableIdConflict {
                id: chunk.revision_id.to_string(),
            });
        }
        return Ok(());
    }
    let current: Option<String> = transaction
        .query_row(
            "SELECT revision_id FROM current_chunks WHERE chunk_id = ?1",
            [chunk.chunk_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if current.as_deref()
        != chunk
            .prior_revision_id
            .as_ref()
            .map(|revision| revision.as_str())
    {
        return Err(StoreError::StableIdConflict {
            id: chunk.revision_id.to_string(),
        });
    }
    transaction.execute(
        "INSERT INTO chunk_revisions(revision_id, chunk_id, prior_revision_id, checksum, window_start, window_end, generated_at, input_digest, body_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            chunk.revision_id.as_str(),
            chunk.chunk_id.as_str(),
            chunk.prior_revision_id.as_ref().map(|id| id.as_str()),
            record.checksum(),
            chunk.window.start.to_rfc3339(),
            chunk.window.end.to_rfc3339(),
            chunk.generated_at.to_rfc3339(),
            chunk.input_digest,
            body,
        ],
    )?;
    faults.check(FaultPoint::AfterRowInsert)?;
    for (ordinal, event_id) in chunk.supporting_event_ids.iter().enumerate() {
        let ordinal = i64::try_from(ordinal)
            .map_err(|_| StoreError::InvalidPath("chunk evidence ordinal overflow".to_owned()))?;
        transaction.execute(
            "INSERT INTO chunk_evidence_refs(revision_id, event_id, ordinal) VALUES(?1, ?2, ?3)",
            params![chunk.revision_id.as_str(), event_id.as_str(), ordinal],
        )?;
    }
    for estimate in &chunk.duration_estimates {
        transaction.execute(
            "INSERT INTO chunk_dimensions(revision_id, dimension, dimension_key, label, estimated_seconds) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                chunk.revision_id.as_str(),
                enum_value(&estimate.dimension)?,
                estimate.key,
                estimate.label,
                estimate.estimated_seconds,
            ],
        )?;
    }
    for (ordinal, transition) in chunk.transitions.iter().enumerate() {
        let ordinal = i64::try_from(ordinal)
            .map_err(|_| StoreError::InvalidPath("chunk transition ordinal overflow".to_owned()))?;
        transaction.execute(
            "INSERT INTO chunk_transitions(revision_id, ordinal, at, from_key, to_key, supporting_event_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                chunk.revision_id.as_str(),
                ordinal,
                transition.at.to_rfc3339(),
                transition.from_key,
                transition.to_key,
                transition.supporting_event_id.as_str(),
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO current_chunks(chunk_id, revision_id) VALUES(?1, ?2) ON CONFLICT(chunk_id) DO UPDATE SET revision_id=excluded.revision_id",
        params![chunk.chunk_id.as_str(), chunk.revision_id.as_str()],
    )?;
    faults.check(FaultPoint::AfterCurrentPointerUpdate)?;
    transaction.execute(
        "INSERT INTO aggregation_watermark(singleton, through_utc, revision_id) VALUES(1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET through_utc=excluded.through_utc, revision_id=excluded.revision_id WHERE aggregation_watermark.through_utc IS NULL OR excluded.through_utc >= aggregation_watermark.through_utc",
        params![chunk.window.end.to_rfc3339(), chunk.revision_id.as_str()],
    )?;
    faults.check(FaultPoint::AfterWatermarkUpdate)?;
    let device_id: Option<String> = transaction
        .query_row(
            "SELECT json_extract(events.body_json, '$.device_id')
             FROM chunk_evidence_refs refs
             JOIN events ON events.event_id=refs.event_id
             WHERE refs.revision_id=?1
             ORDER BY refs.ordinal
             LIMIT 1",
            [chunk.revision_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(device_id) = device_id {
        transaction.execute(
            "DELETE FROM aggregation_pending_buckets
             WHERE device_id=?1 AND bucket_start=?2
               AND NOT EXISTS (
                 SELECT 1
                 FROM aggregation_bucket_events membership
                 WHERE membership.device_id=?1 AND membership.bucket_start=?2
                   AND NOT EXISTS (
                     SELECT 1 FROM chunk_evidence_refs refs
                     WHERE refs.revision_id=?3 AND refs.event_id=membership.event_id))
               AND NOT EXISTS (
                 SELECT 1
                 FROM chunk_evidence_refs refs
                 WHERE refs.revision_id=?3
                   AND NOT EXISTS (
                     SELECT 1 FROM aggregation_bucket_events membership
                     WHERE membership.device_id=?1 AND membership.bucket_start=?2
                       AND membership.event_id=refs.event_id))",
            params![
                device_id,
                chunk.window.start.to_rfc3339(),
                chunk.revision_id.as_str()
            ],
        )?;
    }
    Ok(())
}

fn update_cursor(transaction: &Transaction<'_>, record: &VerifiedRecord) -> Result<()> {
    let end_offset = i64::try_from(record.end_offset())
        .map_err(|_| StoreError::InvalidPath("journal cursor exceeds SQLite range".to_owned()))?;
    transaction.execute(
        "INSERT INTO projection_cursors(family, shard, byte_offset) VALUES(?1, ?2, ?3) ON CONFLICT(family, shard) DO UPDATE SET byte_offset=excluded.byte_offset",
        params![
            record.family().cursor_name(),
            record.shard(),
            end_offset,
        ],
    )?;
    Ok(())
}

fn current_cursor(transaction: &Transaction<'_>, record: &VerifiedRecord) -> Result<u64> {
    let cursor: Option<i64> = transaction
        .query_row(
            "SELECT byte_offset FROM projection_cursors WHERE family=?1 AND shard=?2",
            params![record.family().cursor_name(), record.shard()],
            |row| row.get(0),
        )
        .optional()?;
    u64::try_from(cursor.unwrap_or_default()).map_err(|_| {
        StoreError::SqliteIdentity("projection cursor is outside the supported range".to_owned())
    })
}

fn verify_already_projected(transaction: &Transaction<'_>, record: &VerifiedRecord) -> Result<()> {
    let (table, id_column) = match record.family() {
        JournalFamily::Events => ("events", "event_id"),
        JournalFamily::Chunks => ("chunk_revisions", "revision_id"),
    };
    let sql = format!("SELECT checksum FROM {table} WHERE {id_column}=?1");
    let checksum: Option<String> = transaction
        .query_row(&sql, [record.stable_id()], |row| row.get(0))
        .optional()?;
    match checksum {
        Some(checksum) if checksum == record.checksum() => Ok(()),
        Some(_) => Err(StoreError::StableIdConflict {
            id: record.stable_id().to_owned(),
        }),
        None => Err(StoreError::SqliteIdentity(format!(
            "projection cursor passed missing stable ID {}",
            record.stable_id()
        ))),
    }
}

fn enum_value<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StoreError::InvalidPath("enum did not serialize as a string".to_owned()))
}
