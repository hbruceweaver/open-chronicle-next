use std::time::Duration;

use chronicle_domain::{
    DeviceId, EventEnvelope, EventId, EventKind, EventPayload, EvidenceSource, ObservationContent,
    ScreenshotLifecycle, ScreenshotLifecycleAction, ScreenshotProjectedState,
};
use chrono::Duration as ChronoDuration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    ArtifactStore, CanonicalJournal, FaultInjector, JournalFamily, LockManager, ManagedRoot,
    ProjectionSnapshot, Projector, RepairConfirmation, RepairReport, Result, SqliteStore,
    scan_artifact_revisions,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub event_records: usize,
    pub chunk_records: usize,
    pub artifact_revisions: usize,
    pub partial_tail_diagnostics: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RecoveryManager {
    root: ManagedRoot,
    locks: LockManager,
}

impl RecoveryManager {
    pub fn new(root: ManagedRoot) -> Self {
        Self {
            locks: LockManager::new(root.clone(), Duration::from_secs(1)),
            root,
        }
    }

    pub fn verify_journals(&self, repair_partial: bool) -> Result<RecoveryReport> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        let journal = CanonicalJournal::new(self.root.clone());
        let events = journal.scan_all(JournalFamily::Events, repair_partial)?;
        let chunks = journal.scan_all(JournalFamily::Chunks, repair_partial)?;
        let artifacts = scan_artifact_revisions(&self.root)?;
        let mut diagnostics = Vec::new();
        diagnostics.extend(events.health.diagnostic_copy);
        diagnostics.extend(chunks.health.diagnostic_copy);
        Ok(RecoveryReport {
            event_records: events.records.len(),
            chunk_records: chunks.records.len(),
            artifact_revisions: artifacts.len(),
            partial_tail_diagnostics: diagnostics,
        })
    }

    pub fn recover_startup(&self) -> Result<RecoveryReport> {
        self.recover_startup_with_faults(FaultInjector::none())
    }

    pub fn recover_startup_with_faults(&self, faults: FaultInjector) -> Result<RecoveryReport> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        match SqliteStore::open(self.root.clone()) {
            Ok(sqlite) => match self.project_canonical(&sqlite, faults) {
                Ok(report) => Ok(report),
                Err(error) if projection_is_rebuildable(&error) => self
                    .rebuild_index_locked(faults)
                    .map(|(report, _snapshot)| report),
                Err(error) => Err(error),
            },
            Err(crate::StoreError::Sqlite(_)) | Err(crate::StoreError::SqliteIdentity(_)) => self
                .rebuild_index_locked(faults)
                .map(|(report, _snapshot)| report),
            Err(error) => Err(error),
        }
    }

    fn project_canonical(
        &self,
        sqlite: &SqliteStore,
        faults: FaultInjector,
    ) -> Result<RecoveryReport> {
        let projector = Projector::new(sqlite.clone());
        let journal = CanonicalJournal::new(self.root.clone());
        let mut events = journal.scan_all(JournalFamily::Events, true)?;
        let chunks = journal.scan_all(JournalFamily::Chunks, true)?;
        events.records.extend(reconcile_screenshots(
            &self.root,
            &journal,
            &events.records,
            faults,
        )?);
        project_unindexed(sqlite, &projector, events.records.iter())?;
        project_unindexed(sqlite, &projector, chunks.records.iter())?;
        let artifact_store = ArtifactStore::new(self.root.clone(), projector.clone());
        let artifacts = artifact_store.scan_all()?;
        for artifact in &artifacts {
            projector.project_artifact(artifact, FaultInjector::none())?;
        }
        project_registration_receipts(sqlite, &self.root)?;
        let mut diagnostics = Vec::new();
        diagnostics.extend(events.health.diagnostic_copy);
        diagnostics.extend(chunks.health.diagnostic_copy);
        Ok(RecoveryReport {
            event_records: events.records.len(),
            chunk_records: chunks.records.len(),
            artifact_revisions: artifacts.len(),
            partial_tail_diagnostics: diagnostics,
        })
    }

    pub fn repair_journal(
        &self,
        family: JournalFamily,
        shard: &str,
        device_id: DeviceId,
        confirmation: RepairConfirmation,
    ) -> Result<RepairReport> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        CanonicalJournal::new(self.root.clone()).repair_corrupt_shard(
            family,
            shard,
            device_id,
            confirmation,
            FaultInjector::none(),
        )
    }

    pub fn repair_journal_with_faults(
        &self,
        family: JournalFamily,
        shard: &str,
        device_id: DeviceId,
        confirmation: RepairConfirmation,
        faults: FaultInjector,
    ) -> Result<RepairReport> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        CanonicalJournal::new(self.root.clone()).repair_corrupt_shard(
            family,
            shard,
            device_id,
            confirmation,
            faults,
        )
    }

    pub fn rebuild_index(&self) -> Result<(RecoveryReport, ProjectionSnapshot)> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        self.rebuild_index_locked(FaultInjector::none())
    }

    fn rebuild_index_locked(
        &self,
        faults: FaultInjector,
    ) -> Result<(RecoveryReport, ProjectionSnapshot)> {
        let temp_name = format!("index.rebuild-{}.sqlite3", Uuid::now_v7());
        let temp_sqlite = SqliteStore::open_named(self.root.clone(), &temp_name)?;
        let projector = Projector::new(temp_sqlite.clone());
        let journal = CanonicalJournal::new(self.root.clone());
        let mut events = journal.scan_all(JournalFamily::Events, true)?;
        let chunks = journal.scan_all(JournalFamily::Chunks, true)?;
        events.records.extend(reconcile_screenshots(
            &self.root,
            &journal,
            &events.records,
            faults,
        )?);
        for record in events.records.iter().chain(chunks.records.iter()) {
            projector.project_record(record, FaultInjector::none())?;
        }
        let artifact_store = ArtifactStore::new(self.root.clone(), projector.clone());
        let artifacts = artifact_store.scan_all()?;
        for artifact in &artifacts {
            projector.project_artifact(artifact, FaultInjector::none())?;
        }
        project_registration_receipts(&temp_sqlite, &self.root)?;
        let snapshot = temp_sqlite.snapshot_ids()?;
        temp_sqlite.checkpoint()?;
        drop(artifact_store);
        drop(projector);
        drop(temp_sqlite);

        let diagnostic_name = format!(
            "diagnostics/index-before-rebuild-{}.sqlite3",
            Uuid::now_v7()
        );
        if self.root.exists("index.sqlite3")? {
            self.root.rename("index.sqlite3", &diagnostic_name)?;
        }
        remove_if_present(&self.root, "index.sqlite3-wal")?;
        remove_if_present(&self.root, "index.sqlite3-shm")?;
        remove_if_present(&self.root, &format!("{temp_name}-wal"))?;
        remove_if_present(&self.root, &format!("{temp_name}-shm"))?;
        self.root.rename(&temp_name, "index.sqlite3")?;
        let rebuilt = SqliteStore::open(self.root.clone())?;
        let rebuilt_snapshot = rebuilt.snapshot_ids()?;
        if rebuilt_snapshot != snapshot {
            return Err(crate::StoreError::SqliteIdentity(
                "rebuilt projection changed during atomic replacement".to_owned(),
            ));
        }
        let mut diagnostics = Vec::new();
        diagnostics.extend(events.health.diagnostic_copy);
        diagnostics.extend(chunks.health.diagnostic_copy);
        Ok((
            RecoveryReport {
                event_records: events.records.len(),
                chunk_records: chunks.records.len(),
                artifact_revisions: artifacts.len(),
                partial_tail_diagnostics: diagnostics,
            },
            snapshot,
        ))
    }
}

fn project_unindexed<'a>(
    sqlite: &SqliteStore,
    projector: &Projector,
    records: impl Iterator<Item = &'a crate::VerifiedRecord>,
) -> Result<()> {
    let records = records.collect::<Vec<_>>();
    let mut cursors = std::collections::HashMap::<(JournalFamily, String), u64>::new();
    let mut boundaries =
        std::collections::HashMap::<(JournalFamily, String), std::collections::HashSet<u64>>::new();
    for record in &records {
        let key = (record.family(), record.shard().to_owned());
        boundaries
            .entry(key)
            .or_default()
            .insert(record.end_offset());
    }
    for (key, valid_boundaries) in &boundaries {
        let cursor = sqlite.projection_cursor(key.0, &key.1)?;
        if cursor != 0 && !valid_boundaries.contains(&cursor) {
            return Err(crate::StoreError::SqliteIdentity(format!(
                "projection cursor {cursor} is not on a verified record boundary in {}",
                key.1
            )));
        }
        cursors.insert(key.clone(), cursor);
    }
    for record in records {
        let key = (record.family(), record.shard().to_owned());
        let cursor = *cursors.get(&key).ok_or_else(|| {
            crate::StoreError::SqliteIdentity("verified shard has no cursor state".to_owned())
        })?;
        if record.end_offset() <= cursor {
            projector.project_record(record, FaultInjector::none())?;
            continue;
        }
        if record.start_offset() != cursor {
            return Err(crate::StoreError::SqliteIdentity(format!(
                "projection cursor {cursor} is not on a verified record boundary in {}",
                record.shard()
            )));
        }
        projector.project_record(record, FaultInjector::none())?;
        cursors.insert(key, record.end_offset());
    }
    Ok(())
}

fn projection_is_rebuildable(error: &crate::StoreError) -> bool {
    matches!(
        error,
        crate::StoreError::Sqlite(_)
            | crate::StoreError::SqliteIdentity(_)
            | crate::StoreError::StableIdConflict { .. }
            | crate::StoreError::ArtifactConflict
    )
}

fn project_registration_receipts(sqlite: &SqliteStore, root: &ManagedRoot) -> Result<()> {
    let value = if root.exists("receipts/agent-registrations.json")? {
        Some(serde_json::from_slice::<Value>(
            &root.read("receipts/agent-registrations.json")?,
        )?)
    } else {
        None
    };
    let records = value.as_ref().map_or_else(Vec::new, |value| {
        value
            .as_array()
            .map_or_else(|| vec![value], |array| array.iter().collect())
    });
    let mut connection = sqlite.connection()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute("DELETE FROM registration_receipts", [])?;
    for (index, record) in records.into_iter().enumerate() {
        let receipt_id = record
            .get("receipt_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("operational-receipt-{index}"));
        let client_id = record
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown-client");
        let updated_at = record
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or("1970-01-01T00:00:00Z");
        transaction.execute(
            "INSERT INTO registration_receipts(receipt_id, client_id, receipt_json, updated_at) VALUES(?1, ?2, ?3, ?4) ON CONFLICT(receipt_id) DO UPDATE SET client_id=excluded.client_id, receipt_json=excluded.receipt_json, updated_at=excluded.updated_at",
            rusqlite::params![receipt_id, client_id, serde_json::to_string(record)?, updated_at],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn remove_if_present(root: &ManagedRoot, relative: &str) -> Result<()> {
    match root.unlink(relative) {
        Ok(()) => Ok(()),
        Err(crate::StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn reconcile_screenshots(
    root: &ManagedRoot,
    journal: &CanonicalJournal,
    records: &[crate::VerifiedRecord],
    faults: FaultInjector,
) -> Result<Vec<crate::VerifiedRecord>> {
    let mut observations = std::collections::HashMap::new();
    let mut terminals = std::collections::HashMap::new();
    let mut delete_requests = std::collections::HashMap::new();
    let mut delete_completions = std::collections::HashSet::new();
    let mut events_by_id = std::collections::HashMap::new();
    let mut lifecycle_events = Vec::new();
    for record in records {
        let text = std::str::from_utf8(record.body_bytes())
            .map_err(|error| crate::StoreError::InvalidPath(error.to_string()))?;
        let event = EventEnvelope::parse(text)?;
        events_by_id.insert(event.event_id.to_string(), event.clone());
        match &event.payload {
            EventPayload::ObservationAttempt(attempt) => {
                if let ObservationContent::Captured(content) = &attempt.content
                    && let Some(image) = &content.image
                {
                    let derived = crate::artifacts::derived_screenshot_path(
                        &event,
                        image.artifact_id.as_str(),
                    );
                    if image.managed_relative_path.as_str() != derived {
                        return Err(crate::StoreError::InvalidPath(
                            "canonical image path violates screenshot derivation".to_owned(),
                        ));
                    }
                    observations.insert(
                        image.artifact_id.to_string(),
                        (event.clone(), image.clone()),
                    );
                }
            }
            EventPayload::ScreenshotLifecycle(lifecycle) => {
                lifecycle_events.push(lifecycle.clone());
                match lifecycle.action {
                    ScreenshotLifecycleAction::WriteCompleted
                    | ScreenshotLifecycleAction::Missing
                    | ScreenshotLifecycleAction::WriteFailed => {
                        terminals.insert(lifecycle.artifact_id.to_string(), lifecycle.action);
                    }
                    ScreenshotLifecycleAction::DeleteRequested => {
                        delete_requests.insert(lifecycle.artifact_id.to_string(), event.clone());
                    }
                    ScreenshotLifecycleAction::DeleteCompleted => {
                        delete_completions.insert(lifecycle.artifact_id.to_string());
                    }
                }
            }
            EventPayload::RecordingGap(_) => {}
        }
    }
    validate_lifecycle_chains(&lifecycle_events, &events_by_id)?;

    let mut provisional_paths = std::collections::HashMap::new();
    for date in screenshot_date_directories(root)? {
        let directory = format!("screenshots/{date}");
        for name in root.list_file_names(&directory)? {
            if let Some(artifact_id) = name
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".provisional"))
            {
                provisional_paths.insert(artifact_id.to_owned(), format!("{directory}/{name}"));
            }
        }
    }

    let mut appended = Vec::new();
    for (artifact_id, (observation, image)) in &observations {
        if let Some(terminal) = terminals.get(artifact_id) {
            if let Some(provisional) = provisional_paths.remove(artifact_id) {
                match root.unlink(&provisional) {
                    Ok(()) => {}
                    Err(crate::StoreError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            if *terminal == ScreenshotLifecycleAction::WriteCompleted
                && !delete_requests.contains_key(artifact_id)
                && !delete_completions.contains(artifact_id)
                && !root.exists(image.managed_relative_path.as_str())?
            {
                let missing = recovery_lifecycle_event(
                    observation,
                    ScreenshotLifecycleAction::Missing,
                    ScreenshotProjectedState::Missing,
                    None,
                )?;
                appended.push(journal.append_event(&missing, FaultInjector::none())?);
            }
            continue;
        }
        let final_path = image.managed_relative_path.as_str();
        if !root.exists(final_path)?
            && let Some(provisional) = provisional_paths.remove(artifact_id)
        {
            root.rename(&provisional, final_path)?;
        }
        let retained = root.exists(final_path)?;
        if retained {
            let parent = final_path
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .ok_or_else(|| crate::StoreError::InvalidPath(final_path.to_owned()))?;
            root.sync_directory(parent)?;
            faults.check(crate::FaultPoint::AfterImagePromotionDirectorySync)?;
        }
        let completion = recovery_lifecycle_event(
            observation,
            if retained {
                ScreenshotLifecycleAction::WriteCompleted
            } else {
                ScreenshotLifecycleAction::WriteFailed
            },
            if retained {
                ScreenshotProjectedState::Retained
            } else {
                ScreenshotProjectedState::WriteFailed
            },
            None,
        )?;
        appended.push(journal.append_event(&completion, FaultInjector::none())?);
    }

    for (_artifact_id, provisional) in provisional_paths {
        match root.unlink(&provisional) {
            Ok(()) => {}
            Err(crate::StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    for (artifact_id, request) in delete_requests {
        if delete_completions.contains(&artifact_id) {
            continue;
        }
        let requested = match &request.payload {
            EventPayload::ScreenshotLifecycle(lifecycle) => lifecycle,
            _ => continue,
        };
        let (observation, image) = observations.get(&artifact_id).ok_or_else(|| {
            crate::StoreError::InvalidPath(
                "delete request source observation was not found".to_owned(),
            )
        })?;
        if requested.source_event_id != observation.event_id {
            return Err(crate::StoreError::InvalidPath(
                "delete request source observation does not match".to_owned(),
            ));
        }
        let managed_relative_path = image.managed_relative_path.as_str();
        match root.unlink(managed_relative_path) {
            Ok(()) => {}
            Err(crate::StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = managed_relative_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .ok_or_else(|| crate::StoreError::InvalidPath(managed_relative_path.to_owned()))?;
        root.sync_directory(parent)?;
        faults.check(crate::FaultPoint::AfterImageUnlinkDirectorySync)?;
        let state = match requested.deletion_cause {
            Some(chronicle_domain::ScreenshotDeletionCause::RetentionExpired) => {
                ScreenshotProjectedState::Expired
            }
            Some(chronicle_domain::ScreenshotDeletionCause::UserRequested) => {
                ScreenshotProjectedState::UserDeleted
            }
            None => continue,
        };
        let completion = recovery_lifecycle_event(
            &request,
            ScreenshotLifecycleAction::DeleteCompleted,
            state,
            requested.deletion_cause,
        )?;
        appended.push(journal.append_event(&completion, FaultInjector::none())?);
    }
    Ok(appended)
}

fn validate_lifecycle_chains(
    lifecycles: &[ScreenshotLifecycle],
    events_by_id: &std::collections::HashMap<String, EventEnvelope>,
) -> Result<()> {
    let mut sources = std::collections::HashMap::<String, String>::new();
    let mut requests = std::collections::HashMap::<String, &ScreenshotLifecycle>::new();
    let mut write_terminals = std::collections::HashMap::new();
    for lifecycle in lifecycles {
        let artifact_id = lifecycle.artifact_id.to_string();
        let source_id = lifecycle.source_event_id.to_string();
        if let Some(existing) = sources.insert(artifact_id.clone(), source_id.clone())
            && existing != source_id
        {
            return Err(crate::StoreError::InvalidPath(
                "screenshot lifecycle source provenance changed".to_owned(),
            ));
        }
        let source = events_by_id.get(&source_id).ok_or_else(|| {
            crate::StoreError::InvalidPath(
                "screenshot lifecycle source observation was not found".to_owned(),
            )
        })?;
        let attempt = match &source.payload {
            EventPayload::ObservationAttempt(attempt) => attempt,
            _ => {
                return Err(crate::StoreError::InvalidPath(
                    "screenshot lifecycle source is not an observation".to_owned(),
                ));
            }
        };
        let source_artifact = match &attempt.content {
            ObservationContent::Captured(content) => content.image.as_ref(),
            _ => None,
        }
        .map(|image| image.artifact_id.as_str());
        if source_artifact.is_some_and(|artifact| artifact != lifecycle.artifact_id.as_str()) {
            return Err(crate::StoreError::InvalidPath(
                "screenshot lifecycle artifact does not match its observation".to_owned(),
            ));
        }
        if source_artifact.is_none()
            && matches!(
                lifecycle.action,
                ScreenshotLifecycleAction::WriteCompleted | ScreenshotLifecycleAction::WriteFailed
            )
        {
            return Err(crate::StoreError::InvalidPath(
                "screenshot write lifecycle has no observation image intent".to_owned(),
            ));
        }
        match lifecycle.action {
            ScreenshotLifecycleAction::WriteCompleted | ScreenshotLifecycleAction::WriteFailed => {
                if write_terminals
                    .insert(artifact_id, lifecycle.action)
                    .is_some()
                {
                    return Err(crate::StoreError::InvalidPath(
                        "screenshot has multiple initial write outcomes".to_owned(),
                    ));
                }
            }
            ScreenshotLifecycleAction::Missing => match write_terminals.get_mut(&artifact_id) {
                None => {
                    write_terminals.insert(artifact_id, lifecycle.action);
                }
                Some(state) if *state == ScreenshotLifecycleAction::WriteCompleted => {
                    *state = ScreenshotLifecycleAction::Missing;
                }
                Some(_) => {
                    return Err(crate::StoreError::InvalidPath(
                        "screenshot has an invalid missing transition".to_owned(),
                    ));
                }
            },
            ScreenshotLifecycleAction::DeleteRequested => {
                if requests.insert(artifact_id, lifecycle).is_some() {
                    return Err(crate::StoreError::InvalidPath(
                        "screenshot has multiple delete requests".to_owned(),
                    ));
                }
            }
            ScreenshotLifecycleAction::DeleteCompleted => {
                let request = requests.get(&artifact_id).ok_or_else(|| {
                    crate::StoreError::InvalidPath(
                        "screenshot delete completion has no request".to_owned(),
                    )
                })?;
                if request.source_event_id != lifecycle.source_event_id
                    || request.deletion_cause != lifecycle.deletion_cause
                    || request.requested_at != lifecycle.requested_at
                {
                    return Err(crate::StoreError::InvalidPath(
                        "screenshot delete completion does not match its request".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn recovery_lifecycle_event(
    source: &EventEnvelope,
    action: ScreenshotLifecycleAction,
    projected_state: ScreenshotProjectedState,
    deletion_cause: Option<chronicle_domain::ScreenshotDeletionCause>,
) -> Result<EventEnvelope> {
    let source_lifecycle = match &source.payload {
        EventPayload::ScreenshotLifecycle(lifecycle) => Some(lifecycle),
        _ => None,
    };
    let (artifact_id, source_event_id, requested_at) = if let Some(lifecycle) = source_lifecycle {
        (
            lifecycle.artifact_id.clone(),
            lifecycle.source_event_id.clone(),
            lifecycle.requested_at,
        )
    } else if let EventPayload::ObservationAttempt(attempt) = &source.payload {
        let image = match &attempt.content {
            ObservationContent::Captured(content) => content.image.as_ref(),
            _ => None,
        }
        .ok_or_else(|| crate::StoreError::InvalidPath("recovery source has no image".to_owned()))?;
        (image.artifact_id.clone(), source.event_id.clone(), None)
    } else {
        return Err(crate::StoreError::InvalidPath(
            "recovery source is not image evidence".to_owned(),
        ));
    };
    let digest = crate::checksum::checksum_bytes(format!("{action:?}:{artifact_id}").as_bytes());
    let event_id = EventId::new(format!("recovery-image-{}", &digest[..32]))
        .map_err(|error| crate::StoreError::InvalidPath(error.to_string()))?;
    let completed_at = source.recorded_at + ChronoDuration::milliseconds(1);
    let event = EventEnvelope {
        schema_version: chronicle_domain::CONTRACT_VERSION.to_owned(),
        event_id,
        device_id: source.device_id.clone(),
        scheduled_at: None,
        observed_at: completed_at,
        recorded_at: completed_at,
        display_timezone: source.display_timezone.clone(),
        source: EvidenceSource {
            adapter: "image-recovery".to_owned(),
            version: "1".to_owned(),
        },
        kind: EventKind::ScreenshotLifecycle,
        payload: EventPayload::ScreenshotLifecycle(ScreenshotLifecycle {
            artifact_id,
            action,
            deletion_cause,
            projected_state,
            requested_at,
            completed_at: Some(completed_at),
            source_event_id,
        }),
    };
    event.validate().map_err(|reason| {
        crate::StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
    })?;
    Ok(event)
}

fn screenshot_date_directories(root: &ManagedRoot) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.path().join("screenshots"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            crate::StoreError::InvalidPath(
                "screenshot directory name is not valid UTF-8".to_owned(),
            )
        })?;
        let valid = name.len() == 10
            && name.bytes().enumerate().all(|(index, byte)| {
                matches!(index, 4 | 7) && byte == b'-'
                    || !matches!(index, 4 | 7) && byte.is_ascii_digit()
            });
        if !valid {
            return Err(crate::StoreError::InvalidPath(format!(
                "unexpected screenshot directory: {name}"
            )));
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}
