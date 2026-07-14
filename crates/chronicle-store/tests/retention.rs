mod common;

use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration as StdDuration;

use chronicle_domain::{
    EventEnvelope, EventId, EventPayload, ImageArtifactId, ManagedRelativePath, ObservationContent,
    ScreenshotLifecycleAction,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, JournalFamily, LockManager, ManagedRoot,
    RecoveryManager, RetentionConfirmation, ScreenshotStore, StoreError,
};
use chrono::{DateTime, Duration, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC test timestamp")
}

fn cloned_image_pair(
    events: &[EventEnvelope],
    ordinal: u32,
) -> Result<(EventEnvelope, EventEnvelope), Box<dyn std::error::Error>> {
    let mut observation = events[0].clone();
    let mut completion = events[1].clone();
    let offset = Duration::minutes(i64::from(ordinal));
    observation.event_id = EventId::new(format!("evt-image-{ordinal}"))?;
    observation.scheduled_at = observation.scheduled_at.map(|value| value + offset);
    observation.observed_at += offset;
    observation.recorded_at += offset;
    let EventPayload::ObservationAttempt(attempt) = &mut observation.payload else {
        return Err("fixture observation missing".into());
    };
    let ObservationContent::Captured(content) = &mut attempt.content else {
        return Err("fixture captured content missing".into());
    };
    let image = content.image.as_mut().ok_or("fixture image missing")?;
    image.artifact_id = ImageArtifactId::new(format!("img-{ordinal:03}"))?;
    image.managed_relative_path =
        ManagedRelativePath::new(format!("screenshots/2026-07-13/img-{ordinal:03}.heic"))?;
    image.expires_at += offset;
    image.content_hash = format!("hash-{ordinal}");
    content.content_hash = image.content_hash.clone();

    completion.event_id = EventId::new(format!("evt-image-{ordinal}-write"))?;
    completion.observed_at += offset;
    completion.recorded_at += offset;
    let EventPayload::ScreenshotLifecycle(lifecycle) = &mut completion.payload else {
        return Err("fixture lifecycle missing".into());
    };
    lifecycle.artifact_id = image.artifact_id.clone();
    lifecycle.source_event_id = observation.event_id.clone();
    lifecycle.completed_at = lifecycle.completed_at.map(|value| value + offset);
    observation.validate()?;
    completion.validate()?;
    Ok((observation, completion))
}

#[test]
fn retention_preview_apply_deletes_only_image_bytes_and_preserves_evidence()
-> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::events()?;
    let screenshots =
        ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    screenshots.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let chunk = common::chunks()?.remove(0);
    CanonicalJournal::new(root.clone()).append_chunk(&chunk, FaultInjector::none())?;

    let preview = screenshots.preview_retention(at("2026-07-14T09:00:16Z"))?;
    assert_eq!(preview.candidate_artifact_ids.len(), 1);
    assert_eq!(preview.candidate_bytes, 21);
    let before = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)?
        .records;
    let chunks_before = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Chunks, false)?
        .records
        .into_iter()
        .map(|record| record.stable_id().to_owned())
        .collect::<Vec<_>>();

    let result = screenshots.apply_retention(
        RetentionConfirmation::confirmed(preview),
        at("2026-07-14T09:00:17Z"),
        FaultInjector::none(),
    )?;
    assert_eq!(result.deleted_artifact_ids.len(), 1);
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);

    let after = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)?
        .records;
    assert_eq!(after.len(), before.len() + 2);
    assert_eq!(
        CanonicalJournal::new(root.clone())
            .scan_all(JournalFamily::Chunks, false)?
            .records
            .into_iter()
            .map(|record| record.stable_id().to_owned())
            .collect::<Vec<_>>(),
        chunks_before
    );
    let source = after
        .iter()
        .find(|record| record.stable_id() == events[0].event_id.as_str())
        .expect("source observation remains canonical");
    let source = EventEnvelope::parse(std::str::from_utf8(source.body_bytes()).expect("UTF-8"))?;
    assert!(matches!(
        source.payload,
        EventPayload::ObservationAttempt(_)
    ));
    assert_eq!(sqlite.snapshot_ids()?.event_ids.len(), after.len());
    assert_eq!(
        sqlite.snapshot_ids()?.screenshot_lifecycle,
        vec![("img-001".to_owned(), "expired".to_owned())]
    );
    Ok(())
}

#[test]
fn retention_apply_rejects_inventory_changes_and_unconfirmed_requests()
-> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let screenshots =
        ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    screenshots.retain(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        FaultInjector::none(),
    )?;
    let preview = screenshots.preview_retention(at("2026-07-14T09:00:16Z"))?;
    assert!(
        screenshots
            .apply_retention(
                RetentionConfirmation::unconfirmed(preview.clone()),
                at("2026-07-14T09:00:17Z"),
                FaultInjector::none(),
            )
            .is_err()
    );

    // A lifecycle change after preview makes the inventory digest stale rather
    // than silently changing the deletion set.
    let request = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action == ScreenshotLifecycleAction::DeleteRequested
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .expect("delete request fixture");
    let completion = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                EventPayload::ScreenshotLifecycle(lifecycle)
                    if lifecycle.action == ScreenshotLifecycleAction::DeleteCompleted
                        && lifecycle.artifact_id.as_str() == "img-001"
            )
        })
        .expect("delete completion fixture");
    screenshots.delete(request, completion, FaultInjector::none())?;
    assert!(
        screenshots
            .apply_retention(
                RetentionConfirmation::confirmed(preview),
                at("2026-07-14T09:00:17Z"),
                FaultInjector::none(),
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn interrupted_multi_image_apply_recovers_pending_without_expanding_preview()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let screenshots =
        ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    screenshots.retain(
        &events[0],
        b"first-image",
        &events[1],
        FaultInjector::none(),
    )?;
    let (second, second_completion) = cloned_image_pair(&events, 2)?;
    screenshots.retain(
        &second,
        b"second-image",
        &second_completion,
        FaultInjector::none(),
    )?;
    let preview = screenshots.preview_retention(at("2026-07-14T09:05:00Z"))?;
    assert_eq!(preview.candidate_artifact_ids.len(), 2);

    assert!(matches!(
        screenshots.apply_retention(
            RetentionConfirmation::confirmed(preview.clone()),
            at("2026-07-14T09:05:01Z"),
            FaultInjector::at_occurrence(FaultPoint::AfterDeleteRequest, 1),
        ),
        Err(StoreError::InjectedFault(FaultPoint::AfterDeleteRequest))
    ));
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
    assert!(root.exists("screenshots/2026-07-13/img-002.heic")?);

    // A capture arriving after the interrupted preview is never absorbed into
    // the old deletion set.
    let (third, third_completion) = cloned_image_pair(&events, 3)?;
    screenshots.retain(
        &third,
        b"third-image",
        &third_completion,
        FaultInjector::none(),
    )?;
    RecoveryManager::new(root.clone()).recover_startup_at(at("2026-07-14T09:05:02Z"))?;
    assert!(!root.exists("screenshots/2026-07-13/img-002.heic")?);
    assert!(root.exists("screenshots/2026-07-13/img-003.heic")?);

    assert!(matches!(
        screenshots.apply_retention(
            RetentionConfirmation::confirmed(preview),
            at("2026-07-14T09:05:02Z"),
            FaultInjector::none(),
        ),
        Err(StoreError::RetentionPreviewStale)
    ));
    let fresh = screenshots.preview_retention(at("2026-07-14T09:05:00Z"))?;
    assert_eq!(
        fresh
            .candidate_artifact_ids
            .iter()
            .map(ImageArtifactId::as_str)
            .collect::<Vec<_>>(),
        vec!["img-003"]
    );
    Ok(())
}

#[test]
fn inventory_ignores_valid_nonimage_lifecycle_records() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let screenshots = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root), projector)?;
    let preview = screenshots.preview_retention(at("2026-07-14T09:05:00Z"))?;
    assert!(preview.candidate_artifact_ids.is_empty());
    Ok(())
}

#[test]
fn same_process_screenshot_writers_serialize_per_store() -> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let first = (events[0].clone(), events[1].clone());
    let second = cloned_image_pair(&events, 2)?;
    let screenshots = Arc::new(ScreenshotStore::new(
        root.clone(),
        CanonicalJournal::new(root.clone()),
        projector,
    )?);
    let barrier = Arc::new(Barrier::new(3));
    let handles = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, (observation, completion))| {
            let screenshots = Arc::clone(&screenshots);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                screenshots.retain(
                    &observation,
                    format!("image-{index}").as_bytes(),
                    &completion,
                    FaultInjector::none(),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for handle in handles {
        handle.join().map_err(|_| "screenshot writer panicked")??;
    }
    assert_eq!(
        CanonicalJournal::new(root)
            .scan_all(JournalFamily::Events, false)?
            .records
            .len(),
        4
    );
    Ok(())
}

#[test]
fn concurrent_retain_and_apply_never_delete_the_unpreviewed_capture()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let screenshots =
        ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?;
    screenshots.retain(
        &events[0],
        b"first-image",
        &events[1],
        FaultInjector::none(),
    )?;
    let preview = screenshots.preview_retention(at("2026-07-14T09:05:00Z"))?;
    let (second, second_completion) = cloned_image_pair(&events, 2)?;
    let apply_store = screenshots.clone();
    let retain_store = screenshots.clone();
    let barrier = Arc::new(Barrier::new(3));

    let apply_barrier = Arc::clone(&barrier);
    let apply = std::thread::spawn(move || {
        apply_barrier.wait();
        apply_store.apply_retention(
            RetentionConfirmation::confirmed(preview),
            at("2026-07-14T09:05:01Z"),
            FaultInjector::none(),
        )
    });
    let retain_barrier = Arc::clone(&barrier);
    let retain = std::thread::spawn(move || {
        retain_barrier.wait();
        retain_store.retain(
            &second,
            b"second-image",
            &second_completion,
            FaultInjector::none(),
        )
    });
    barrier.wait();

    let apply_result = apply.join().map_err(|_| "retention apply panicked")?;
    retain.join().map_err(|_| "screenshot retain panicked")??;
    assert!(root.exists("screenshots/2026-07-13/img-002.heic")?);
    match apply_result {
        Ok(result) => {
            assert_eq!(
                result
                    .deleted_artifact_ids
                    .iter()
                    .map(ImageArtifactId::as_str)
                    .collect::<Vec<_>>(),
                vec!["img-001"]
            );
            assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
        }
        Err(StoreError::RetentionPreviewStale) => {
            assert!(root.exists("screenshots/2026-07-13/img-001.heic")?);
        }
        Err(error) => return Err(error.into()),
    }
    RecoveryManager::new(root).recover_startup()?;
    Ok(())
}

#[test]
fn cross_process_screenshot_lock_times_out_then_acquires() -> Result<(), Box<dyn std::error::Error>>
{
    if let Ok(expectation) = std::env::var("CHRONICLE_SCREENSHOT_LOCK_CHILD") {
        let root_path = std::env::var("CHRONICLE_SCREENSHOT_LOCK_ROOT")?;
        let root = ManagedRoot::initialize(root_path)?;
        let locks = LockManager::new(root, StdDuration::from_millis(150));
        let shared = locks.shared_request()?;
        let result = shared.screenshots();
        match expectation.as_str() {
            "timeout" => assert!(matches!(
                result,
                Err(StoreError::LockTimeout(label)) if label == "screenshot inventory"
            )),
            "acquire" => assert!(result.is_ok()),
            _ => return Err("unknown child lock expectation".into()),
        }
        return Ok(());
    }

    let (temporary, root, _sqlite, _projector) = common::store()?;
    let locks = LockManager::new(root.clone(), StdDuration::from_secs(2));
    let shared = locks.shared_request()?;
    let screenshot = shared.screenshots()?;
    run_lock_child(temporary.path().join("store"), "timeout")?;
    drop(screenshot);
    drop(shared);
    run_lock_child(temporary.path().join("store"), "acquire")?;
    Ok(())
}

fn run_lock_child(
    root: impl AsRef<std::path::Path>,
    expectation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("cross_process_screenshot_lock_times_out_then_acquires")
        .arg("--nocapture")
        .env("CHRONICLE_SCREENSHOT_LOCK_CHILD", expectation)
        .env("CHRONICLE_SCREENSHOT_LOCK_ROOT", root.as_ref())
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "child lock probe failed ({expectation}): stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
