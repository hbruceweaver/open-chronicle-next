use chronicle_domain::{
    ActivityFilter, ArtifactId, ArtifactRevisionId, ChunkId, ChunkRevision,
    DerivedArtifactRevision, DeviceId, EventEnvelope, EventId, EventPayload, ImageMetadata,
    ObservationContent, QueryArtifact, QueryEvent, QueryEventPayload, QueryObservation,
    QueryObservationContent, ScreenshotProjectedState, UtcRange,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, SqliteStore, StoreError};

const MAX_EVENT_ROWS: usize = 100_000;
const MAX_CHUNK_ROWS: usize = 105_408;
const MAX_SUPPORTING_EVENTS: usize = 1_000;
const MAX_SHARED_PAGE_ITEMS: u32 = 100;
const AGGREGATION_BATCH_SIZE: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAggregationBucket {
    pub device_id: DeviceId,
    pub bucket_start: DateTime<Utc>,
    pub finalization_cadence_seconds: u32,
    pub generation_at: Option<DateTime<Utc>>,
}

/// Typed, read-only access to the rebuildable projection. Canonical JSON remains
/// the source of every returned fact; SQL is only an index and selection layer.
#[derive(Clone, Debug)]
pub struct StoreQueries {
    source: QuerySource,
}

#[derive(Clone, Debug)]
enum QuerySource {
    Store(SqliteStore),
    Snapshot(Arc<Mutex<Connection>>),
}

impl StoreQueries {
    pub const fn new(sqlite: SqliteStore) -> Self {
        Self {
            source: QuerySource::Store(sqlite),
        }
    }

    /// Pins all reads from this clone family to one SQLite WAL snapshot. The
    /// snapshot does not block projection writers and is released when the last
    /// clone drops.
    pub fn snapshot(&self) -> Result<Self> {
        match &self.source {
            QuerySource::Snapshot(_) => Ok(self.clone()),
            QuerySource::Store(sqlite) => {
                let connection = sqlite.connection()?;
                connection.execute_batch("BEGIN DEFERRED TRANSACTION")?;
                let _: i64 =
                    connection
                        .query_row("SELECT count(*) FROM schema_versions", [], |row| row.get(0))?;
                Ok(Self {
                    source: QuerySource::Snapshot(Arc::new(Mutex::new(connection))),
                })
            }
        }
    }

    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        match &self.source {
            QuerySource::Store(sqlite) => {
                let connection = sqlite.connection()?;
                operation(&connection)
            }
            QuerySource::Snapshot(connection) => {
                let connection = connection
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                operation(&connection)
            }
        }
    }

    pub fn event(&self, event_id: &EventId, include_ocr: bool) -> Result<Option<QueryEvent>> {
        self.with_connection(|connection| {
            let body: Option<String> = connection
                .query_row(
                    "SELECT body_json FROM events WHERE event_id=?1",
                    [event_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            body.map(|body| self.query_event_from_json(connection, &body, include_ocr))
                .transpose()
        })
    }

    pub fn events_in_range(&self, range: &UtcRange) -> Result<Vec<EventEnvelope>> {
        range
            .validate()
            .map_err(|reason| StoreError::InvalidPath(reason.to_owned()))?;
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT body_json FROM events
             WHERE (json_extract(body_json, '$.observed_at') >= ?1
                    AND json_extract(body_json, '$.observed_at') < ?2)
                OR (kind='recording-gap'
                    AND json_extract(body_json, '$.payload.data.start') < ?2
                    AND json_extract(body_json, '$.payload.data.end') > ?1)
             ORDER BY json_extract(body_json, '$.observed_at'), event_id
             LIMIT ?3",
            )?;
            let limit = i64::try_from(MAX_EVENT_ROWS + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let rows = statement.query_map(
                params![range.start.to_rfc3339(), range.end.to_rfc3339(), limit],
                |row| row.get::<_, String>(0),
            )?;
            let events = rows
                .map(|row| EventEnvelope::parse(&row?).map_err(StoreError::from))
                .collect::<Result<Vec<_>>>()?;
            if events.len() > MAX_EVENT_ROWS {
                return Err(StoreError::InvalidPath(
                    "event range exceeds the bounded query row limit".to_owned(),
                ));
            }
            Ok(events)
        })
    }

    pub fn aggregation_events_for_bucket(
        &self,
        device_id: &DeviceId,
        bucket_start: DateTime<Utc>,
    ) -> Result<Vec<EventEnvelope>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT events.body_json
             FROM aggregation_bucket_events membership
             JOIN events ON events.event_id=membership.event_id
             WHERE membership.device_id=?1 AND membership.bucket_start=?2
             ORDER BY json_extract(events.body_json, '$.observed_at'), events.event_id
             LIMIT ?3",
            )?;
            let limit = i64::try_from(MAX_EVENT_ROWS + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let rows = statement.query_map(
                params![device_id.as_str(), bucket_start.to_rfc3339(), limit],
                |row| row.get::<_, String>(0),
            )?;
            let events = rows
                .map(|row| EventEnvelope::parse(&row?).map_err(StoreError::from))
                .collect::<Result<Vec<_>>>()?;
            if events.len() > MAX_EVENT_ROWS {
                return Err(StoreError::InvalidPath(
                    "aggregation bucket exceeds the bounded event row limit".to_owned(),
                ));
            }
            Ok(events)
        })
    }

    pub fn filtered_events(
        &self,
        filter: &ActivityFilter,
        include_ocr: bool,
    ) -> Result<Vec<QueryEvent>> {
        let events = self.events_in_range(&filter.range)?;
        self.with_connection(|connection| {
            events
                .into_iter()
                .filter(|event| event_matches_filter(event, filter))
                .map(|event| self.query_event(connection, event, include_ocr))
                .collect()
        })
    }

    pub fn current_chunk(&self, chunk_id: &ChunkId) -> Result<Option<ChunkRevision>> {
        self.with_connection(|connection| {
            let body: Option<String> = connection
                .query_row(
                    "SELECT revision.body_json
                 FROM current_chunks current
                 JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
                 WHERE current.chunk_id=?1",
                    [chunk_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            body.map(|body| ChunkRevision::parse(&body).map_err(StoreError::from))
                .transpose()
        })
    }

    pub fn artifact(
        &self,
        artifact_id: &ArtifactId,
        revision_id: Option<&ArtifactRevisionId>,
    ) -> Result<Option<QueryArtifact>> {
        self.with_connection(|connection| {
            let body: Option<String> = match revision_id {
                Some(revision_id) => connection
                    .query_row(
                        "SELECT body_json FROM artifact_revisions
                         WHERE artifact_id=?1 AND revision_id=?2",
                        params![artifact_id.as_str(), revision_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?,
                None => connection
                    .query_row(
                        "SELECT revision.body_json
                         FROM current_artifacts current
                         JOIN artifact_revisions revision
                           ON revision.revision_id=current.revision_id
                         WHERE current.artifact_id=?1",
                        [artifact_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?,
            };
            body.map(|body| {
                DerivedArtifactRevision::parse(&body)
                    .map(QueryArtifact::from)
                    .map_err(StoreError::from)
            })
            .transpose()
        })
    }

    pub fn current_artifact_page(
        &self,
        range: &UtcRange,
        after_artifact_id: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<QueryArtifact>, bool)> {
        range.validate().map_err(StoreError::InvalidPath)?;
        if limit == 0 || limit > MAX_SHARED_PAGE_ITEMS {
            return Err(StoreError::InvalidPath(format!(
                "artifact page limit must be 1..={MAX_SHARED_PAGE_ITEMS}"
            )));
        }
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT revision.body_json
                 FROM current_artifacts current
                 JOIN artifact_revisions revision ON revision.revision_id=current.revision_id
                 WHERE revision.created_at >= ?1 AND revision.created_at < ?2
                   AND NOT EXISTS (
                     SELECT 1
                     FROM artifact_evidence_refs refs
                     WHERE refs.revision_id=revision.revision_id
                       AND (
                         (refs.evidence_kind='event' AND NOT EXISTS (
                           SELECT 1 FROM events event
                           WHERE event.event_id=refs.evidence_id
                             AND json_extract(event.body_json, '$.observed_at') >= ?1
                             AND json_extract(event.body_json, '$.observed_at') < ?2
                             AND (json_extract(event.body_json, '$.scheduled_at') IS NULL
                                  OR (json_extract(event.body_json, '$.scheduled_at') >= ?1
                                      AND json_extract(event.body_json, '$.scheduled_at') < ?2))
                             AND (event.kind <> 'recording-gap'
                                  OR (json_extract(event.body_json, '$.payload.data.start') >= ?1
                                      AND json_extract(event.body_json, '$.payload.data.end') <= ?2))
                         ))
                         OR (refs.evidence_kind='chunk' AND NOT EXISTS (
                           SELECT 1
                           FROM current_chunks chunk
                           JOIN chunk_revisions chunk_revision
                             ON chunk_revision.revision_id=chunk.revision_id
                           WHERE chunk.chunk_id=refs.evidence_id
                             AND chunk_revision.window_start >= ?1
                             AND chunk_revision.window_end <= ?2
                         ))
                         OR refs.evidence_kind NOT IN ('event', 'chunk')
                       )
                   )
                   AND (?3 IS NULL OR current.artifact_id > ?3)
                 ORDER BY current.artifact_id
                 LIMIT ?4",
            )?;
            let rows = statement.query_map(
                params![
                    range.start.to_rfc3339(),
                    range.end.to_rfc3339(),
                    after_artifact_id,
                    i64::from(limit.saturating_add(1)),
                ],
                |row| row.get::<_, String>(0),
            )?;
            let mut artifacts = rows
                .map(|row| {
                    DerivedArtifactRevision::parse(&row?)
                        .map(QueryArtifact::from)
                        .map_err(StoreError::from)
                })
                .collect::<Result<Vec<_>>>()?;
            let truncated = artifacts.len() > usize::try_from(limit).unwrap_or(usize::MAX);
            artifacts.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok((artifacts, truncated))
        })
    }

    pub fn current_chunks_in_range(&self, range: &UtcRange) -> Result<Vec<ChunkRevision>> {
        range
            .validate()
            .map_err(|reason| StoreError::InvalidPath(reason.to_owned()))?;
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT revision.body_json
             FROM current_chunks current
             JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
             WHERE revision.window_start < ?2 AND revision.window_end > ?1
             ORDER BY revision.window_start, current.chunk_id
             LIMIT ?3",
            )?;
            let limit = i64::try_from(MAX_CHUNK_ROWS + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let rows = statement.query_map(
                params![range.start.to_rfc3339(), range.end.to_rfc3339(), limit],
                |row| row.get::<_, String>(0),
            )?;
            let chunks = rows
                .map(|row| ChunkRevision::parse(&row?).map_err(StoreError::from))
                .collect::<Result<Vec<_>>>()?;
            if chunks.len() > MAX_CHUNK_ROWS {
                return Err(StoreError::InvalidPath(
                    "chunk range exceeds the bounded query row limit".to_owned(),
                ));
            }
            Ok(chunks)
        })
    }

    /// SQL-filtered keyset page used by the shared service. Filtering and the
    /// page+1 bound happen before canonical JSON materialization.
    pub fn current_chunk_page(
        &self,
        filter: &ActivityFilter,
        after_chunk_id: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<ChunkRevision>, bool)> {
        filter.range.validate().map_err(StoreError::InvalidPath)?;
        if limit == 0 || limit > MAX_SHARED_PAGE_ITEMS {
            return Err(StoreError::InvalidPath(format!(
                "chunk page limit must be 1..={MAX_SHARED_PAGE_ITEMS}"
            )));
        }
        self.with_connection(|connection| {
            let cursor = after_chunk_id
                .map(|chunk_id| {
                    connection
                        .query_row(
                            "SELECT revision.window_start, current.chunk_id
                             FROM current_chunks current
                             JOIN chunk_revisions revision
                               ON revision.revision_id=current.revision_id
                             WHERE current.chunk_id=?1",
                            [chunk_id],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()?
                        .ok_or(StoreError::CursorScopeMismatch)
                })
                .transpose()?;
            let (cursor_start, cursor_id) = cursor
                .map(|(start, id)| (Some(start), Some(id)))
                .unwrap_or((None, None));
            let evidence_states = serde_json::to_string(&filter.evidence_states)?;
            let evidence_state_count = i64::try_from(filter.evidence_states.len())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let filters_present = i64::from(
                filter.application_bundle_id.is_some()
                    || filter.window_text.is_some()
                    || filter.authorized_domain.is_some()
                    || !filter.evidence_states.is_empty(),
            );
            let sql_limit = i64::from(limit.saturating_add(1));
            let mut statement = connection.prepare(
                "SELECT revision.body_json
                 FROM current_chunks current
                 JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
                 WHERE revision.window_start < ?2 AND revision.window_end > ?1
                   AND (?3 IS NULL OR revision.window_start > ?3
                        OR (revision.window_start = ?3 AND current.chunk_id > ?4))
                   AND (?5 = 0 OR EXISTS (
                       SELECT 1
                       FROM chunk_evidence_refs refs
                       JOIN observations ON observations.event_id=refs.event_id
                       WHERE refs.revision_id=current.revision_id
                         AND (?6 IS NULL OR observations.application_bundle_id=?6)
                         AND (?7 IS NULL OR instr(
                              lower(coalesce(observations.window_title, '')), lower(?7)) > 0)
                         AND (?8 IS NULL OR observations.authorized_domain=?8)
                         AND (?9 = 0 OR observations.evidence_state IN (
                              SELECT value FROM json_each(?10)))
                   ))
                 ORDER BY revision.window_start, current.chunk_id
                 LIMIT ?11",
            )?;
            let rows = statement.query_map(
                params![
                    filter.range.start.to_rfc3339(),
                    filter.range.end.to_rfc3339(),
                    cursor_start,
                    cursor_id,
                    filters_present,
                    filter.application_bundle_id,
                    filter.window_text,
                    filter.authorized_domain,
                    evidence_state_count,
                    evidence_states,
                    sql_limit,
                ],
                |row| row.get::<_, String>(0),
            )?;
            let mut chunks = rows
                .map(|row| ChunkRevision::parse(&row?).map_err(StoreError::from))
                .collect::<Result<Vec<_>>>()?;
            let truncated = chunks.len() > usize::try_from(limit).unwrap_or(usize::MAX);
            chunks.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok((chunks, truncated))
        })
    }

    pub fn bounded_query_events_in_range(
        &self,
        range: &UtcRange,
        include_ocr: bool,
        limit: u32,
    ) -> Result<Vec<QueryEvent>> {
        range.validate().map_err(StoreError::InvalidPath)?;
        if limit == 0 || limit > MAX_SUPPORTING_EVENTS as u32 {
            return Err(StoreError::InvalidPath(format!(
                "event response limit must be 1..={MAX_SUPPORTING_EVENTS}"
            )));
        }
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT body_json FROM events
                 WHERE (json_extract(body_json, '$.observed_at') >= ?1
                        AND json_extract(body_json, '$.observed_at') < ?2)
                    OR (kind='recording-gap'
                        AND json_extract(body_json, '$.payload.data.start') < ?2
                        AND json_extract(body_json, '$.payload.data.end') > ?1)
                 ORDER BY json_extract(body_json, '$.observed_at'), event_id
                 LIMIT ?3",
            )?;
            let sql_limit = i64::from(limit.saturating_add(1));
            let rows = statement.query_map(
                params![range.start.to_rfc3339(), range.end.to_rfc3339(), sql_limit],
                |row| row.get::<_, String>(0),
            )?;
            let events = rows
                .map(|row| self.query_event_from_json(connection, &row?, include_ocr))
                .collect::<Result<Vec<_>>>()?;
            if events.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
                return Err(StoreError::InvalidPath(
                    "moment response exceeds the bounded event row limit".to_owned(),
                ));
            }
            Ok(events)
        })
    }

    pub fn supporting_events(
        &self,
        chunk_id: &ChunkId,
        include_ocr: bool,
    ) -> Result<Vec<QueryEvent>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT events.body_json
             FROM current_chunks current
             JOIN chunk_evidence_refs refs ON refs.revision_id=current.revision_id
             JOIN events ON events.event_id=refs.event_id
             WHERE current.chunk_id=?1
             ORDER BY refs.ordinal
             LIMIT ?2",
            )?;
            let limit = i64::try_from(MAX_SUPPORTING_EVENTS + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let rows = statement.query_map(params![chunk_id.as_str(), limit], |row| {
                row.get::<_, String>(0)
            })?;
            let events = rows
                .map(|row| self.query_event_from_json(connection, &row?, include_ocr))
                .collect::<Result<Vec<_>>>()?;
            if events.len() > MAX_SUPPORTING_EVENTS {
                return Err(StoreError::InvalidPath(
                    "chunk evidence exceeds the bounded query row limit".to_owned(),
                ));
            }
            Ok(events)
        })
    }

    pub fn aggregation_watermark(
        &self,
    ) -> Result<Option<(DateTime<Utc>, chronicle_domain::ChunkRevisionId)>> {
        self.with_connection(|connection| {
            let value: Option<(String, String)> = connection
                .query_row(
                    "SELECT through_utc, revision_id FROM aggregation_watermark WHERE singleton=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            value
                .map(|(through, revision)| {
                    let through = through.parse::<DateTime<Utc>>().map_err(|error| {
                        StoreError::SqliteIdentity(format!(
                            "aggregation watermark is not UTC: {error}"
                        ))
                    })?;
                    let revision = chronicle_domain::ChunkRevisionId::new(revision)
                        .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?;
                    Ok((through, revision))
                })
                .transpose()
        })
    }

    pub fn pending_aggregation_buckets(&self) -> Result<Vec<PendingAggregationBucket>> {
        self.pending_aggregation_buckets_query(None, 30)
            .map(|(values, _)| values)
    }

    pub fn due_aggregation_bucket_batch(
        &self,
        now: DateTime<Utc>,
        configured_cadence_seconds: u32,
    ) -> Result<(Vec<PendingAggregationBucket>, bool)> {
        self.pending_aggregation_buckets_query(Some(now), configured_cadence_seconds)
    }

    fn pending_aggregation_buckets_query(
        &self,
        due_at: Option<DateTime<Utc>>,
        configured_cadence_seconds: u32,
    ) -> Result<(Vec<PendingAggregationBucket>, bool)> {
        if !matches!(configured_cadence_seconds, 30 | 60) {
            return Err(StoreError::InvalidPath(
                "aggregation cadence must be 30 or 60 seconds".to_owned(),
            ));
        }
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT device_id, bucket_start, finalization_cadence_seconds, generation_at
             FROM aggregation_pending_buckets
             WHERE (?1 IS NULL OR bucket_start_epoch + 300
                    + max(finalization_cadence_seconds, ?2) <= ?1)
             ORDER BY bucket_start_epoch, device_id
             LIMIT ?3",
            )?;
            let limit = i64::try_from(AGGREGATION_BATCH_SIZE + 1)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let rows = statement.query_map(
                params![
                    due_at.map(|value| value.timestamp()),
                    configured_cadence_seconds,
                    limit,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )?;
            let mut values = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            let has_more = values.len() > AGGREGATION_BATCH_SIZE;
            values.truncate(AGGREGATION_BATCH_SIZE);
            let values = values
                .into_iter()
                .map(|(device, start, cadence, generation_at)| {
                    let device_id = DeviceId::new(device)
                        .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?;
                    let bucket_start = start.parse::<DateTime<Utc>>().map_err(|error| {
                        StoreError::SqliteIdentity(format!(
                            "pending aggregation bucket is not UTC: {error}"
                        ))
                    })?;
                    let generation_at = generation_at
                        .map(|value| {
                            value.parse::<DateTime<Utc>>().map_err(|error| {
                                StoreError::SqliteIdentity(format!(
                                    "aggregation generation time is not UTC: {error}"
                                ))
                            })
                        })
                        .transpose()?;
                    Ok(PendingAggregationBucket {
                        device_id,
                        bucket_start,
                        finalization_cadence_seconds: cadence,
                        generation_at,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((values, has_more))
        })
    }

    pub(crate) fn query_event_from_json(
        &self,
        connection: &rusqlite::Connection,
        body: &str,
        include_ocr: bool,
    ) -> Result<QueryEvent> {
        self.query_event(connection, EventEnvelope::parse(body)?, include_ocr)
    }

    pub(crate) fn query_event(
        &self,
        connection: &rusqlite::Connection,
        event: EventEnvelope,
        include_ocr: bool,
    ) -> Result<QueryEvent> {
        let payload = match event.payload {
            EventPayload::ObservationAttempt(attempt) => {
                let content = match attempt.content {
                    ObservationContent::Captured(content) => {
                        let image = content
                            .image
                            .as_ref()
                            .map(|image| image_metadata(connection, &image.artifact_id))
                            .transpose()?
                            .flatten();
                        QueryObservationContent::Captured {
                            context: content.context,
                            content_hash: content.content_hash,
                            ocr: include_ocr.then_some(content.ocr).flatten(),
                            image,
                        }
                    }
                    ObservationContent::Unchanged(content) => {
                        let image = content
                            .image_artifact_id
                            .as_ref()
                            .map(|artifact_id| image_metadata(connection, artifact_id))
                            .transpose()?
                            .flatten();
                        QueryObservationContent::Unchanged {
                            context: content.context,
                            content_hash: content.content_hash,
                            previous_event_id: content.previous_event_id,
                            reused_ocr_event_id: content.reused_ocr_event_id,
                            image,
                        }
                    }
                    ObservationContent::Protected(content) => {
                        QueryObservationContent::Protected(content)
                    }
                    ObservationContent::NoEvidence(content) => {
                        QueryObservationContent::NoEvidence(content)
                    }
                };
                QueryEventPayload::ObservationAttempt(Box::new(QueryObservation {
                    cadence_seconds: attempt.cadence_seconds,
                    attempt_status: attempt.attempt_status,
                    evidence_state: attempt.evidence_state,
                    presence_state: attempt.presence_state,
                    idle_seconds: attempt.idle_seconds,
                    ocr_state: attempt.ocr_state,
                    content,
                }))
            }
            EventPayload::RecordingGap(gap) => QueryEventPayload::RecordingGap(gap),
            EventPayload::ScreenshotLifecycle(lifecycle) => {
                QueryEventPayload::ScreenshotLifecycle(lifecycle)
            }
        };
        Ok(QueryEvent {
            event_id: event.event_id,
            device_id: event.device_id,
            scheduled_at: event.scheduled_at,
            observed_at: event.observed_at,
            recorded_at: event.recorded_at,
            display_timezone: event.display_timezone,
            source: event.source,
            kind: event.kind,
            payload,
        })
    }
}

fn image_metadata(
    connection: &rusqlite::Connection,
    artifact_id: &chronicle_domain::ImageArtifactId,
) -> Result<Option<ImageMetadata>> {
    let projected: Option<(String, String)> = connection
        .query_row(
            "SELECT lifecycle.state, events.body_json
             FROM screenshot_lifecycle lifecycle
             JOIN events ON events.event_id=lifecycle.source_event_id
             WHERE lifecycle.artifact_id=?1",
            [artifact_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    projected
        .map(|(state, source_body)| {
            let state = serde_json::from_value::<ScreenshotProjectedState>(
                serde_json::Value::String(state),
            )?;
            let source = EventEnvelope::parse(&source_body)?;
            let expires_at = match source.payload {
                EventPayload::ObservationAttempt(attempt) => match attempt.content {
                    ObservationContent::Captured(content) => content
                        .image
                        .filter(|image| image.artifact_id == *artifact_id)
                        .map(|image| image.expires_at),
                    ObservationContent::Unchanged(_)
                    | ObservationContent::Protected(_)
                    | ObservationContent::NoEvidence(_) => None,
                },
                EventPayload::RecordingGap(_) | EventPayload::ScreenshotLifecycle(_) => None,
            }
            .ok_or_else(|| {
                StoreError::SqliteIdentity(
                    "screenshot lifecycle source has no matching image intent".to_owned(),
                )
            })?;
            Ok(ImageMetadata {
                artifact_id: artifact_id.clone(),
                state,
                expires_at: Some(expires_at),
            })
        })
        .transpose()
}

pub(crate) fn event_matches_filter(event: &EventEnvelope, filter: &ActivityFilter) -> bool {
    let EventPayload::ObservationAttempt(attempt) = &event.payload else {
        return filter.application_bundle_id.is_none()
            && filter.window_text.is_none()
            && filter.authorized_domain.is_none()
            && filter.evidence_states.is_empty();
    };
    if !filter.evidence_states.is_empty()
        && !filter.evidence_states.contains(&attempt.evidence_state)
    {
        return false;
    }
    let context = match &attempt.content {
        ObservationContent::Captured(content) => Some(&content.context),
        ObservationContent::Unchanged(content) => Some(&content.context),
        ObservationContent::Protected(_) | ObservationContent::NoEvidence(_) => None,
    };
    if filter
        .application_bundle_id
        .as_ref()
        .is_some_and(|expected| {
            context.is_none_or(|context| &context.application_bundle_id != expected)
        })
    {
        return false;
    }
    if filter.window_text.as_ref().is_some_and(|expected| {
        let expected = expected.to_lowercase();
        context
            .and_then(|context| context.window_title.as_ref())
            .is_none_or(|title| !title.to_lowercase().contains(&expected))
    }) {
        return false;
    }
    if filter.authorized_domain.as_ref().is_some_and(|expected| {
        context
            .and_then(|context| context.authorized_domain.as_ref())
            .is_none_or(|domain| &domain.domain != expected)
    }) {
        return false;
    }
    true
}
use std::sync::{Arc, Mutex};
