use std::collections::HashMap;
use std::time::Duration;

use chronicle_domain::{ChunkRevision, EventEnvelope, EventPayload, EvidenceState, OcrState};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::checksum::checksum_bytes;
use crate::maintenance::ensure_normal_store_access;
use crate::permissions::secure_file;
use crate::{JournalFamily, ManagedRoot, Result, StoreError, StoreGeneration};

pub const SQLITE_MINIMUM_VERSION_NUMBER: i32 = 3_051_003;
pub const SQLITE_BUNDLED_VERSION: &str = "3.53.2";
pub const SQLITE_BUNDLED_SOURCE_ID: &str =
    "2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24";
pub const STORE_BUILD_ID: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct SqliteStore {
    root: ManagedRoot,
    file_name: String,
    generation: StoreGeneration,
}

impl SqliteStore {
    pub fn open(root: ManagedRoot) -> Result<Self> {
        Self::open_named(root, "index.sqlite3")
    }

    pub(crate) fn open_named(root: ManagedRoot, file_name: &str) -> Result<Self> {
        if file_name.contains('/') || file_name.contains('\\') || file_name.is_empty() {
            return Err(StoreError::InvalidPath(file_name.to_owned()));
        }
        ensure_normal_store_access(&root)?;
        let generation = StoreGeneration::initialize(&root)?;
        let store = Self {
            root,
            file_name: file_name.to_owned(),
            generation,
        };
        let mut connection = store.open_connection()?;
        store.migrate(&mut connection)?;
        drop(connection);
        let file = store
            .root
            .open_file(&store.file_name, false, false, false)?;
        secure_file(&file, &store.file_name)?;
        ensure_normal_store_access(&store.root)?;
        store.generation.ensure_current(&store.root)?;
        Ok(store)
    }

    pub fn connection(&self) -> Result<Connection> {
        ensure_normal_store_access(&self.root)?;
        self.generation.ensure_current(&self.root)?;
        let connection = self.open_connection()?;
        let identity: Option<(i64, String)> = connection
            .query_row(
                "SELECT version, build_id FROM schema_versions WHERE component='store'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let user_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if identity != Some((5, STORE_BUILD_ID.to_owned())) || user_version != 5 {
            return Err(StoreError::SqliteIdentity(
                "projection migration/build identity mismatch".to_owned(),
            ));
        }
        Ok(connection)
    }

    fn open_connection(&self) -> Result<Connection> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection =
            Connection::open_with_flags(self.root.path().join(&self.file_name), flags)?;
        assert_sqlite_identity(&connection)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let foreign_keys: i64 =
            connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        let journal_mode: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        let synchronous: i64 =
            connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
        if foreign_keys != 1 || !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
            return Err(StoreError::SqliteIdentity(format!(
                "required PRAGMAs not active: foreign_keys={foreign_keys}, journal_mode={journal_mode}, synchronous={synchronous}"
            )));
        }
        Ok(connection)
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn checkpoint(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(connection);
        let file = self.root.open_file(&self.file_name, false, false, false)?;
        file.sync_all()?;
        let root_file = std::fs::File::open(self.root.path())?;
        root_file.sync_all()?;
        Ok(())
    }

    /// Materialize algorithm/store-generation upgrades into the durable derived
    /// dirty set once. Normal aggregation ticks then read only forward buckets
    /// and this bounded pending set instead of rescanning all current chunks.
    pub fn prepare_aggregation_build(
        &self,
        aggregator_version: &str,
        store_generation: u64,
        generation_at: DateTime<Utc>,
    ) -> Result<()> {
        if aggregator_version.is_empty() || store_generation == 0 {
            return Err(StoreError::InvalidPath(
                "aggregation build provenance must be non-empty and nonzero".to_owned(),
            ));
        }
        let generation = i64::try_from(store_generation)
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, i64)> = transaction
            .query_row(
                "SELECT aggregator_version, store_generation
                 FROM aggregation_build_state WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current
            .as_ref()
            .is_some_and(|(version, existing_generation)| {
                version == aggregator_version && *existing_generation == generation
            })
        {
            return Ok(());
        }
        transaction.execute(
            "INSERT INTO aggregation_pending_buckets(
                 device_id, bucket_start, bucket_start_epoch,
                 finalization_cadence_seconds, generation_at)
             SELECT json_extract(events.body_json, '$.device_id'), revision.window_start,
                    unixepoch(revision.window_start),
                    json_extract(revision.body_json, '$.finalization_cadence_seconds'), ?3
             FROM current_chunks current
             JOIN chunk_revisions revision ON revision.revision_id=current.revision_id
             JOIN chunk_evidence_refs refs
               ON refs.revision_id=revision.revision_id AND refs.ordinal=0
             JOIN events ON events.event_id=refs.event_id
             WHERE json_extract(revision.body_json, '$.aggregator_version') <> ?1
                OR json_extract(revision.body_json, '$.store_generation') <> ?2
             ON CONFLICT(device_id, bucket_start) DO UPDATE SET
               finalization_cadence_seconds=max(
                 aggregation_pending_buckets.finalization_cadence_seconds,
                 excluded.finalization_cadence_seconds),
               generation_at=excluded.generation_at",
            params![aggregator_version, generation, generation_at.to_rfc3339()],
        )?;
        transaction.execute(
            "INSERT INTO aggregation_build_state(singleton, aggregator_version, store_generation)
             VALUES(1, ?1, ?2)
             ON CONFLICT(singleton) DO UPDATE SET
               aggregator_version=excluded.aggregator_version,
               store_generation=excluded.store_generation",
            params![aggregator_version, generation],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn snapshot_ids(&self) -> Result<ProjectionSnapshot> {
        let connection = self.connection()?;
        Ok(ProjectionSnapshot {
            event_ids: collect_strings(
                &connection,
                "SELECT event_id FROM events ORDER BY event_id",
            )?,
            chunk_revision_ids: collect_strings(
                &connection,
                "SELECT revision_id FROM chunk_revisions ORDER BY revision_id",
            )?,
            current_chunks: collect_pairs(
                &connection,
                "SELECT chunk_id, revision_id FROM current_chunks ORDER BY chunk_id",
            )?,
            artifact_revision_ids: collect_strings(
                &connection,
                "SELECT revision_id FROM artifact_revisions ORDER BY revision_id",
            )?,
            current_artifacts: collect_pairs(
                &connection,
                "SELECT artifact_id, revision_id FROM current_artifacts ORDER BY artifact_id",
            )?,
            screenshot_lifecycle: collect_pairs(
                &connection,
                "SELECT artifact_id, state FROM screenshot_lifecycle ORDER BY artifact_id",
            )?,
            projection_digest: projection_digest(&connection)?,
        })
    }

    pub fn projection_cursor(&self, family: JournalFamily, shard: &str) -> Result<u64> {
        let connection = self.connection()?;
        let cursor: Option<i64> = connection
            .query_row(
                "SELECT byte_offset FROM projection_cursors WHERE family=?1 AND shard=?2",
                params![family.cursor_name(), shard],
                |row| row.get(0),
            )
            .optional()?;
        u64::try_from(cursor.unwrap_or_default()).map_err(|_| {
            StoreError::SqliteIdentity(
                "projection cursor is outside the supported range".to_owned(),
            )
        })
    }

    pub fn projection_cursors(&self, family: JournalFamily) -> Result<HashMap<String, u64>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT shard, byte_offset FROM projection_cursors WHERE family=?1")?;
        let rows = statement.query_map([family.cursor_name()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.map(|row| {
            let (shard, cursor) = row?;
            let cursor = u64::try_from(cursor).map_err(|_| {
                StoreError::SqliteIdentity(
                    "projection cursor is outside the supported range".to_owned(),
                )
            })?;
            Ok((shard, cursor))
        })
        .collect()
    }

    pub fn event_checksum(&self, event_id: &chronicle_domain::EventId) -> Result<Option<String>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT checksum FROM events WHERE event_id=?1",
                [event_id.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn clear_pending_aggregation_bucket(
        &self,
        device_id: &chronicle_domain::DeviceId,
        bucket_start: DateTime<Utc>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM aggregation_pending_buckets WHERE device_id=?1 AND bucket_start=?2",
            params![device_id.as_str(), bucket_start.to_rfc3339()],
        )?;
        Ok(())
    }

    fn migrate(&self, connection: &mut Connection) -> Result<()> {
        let mut user_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if user_version == 0 {
            connection.execute_batch(include_str!("../migrations/0001_init.sql"))?;
            user_version = 1;
        }
        if user_version == 1 {
            connection.execute_batch(include_str!("../migrations/0002_aggregation_index.sql"))?;
            user_version = 2;
        }
        if user_version == 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(
                "../migrations/0003_health_operation_facts.sql"
            ))?;
            backfill_health_operation_facts(&transaction)?;
            transaction.pragma_update(None, "user_version", 3)?;
            transaction.commit()?;
            user_version = 3;
        }
        if user_version == 3 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !retention_state_has_expiry(&transaction)? {
                transaction
                    .execute("ALTER TABLE retention_state ADD COLUMN expires_at TEXT", [])?;
            }
            transaction.execute_batch(include_str!("../migrations/0004_retention_health.sql"))?;
            backfill_retention_state(&transaction, &self.root)?;
            transaction.pragma_update(None, "user_version", 4)?;
            transaction.commit()?;
            user_version = 4;
        }
        if user_version == 4 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction
                .execute_batch(include_str!("../migrations/0005_projection_identity.sql"))?;
            transaction.execute(
                "INSERT INTO projection_identity(singleton, instance_id) VALUES(1, ?1)
                 ON CONFLICT(singleton) DO NOTHING",
                [Uuid::now_v7().to_string()],
            )?;
            transaction.pragma_update(None, "user_version", 5)?;
            transaction.commit()?;
            user_version = 5;
        }
        if user_version != 5 {
            return Err(StoreError::SqliteIdentity(format!(
                "unsupported projection schema version {user_version}"
            )));
        }
        connection.execute(
            "INSERT INTO schema_versions(component, version, build_id) VALUES('store', 5, ?1) ON CONFLICT(component) DO UPDATE SET version=excluded.version, build_id=excluded.build_id",
            [STORE_BUILD_ID],
        )?;
        let generation_number = i64::try_from(self.generation.generation).map_err(|_| {
            StoreError::InvalidPath("store generation exceeds SQLite range".to_owned())
        })?;
        connection.execute(
            "INSERT INTO store_generation(singleton, generation, epoch_id) VALUES(1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET generation=excluded.generation, epoch_id=excluded.epoch_id",
            params![generation_number, self.generation.epoch_id.to_string()],
        )?;
        Ok(())
    }
}

fn retention_state_has_expiry(transaction: &Transaction<'_>) -> Result<bool> {
    let mut statement = transaction.prepare("PRAGMA table_info(retention_state)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "expires_at" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn backfill_retention_state(transaction: &Transaction<'_>, root: &ManagedRoot) -> Result<()> {
    transaction.execute("DELETE FROM retention_state", [])?;
    let journal = crate::CanonicalJournal::new(root.clone());
    for record in journal.scan_all(JournalFamily::Events, false)?.records {
        let event = EventEnvelope::parse(
            std::str::from_utf8(record.body_bytes())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
        )?;
        match &event.payload {
            EventPayload::ObservationAttempt(attempt) => {
                if let chronicle_domain::ObservationContent::Captured(content) = &attempt.content
                    && let Some(image) = &content.image
                {
                    transaction.execute(
                        "INSERT INTO retention_state(artifact_id, state, updated_at, expires_at)
                         VALUES(?1, 'write-pending', ?2, ?3)
                         ON CONFLICT(artifact_id) DO UPDATE SET
                           state=excluded.state,
                           updated_at=excluded.updated_at,
                           expires_at=excluded.expires_at",
                        params![
                            image.artifact_id.as_str(),
                            event.recorded_at.to_rfc3339(),
                            image.expires_at.to_rfc3339()
                        ],
                    )?;
                }
            }
            EventPayload::ScreenshotLifecycle(lifecycle) => {
                transaction.execute(
                    "INSERT INTO retention_state(artifact_id, state, updated_at, expires_at)
                     VALUES(?1, ?2, ?3, NULL)
                     ON CONFLICT(artifact_id) DO UPDATE SET
                       state=excluded.state,
                       updated_at=excluded.updated_at",
                    params![
                        lifecycle.artifact_id.as_str(),
                        serde_json::to_value(lifecycle.projected_state)?
                            .as_str()
                            .ok_or_else(|| StoreError::SqliteIdentity(
                                "screenshot state did not serialize as text".to_owned()
                            ))?,
                        event.recorded_at.to_rfc3339()
                    ],
                )?;
            }
            EventPayload::RecordingGap(_) => {}
        }
    }
    Ok(())
}

fn backfill_health_operation_facts(transaction: &Transaction<'_>) -> Result<()> {
    {
        let mut statement =
            transaction.prepare("SELECT body_json FROM events ORDER BY event_id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let body = row.get::<_, String>(0)?;
            let event = EventEnvelope::parse(&body)?;
            insert_health_operation_fact(
                transaction,
                "event-projected",
                event.event_id.as_str(),
                event.recorded_at,
            )?;
            if let EventPayload::ObservationAttempt(attempt) = event.payload {
                let scheduled_at = event.scheduled_at.ok_or_else(|| {
                    StoreError::SqliteIdentity(
                        "projected observation attempt has no scheduled_at".to_owned(),
                    )
                })?;
                insert_health_operation_fact(
                    transaction,
                    "scheduled-attempt",
                    event.event_id.as_str(),
                    scheduled_at,
                )?;
                if matches!(
                    attempt.evidence_state,
                    EvidenceState::CapturedNew | EvidenceState::CapturedUnchanged
                ) {
                    insert_health_operation_fact(
                        transaction,
                        "successful-capture",
                        event.event_id.as_str(),
                        event.observed_at,
                    )?;
                }
                if matches!(
                    attempt.ocr_state,
                    OcrState::Complete | OcrState::Empty | OcrState::Partial
                ) {
                    insert_health_operation_fact(
                        transaction,
                        "successful-ocr",
                        event.event_id.as_str(),
                        event.observed_at,
                    )?;
                }
            }
        }
    }

    {
        let mut statement =
            transaction.prepare("SELECT body_json FROM chunk_revisions ORDER BY revision_id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let body = row.get::<_, String>(0)?;
            let chunk = ChunkRevision::parse(&body)?;
            insert_health_operation_fact(
                transaction,
                "chunk-projected",
                chunk.revision_id.as_str(),
                chunk.generated_at,
            )?;
        }
    }
    Ok(())
}

fn insert_health_operation_fact(
    transaction: &Transaction<'_>,
    fact_type: &str,
    stable_id: &str,
    occurred_at: DateTime<Utc>,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO health_operation_facts(
             fact_type, stable_id, occurred_at, occurred_epoch, occurred_subsec_nanos)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![
            fact_type,
            stable_id,
            occurred_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
            occurred_at.timestamp(),
            occurred_at.timestamp_subsec_nanos(),
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSnapshot {
    pub event_ids: Vec<String>,
    pub chunk_revision_ids: Vec<String>,
    pub current_chunks: Vec<(String, String)>,
    pub artifact_revision_ids: Vec<String>,
    pub current_artifacts: Vec<(String, String)>,
    pub screenshot_lifecycle: Vec<(String, String)>,
    pub projection_digest: String,
}

pub fn assert_sqlite_identity(connection: &Connection) -> Result<()> {
    let runtime_version: String =
        connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let runtime_source_id: String =
        connection.query_row("SELECT sqlite_source_id()", [], |row| row.get(0))?;
    if rusqlite::version_number() < SQLITE_MINIMUM_VERSION_NUMBER
        || runtime_version != SQLITE_BUNDLED_VERSION
        || rusqlite::version() != SQLITE_BUNDLED_VERSION
        || runtime_source_id != SQLITE_BUNDLED_SOURCE_ID
    {
        return Err(StoreError::SqliteIdentity(format!(
            "runtime version/source differs from pinned bundle: version={runtime_version}, source_id={runtime_source_id}"
        )));
    }
    Ok(())
}

fn collect_strings(connection: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn collect_pairs(connection: &Connection, sql: &str) -> Result<Vec<(String, String)>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn projection_digest(connection: &Connection) -> Result<String> {
    let mut bytes = Vec::new();
    for (sql, columns) in [
        ("SELECT * FROM schema_versions ORDER BY component", 3),
        ("SELECT * FROM projection_cursors ORDER BY family, shard", 3),
        ("SELECT * FROM events ORDER BY event_id", 5),
        ("SELECT * FROM observations ORDER BY event_id", 11),
        (
            "SELECT * FROM health_operation_facts ORDER BY fact_type, stable_id",
            5,
        ),
        (
            "SELECT event_id, text FROM ocr_fts ORDER BY event_id, text",
            2,
        ),
        ("SELECT * FROM screenshot_lifecycle ORDER BY artifact_id", 7),
        ("SELECT * FROM chunk_revisions ORDER BY revision_id", 9),
        ("SELECT * FROM current_chunks ORDER BY chunk_id", 2),
        (
            "SELECT * FROM chunk_evidence_refs ORDER BY revision_id, ordinal",
            3,
        ),
        (
            "SELECT * FROM chunk_dimensions ORDER BY revision_id, dimension, dimension_key",
            5,
        ),
        (
            "SELECT * FROM chunk_transitions ORDER BY revision_id, ordinal",
            6,
        ),
        ("SELECT * FROM artifact_revisions ORDER BY revision_id", 7),
        ("SELECT * FROM current_artifacts ORDER BY artifact_id", 2),
        (
            "SELECT * FROM artifact_evidence_refs ORDER BY revision_id, evidence_kind, evidence_id",
            3,
        ),
        ("SELECT * FROM retention_state ORDER BY artifact_id", 4),
        ("SELECT * FROM store_generation ORDER BY singleton", 3),
        ("SELECT * FROM aggregation_watermark ORDER BY singleton", 3),
        (
            "SELECT * FROM aggregation_pending_buckets ORDER BY device_id, bucket_start",
            5,
        ),
        (
            "SELECT * FROM aggregation_bucket_events ORDER BY device_id, bucket_start, event_id",
            3,
        ),
        (
            "SELECT * FROM aggregation_build_state ORDER BY singleton",
            3,
        ),
        ("SELECT * FROM registration_receipts ORDER BY receipt_id", 4),
    ] {
        bytes.extend_from_slice(sql.as_bytes());
        let mut statement = connection.prepare(sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for column in 0..columns {
                use rusqlite::types::ValueRef;
                match row.get_ref(column)? {
                    ValueRef::Null => bytes.extend_from_slice(b"N;"),
                    ValueRef::Integer(value) => {
                        bytes.extend_from_slice(format!("I{value};").as_bytes());
                    }
                    ValueRef::Real(value) => {
                        bytes.extend_from_slice(format!("R{:016x};", value.to_bits()).as_bytes());
                    }
                    ValueRef::Text(value) => {
                        bytes.extend_from_slice(format!("T{}:", value.len()).as_bytes());
                        bytes.extend_from_slice(value);
                        bytes.push(b';');
                    }
                    ValueRef::Blob(value) => {
                        bytes.extend_from_slice(format!("B{}:", value.len()).as_bytes());
                        bytes.extend_from_slice(value);
                        bytes.push(b';');
                    }
                }
            }
        }
    }
    Ok(checksum_bytes(&bytes))
}
