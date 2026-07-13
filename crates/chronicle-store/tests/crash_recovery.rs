mod common;

use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, ManagedRoot, Projector, RecoveryManager,
    SqliteStore, StoreError,
};

#[test]
fn journal_ahead_of_projection_replays_exactly_once() -> chronicle_store::Result<()> {
    for point in [
        FaultPoint::AfterRowInsert,
        FaultPoint::AfterCursorUpdate,
        FaultPoint::BeforeTransactionCommit,
        FaultPoint::AfterTransactionCommit,
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let journal = CanonicalJournal::new(root.clone());
        let record = journal.append_event(&common::events()?.remove(2), FaultInjector::none())?;
        assert!(matches!(
            projector.project_record(&record, FaultInjector::at(point)),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        RecoveryManager::new(root.clone()).recover_startup()?;
        RecoveryManager::new(root).recover_startup()?;
        let snapshot = sqlite.snapshot_ids()?;
        assert_eq!(snapshot.event_ids.len(), 1, "fault point {point:?}");
        assert_eq!(
            sqlite.projection_cursor(chronicle_store::JournalFamily::Events, record.shard())?,
            record.end_offset(),
            "fault point {point:?}"
        );
    }
    Ok(())
}

#[test]
fn committed_but_unacknowledged_projection_is_idempotent() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let record = CanonicalJournal::new(root.clone())
        .append_event(&common::events()?.remove(2), FaultInjector::none())?;
    assert!(matches!(
        projector.project_record(
            &record,
            FaultInjector::at(FaultPoint::AfterTransactionCommit)
        ),
        Err(StoreError::InjectedFault(
            FaultPoint::AfterTransactionCommit
        ))
    ));
    projector.project_record(&record, FaultInjector::none())?;
    assert_eq!(sqlite.snapshot_ids()?.event_ids.len(), 1);
    Ok(())
}

#[test]
fn out_of_order_projection_cannot_advance_past_missing_record() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let journal = CanonicalJournal::new(root);
    let events = common::events()?;
    let first = journal.append_event(&events[2], FaultInjector::none())?;
    let second = journal.append_event(&events[3], FaultInjector::none())?;
    assert!(matches!(
        projector.project_record(&second, FaultInjector::none()),
        Err(StoreError::SqliteIdentity(message))
            if message.contains("cannot advance across unprojected bytes")
    ));
    assert_eq!(
        sqlite.projection_cursor(chronicle_store::JournalFamily::Events, first.shard())?,
        0
    );
    assert!(sqlite.snapshot_ids()?.event_ids.is_empty());
    projector.project_record(&first, FaultInjector::none())?;
    projector.project_record(&second, FaultInjector::none())?;
    projector.project_record(&first, FaultInjector::none())?;
    assert_eq!(sqlite.snapshot_ids()?.event_ids.len(), 2);
    assert_eq!(
        sqlite.projection_cursor(chronicle_store::JournalFamily::Events, first.shard())?,
        second.end_offset()
    );
    sqlite
        .connection()?
        .execute("DELETE FROM events WHERE event_id=?1", [first.stable_id()])?;
    assert!(matches!(
        projector.project_record(&first, FaultInjector::none()),
        Err(StoreError::SqliteIdentity(message))
            if message.contains("passed missing stable ID")
    ));
    Ok(())
}

#[test]
fn child_process_abort_after_journal_sync_replays_record() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("crash_after_journal_sync_child")
        .arg("--nocapture")
        .env("CHRONICLE_CRASH_ROOT", &root_path)
        .status()?;
    assert!(!status.success());
    let root = ManagedRoot::initialize(&root_path)?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(SqliteStore::open(root)?.snapshot_ids()?.event_ids.len(), 1);
    Ok(())
}

#[test]
fn crash_after_journal_sync_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_CRASH_ROOT") else {
        return Ok(());
    };
    let root = ManagedRoot::initialize(root_path)?;
    CanonicalJournal::new(root).append_event(
        &common::events()?.remove(2),
        FaultInjector::abort_at(FaultPoint::AfterJournalSync),
    )?;
    Err(StoreError::InvalidPath(
        "abort injection unexpectedly returned".to_owned(),
    ))
}

#[test]
fn child_process_abort_inside_projection_transaction_rolls_back() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path)?;
    CanonicalJournal::new(root.clone())
        .append_event(&common::events()?.remove(2), FaultInjector::none())?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("crash_inside_projection_child")
        .arg("--nocapture")
        .env("CHRONICLE_CRASH_ROOT", &root_path)
        .status()?;
    assert!(!status.success());
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(SqliteStore::open(root)?.snapshot_ids()?.event_ids.len(), 1);
    Ok(())
}

#[test]
fn crash_inside_projection_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_CRASH_ROOT") else {
        return Ok(());
    };
    let root = ManagedRoot::initialize(root_path)?;
    let sqlite = SqliteStore::open(root.clone())?;
    let record = CanonicalJournal::new(root)
        .scan_all(chronicle_store::JournalFamily::Events, false)?
        .records
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::InvalidPath("missing crash fixture".to_owned()))?;
    Projector::new(sqlite)
        .project_record(&record, FaultInjector::abort_at(FaultPoint::AfterRowInsert))?;
    Err(StoreError::InvalidPath(
        "abort injection unexpectedly returned".to_owned(),
    ))
}
