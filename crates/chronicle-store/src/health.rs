use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chronicle_domain::{
    ChunkRevision, EventEnvelope, HealthOperationTimes, ScreenshotRetentionHealthSummary,
    StorageHealthSummary,
};
use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;

use crate::{CanonicalJournal, JournalFamily, ManagedRoot, Result, SqliteStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreHealthMetrics {
    pub latest: HealthOperationTimes,
    pub aggregation_watermark: Option<DateTime<Utc>>,
    pub aggregation_pending_buckets: u64,
    pub projection_pending_records: u64,
    pub projection_lag_seconds: u64,
    pub screenshot_retention: ScreenshotRetentionHealthSummary,
}

const MANAGED_SIZE_CACHE_TTL: Duration = Duration::from_secs(30);

pub fn storage_health_summary(root: &ManagedRoot) -> Result<StorageHealthSummary> {
    Ok(StorageHealthSummary {
        managed_bytes: managed_size_cached(root.path())?,
        available_bytes: storage_available_bytes(root)?,
    })
}

pub fn storage_available_bytes(root: &ManagedRoot) -> Result<u64> {
    let stat = rustix::fs::statvfs(root.path()).map_err(crate::permissions::io_error)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

pub fn store_health_metrics(
    root: &ManagedRoot,
    sqlite: &SqliteStore,
    observed_at: DateTime<Utc>,
) -> Result<StoreHealthMetrics> {
    let connection = sqlite.connection()?;
    let last_scheduled_attempt_at =
        query_latest_fact(&connection, "scheduled-attempt", observed_at)?;
    let last_successful_capture_at =
        query_latest_fact(&connection, "successful-capture", observed_at)?;
    let last_successful_ocr_at = query_latest_fact(&connection, "successful-ocr", observed_at)?;
    let last_event_projection_at = query_latest_fact(&connection, "event-projected", observed_at)?;
    let last_chunk_at = query_latest_fact(&connection, "chunk-projected", observed_at)?;
    let last_projection_at = [last_event_projection_at, last_chunk_at]
        .into_iter()
        .flatten()
        .max();
    let aggregation_watermark = query_utc(
        &connection,
        "SELECT through_utc FROM aggregation_watermark
         WHERE singleton=1 AND through_utc <= ?1",
        observed_at,
    )?;
    let pending: i64 = connection.query_row(
        "SELECT count(*) FROM aggregation_pending_buckets",
        [],
        |row| row.get(0),
    )?;
    let aggregation_pending_buckets = u64::try_from(pending)
        .map_err(|_| StoreError::SqliteIdentity("negative aggregation pending count".to_owned()))?;
    let screenshot_retention = screenshot_retention_health(&connection)?;
    drop(connection);

    let journal = CanonicalJournal::new(root.clone());
    let pending_events = journal.unprojected_records(JournalFamily::Events, sqlite)?;
    let pending_chunks = journal.unprojected_records(JournalFamily::Chunks, sqlite)?;
    let pending_count = pending_events
        .len()
        .checked_add(pending_chunks.len())
        .ok_or_else(|| StoreError::InvalidPath("projection lag count overflow".to_owned()))?;
    let projection_pending_records = u64::try_from(pending_count)
        .map_err(|_| StoreError::InvalidPath("projection lag count overflow".to_owned()))?;
    let mut last_journal_at = last_projection_at;
    for record in pending_events {
        let event = EventEnvelope::parse(
            std::str::from_utf8(record.body_bytes())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
        )?;
        if event.recorded_at <= observed_at {
            last_journal_at = Some(
                last_journal_at.map_or(event.recorded_at, |prior| prior.max(event.recorded_at)),
            );
        }
    }
    for record in pending_chunks {
        let chunk = ChunkRevision::parse(
            std::str::from_utf8(record.body_bytes())
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
        )?;
        if chunk.generated_at <= observed_at {
            last_journal_at = Some(
                last_journal_at.map_or(chunk.generated_at, |prior| prior.max(chunk.generated_at)),
            );
        }
    }
    let projection_lag_seconds = if projection_pending_records == 0 {
        0
    } else {
        let since = last_projection_at
            .or(last_journal_at)
            .unwrap_or(observed_at);
        u64::try_from((observed_at - since).num_seconds().max(0)).unwrap_or(u64::MAX)
    };
    Ok(StoreHealthMetrics {
        latest: HealthOperationTimes {
            last_scheduled_attempt_at,
            last_successful_capture_at,
            last_successful_ocr_at,
            last_journal_at,
            last_projection_at,
            last_chunk_at,
        },
        aggregation_watermark,
        aggregation_pending_buckets,
        projection_pending_records,
        projection_lag_seconds,
        screenshot_retention,
    })
}

fn screenshot_retention_health(
    connection: &rusqlite::Connection,
) -> Result<ScreenshotRetentionHealthSummary> {
    let mut summary = ScreenshotRetentionHealthSummary::default();
    let mut statement = connection
        .prepare("SELECT state, count(*) FROM retention_state GROUP BY state ORDER BY state")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (state, count) = row?;
        let count = u64::try_from(count).map_err(|_| {
            StoreError::SqliteIdentity("negative screenshot retention count".to_owned())
        })?;
        match state.as_str() {
            "write-pending" => summary.write_pending = count,
            "retained" => summary.retained = count,
            "delete-pending" => summary.delete_pending = count,
            "expired" => summary.expired = count,
            "user-deleted" => summary.user_deleted = count,
            "missing" => summary.missing = count,
            "write-failed" => summary.write_failed = count,
            _ => {
                return Err(StoreError::SqliteIdentity(format!(
                    "unknown screenshot retention state: {state}"
                )));
            }
        }
    }
    let next_expiry: Option<String> = connection
        .query_row(
            "SELECT min(expires_at) FROM retention_state
             WHERE state='retained' AND expires_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    summary.next_expiry_at = next_expiry
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|error| {
                StoreError::SqliteIdentity(format!(
                    "screenshot retention expiry is not UTC: {error}"
                ))
            })
        })
        .transpose()?;
    Ok(summary)
}

fn query_utc(
    connection: &rusqlite::Connection,
    sql: &str,
    observed_at: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let value: Option<String> = connection
        .query_row(sql, [observed_at.to_rfc3339()], |row| row.get(0))
        .optional()?
        .flatten();
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|error| {
                StoreError::SqliteIdentity(format!("health timestamp is not UTC: {error}"))
            })
        })
        .transpose()
}

fn query_latest_fact(
    connection: &rusqlite::Connection,
    fact_type: &str,
    observed_at: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let value: Option<String> = connection
        .query_row(
            "SELECT occurred_at FROM health_operation_facts
             WHERE fact_type=?1
               AND (occurred_epoch, occurred_subsec_nanos) <= (?2, ?3)
             ORDER BY occurred_epoch DESC, occurred_subsec_nanos DESC, stable_id DESC
             LIMIT 1",
            rusqlite::params![
                fact_type,
                observed_at.timestamp(),
                observed_at.timestamp_subsec_nanos()
            ],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|error| {
                StoreError::SqliteIdentity(format!("health timestamp is not UTC: {error}"))
            })
        })
        .transpose()
}

fn managed_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            total = total
                .checked_add(managed_size(&entry.path())?)
                .ok_or_else(|| {
                    StoreError::InvalidPath("managed storage size overflow".to_owned())
                })?;
        } else if metadata.is_file() {
            total = total.checked_add(metadata.len()).ok_or_else(|| {
                StoreError::InvalidPath("managed storage size overflow".to_owned())
            })?;
        }
    }
    Ok(total)
}

fn managed_size_cached(path: &Path) -> Result<u64> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, u64)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((measured_at, bytes)) = cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(path)
        .copied()
        && measured_at.elapsed() < MANAGED_SIZE_CACHE_TTL
    {
        return Ok(bytes);
    }
    let bytes = managed_size(path)?;
    cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(path.to_path_buf(), (Instant::now(), bytes));
    Ok(bytes)
}
