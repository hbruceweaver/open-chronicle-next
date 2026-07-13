mod common;

use std::sync::{Arc, Barrier};

use chronicle_domain::{ArtifactRevisionId, ArtifactStatus};
use chronicle_domain::{EventPayload, ObservationContent};
use chronicle_store::{
    ArtifactStore, CanonicalJournal, FaultInjector, FaultPoint, RecoveryManager, ScreenshotStore,
    StoreError,
};

#[test]
fn immutable_artifact_is_restored_by_rebuild() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let artifact = common::artifact()?;
    ArtifactStore::new(root.clone(), projector).write_revision(&artifact, FaultInjector::none())?;
    let before = sqlite.snapshot_ids()?;
    let (_, after) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn two_writers_from_one_prior_have_one_winner() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let base = common::artifact()?;
    let store = ArtifactStore::new(root, projector);
    store.write_revision(&base, FaultInjector::none())?;
    let mut left = base.clone();
    left.revision_id = ArtifactRevisionId::new("artifact-revision-left")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    left.prior_revision_id = Some(base.revision_id.clone());
    left.expected_prior_revision_id = Some(base.revision_id.clone());
    left.status = ArtifactStatus::Accepted;
    let mut right = left.clone();
    right.revision_id = ArtifactRevisionId::new("artifact-revision-right")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    let barrier = Arc::new(Barrier::new(3));
    let handles = [left, right].map(|revision| {
        let store = store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.write_revision(&revision, FaultInjector::none())
        })
    });
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().map_err(|_| StoreError::ArtifactConflict))
        .collect::<chronicle_store::Result<Vec<_>>>()?;
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::ArtifactConflict)))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn interrupted_image_retention_never_leaves_false_acknowledgement() -> chronicle_store::Result<()> {
    for point in [
        FaultPoint::AfterProvisionalImageSync,
        FaultPoint::AfterObservationAppend,
        FaultPoint::AfterImagePromotion,
        FaultPoint::AfterImagePromotionDirectorySync,
        FaultPoint::AfterLifecycleCompletion,
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let events = common::events()?;
        let observation = &events[0];
        let completion = &events[1];
        let final_path = match &observation.payload {
            EventPayload::ObservationAttempt(attempt) => match &attempt.content {
                ObservationContent::Captured(content) => content
                    .image
                    .as_ref()
                    .map(|image| image.managed_relative_path.as_str()),
                _ => None,
            },
            _ => None,
        }
        .ok_or_else(|| StoreError::InvalidPath("fixture has no image".to_owned()))?;
        let screenshot_store =
            ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
        assert!(matches!(
            screenshot_store.retain(
                observation,
                b"synthetic-image-bytes",
                completion,
                FaultInjector::at(point)
            ),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        RecoveryManager::new(root.clone()).recover_startup()?;
        let snapshot = sqlite.snapshot_ids()?;
        if point == FaultPoint::AfterProvisionalImageSync {
            assert!(!root.exists(final_path)?);
            assert!(snapshot.screenshot_lifecycle.is_empty());
        } else {
            assert!(root.exists(final_path)?);
            assert_eq!(
                snapshot.screenshot_lifecycle,
                vec![("img-001".to_owned(), "retained".to_owned())]
            );
        }
        let provisional = root
            .path()
            .join("screenshots/2026-07-13/.img-001.provisional");
        assert!(!provisional.exists());
    }
    Ok(())
}

#[test]
fn interrupted_two_phase_image_deletion_completes_on_recovery() -> chronicle_store::Result<()> {
    for point in [
        FaultPoint::AfterDeleteRequest,
        FaultPoint::AfterImageUnlink,
        FaultPoint::AfterImageUnlinkDirectorySync,
        FaultPoint::AfterDeleteCompletion,
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let events = common::events()?;
        let screenshot_store =
            ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
        screenshot_store.retain(
            &events[0],
            b"synthetic-image-bytes",
            &events[1],
            FaultInjector::none(),
        )?;
        let request = events
            .iter()
            .find(|event| {
                matches!(
                    &event.payload,
                    EventPayload::ScreenshotLifecycle(lifecycle)
                        if lifecycle.action == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                            && lifecycle.artifact_id.as_str() == "img-001"
                )
            })
            .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
        let completion = events
            .iter()
            .find(|event| {
                matches!(
                    &event.payload,
                    EventPayload::ScreenshotLifecycle(lifecycle)
                        if lifecycle.action == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                            && lifecycle.artifact_id.as_str() == "img-001"
                )
            })
            .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
        assert!(matches!(
            screenshot_store.delete(
                request,
                completion,
                FaultInjector::at(point)
            ),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        RecoveryManager::new(root.clone()).recover_startup()?;
        assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
        assert_eq!(
            sqlite.snapshot_ids()?.screenshot_lifecycle,
            vec![("img-001".to_owned(), "expired".to_owned())]
        );
    }
    Ok(())
}

#[test]
fn canonical_artifact_fault_matrix_is_idempotent() -> chronicle_store::Result<()> {
    for point in [
        FaultPoint::AfterArtifactRename,
        FaultPoint::AfterArtifactDirectorySync,
        FaultPoint::AfterCurrentPointerUpdate,
        FaultPoint::BeforeTransactionCommit,
        FaultPoint::AfterTransactionCommit,
    ] {
        let (_temporary, root, sqlite, projector) = common::store()?;
        let store = ArtifactStore::new(root, projector);
        let artifact = common::artifact()?;
        assert!(matches!(
            store.write_revision(&artifact, FaultInjector::at(point)),
            Err(StoreError::InjectedFault(actual)) if actual == point
        ));
        if point == FaultPoint::AfterArtifactRename {
            assert!(matches!(
                store.write_revision(
                    &artifact,
                    FaultInjector::at(FaultPoint::AfterArtifactDirectorySync)
                ),
                Err(StoreError::InjectedFault(
                    FaultPoint::AfterArtifactDirectorySync
                ))
            ));
        }
        store.write_revision(&artifact, FaultInjector::none())?;
        let snapshot = sqlite.snapshot_ids()?;
        assert_eq!(
            snapshot.artifact_revision_ids.len(),
            1,
            "fault point {point:?}"
        );
        assert_eq!(
            snapshot.current_artifacts,
            vec![(
                artifact.artifact_id.to_string(),
                artifact.revision_id.to_string()
            )],
            "fault point {point:?}"
        );
    }
    Ok(())
}

#[test]
fn promotion_recovery_syncs_directory_before_completion() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    assert!(matches!(
        store.retain(
            &events[0],
            b"synthetic-image-bytes",
            &events[1],
            FaultInjector::at(FaultPoint::AfterImagePromotion)
        ),
        Err(StoreError::InjectedFault(FaultPoint::AfterImagePromotion))
    ));
    assert!(matches!(
        RecoveryManager::new(root.clone()).recover_startup_with_faults(FaultInjector::at(
            FaultPoint::AfterImagePromotionDirectorySync
        )),
        Err(StoreError::InjectedFault(
            FaultPoint::AfterImagePromotionDirectorySync
        ))
    ));
    assert_eq!(
        CanonicalJournal::new(root.clone())
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        1
    );
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        2
    );
    Ok(())
}

#[test]
fn deletion_recovery_syncs_directory_before_completion() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    store.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    let completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
    assert!(matches!(
        store.delete(
            request,
            completion,
            FaultInjector::at(FaultPoint::AfterImageUnlink)
        ),
        Err(StoreError::InjectedFault(FaultPoint::AfterImageUnlink))
    ));
    assert!(matches!(
        RecoveryManager::new(root.clone()).recover_startup_with_faults(FaultInjector::at(
            FaultPoint::AfterImageUnlinkDirectorySync
        )),
        Err(StoreError::InjectedFault(
            FaultPoint::AfterImageUnlinkDirectorySync
        ))
    ));
    let records_before_completion = CanonicalJournal::new(root.clone())
        .scan_all(chronicle_store::JournalFamily::Events, false)?
        .records
        .len();
    assert_eq!(records_before_completion, 3);
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        4
    );
    Ok(())
}

#[test]
fn live_deletion_retry_syncs_not_found_directory_before_completion() -> chronicle_store::Result<()>
{
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    store.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    let completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
    assert!(matches!(
        store.delete(
            request,
            completion,
            FaultInjector::at(FaultPoint::AfterImageUnlink)
        ),
        Err(StoreError::InjectedFault(FaultPoint::AfterImageUnlink))
    ));
    assert!(matches!(
        store.delete(
            request,
            completion,
            FaultInjector::at(FaultPoint::AfterImageUnlinkDirectorySync)
        ),
        Err(StoreError::InjectedFault(
            FaultPoint::AfterImageUnlinkDirectorySync
        ))
    ));
    assert_eq!(
        CanonicalJournal::new(root.clone())
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        3
    );
    store.delete(request, completion, FaultInjector::none())?;
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        4
    );
    Ok(())
}

#[test]
fn delete_rejects_completion_with_wrong_source_provenance() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    store.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    let mut invalid_completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .cloned()
        .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
    if let EventPayload::ScreenshotLifecycle(lifecycle) = &mut invalid_completion.payload {
        lifecycle.source_event_id = events[2].event_id.clone();
    }
    assert!(matches!(
        store.delete(request, &invalid_completion, FaultInjector::none()),
        Err(StoreError::InvalidPath(message)) if message == "invalid delete completion"
    ));
    assert!(root.exists("screenshots/2026-07-13/img-001.heic")?);
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        2
    );
    Ok(())
}

#[test]
fn startup_rejects_mismatched_canonical_delete_completion() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let events = common::events()?;
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    let mut invalid_completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .cloned()
        .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
    if let EventPayload::ScreenshotLifecycle(lifecycle) = &mut invalid_completion.payload {
        lifecycle.source_event_id = events[2].event_id.clone();
    }
    let journal = CanonicalJournal::new(root.clone());
    journal.append_event(&events[0], FaultInjector::none())?;
    journal.append_event(request, FaultInjector::none())?;
    journal.append_event(&invalid_completion, FaultInjector::none())?;
    assert!(matches!(
        RecoveryManager::new(root).recover_startup(),
        Err(StoreError::InvalidPath(message))
            if message.contains("source provenance changed")
    ));
    Ok(())
}

#[test]
fn startup_rejects_completed_delete_chain_for_wrong_observation_artifact()
-> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let events = common::events()?;
    let mut request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-user-delete"
            )
        })
        .cloned()
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    let mut completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-user-delete"
            )
        })
        .cloned()
        .ok_or_else(|| StoreError::InvalidPath("missing delete completion fixture".to_owned()))?;
    for event in [&mut request, &mut completion] {
        if let EventPayload::ScreenshotLifecycle(lifecycle) = &mut event.payload {
            lifecycle.source_event_id = events[0].event_id.clone();
        }
    }
    let journal = CanonicalJournal::new(root.clone());
    journal.append_event(&events[0], FaultInjector::none())?;
    journal.append_event(&request, FaultInjector::none())?;
    journal.append_event(&completion, FaultInjector::none())?;
    assert!(matches!(
        RecoveryManager::new(root).recover_startup(),
        Err(StoreError::InvalidPath(message))
            if message.contains("artifact does not match")
    ));
    Ok(())
}

#[test]
fn deletion_recovery_refuses_completion_without_source_observation() -> chronicle_store::Result<()>
{
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let events = common::events()?;
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action
                        == chronicle_domain::ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .ok_or_else(|| StoreError::InvalidPath("missing delete request fixture".to_owned()))?;
    CanonicalJournal::new(root.clone()).append_event(request, FaultInjector::none())?;
    assert!(matches!(
        RecoveryManager::new(root.clone()).recover_startup(),
        Err(StoreError::InvalidPath(message))
            if message.contains("source observation was not found")
    ));
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .len(),
        1
    );
    Ok(())
}

#[test]
fn rebuild_orders_artifacts_by_revision_chain_not_authored_time() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let store = ArtifactStore::new(root.clone(), projector);
    let base = common::artifact()?;
    store.write_revision(&base, FaultInjector::none())?;
    let mut child = base.clone();
    child.revision_id = ArtifactRevisionId::new("artifact-revision-older-child")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    child.prior_revision_id = Some(base.revision_id.clone());
    child.expected_prior_revision_id = Some(base.revision_id.clone());
    child.created_at = base.created_at - chrono::Duration::hours(1);
    store.write_revision(&child, FaultInjector::none())?;
    let (_, rebuilt) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(
        rebuilt.current_artifacts,
        vec![(base.artifact_id.to_string(), child.revision_id.to_string())]
    );
    assert_eq!(sqlite.snapshot_ids()?, rebuilt);
    Ok(())
}

#[test]
fn screenshot_path_is_derived_and_cannot_overwrite_operational_files() -> chronicle_store::Result<()>
{
    let (_temporary, root, _sqlite, projector) = common::store()?;
    root.atomic_write("config.json", b"sentinel")?;
    let events = common::events()?;
    let mut malicious = events[0].clone();
    if let EventPayload::ObservationAttempt(attempt) = &mut malicious.payload
        && let ObservationContent::Captured(content) = &mut attempt.content
        && let Some(image) = &mut content.image
    {
        image.managed_relative_path = chronicle_domain::ManagedRelativePath::new("config.json")
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    }
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    assert!(matches!(
        store.retain(
            &malicious,
            b"synthetic-image-bytes",
            &events[1],
            FaultInjector::none()
        ),
        Err(StoreError::InvalidPath(_))
    ));
    assert_eq!(root.read("config.json")?, b"sentinel");
    assert!(
        CanonicalJournal::new(root)
            .scan_all(chronicle_store::JournalFamily::Events, false)?
            .records
            .is_empty()
    );
    Ok(())
}

#[test]
fn startup_reconciliation_rejects_a_canonical_screenshot_path_escape() -> chronicle_store::Result<()>
{
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    root.atomic_write("config.json", b"sentinel")?;
    let mut malicious = common::events()?.remove(0);
    if let EventPayload::ObservationAttempt(attempt) = &mut malicious.payload
        && let ObservationContent::Captured(content) = &mut attempt.content
        && let Some(image) = &mut content.image
    {
        image.managed_relative_path = chronicle_domain::ManagedRelativePath::new("config.json")
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    }
    CanonicalJournal::new(root.clone()).append_event(&malicious, FaultInjector::none())?;
    assert!(matches!(
        RecoveryManager::new(root.clone()).recover_startup(),
        Err(StoreError::InvalidPath(message))
            if message.contains("screenshot derivation")
    ));
    assert_eq!(root.read("config.json")?, b"sentinel");
    Ok(())
}

#[test]
fn stale_screenshot_handle_cannot_write_after_generation_change() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    let generation = chronicle_store::StoreGeneration::load(&root)?;
    generation.increment(&root)?;
    let events = common::events()?;
    assert!(matches!(
        store.retain(
            &events[0],
            b"synthetic-image-bytes",
            &events[1],
            FaultInjector::none()
        ),
        Err(StoreError::StaleGeneration { .. })
    ));
    assert!(matches!(
        sqlite.snapshot_ids(),
        Err(StoreError::StaleGeneration { .. })
    ));
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
    Ok(())
}

#[test]
fn startup_removes_provisional_left_after_terminal_lifecycle() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    store.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    root.atomic_write(
        "screenshots/2026-07-13/.img-001.provisional",
        b"orphaned-retry",
    )?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert!(!root.exists("screenshots/2026-07-13/.img-001.provisional")?);
    Ok(())
}

#[test]
fn retained_image_missing_on_startup_adds_missing_lifecycle_and_rebuilds()
-> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    store.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    root.unlink("screenshots/2026-07-13/img-001.heic")?;
    RecoveryManager::new(root.clone()).recover_startup()?;
    assert_eq!(
        sqlite.snapshot_ids()?.screenshot_lifecycle,
        vec![("img-001".to_owned(), "missing".to_owned())]
    );
    let before = sqlite.snapshot_ids()?;
    let (_report, rebuilt) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(before, rebuilt);
    Ok(())
}
