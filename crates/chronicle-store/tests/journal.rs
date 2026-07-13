mod common;

use std::io::Write;

use chronicle_store::{CanonicalJournal, FaultInjector, JournalFamily, StoreError};

#[test]
fn appends_checksummed_lines_and_recovers_partial_tail() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let event = common::events()?.remove(0);
    let journal = CanonicalJournal::new(root.clone());
    let record = journal.append_event(&event, FaultInjector::none())?;
    assert_eq!(record.stable_id(), event.event_id.as_str());
    assert!(record.end_offset() > record.start_offset());

    let mut file = root.open_file("evidence/events/2026-07-13.jsonl", false, true, false)?;
    file.write_all(b"{partial")?;
    file.sync_all()?;
    let report = journal.scan_shard(JournalFamily::Events, "2026-07-13.jsonl", true)?;
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.health.partial_tail_bytes, 8);
    assert!(report.health.diagnostic_copy.is_some());
    assert_eq!(
        std::fs::metadata(root.path().join("evidence/events/2026-07-13.jsonl"))?.len(),
        record.end_offset()
    );
    Ok(())
}

#[test]
fn corrupt_complete_line_blocks_projection_without_truncation() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let journal = CanonicalJournal::new(root.clone());
    journal.append_event(&common::events()?.remove(0), FaultInjector::none())?;
    let path = root.path().join("evidence/events/2026-07-13.jsonl");
    let mut bytes = std::fs::read(&path)?;
    let checksum = bytes
        .windows(10)
        .position(|window| window == b"\"checksum\"")
        .ok_or_else(|| StoreError::InvalidPath("fixture checksum missing".to_owned()))?;
    let mutate = checksum + 13;
    bytes[mutate] = if bytes[mutate] == b'a' { b'b' } else { b'a' };
    std::fs::write(&path, &bytes)?;
    let before = std::fs::read(&path)?;
    assert!(matches!(
        journal.scan_shard(JournalFamily::Events, "2026-07-13.jsonl", true),
        Err(StoreError::CorruptRecord { .. })
    ));
    assert_eq!(std::fs::read(path)?, before);
    Ok(())
}

#[test]
fn stable_id_replay_is_idempotent_and_mismatch_is_critical() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let journal = CanonicalJournal::new(root.clone());
    let event = common::events()?.remove(2);
    let first = journal.append_event(&event, FaultInjector::none())?;
    let replay = journal.append_event(&event, FaultInjector::none())?;
    assert_eq!(first.start_offset(), replay.start_offset());
    let mut conflicting = event;
    conflicting.display_timezone = "UTC".to_owned();
    assert!(matches!(
        journal.append_event(&conflicting, FaultInjector::none()),
        Err(StoreError::StableIdConflict { .. })
    ));
    assert_eq!(
        journal
            .scan_all(JournalFamily::Events, false)?
            .records
            .len(),
        1
    );
    Ok(())
}

#[test]
fn failed_sync_boundary_has_not_durable_critical_health() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let journal = CanonicalJournal::new(root);
    let event = common::events()?.remove(2);
    assert!(matches!(
        journal.append_event(
            &event,
            FaultInjector::at(chronicle_store::FaultPoint::AfterJournalAppend)
        ),
        Err(StoreError::InjectedFault(
            chronicle_store::FaultPoint::AfterJournalAppend
        ))
    ));
    let retried = journal.append_event(&event, FaultInjector::none())?;
    assert_eq!(retried.stable_id(), event.event_id.as_str());
    assert_eq!(
        journal
            .scan_all(JournalFamily::Events, false)?
            .records
            .len(),
        1
    );
    let health = chronicle_store::critical_storage_health(chrono::Utc::now());
    assert_eq!(health.severity, chronicle_domain::HealthSeverity::Critical);
    assert_eq!(
        health.acknowledgement,
        Some(chronicle_domain::DurableAcknowledgement::NotDurable)
    );
    assert!(!health.factual_message.contains('/'));
    Ok(())
}

#[test]
fn confirmed_repair_resumes_from_every_persisted_boundary() -> chronicle_store::Result<()> {
    for point in [
        chronicle_store::FaultPoint::AfterRepairArchive,
        chronicle_store::FaultPoint::AfterRepairSuccessor,
        chronicle_store::FaultPoint::AfterRepairOriginalUnlink,
        chronicle_store::FaultPoint::AfterRepairMarker,
    ] {
        let (_temporary, root, _sqlite, _projector) = common::store()?;
        let journal = CanonicalJournal::new(root.clone());
        let events = common::events()?;
        for event in [&events[2], &events[3], &events[4]] {
            journal.append_event(event, FaultInjector::none())?;
        }
        let original_path = root.path().join("evidence/events/2026-07-13.jsonl");
        let mut damaged = std::fs::read(&original_path)?;
        let first_end = damaged
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .ok_or_else(|| StoreError::RepairIncomplete("missing first line".to_owned()))?;
        mutate_checksum(&mut damaged[first_end..])?;
        std::fs::write(&original_path, &damaged)?;
        let manager = chronicle_store::RecoveryManager::new(root.clone());
        assert!(matches!(
            manager.repair_journal_with_faults(
                JournalFamily::Events,
                "2026-07-13.jsonl",
                events[2].device_id.clone(),
                chronicle_store::RepairConfirmation::confirm(
                    chronicle_store::RepairConfirmation::required_phrase(),
                )?,
                FaultInjector::at(point),
            ),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        let report = manager.repair_journal(
            JournalFamily::Events,
            "2026-07-13.jsonl",
            events[2].device_id.clone(),
            chronicle_store::RepairConfirmation::confirm(
                chronicle_store::RepairConfirmation::required_phrase(),
            )?,
        )?;
        let repeat = manager.repair_journal(
            JournalFamily::Events,
            "2026-07-13.jsonl",
            events[2].device_id.clone(),
            chronicle_store::RepairConfirmation::confirm(
                chronicle_store::RepairConfirmation::required_phrase(),
            )?,
        )?;
        assert_eq!(report, repeat, "fault point {point:?}");
        assert_eq!(root.read(&report.archived_original)?, damaged);
        assert_eq!(
            journal
                .scan_all(JournalFamily::Events, false)?
                .records
                .len(),
            2,
            "fault point {point:?}"
        );
        let (_, snapshot) = manager.rebuild_index()?;
        assert_eq!(snapshot.event_ids.len(), 2, "fault point {point:?}");
    }
    Ok(())
}

#[test]
fn child_process_abort_during_repair_resumes_to_one_successor() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = chronicle_store::ManagedRoot::initialize(&root_path)?;
    let journal = CanonicalJournal::new(root.clone());
    let events = common::events()?;
    for event in [&events[2], &events[3], &events[4]] {
        journal.append_event(event, FaultInjector::none())?;
    }
    let original_path = root.path().join("evidence/events/2026-07-13.jsonl");
    let mut damaged = std::fs::read(&original_path)?;
    let first_end = damaged
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .ok_or_else(|| StoreError::RepairIncomplete("missing first line".to_owned()))?;
    mutate_checksum(&mut damaged[first_end..])?;
    std::fs::write(&original_path, &damaged)?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("repair_crash_process_child")
        .arg("--nocapture")
        .env("CHRONICLE_REPAIR_CRASH_ROOT", &root_path)
        .status()?;
    assert!(!status.success());
    let manager = chronicle_store::RecoveryManager::new(root.clone());
    let report = manager.repair_journal(
        JournalFamily::Events,
        "2026-07-13.jsonl",
        events[2].device_id.clone(),
        chronicle_store::RepairConfirmation::confirm(
            chronicle_store::RepairConfirmation::required_phrase(),
        )?,
    )?;
    assert_eq!(root.read(&report.archived_original)?, damaged);
    assert_eq!(
        journal
            .scan_all(JournalFamily::Events, false)?
            .records
            .len(),
        2
    );
    assert_eq!(
        root.list_file_names("evidence/events")?
            .into_iter()
            .filter(|name| name.contains(".repair-"))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn repair_crash_process_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_REPAIR_CRASH_ROOT") else {
        return Ok(());
    };
    let root = chronicle_store::ManagedRoot::initialize(root_path)?;
    let events = common::events()?;
    chronicle_store::RecoveryManager::new(root).repair_journal_with_faults(
        JournalFamily::Events,
        "2026-07-13.jsonl",
        events[2].device_id.clone(),
        chronicle_store::RepairConfirmation::confirm(
            chronicle_store::RepairConfirmation::required_phrase(),
        )?,
        FaultInjector::abort_at(chronicle_store::FaultPoint::AfterRepairSuccessor),
    )?;
    Err(StoreError::RepairIncomplete(
        "repair abort injection unexpectedly returned".to_owned(),
    ))
}

#[test]
fn confirmed_event_repair_preserves_original_and_starts_successor() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let journal = CanonicalJournal::new(root.clone());
    let events = common::events()?;
    for event in [&events[2], &events[3], &events[4]] {
        journal.append_event(event, FaultInjector::none())?;
    }
    let original_path = root.path().join("evidence/events/2026-07-13.jsonl");
    let mut damaged = std::fs::read(&original_path)?;
    let first_end = damaged
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .ok_or_else(|| StoreError::RepairIncomplete("missing first line".to_owned()))?;
    mutate_checksum(&mut damaged[first_end..])?;
    std::fs::write(&original_path, &damaged)?;
    assert!(matches!(
        journal.scan_all(JournalFamily::Events, false),
        Err(StoreError::CorruptRecord { .. })
    ));
    assert!(matches!(
        chronicle_store::RepairConfirmation::confirm("yes"),
        Err(StoreError::RepairNotConfirmed)
    ));
    let manager = chronicle_store::RecoveryManager::new(root.clone());
    let report = manager.repair_journal(
        JournalFamily::Events,
        "2026-07-13.jsonl",
        events[2].device_id.clone(),
        chronicle_store::RepairConfirmation::confirm(
            chronicle_store::RepairConfirmation::required_phrase(),
        )?,
    )?;
    assert_eq!(report.verified_prefix_bytes, first_end as u64);
    assert_eq!(root.read(&report.archived_original)?, damaged);
    assert_eq!(root.read(&report.quarantined_bytes)?, damaged[first_end..]);
    assert!(!original_path.exists());
    let repaired = journal.scan_all(JournalFamily::Events, false)?;
    assert_eq!(repaired.records.len(), 2);
    assert_eq!(repaired.records[0].stable_id(), events[2].event_id.as_str());
    assert_eq!(
        repaired.records[1].stable_id(),
        report.repair_event_id.as_str()
    );

    let mut after = events[3].clone();
    after.event_id = chronicle_domain::EventId::new("evt-after-repair")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    journal.append_event(&after, FaultInjector::none())?;
    let active = journal.scan_all(JournalFamily::Events, false)?;
    assert_eq!(active.records.len(), 3);
    let (_recovery, snapshot) = manager.rebuild_index()?;
    assert_eq!(snapshot.event_ids.len(), 3);
    assert!(!snapshot.event_ids.contains(&events[3].event_id.to_string()));
    assert!(!snapshot.event_ids.contains(&events[4].event_id.to_string()));
    Ok(())
}

#[test]
fn confirmed_chunk_repair_quarantines_later_revision_and_records_marker()
-> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let journal = CanonicalJournal::new(root.clone());
    let chunks = common::chunks()?;
    journal.append_chunk(&chunks[0], FaultInjector::none())?;
    journal.append_chunk(&chunks[1], FaultInjector::none())?;
    let original_path = root.path().join("aggregates/chunks/2026-07-13.jsonl");
    let mut damaged = std::fs::read(&original_path)?;
    let first_end = damaged
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .ok_or_else(|| StoreError::RepairIncomplete("missing first chunk".to_owned()))?;
    mutate_checksum(&mut damaged[first_end..])?;
    std::fs::write(&original_path, &damaged)?;
    let manager = chronicle_store::RecoveryManager::new(root.clone());
    let report = manager.repair_journal(
        JournalFamily::Chunks,
        "2026-07-13.jsonl",
        common::events()?.remove(2).device_id,
        chronicle_store::RepairConfirmation::confirm(
            chronicle_store::RepairConfirmation::required_phrase(),
        )?,
    )?;
    assert_eq!(root.read(&report.archived_original)?, damaged);
    assert_eq!(root.read(&report.quarantined_bytes)?, damaged[first_end..]);
    assert_eq!(
        journal
            .scan_all(JournalFamily::Chunks, false)?
            .records
            .len(),
        1
    );
    let events = journal.scan_all(JournalFamily::Events, false)?;
    assert_eq!(events.records.len(), 1);
    assert_eq!(
        events.records[0].stable_id(),
        report.repair_event_id.as_str()
    );
    let (_recovery, snapshot) = manager.rebuild_index()?;
    assert_eq!(snapshot.chunk_revision_ids, vec!["chunk-rev-001"]);
    assert_eq!(
        snapshot.current_chunks,
        vec![(
            "chunk-20260713T0900Z".to_owned(),
            "chunk-rev-001".to_owned()
        )]
    );
    Ok(())
}

fn mutate_checksum(bytes: &mut [u8]) -> chronicle_store::Result<()> {
    let checksum = bytes
        .windows(10)
        .position(|window| window == b"\"checksum\"")
        .ok_or_else(|| StoreError::RepairIncomplete("checksum field missing".to_owned()))?;
    let mutate = checksum + 13;
    bytes[mutate] = if bytes[mutate] == b'a' { b'b' } else { b'a' };
    Ok(())
}
