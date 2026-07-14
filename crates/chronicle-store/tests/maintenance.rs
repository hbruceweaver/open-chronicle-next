mod common;

use std::error::Error;
use std::time::Duration;

use chronicle_store::{
    CanonicalJournal, EvidenceDeletionConfirmation, EvidenceDeletionOptions, EvidenceDeletionState,
    FaultInjector, LockManager, MaintenanceFaultInjector, MaintenanceFaultPoint,
    MaintenanceFileClass, MaintenanceStore, Projector, SqliteStore, StoreError, StoreGeneration,
};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid timestamp")
}

fn install_preserved_state(root: &chronicle_store::ManagedRoot) -> chronicle_store::Result<()> {
    root.atomic_write("config.json", br#"{"recording_preference":false}"#)?;
    root.atomic_write("receipts/agent-registrations.json", br#"{"agents":[]}"#)?;
    root.atomic_write("receipts/disclosure-grants.json", br#"{"grants":[]}"#)?;
    root.atomic_write("receipts/runtime-session.json", br#"{"active":false}"#)?;
    Ok(())
}

#[test]
fn evidence_deletion_is_explicit_generation_bound_and_preserves_control_state()
-> Result<(), Box<dyn Error>> {
    let (temporary, root, stale_sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    install_preserved_state(&root)?;
    root.atomic_write("screenshots/managed.heic", b"managed image")?;
    root.atomic_write("diagnostics/health.json", b"diagnostic")?;
    root.atomic_write("exports/keep.json", b"stable export")?;
    let external = temporary.path().join("external-copy.json");
    std::fs::write(&external, b"outside managed root")?;

    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions {
            preserve_exports: true,
        },
        at("2026-07-14T10:00:00Z"),
    )?;
    assert!(!preview.deletion.files.is_empty());
    assert!(
        preview
            .deletion
            .files
            .iter()
            .all(|item| !item.relative_path.starts_with('/'))
    );
    assert!(
        !preview
            .deletion
            .files
            .iter()
            .any(|item| item.relative_path == "exports/keep.json")
    );
    assert_eq!(StoreGeneration::load(&root)?.generation, 1);

    let rejected = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::unconfirmed(&preview),
        at("2026-07-14T10:01:00Z"),
        MaintenanceFaultInjector::none(),
    );
    assert!(matches!(rejected, Err(StoreError::InvalidPath(_))));
    assert_eq!(StoreGeneration::load(&root)?.generation, 1);
    assert!(root.exists("evidence/events/2026-07-13.jsonl")?);

    let result = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T10:02:00Z"),
        MaintenanceFaultInjector::none(),
    )?;
    assert_eq!(result.receipt.state, EvidenceDeletionState::Complete);
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);
    assert!(result.remaining_evidence.files.is_empty());
    assert!(!root.exists("screenshots/managed.heic")?);
    assert!(!root.exists("diagnostics/health.json")?);
    assert!(root.exists("config.json")?);
    assert!(root.exists("receipts/agent-registrations.json")?);
    assert!(root.exists("receipts/disclosure-grants.json")?);
    assert!(root.exists("receipts/runtime-session.json")?);
    assert!(root.exists("receipts/evidence-deletion.json")?);
    assert!(root.exists("exports/keep.json")?);
    assert_eq!(std::fs::read(&external)?, b"outside managed root");
    assert!(matches!(
        stale_sqlite.connection(),
        Err(StoreError::StaleGeneration {
            expected: 1,
            actual: 2
        })
    ));
    assert!(
        SqliteStore::open(root)?
            .snapshot_ids()?
            .event_ids
            .is_empty()
    );
    Ok(())
}

#[test]
fn deletion_preview_fails_closed_when_managed_evidence_changes() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T11:00:00Z"),
    )?;
    root.atomic_write("diagnostics/late.json", b"appeared after preview")?;

    let error = maintenance
        .finalize_evidence_deletion(
            EvidenceDeletionConfirmation::confirmed(&preview),
            at("2026-07-14T11:01:00Z"),
            MaintenanceFaultInjector::none(),
        )
        .expect_err("stale preview must not commit");
    assert!(matches!(error, StoreError::InvalidPath(_)));
    assert_eq!(StoreGeneration::load(&root)?.generation, 1);
    assert!(root.exists("diagnostics/late.json")?);
    assert_eq!(
        maintenance
            .evidence_deletion_receipt()?
            .expect("prepared receipt")
            .state,
        EvidenceDeletionState::Prepared
    );
    Ok(())
}

#[test]
fn deletion_resumes_after_generation_increment_without_incrementing_twice()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, stale_sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let stale_journal = CanonicalJournal::new(root.clone());
    install_preserved_state(&root)?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:00:00Z"),
    )?;
    SqliteStore::open(root.clone())?;
    stale_sqlite.connection()?;

    let injected = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T12:01:00Z"),
        MaintenanceFaultInjector::at(MaintenanceFaultPoint::AfterGenerationIncrement),
    );
    assert!(matches!(injected, Err(StoreError::InvalidPath(_))));
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);
    assert_eq!(
        maintenance
            .evidence_deletion_receipt()?
            .expect("commit-intent receipt")
            .state,
        EvidenceDeletionState::CommitIntent
    );
    assert!(matches!(
        SqliteStore::open(root.clone()),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        stale_sqlite.connection(),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        LockManager::new(root.clone(), Duration::from_millis(100)).shared_request(),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        StoreGeneration::load(&root)?.increment(&root),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        stale_journal.append_event(&common::events()?[0], FaultInjector::none()),
        Err(StoreError::MaintenanceInProgress)
    ));

    let resumed = maintenance
        .resume_evidence_deletion(at("2026-07-14T12:02:00Z"), MaintenanceFaultInjector::none())?;
    assert_eq!(resumed.receipt.state, EvidenceDeletionState::Complete);
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);
    assert!(resumed.remaining_evidence.files.is_empty());
    assert!(root.exists("config.json")?);
    let fresh_sqlite = SqliteStore::open(root.clone())?;
    assert!(matches!(
        stale_sqlite.connection(),
        Err(StoreError::StaleGeneration { .. })
    ));
    let record =
        CanonicalJournal::new(root).append_event(&common::events()?[0], FaultInjector::none())?;
    Projector::new(fresh_sqlite).project_record(&record, FaultInjector::none())?;
    Ok(())
}

#[test]
fn deletion_resumes_after_partial_file_deletion_and_completion_boundary()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:30:00Z"),
    )?;

    let interrupted = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T12:31:00Z"),
        MaintenanceFaultInjector::at_occurrence(MaintenanceFaultPoint::AfterFileDeletion, 0),
    );
    assert!(matches!(interrupted, Err(StoreError::InvalidPath(_))));
    let partial = maintenance
        .evidence_deletion_receipt()?
        .expect("deletion receipt");
    assert_eq!(partial.state, EvidenceDeletionState::Deleting);
    assert_eq!(partial.deleted_relative_paths.len(), 1);
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);

    let before_completion = maintenance.resume_evidence_deletion(
        at("2026-07-14T12:32:00Z"),
        MaintenanceFaultInjector::at(MaintenanceFaultPoint::BeforeCompletion),
    );
    assert!(matches!(before_completion, Err(StoreError::InvalidPath(_))));
    let result = maintenance
        .resume_evidence_deletion(at("2026-07-14T12:33:00Z"), MaintenanceFaultInjector::none())?;
    assert_eq!(result.receipt.state, EvidenceDeletionState::Complete);
    assert_eq!(StoreGeneration::load(&root)?.generation, 2);
    assert!(result.remaining_evidence.files.is_empty());
    Ok(())
}

#[test]
fn maintenance_waits_for_the_store_wide_exclusive_lock_before_preparing()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let locks = LockManager::new(root.clone(), Duration::from_millis(20));
    let _shared = locks.shared_request()?;
    let maintenance = MaintenanceStore::open(root, Duration::from_millis(20))?;

    let error = maintenance
        .prepare_evidence_deletion(
            EvidenceDeletionOptions::default(),
            at("2026-07-14T12:45:00Z"),
        )
        .expect_err("exclusive maintenance must not bypass a shared request");
    assert!(matches!(error, StoreError::LockTimeout(_)));
    assert!(maintenance.evidence_deletion_receipt()?.is_none());
    Ok(())
}

#[test]
fn point_in_time_inventories_require_the_store_wide_exclusive_lock() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let locks = LockManager::new(root.clone(), Duration::from_millis(20));
    let _shared = locks.shared_request()?;
    let maintenance = MaintenanceStore::open(root, Duration::from_millis(20))?;

    assert!(matches!(
        maintenance.evidence_inventory(EvidenceDeletionOptions::default()),
        Err(StoreError::LockTimeout(_))
    ));
    assert!(matches!(
        maintenance.factory_reset_inventory(at("2026-07-14T12:46:00Z")),
        Err(StoreError::LockTimeout(_))
    ));
    Ok(())
}

#[test]
fn corrupted_committed_receipt_cannot_reclassify_settings_as_evidence() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    install_preserved_state(&root)?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:50:00Z"),
    )?;
    let interrupted = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T12:51:00Z"),
        MaintenanceFaultInjector::at(MaintenanceFaultPoint::AfterCommitIntent),
    );
    assert!(matches!(interrupted, Err(StoreError::InvalidPath(_))));

    let mut receipt = maintenance
        .evidence_deletion_receipt()?
        .expect("commit-intent receipt");
    let config = root.read("config.json")?;
    let first = receipt
        .preview
        .deletion
        .files
        .first_mut()
        .expect("fixture deletion item");
    first.relative_path = "config.json".to_owned();
    first.class = MaintenanceFileClass::EventJournal;
    first.bytes = config.len() as u64;
    first.checksum = chronicle_store::checksum::checksum_bytes(&config);
    receipt
        .preview
        .deletion
        .files
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    receipt.preview.deletion.total_bytes = receipt
        .preview
        .deletion
        .files
        .iter()
        .map(|item| item.bytes)
        .sum();
    receipt.preview.deletion.digest = chronicle_store::checksum::checksum_bytes(
        &chronicle_store::checksum::canonical_json(&receipt.preview.deletion.files)?,
    );
    root.atomic_write(
        "receipts/evidence-deletion.json",
        &serde_json::to_vec(&receipt)?,
    )?;

    let error = maintenance
        .resume_evidence_deletion(at("2026-07-14T12:52:00Z"), MaintenanceFaultInjector::none())
        .expect_err("receipt classification corruption must fail closed");
    assert!(matches!(error, StoreError::InvalidPath(_)));
    assert_eq!(StoreGeneration::load(&root)?.generation, 1);
    assert!(root.exists("config.json")?);
    Ok(())
}

#[test]
fn forged_complete_receipt_cannot_skip_committed_deletion() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, stale_sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let stale_journal = CanonicalJournal::new(root.clone());
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:53:00Z"),
    )?;
    let interrupted = maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T12:54:00Z"),
        MaintenanceFaultInjector::at(MaintenanceFaultPoint::AfterGenerationIncrement),
    );
    assert!(matches!(interrupted, Err(StoreError::InvalidPath(_))));

    let mut receipt: serde_json::Value =
        serde_json::from_slice(&root.read("receipts/evidence-deletion.json")?)?;
    receipt["state"] = serde_json::Value::String("complete".to_owned());
    receipt["committed_generation"] = serde_json::Value::from(2_u64);
    receipt["completed_at"] = serde_json::Value::String("2026-07-14T12:54:30Z".to_owned());
    receipt["deleted_relative_paths"] = serde_json::Value::Array(
        preview
            .deletion
            .files
            .iter()
            .map(|item| serde_json::Value::String(item.relative_path.clone()))
            .collect(),
    );
    root.atomic_write(
        "receipts/evidence-deletion.json",
        &serde_json::to_vec(&receipt)?,
    )?;

    assert!(matches!(
        SqliteStore::open(root.clone()),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        stale_sqlite.connection(),
        Err(StoreError::MaintenanceInProgress)
    ));
    assert!(matches!(
        stale_journal.append_event(&common::events()?[0], FaultInjector::none()),
        Err(StoreError::MaintenanceInProgress)
    ));

    let error = maintenance
        .resume_evidence_deletion(at("2026-07-14T12:55:00Z"), MaintenanceFaultInjector::none())
        .expect_err("a forged complete receipt cannot conceal undeleted evidence");
    assert!(matches!(error, StoreError::InvalidPath(_)));
    assert!(root.exists("evidence/events/2026-07-13.jsonl")?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn inventory_rejects_symlinks_and_never_follows_them() -> Result<(), Box<dyn Error>> {
    let (temporary, root, _sqlite, _projector) = common::store()?;
    let external = temporary.path().join("external-image.heic");
    std::fs::write(&external, b"external image")?;
    std::os::unix::fs::symlink(&external, root.path().join("screenshots/link.heic"))?;
    let maintenance = MaintenanceStore::open(root, Duration::from_millis(100))?;

    let error = maintenance
        .evidence_inventory(EvidenceDeletionOptions::default())
        .expect_err("symlink inventory must fail closed");
    assert!(matches!(error, StoreError::InvalidPath(_)));
    assert_eq!(std::fs::read(external)?, b"external image");
    Ok(())
}

#[cfg(unix)]
#[test]
fn inventory_rejects_a_managed_tree_replaced_by_a_symlink() -> Result<(), Box<dyn Error>> {
    let (temporary, root, _sqlite, _projector) = common::store()?;
    let external = temporary.path().join("external-screenshots");
    std::fs::create_dir(&external)?;
    std::fs::remove_dir(root.path().join("screenshots"))?;
    std::os::unix::fs::symlink(&external, root.path().join("screenshots"))?;
    let maintenance = MaintenanceStore::open(root, Duration::from_millis(100))?;

    let error = maintenance
        .evidence_inventory(EvidenceDeletionOptions::default())
        .expect_err("a top-level managed tree symlink must fail closed");
    assert!(matches!(error, StoreError::InvalidPath(_)));
    assert!(external.is_dir());
    Ok(())
}

#[test]
fn completed_receipt_rechecks_empty_projection_and_exact_path_coverage()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    sqlite.checkpoint()?;
    let old_projection = root.read("index.sqlite3")?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;
    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:55:00Z"),
    )?;
    maintenance.finalize_evidence_deletion(
        EvidenceDeletionConfirmation::confirmed(&preview),
        at("2026-07-14T12:56:00Z"),
        MaintenanceFaultInjector::none(),
    )?;

    root.atomic_write("index.sqlite3", &old_projection)?;
    let error = maintenance
        .resume_evidence_deletion(at("2026-07-14T12:57:00Z"), MaintenanceFaultInjector::none())
        .expect_err("a complete receipt cannot conceal a restored non-empty projection");
    assert!(matches!(error, StoreError::InvalidPath(_)));

    let mut receipt: serde_json::Value =
        serde_json::from_slice(&root.read("receipts/evidence-deletion.json")?)?;
    receipt["deleted_relative_paths"]
        .as_array_mut()
        .expect("deleted path list")
        .pop();
    root.atomic_write(
        "receipts/evidence-deletion.json",
        &serde_json::to_vec(&receipt)?,
    )?;
    assert!(matches!(
        maintenance.evidence_deletion_receipt(),
        Err(StoreError::InvalidPath(_))
    ));
    Ok(())
}

#[test]
fn empty_journal_index_is_preserved_operational_state_not_omitted() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    root.atomic_write("receipts/journal-events-index.jsonl", b"")?;
    let maintenance = MaintenanceStore::open(root, Duration::from_millis(100))?;

    let preview = maintenance.prepare_evidence_deletion(
        EvidenceDeletionOptions::default(),
        at("2026-07-14T12:58:00Z"),
    )?;
    assert!(
        !preview
            .deletion
            .files
            .iter()
            .any(|item| item.relative_path == "receipts/journal-events-index.jsonl")
    );
    assert!(preview.preserved.files.iter().any(|item| {
        item.relative_path == "receipts/journal-events-index.jsonl"
            && item.class == MaintenanceFileClass::JournalIndex
            && item.bytes == 0
    }));
    Ok(())
}

#[test]
fn factory_reset_inventory_is_non_mutating_and_marks_external_copies_out_of_scope()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    install_preserved_state(&root)?;
    let maintenance = MaintenanceStore::open(root.clone(), Duration::from_millis(100))?;

    let inventory = maintenance.factory_reset_inventory(at("2026-07-14T13:00:00Z"))?;
    assert!(inventory.external_copies_outside_control);
    assert!(
        inventory
            .removal
            .files
            .iter()
            .any(|item| item.relative_path == "config.json")
    );
    assert!(inventory.removal.files.iter().any(|item| item.relative_path
        == "receipts/agent-registrations.json"
        && item.class == MaintenanceFileClass::RegistrationReceipt));
    assert!(inventory.removal.files.iter().any(|item| {
        item.relative_path == "receipts/disclosure-grants.json"
            && item.class == MaintenanceFileClass::DisclosureGrantReceipt
    }));
    assert!(root.exists("config.json")?);
    assert_eq!(StoreGeneration::load(&root)?.generation, 1);
    Ok(())
}
