mod common;

use chronicle_store::RecoveryManager;

#[test]
fn canonical_sources_rebuild_to_identical_projection() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    root.atomic_write(
        "receipts/agent-registrations.json",
        br#"[{"receipt_id":"receipt-001","client_id":"codex","updated_at":"2026-07-13T09:00:00Z","scope":"read"}]"#,
    )?;
    root.atomic_write("config.json", br#"{"capture_interval_seconds":30}"#)?;
    common::seed_canonical(&root, &projector)?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    let canonical_projection = sqlite.snapshot_ids()?;
    let retained_expiry: String = sqlite.connection()?.query_row(
        "SELECT expires_at FROM retention_state WHERE artifact_id='img-001'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(retained_expiry, "2026-07-14T09:00:16+00:00");
    sqlite.connection()?.execute(
        "UPDATE retention_state SET expires_at='2099-01-01T00:00:00+00:00'
         WHERE artifact_id='img-001'",
        [],
    )?;
    assert_ne!(sqlite.snapshot_ids()?, canonical_projection);
    let (_report, repaired_projection) = RecoveryManager::new(root.clone()).rebuild_index()?;
    assert_eq!(repaired_projection, canonical_projection);
    let connection = sqlite.connection()?;
    let ocr_rows: i64 =
        connection.query_row("SELECT count(*) FROM ocr_fts", [], |row| row.get(0))?;
    let typed_rows: i64 =
        connection.query_row("SELECT count(*) FROM observations", [], |row| row.get(0))?;
    let receipt_rows: i64 =
        connection.query_row("SELECT count(*) FROM registration_receipts", [], |row| {
            row.get(0)
        })?;
    assert!(ocr_rows > 0);
    assert!(typed_rows > 0);
    assert_eq!(receipt_rows, 1);
    drop(connection);
    root.unlink("receipts/agent-registrations.json")?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(
        sqlite.connection()?.query_row(
            "SELECT count(*) FROM registration_receipts",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    root.atomic_write(
        "receipts/agent-registrations.json",
        br#"[{"receipt_id":"receipt-002","client_id":"claude","updated_at":"2026-07-13T10:00:00Z","scope":"read"}]"#,
    )?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(
        sqlite.connection()?.query_row(
            "SELECT receipt_id FROM registration_receipts",
            [],
            |row| row.get::<_, String>(0),
        )?,
        "receipt-002"
    );
    let before = sqlite.snapshot_ids()?;
    let (report, rebuilt) = RecoveryManager::new(root.clone()).rebuild_index()?;
    assert_eq!(before, rebuilt);
    assert_eq!(sqlite.snapshot_ids()?, rebuilt);
    assert_eq!(
        root.read("config.json")?,
        br#"{"capture_interval_seconds":30}"#
    );
    assert_eq!(report.event_records, 14);
    assert_eq!(report.chunk_records, 2);
    assert_eq!(report.artifact_revisions, 1);
    Ok(())
}

#[test]
fn startup_rejects_projection_cursor_between_verified_records() -> chronicle_store::Result<()> {
    for beyond_end in [false, true] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let record = chronicle_store::CanonicalJournal::new(root.clone()).append_event(
            &common::events()?.remove(2),
            chronicle_store::FaultInjector::none(),
        )?;
        projector.project_record(&record, chronicle_store::FaultInjector::none())?;
        let invalid_cursor = if beyond_end {
            record.end_offset() + 1
        } else {
            1
        };
        let invalid_cursor = i64::try_from(invalid_cursor)
            .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?;
        sqlite.connection()?.execute(
            "UPDATE projection_cursors SET byte_offset=?1 WHERE family='events' AND shard=?2",
            rusqlite::params![invalid_cursor, record.shard()],
        )?;
        RecoveryManager::new(root.clone()).recover_startup()?;
        let reopened = chronicle_store::SqliteStore::open(root)?;
        assert_eq!(
            reopened.projection_cursor(chronicle_store::JournalFamily::Events, record.shard())?,
            record.end_offset()
        );
    }
    Ok(())
}

#[test]
fn startup_rebuilds_when_valid_cursor_masks_missing_projected_row() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let journal = chronicle_store::CanonicalJournal::new(root.clone());
    let events = common::events()?;
    let first = journal.append_event(&events[2], chronicle_store::FaultInjector::none())?;
    let second = journal.append_event(&events[3], chronicle_store::FaultInjector::none())?;
    projector.project_record(&first, chronicle_store::FaultInjector::none())?;
    projector.project_record(&second, chronicle_store::FaultInjector::none())?;
    sqlite
        .connection()?
        .execute("DELETE FROM events WHERE event_id=?1", [first.stable_id()])?;
    assert_eq!(sqlite.snapshot_ids()?.event_ids.len(), 1);
    RecoveryManager::new(root.clone()).recover_startup()?;
    let reopened = chronicle_store::SqliteStore::open(root.clone())?;
    assert_eq!(reopened.snapshot_ids()?.event_ids.len(), 2);
    assert_eq!(
        reopened.projection_cursor(chronicle_store::JournalFamily::Events, first.shard())?,
        second.end_offset()
    );
    let before = reopened.snapshot_ids()?;
    let (_report, rebuilt) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(before, rebuilt);
    Ok(())
}

#[test]
fn corrupt_sqlite_is_rebuilt_automatically_on_startup() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let before = sqlite.snapshot_ids()?;
    std::fs::write(root.path().join("index.sqlite3"), b"not a sqlite database")?;
    chronicle_store::RecoveryManager::new(root.clone()).recover_startup()?;
    let after = chronicle_store::SqliteStore::open(root)?.snapshot_ids()?;
    assert_eq!(before, after);
    Ok(())
}
