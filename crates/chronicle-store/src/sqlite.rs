use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::checksum::checksum_bytes;
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
        let generation = StoreGeneration::initialize(&root)?;
        let store = Self {
            root,
            file_name: file_name.to_owned(),
            generation,
        };
        let connection = store.open_connection()?;
        store.migrate(&connection)?;
        drop(connection);
        let file = store
            .root
            .open_file(&store.file_name, false, false, false)?;
        secure_file(&file, &store.file_name)?;
        Ok(store)
    }

    pub fn connection(&self) -> Result<Connection> {
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
        if identity != Some((1, STORE_BUILD_ID.to_owned())) || user_version != 1 {
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

    fn migrate(&self, connection: &Connection) -> Result<()> {
        connection.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        connection.execute(
            "INSERT INTO schema_versions(component, version, build_id) VALUES('store', 1, ?1) ON CONFLICT(component) DO UPDATE SET version=excluded.version, build_id=excluded.build_id",
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
        ("SELECT * FROM retention_state ORDER BY artifact_id", 3),
        ("SELECT * FROM store_generation ORDER BY singleton", 3),
        ("SELECT * FROM aggregation_watermark ORDER BY singleton", 3),
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
