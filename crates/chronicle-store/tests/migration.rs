use std::error::Error;
use std::os::unix::fs::PermissionsExt;

use chronicle_store::{ManagedRoot, SqliteStore};

#[test]
fn committed_v1_database_migrates_to_v2_without_reset() -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let path = root.path().join("index.sqlite3");
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    connection.execute(
        "INSERT INTO schema_versions(component, version, build_id)
         VALUES('store', 1, 'v1-test')",
        [],
    )?;
    let body =
        include_str!("../../../fixtures/synthetic/session-v1/ae4-ten-scheduled-events.jsonl")
            .lines()
            .next()
            .ok_or("fixture is empty")?;
    connection.execute(
        "INSERT INTO events(event_id, checksum, kind, recorded_at, body_json)
         VALUES('ae4-evt-01', 'checksum', 'observation-attempt',
                '2026-07-13T09:00:16Z', ?1)",
        [body],
    )?;
    drop(connection);
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&path, permissions)?;

    let store = SqliteStore::open(root)?;
    let connection = store.connection()?;
    let user_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(user_version, 2);
    let schema_version: i64 = connection.query_row(
        "SELECT version FROM schema_versions WHERE component='store'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(schema_version, 2);
    let preserved: i64 = connection.query_row(
        "SELECT count(*) FROM events WHERE event_id='ae4-evt-01'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(preserved, 1);
    let table_count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master
         WHERE type='table' AND name IN (
           'aggregation_pending_buckets',
           'aggregation_bucket_events',
           'aggregation_build_state')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(table_count, 3);
    let pending: i64 = connection.query_row(
        "SELECT count(*) FROM aggregation_pending_buckets
         WHERE device_id='dev-synthetic' AND bucket_start='2026-07-13T09:00:00+00:00'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(pending, 1);
    let membership: i64 = connection.query_row(
        "SELECT count(*) FROM aggregation_bucket_events WHERE event_id='ae4-evt-01'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(membership, 1);
    Ok(())
}
