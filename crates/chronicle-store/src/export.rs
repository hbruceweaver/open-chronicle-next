use std::collections::HashSet;

use chronicle_domain::{
    ActivityFilter, ChunkRevision, DerivedArtifactRevision, ExportCounts, JournalCutoff,
    QueryArtifact, QueryEvent, UtcRange,
};
use rusqlite::{Connection, params};

use crate::{Result, StoreError, StoreQueries};

const MAX_EXPORT_EVENTS: u64 = 100_000;
const MAX_EXPORT_CHUNKS: u64 = 105_408;
const MAX_EXPORT_ARTIFACTS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct StableSnapshotSelection {
    pub events: Vec<QueryEvent>,
    pub chunks: Vec<ChunkRevision>,
    pub artifacts: Vec<QueryArtifact>,
    pub available_counts: ExportCounts,
    pub journal_cutoffs: Vec<JournalCutoff>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct StableExportBuilder {
    queries: StoreQueries,
}

impl StableExportBuilder {
    pub fn new(queries: StoreQueries) -> Result<Self> {
        Ok(Self {
            queries: queries.snapshot()?,
        })
    }

    pub fn context_packet(
        &self,
        filter: &ActivityFilter,
        include_ocr: bool,
        max_bytes: u64,
    ) -> Result<StableSnapshotSelection> {
        filter.range.validate().map_err(StoreError::InvalidPath)?;
        let mut budget = ByteBudget::new(max_bytes)?;
        self.queries.with_connection(|connection| {
            let available_chunks = count_context_chunks(connection, filter)?;
            if available_chunks > MAX_EXPORT_CHUNKS {
                return Err(StoreError::InvalidPath(
                    "context packet exceeds the bounded chunk work limit".to_owned(),
                ));
            }
            let available_events = count_context_events(connection, filter)?;
            if available_events > MAX_EXPORT_EVENTS {
                return Err(StoreError::InvalidPath(
                    "context packet exceeds the bounded event work limit".to_owned(),
                ));
            }
            let mut chunks = Vec::new();
            let mut events = Vec::new();
            let mut event_ids = HashSet::new();
            let mut truncated = false;
            let chunk_bodies = context_chunk_bodies(connection, filter)?;
            'chunks: for body in chunk_bodies {
                let mut chunk = ChunkRevision::parse(&body)?;
                if !include_ocr {
                    chunk.ocr_extracts.clear();
                }
                if !budget.try_add(&chunk)? {
                    truncated = true;
                    break;
                }
                let revision_id = chunk.revision_id.clone();
                chunks.push(chunk);
                let mut statement = connection.prepare(
                    "SELECT events.body_json
                     FROM chunk_evidence_refs refs
                     JOIN events ON events.event_id=refs.event_id
                     WHERE refs.revision_id=?1
                       AND json_extract(events.body_json, '$.observed_at') >= ?2
                       AND json_extract(events.body_json, '$.observed_at') < ?3
                       AND (json_extract(events.body_json, '$.scheduled_at') IS NULL
                            OR (json_extract(events.body_json, '$.scheduled_at') >= ?2
                                AND json_extract(events.body_json, '$.scheduled_at') < ?3))
                       AND (events.kind <> 'recording-gap'
                            OR (json_extract(events.body_json, '$.payload.data.start') >= ?2
                                AND json_extract(events.body_json, '$.payload.data.end') <= ?3))
                     ORDER BY refs.ordinal",
                )?;
                let rows = statement.query_map(
                    params![
                        revision_id.as_str(),
                        filter.range.start.to_rfc3339(),
                        filter.range.end.to_rfc3339(),
                    ],
                    |row| row.get::<_, String>(0),
                )?;
                for row in rows {
                    let body = row?;
                    let event =
                        self.queries
                            .query_event_from_json(connection, &body, include_ocr)?;
                    if !event_ids.insert(event.event_id.clone()) {
                        continue;
                    }
                    if !budget.try_add(&event)? {
                        truncated = true;
                        break 'chunks;
                    }
                    events.push(event);
                }
            }
            truncated |=
                chunks.len() as u64 != available_chunks || events.len() as u64 != available_events;
            Ok(StableSnapshotSelection {
                events,
                chunks,
                artifacts: Vec::new(),
                available_counts: ExportCounts {
                    events: available_events,
                    chunks: available_chunks,
                    artifacts: 0,
                },
                journal_cutoffs: journal_cutoffs(connection)?,
                truncated,
            })
        })
    }

    pub fn full_export(
        &self,
        range: &UtcRange,
        include_ocr: bool,
        include_derived: bool,
        max_bytes: u64,
    ) -> Result<StableSnapshotSelection> {
        range.validate().map_err(StoreError::InvalidPath)?;
        let mut budget = ByteBudget::new(max_bytes)?;
        self.queries.with_connection(|connection| {
            let available = export_counts(connection, range, include_derived)?;
            if available.events > MAX_EXPORT_EVENTS
                || available.chunks > MAX_EXPORT_CHUNKS
                || available.artifacts > MAX_EXPORT_ARTIFACTS
            {
                return Err(StoreError::InvalidPath(
                    "export exceeds a bounded snapshot work limit".to_owned(),
                ));
            }
            let mut events = Vec::new();
            let mut chunks = Vec::new();
            let mut artifacts = Vec::new();
            let mut truncated = false;

            let mut event_statement = connection.prepare(
                "SELECT body_json FROM events
                 WHERE json_extract(body_json, '$.observed_at') >= ?1
                   AND json_extract(body_json, '$.observed_at') < ?2
                   AND (json_extract(body_json, '$.scheduled_at') IS NULL
                        OR (json_extract(body_json, '$.scheduled_at') >= ?1
                            AND json_extract(body_json, '$.scheduled_at') < ?2))
                   AND (kind <> 'recording-gap'
                        OR (json_extract(body_json, '$.payload.data.start') >= ?1
                            AND json_extract(body_json, '$.payload.data.end') <= ?2))
                 ORDER BY json_extract(body_json, '$.observed_at'), event_id",
            )?;
            let event_rows = event_statement.query_map(
                params![range.start.to_rfc3339(), range.end.to_rfc3339()],
                |row| row.get::<_, String>(0),
            )?;
            for row in event_rows {
                let event = self
                    .queries
                    .query_event_from_json(connection, &row?, include_ocr)?;
                if !budget.try_add(&event)? {
                    truncated = true;
                    break;
                }
                events.push(event);
            }

            if !truncated {
                let mut chunk_statement = connection.prepare(
                    "SELECT body_json FROM chunk_revisions
                     WHERE window_start >= ?1 AND window_end <= ?2
                     ORDER BY window_start, chunk_id, generated_at, revision_id",
                )?;
                let chunk_rows = chunk_statement.query_map(
                    params![range.start.to_rfc3339(), range.end.to_rfc3339()],
                    |row| row.get::<_, String>(0),
                )?;
                for row in chunk_rows {
                    let mut chunk = ChunkRevision::parse(&row?)?;
                    if !include_ocr {
                        chunk.ocr_extracts.clear();
                    }
                    if !budget.try_add(&chunk)? {
                        truncated = true;
                        break;
                    }
                    chunks.push(chunk);
                }
            }

            if include_derived && !truncated {
                let mut artifact_statement = connection.prepare(
                    "SELECT body_json FROM artifact_revisions
                     WHERE created_at >= ?1 AND created_at < ?2
                       AND NOT EXISTS (
                         SELECT 1
                         FROM artifact_evidence_refs refs
                         WHERE refs.revision_id=artifact_revisions.revision_id
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
                     ORDER BY created_at, artifact_id, revision_id",
                )?;
                let artifact_rows = artifact_statement.query_map(
                    params![range.start.to_rfc3339(), range.end.to_rfc3339()],
                    |row| row.get::<_, String>(0),
                )?;
                for row in artifact_rows {
                    let artifact = QueryArtifact::from(DerivedArtifactRevision::parse(&row?)?);
                    artifact
                        .validate_public()
                        .map_err(StoreError::InvalidPath)?;
                    if !budget.try_add(&artifact)? {
                        truncated = true;
                        break;
                    }
                    artifacts.push(artifact);
                }
            }
            truncated |= events.len() as u64 != available.events
                || chunks.len() as u64 != available.chunks
                || artifacts.len() as u64 != available.artifacts;
            Ok(StableSnapshotSelection {
                events,
                chunks,
                artifacts,
                available_counts: available,
                journal_cutoffs: journal_cutoffs(connection)?,
                truncated,
            })
        })
    }
}

struct ByteBudget {
    max: usize,
    used: usize,
}

impl ByteBudget {
    fn new(max: u64) -> Result<Self> {
        let max =
            usize::try_from(max).map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        if max == 0 {
            return Err(StoreError::InvalidPath(
                "snapshot byte limit must be positive".to_owned(),
            ));
        }
        Ok(Self { max, used: 0 })
    }

    fn try_add(&mut self, value: &impl serde::Serialize) -> Result<bool> {
        let size = serde_json::to_vec(value)?.len().saturating_add(1);
        let Some(next) = self.used.checked_add(size) else {
            return Ok(false);
        };
        if next > self.max {
            return Ok(false);
        }
        self.used = next;
        Ok(true)
    }
}

fn count_context_chunks(connection: &Connection, filter: &ActivityFilter) -> Result<u64> {
    let evidence_states = serde_json::to_string(&filter.evidence_states)?;
    let count: i64 = connection.query_row(
        "SELECT count(*)
         FROM current_chunks current
         JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
         WHERE revision.window_start >= ?1 AND revision.window_end <= ?2
           AND (?3 = 0 OR EXISTS (
             SELECT 1 FROM chunk_evidence_refs refs
             JOIN observations ON observations.event_id=refs.event_id
             WHERE refs.revision_id=current.revision_id
               AND (?4 IS NULL OR observations.application_bundle_id=?4)
               AND (?5 IS NULL OR instr(lower(coalesce(observations.window_title, '')), lower(?5)) > 0)
               AND (?6 IS NULL OR observations.authorized_domain=?6)
               AND (?7 = 0 OR observations.evidence_state IN (SELECT value FROM json_each(?8)))
           ))",
        params![
            filter.range.start.to_rfc3339(),
            filter.range.end.to_rfc3339(),
            i64::from(
                filter.application_bundle_id.is_some()
                    || filter.window_text.is_some()
                    || filter.authorized_domain.is_some()
                    || !filter.evidence_states.is_empty()
            ),
            filter.application_bundle_id,
            filter.window_text,
            filter.authorized_domain,
            i64::try_from(filter.evidence_states.len())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
            evidence_states,
        ],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|error| StoreError::SqliteIdentity(error.to_string()))
}

fn count_context_events(connection: &Connection, filter: &ActivityFilter) -> Result<u64> {
    let evidence_states = serde_json::to_string(&filter.evidence_states)?;
    let count: i64 = connection.query_row(
        "SELECT count(DISTINCT refs.event_id)
         FROM current_chunks current
         JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
         JOIN chunk_evidence_refs refs ON refs.revision_id=current.revision_id
         JOIN events ON events.event_id=refs.event_id
         WHERE revision.window_start >= ?1 AND revision.window_end <= ?2
           AND json_extract(events.body_json, '$.observed_at') >= ?1
           AND json_extract(events.body_json, '$.observed_at') < ?2
           AND (json_extract(events.body_json, '$.scheduled_at') IS NULL
                OR (json_extract(events.body_json, '$.scheduled_at') >= ?1
                    AND json_extract(events.body_json, '$.scheduled_at') < ?2))
           AND (events.kind <> 'recording-gap'
                OR (json_extract(events.body_json, '$.payload.data.start') >= ?1
                    AND json_extract(events.body_json, '$.payload.data.end') <= ?2))
           AND (?3 = 0 OR EXISTS (
             SELECT 1 FROM chunk_evidence_refs matched
             JOIN observations ON observations.event_id=matched.event_id
             WHERE matched.revision_id=current.revision_id
               AND (?4 IS NULL OR observations.application_bundle_id=?4)
               AND (?5 IS NULL OR instr(lower(coalesce(observations.window_title, '')), lower(?5)) > 0)
               AND (?6 IS NULL OR observations.authorized_domain=?6)
               AND (?7 = 0 OR observations.evidence_state IN (SELECT value FROM json_each(?8)))
           ))",
        params![
            filter.range.start.to_rfc3339(),
            filter.range.end.to_rfc3339(),
            i64::from(
                filter.application_bundle_id.is_some()
                    || filter.window_text.is_some()
                    || filter.authorized_domain.is_some()
                    || !filter.evidence_states.is_empty()
            ),
            filter.application_bundle_id,
            filter.window_text,
            filter.authorized_domain,
            i64::try_from(filter.evidence_states.len())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
            evidence_states,
        ],
        |row| row.get(0),
    )?;
    u64::try_from(count).map_err(|error| StoreError::SqliteIdentity(error.to_string()))
}

fn context_chunk_bodies(connection: &Connection, filter: &ActivityFilter) -> Result<Vec<String>> {
    let evidence_states = serde_json::to_string(&filter.evidence_states)?;
    let mut statement = connection.prepare(
        "SELECT revision.body_json
         FROM current_chunks current
         JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
         WHERE revision.window_start >= ?1 AND revision.window_end <= ?2
           AND (?3 = 0 OR EXISTS (
             SELECT 1 FROM chunk_evidence_refs refs
             JOIN observations ON observations.event_id=refs.event_id
             WHERE refs.revision_id=current.revision_id
               AND (?4 IS NULL OR observations.application_bundle_id=?4)
               AND (?5 IS NULL OR instr(lower(coalesce(observations.window_title, '')), lower(?5)) > 0)
               AND (?6 IS NULL OR observations.authorized_domain=?6)
               AND (?7 = 0 OR observations.evidence_state IN (SELECT value FROM json_each(?8)))
           ))
         ORDER BY revision.window_start, current.chunk_id",
    )?;
    let rows = statement.query_map(
        params![
            filter.range.start.to_rfc3339(),
            filter.range.end.to_rfc3339(),
            i64::from(
                filter.application_bundle_id.is_some()
                    || filter.window_text.is_some()
                    || filter.authorized_domain.is_some()
                    || !filter.evidence_states.is_empty()
            ),
            filter.application_bundle_id,
            filter.window_text,
            filter.authorized_domain,
            i64::try_from(filter.evidence_states.len())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
            evidence_states,
        ],
        |row| row.get::<_, String>(0),
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StoreError::from)
}

fn export_counts(
    connection: &Connection,
    range: &UtcRange,
    include_derived: bool,
) -> Result<ExportCounts> {
    let events: i64 = connection.query_row(
        "SELECT count(*) FROM events
         WHERE json_extract(body_json, '$.observed_at') >= ?1
           AND json_extract(body_json, '$.observed_at') < ?2
           AND (json_extract(body_json, '$.scheduled_at') IS NULL
                OR (json_extract(body_json, '$.scheduled_at') >= ?1
                    AND json_extract(body_json, '$.scheduled_at') < ?2))
           AND (kind <> 'recording-gap'
                OR (json_extract(body_json, '$.payload.data.start') >= ?1
                    AND json_extract(body_json, '$.payload.data.end') <= ?2))",
        params![range.start.to_rfc3339(), range.end.to_rfc3339()],
        |row| row.get(0),
    )?;
    let chunks: i64 = connection.query_row(
        "SELECT count(*) FROM chunk_revisions WHERE window_start >= ?1 AND window_end <= ?2",
        params![range.start.to_rfc3339(), range.end.to_rfc3339()],
        |row| row.get(0),
    )?;
    let artifacts: i64 = if include_derived {
        connection.query_row(
            "SELECT count(*) FROM artifact_revisions
             WHERE created_at >= ?1 AND created_at < ?2
               AND NOT EXISTS (
                 SELECT 1
                 FROM artifact_evidence_refs refs
                 WHERE refs.revision_id=artifact_revisions.revision_id
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
               )",
            params![range.start.to_rfc3339(), range.end.to_rfc3339()],
            |row| row.get(0),
        )?
    } else {
        0
    };
    Ok(ExportCounts {
        events: u64::try_from(events)
            .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?,
        chunks: u64::try_from(chunks)
            .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?,
        artifacts: u64::try_from(artifacts)
            .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?,
    })
}

fn journal_cutoffs(connection: &Connection) -> Result<Vec<JournalCutoff>> {
    let mut statement = connection.prepare(
        "SELECT family, shard, byte_offset FROM projection_cursors ORDER BY family, shard",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (family, shard, byte_offset) = row?;
        Ok(JournalCutoff {
            family,
            shard,
            byte_offset: u64::try_from(byte_offset)
                .map_err(|error| StoreError::SqliteIdentity(error.to_string()))?,
        })
    })
    .collect()
}
