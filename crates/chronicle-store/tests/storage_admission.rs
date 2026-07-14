mod common;

use std::sync::{Arc, Barrier};

use chronicle_domain::{
    EventEnvelope, EventId, EventPayload, ImageArtifactId, ManagedRelativePath, ObservationContent,
    ScreenshotLifecycleAction,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, JournalFamily, ManagedRoot, ScreenshotStorageLimits,
    ScreenshotStorageState, ScreenshotStore, StoreError, evaluate_screenshot_storage,
};

fn fixed_available(_: &ManagedRoot) -> chronicle_store::Result<u64> {
    Ok(64 * 1024 * 1024)
}

fn limits(quota: u64) -> ScreenshotStorageLimits {
    ScreenshotStorageLimits {
        warning_free_bytes: 32 * 1024 * 1024,
        minimum_free_bytes: 16 * 1024 * 1024,
        managed_image_quota_bytes: quota,
        journal_reserve_bytes: 4 * 1024 * 1024,
    }
}

#[test]
fn exact_thresholds_and_prospective_transaction_budget_are_deterministic() {
    let defaults = ScreenshotStorageLimits::default();
    let warning_edge =
        evaluate_screenshot_storage(defaults.warning_free_bytes, 0, defaults).expect("limits");
    assert_eq!(warning_edge.state, ScreenshotStorageState::Healthy);
    assert_eq!(
        evaluate_screenshot_storage(defaults.warning_free_bytes - 1, 0, defaults)
            .expect("limits")
            .state,
        ScreenshotStorageState::Warning
    );
    assert_eq!(
        evaluate_screenshot_storage(defaults.minimum_free_bytes, 0, defaults)
            .expect("limits")
            .state,
        ScreenshotStorageState::Warning
    );
    assert_eq!(
        evaluate_screenshot_storage(defaults.minimum_free_bytes - 1, 0, defaults)
            .expect("limits")
            .state,
        ScreenshotStorageState::BlockedFreeSpace
    );
    assert_eq!(
        evaluate_screenshot_storage(
            defaults.warning_free_bytes,
            defaults.managed_image_quota_bytes,
            defaults,
        )
        .expect("limits")
        .state,
        ScreenshotStorageState::BlockedImageQuota
    );

    let candidate = 123;
    let exact_available = defaults.minimum_free_bytes + defaults.journal_reserve_bytes + candidate;
    evaluate_screenshot_storage(exact_available, 0, defaults)
        .expect("limits")
        .ensure_candidate_fits(candidate)
        .expect("exact free-space boundary is admitted");
    assert!(matches!(
        evaluate_screenshot_storage(exact_available - 1, 0, defaults)
            .expect("limits")
            .ensure_candidate_fits(candidate),
        Err(StoreError::ScreenshotFreeSpace { .. })
    ));
    evaluate_screenshot_storage(
        exact_available,
        defaults.managed_image_quota_bytes - candidate,
        defaults,
    )
    .expect("limits")
    .ensure_candidate_fits(candidate)
    .expect("exact quota boundary is admitted");
    assert!(matches!(
        evaluate_screenshot_storage(
            exact_available,
            defaults.managed_image_quota_bytes - candidate + 1,
            defaults,
        )
        .expect("limits")
        .ensure_candidate_fits(candidate),
        Err(StoreError::ScreenshotImageQuota { .. })
    ));
    assert!(matches!(
        evaluate_screenshot_storage(u64::MAX, 0, defaults)
            .expect("limits")
            .ensure_candidate_fits(u64::MAX),
        Err(StoreError::ScreenshotFreeSpace {
            required_bytes: u64::MAX,
            ..
        })
    ));
}

fn cloned_image_pair(
    events: &[EventEnvelope],
    ordinal: u32,
) -> Result<(EventEnvelope, EventEnvelope), Box<dyn std::error::Error>> {
    let mut observation = events[0].clone();
    let mut completion = events[1].clone();
    observation.event_id = EventId::new(format!("evt-storage-{ordinal}"))?;
    let EventPayload::ObservationAttempt(attempt) = &mut observation.payload else {
        return Err("fixture observation missing".into());
    };
    let ObservationContent::Captured(content) = &mut attempt.content else {
        return Err("fixture captured content missing".into());
    };
    let image = content.image.as_mut().ok_or("fixture image missing")?;
    image.artifact_id = ImageArtifactId::new(format!("img-storage-{ordinal}"))?;
    image.managed_relative_path =
        ManagedRelativePath::new(format!("screenshots/2026-07-13/img-storage-{ordinal}.heic"))?;
    image.content_hash = format!("storage-hash-{ordinal}");
    content.content_hash = image.content_hash.clone();

    completion.event_id = EventId::new(format!("evt-storage-{ordinal}-write"))?;
    let EventPayload::ScreenshotLifecycle(lifecycle) = &mut completion.payload else {
        return Err("fixture lifecycle missing".into());
    };
    lifecycle.artifact_id = image.artifact_id.clone();
    lifecycle.source_event_id = observation.event_id.clone();
    observation.validate()?;
    completion.validate()?;
    Ok((observation, completion))
}

#[test]
fn managed_image_accounting_is_exact_and_includes_provisional_bytes() -> chronicle_store::Result<()>
{
    let (_temporary, root, _sqlite, projector) = common::store()?;
    root.atomic_write("screenshots/2026-07-12/seed.heic", b"12345")?;
    root.atomic_write("screenshots/2026-07-12/.pending.provisional", b"123")?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root), projector)?
        .with_storage_limits(limits(100))?
        .with_storage_available_bytes_probe(fixed_available);
    let health = store.storage_health()?;
    assert_eq!(health.managed_image_bytes, 8);
    assert_eq!(health.available_bytes, 64 * 1024 * 1024);
    assert_eq!(health.state, ScreenshotStorageState::Healthy);
    Ok(())
}

#[test]
fn unsafe_inventory_objects_fail_closed() -> chronicle_store::Result<()> {
    use std::os::unix::fs::symlink;

    let (_temporary, root, _sqlite, projector) = common::store()?;
    symlink(
        root.path().join("config.json"),
        root.path().join("screenshots/unsafe-link"),
    )?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root), projector)?
        .with_storage_limits(limits(100))?
        .with_storage_available_bytes_probe(fixed_available);
    assert!(matches!(
        store.storage_health(),
        Err(StoreError::InvalidPath(message)) if message.contains("symbolic link")
    ));
    Ok(())
}

#[test]
fn quota_rejection_precedes_every_screenshot_and_canonical_mutation() -> chronicle_store::Result<()>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    root.atomic_write("screenshots/2026-07-12/seed.heic", b"1234")?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?
        .with_storage_limits(limits(6))?
        .with_storage_available_bytes_probe(fixed_available);

    assert!(matches!(
        store.retain(&events[0], b"123", &events[1], FaultInjector::none()),
        Err(StoreError::ScreenshotImageQuota {
            managed_image_bytes: 4,
            candidate_bytes: 3,
            quota_bytes: 6,
        })
    ));
    assert!(!root.exists("screenshots/2026-07-13/.img-001.provisional")?);
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
    assert!(
        CanonicalJournal::new(root.clone())
            .scan_all(JournalFamily::Events, false)?
            .records
            .is_empty()
    );
    assert!(root.list_file_names("evidence/events")?.is_empty());
    let snapshot = sqlite.snapshot_ids()?;
    assert!(snapshot.event_ids.is_empty());
    assert!(snapshot.screenshot_lifecycle.is_empty());
    assert_eq!(store.storage_health()?.managed_image_bytes, 4);
    Ok(())
}

#[test]
fn free_space_rejection_precedes_every_screenshot_and_canonical_mutation()
-> chronicle_store::Result<()> {
    fn below_floor(_: &ManagedRoot) -> chronicle_store::Result<u64> {
        Ok(20 * 1024 * 1024 - 1)
    }

    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?
        .with_storage_limits(limits(100))?
        .with_storage_available_bytes_probe(below_floor);

    assert!(matches!(
        store.retain(&events[0], b"1", &events[1], FaultInjector::none()),
        Err(StoreError::ScreenshotFreeSpace {
            available_bytes,
            required_bytes,
        }) if available_bytes == 20 * 1024 * 1024 - 1
            && required_bytes == 20 * 1024 * 1024 + 1
    ));
    assert!(!root.exists("screenshots/2026-07-13/.img-001.provisional")?);
    assert!(!root.exists("screenshots/2026-07-13/img-001.heic")?);
    assert!(
        CanonicalJournal::new(root.clone())
            .scan_all(JournalFamily::Events, false)?
            .records
            .is_empty()
    );
    assert!(root.list_file_names("evidence/events")?.is_empty());
    let snapshot = sqlite.snapshot_ids()?;
    assert!(snapshot.event_ids.is_empty());
    assert!(snapshot.screenshot_lifecycle.is_empty());
    Ok(())
}

#[test]
fn concurrent_writers_recheck_quota_under_the_inventory_lock()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = common::events()?;
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root.clone()), projector)?
        .with_storage_limits(limits(3))?
        .with_storage_available_bytes_probe(fixed_available);
    let pairs = [
        cloned_image_pair(&events, 1)?,
        cloned_image_pair(&events, 2)?,
    ];
    let barrier = Arc::new(Barrier::new(3));
    let handles = pairs.map(|(observation, completion)| {
        let store = store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.retain(&observation, b"123", &completion, FaultInjector::none())
        })
    });
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread"))
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(StoreError::ScreenshotImageQuota { .. })))
            .count(),
        1
    );
    assert_eq!(store.storage_health()?.managed_image_bytes, 3);
    assert_eq!(sqlite.snapshot_ids()?.event_ids.len(), 2);
    assert_eq!(sqlite.snapshot_ids()?.screenshot_lifecycle.len(), 1);
    Ok(())
}

#[test]
fn deleting_retained_bytes_releases_exact_quota() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, projector) = common::store()?;
    let events = common::events()?;
    let image = b"synthetic-image-bytes";
    let store = ScreenshotStore::new(root.clone(), CanonicalJournal::new(root), projector)?
        .with_storage_limits(limits(image.len() as u64))?
        .with_storage_available_bytes_probe(fixed_available);
    store.retain(&events[0], image, &events[1], FaultInjector::none())?;
    assert_eq!(
        store.storage_health()?.state,
        ScreenshotStorageState::BlockedImageQuota
    );
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
        .ok_or_else(|| StoreError::InvalidPath("delete request fixture missing".to_owned()))?;
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
        .ok_or_else(|| StoreError::InvalidPath("delete completion fixture missing".to_owned()))?;
    store.delete(request, completion, FaultInjector::none())?;
    let health = store.storage_health()?;
    assert_eq!(health.managed_image_bytes, 0);
    assert_eq!(health.state, ScreenshotStorageState::Healthy);
    Ok(())
}
