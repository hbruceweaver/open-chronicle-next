mod common;

use chronicle_domain::{
    DurableAcknowledgement, RequestId, ScreenshotProjectedState, SharedServiceOperation,
    SharedServiceRequest, SharedServiceResult, StudyHealthState,
};
use chronicle_engine::{
    CadenceStamp, ChunkerConfig, EngineError, IngestRequest, RecordingCoordinator, SharedService,
    StudyBoundary,
};
use chronicle_store::{FaultInjector, FaultPoint, ManagedRoot, SqliteStore, StoreError};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC test timestamp")
}

fn stamp(tick: u64) -> CadenceStamp {
    CadenceStamp {
        boot_sequence: "study-test-boot".to_owned(),
        monotonic_tick: tick,
    }
}

fn coordinator() -> Result<(tempfile::TempDir, RecordingCoordinator), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let coordinator = RecordingCoordinator::open_at(
        root,
        ChunkerConfig {
            aggregator_version: "study-test-1".to_owned(),
            max_cadence_seconds: 60,
        },
        at("2026-07-13T09:00:00Z"),
    )?;
    Ok((temporary, coordinator))
}

#[test]
fn personal_mode_is_unbounded_and_study_end_is_exactly_half_open()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, mut coordinator) = coordinator()?;
    assert!(coordinator.capture_allowed(at("2036-07-13T09:00:00Z"))?);

    let start = at("2026-07-13T09:00:00Z");
    let end = at("2026-07-13T10:00:00Z");
    coordinator.configure_study(StudyBoundary { start, end })?;
    assert!(!coordinator.capture_allowed(at("2026-07-13T08:59:59Z"))?);
    assert!(coordinator.capture_allowed(start)?);
    assert!(coordinator.capture_allowed(at("2026-07-13T09:59:59Z"))?);
    assert!(!coordinator.capture_allowed(end)?);
    assert!(!coordinator.capture_allowed(at("2026-07-13T10:00:01Z"))?);
    Ok(())
}

#[test]
fn expired_study_rejects_first_wake_ingest_until_explicit_valid_extension()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, mut coordinator) = coordinator()?;
    let start = at("2026-07-13T09:00:00Z");
    let end = at("2026-07-13T10:00:00Z");
    coordinator.configure_study(StudyBoundary { start, end })?;
    let wake = at("2026-07-13T11:00:00Z");
    let event = common::fixture_events("events.jsonl")?[2].clone();
    let request = IngestRequest {
        event,
        cadence: Some(stamp(1)),
    };
    assert!(matches!(
        coordinator.ingest(request.clone(), wake),
        Err(EngineError::StudyExpired)
    ));
    assert!(
        coordinator
            .extend_study(at("2026-07-13T10:30:00Z"), wake)
            .is_err()
    );
    assert!(coordinator.extend_study(end, wake).is_err());

    let extended_end = at("2026-07-13T12:00:00Z");
    let extended = coordinator.extend_study(extended_end, wake)?;
    assert_eq!(extended.start, start);
    assert_eq!(extended.end, extended_end);
    coordinator.ingest(request, wake)?;
    Ok(())
}

#[test]
fn expired_study_latch_survives_restart_and_wall_clock_rollback_until_extension()
-> Result<(), Box<dyn std::error::Error>> {
    let (temporary, mut coordinator) = coordinator()?;
    let start = at("2026-07-13T09:00:00Z");
    let end = at("2026-07-13T10:00:00Z");
    coordinator.configure_study(StudyBoundary { start, end })?;

    assert!(!coordinator.capture_allowed(end)?);
    assert!(!coordinator.capture_allowed(at("2026-07-13T09:30:00Z"))?);
    drop(coordinator);

    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let mut reopened = RecordingCoordinator::open_at(
        root,
        ChunkerConfig {
            aggregator_version: "study-test-1".to_owned(),
            max_cadence_seconds: 60,
        },
        at("2026-07-13T09:30:00Z"),
    )?;
    assert!(!reopened.capture_allowed(at("2026-07-13T09:30:00Z"))?);

    let extended = reopened.extend_study(at("2026-07-13T11:00:00Z"), at("2026-07-13T09:30:00Z"))?;
    assert_eq!(extended.start, start);
    assert!(reopened.capture_allowed(at("2026-07-13T09:30:00Z"))?);
    Ok(())
}

#[test]
fn config_updates_preserve_unknown_authoritative_fields() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    root.atomic_write(
        "config.json",
        br#"{"capture_interval_seconds":30,"unknown_future":{"keep":true}}"#,
    )?;
    let mut coordinator = RecordingCoordinator::open_at(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "study-test-1".to_owned(),
            max_cadence_seconds: 60,
        },
        at("2026-07-13T09:00:00Z"),
    )?;
    coordinator.configure_study(StudyBoundary {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T10:00:00Z"),
    })?;
    let config: serde_json::Value = serde_json::from_slice(&root.read("config.json")?)?;
    assert_eq!(config["capture_interval_seconds"], 30);
    assert_eq!(config["unknown_future"]["keep"], true);
    assert_eq!(config["recording_mode"]["type"], "study");
    Ok(())
}

#[test]
fn same_mode_rewrites_preserve_nested_unknown_fields_and_mode_changes_drop_them()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    root.atomic_write(
        "config.json",
        br#"{
          "unknown_top":{"keep":true},
          "recording_mode":{
            "type":"study",
            "start":"2026-07-13T09:00:00Z",
            "end":"2026-07-13T10:00:00Z",
            "expired_at":null,
            "warning_minutes":15,
            "future_policy":{"mode":"strict"}
          }
        }"#,
    )?;
    let mut coordinator = RecordingCoordinator::open_at(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "study-test-1".to_owned(),
            max_cadence_seconds: 60,
        },
        at("2026-07-13T09:30:00Z"),
    )?;

    assert!(!coordinator.capture_allowed(at("2026-07-13T10:00:00Z"))?);
    let latched: serde_json::Value = serde_json::from_slice(&root.read("config.json")?)?;
    assert_eq!(latched["recording_mode"]["warning_minutes"], 15);
    assert_eq!(latched["recording_mode"]["future_policy"]["mode"], "strict");
    assert_eq!(
        latched["recording_mode"]["expired_at"],
        "2026-07-13T10:00:00Z"
    );

    coordinator.extend_study(at("2026-07-13T11:00:00Z"), at("2026-07-13T10:15:00Z"))?;
    let extended: serde_json::Value = serde_json::from_slice(&root.read("config.json")?)?;
    assert_eq!(extended["recording_mode"]["warning_minutes"], 15);
    assert_eq!(
        extended["recording_mode"]["future_policy"]["mode"],
        "strict"
    );
    assert_eq!(
        extended["recording_mode"]["expired_at"],
        serde_json::Value::Null
    );

    coordinator.use_personal_mode()?;
    let personal: serde_json::Value = serde_json::from_slice(&root.read("config.json")?)?;
    assert_eq!(personal["recording_mode"]["type"], "personal");
    assert!(personal["recording_mode"].get("warning_minutes").is_none());
    assert!(personal["recording_mode"].get("future_policy").is_none());
    assert_eq!(personal["unknown_top"]["keep"], true);
    Ok(())
}

#[test]
fn retained_image_acknowledgement_requires_lifecycle_completion()
-> Result<(), Box<dyn std::error::Error>> {
    for point in [
        FaultPoint::AfterProvisionalImageSync,
        FaultPoint::AfterObservationAppend,
        FaultPoint::AfterImagePromotion,
        FaultPoint::AfterImagePromotionDirectorySync,
        FaultPoint::AfterLifecycleCompletion,
    ] {
        let (_temporary, mut coordinator) = coordinator()?;
        let events = common::fixture_events("events.jsonl")?;
        assert!(matches!(
            coordinator.retain_screenshot(
                &events[0],
                b"synthetic-image-bytes",
                &events[1],
                stamp(1),
                at("2026-07-13T09:00:17Z"),
                FaultInjector::at(point),
            ),
            Err(EngineError::Store(StoreError::InjectedFault(actual))) if actual == point
        ));
        coordinator.reconcile_pending_images(at("2026-07-13T09:00:18Z"))?;
        coordinator.ingest(
            IngestRequest {
                event: events[2].clone(),
                cadence: Some(stamp(2)),
            },
            at("2026-07-13T09:00:46Z"),
        )?;

        let root = ManagedRoot::initialize(_temporary.path().join("store"))?;
        let snapshot = SqliteStore::open(root.clone())?.snapshot_ids()?;
        let provisional = root.exists("screenshots/2026-07-13/.img-001.provisional")?;
        assert!(!provisional, "live reconciliation left provisional bytes");
        if point == FaultPoint::AfterProvisionalImageSync {
            assert!(snapshot.screenshot_lifecycle.is_empty());
        } else {
            assert_eq!(
                snapshot.screenshot_lifecycle,
                vec![("img-001".to_owned(), "retained".to_owned())]
            );
        }
    }

    let (_temporary, mut coordinator) = coordinator()?;
    let events = common::fixture_events("events.jsonl")?;
    let acknowledgement = coordinator.retain_screenshot(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        stamp(1),
        at("2026-07-13T09:00:17Z"),
        FaultInjector::none(),
    )?;
    assert_eq!(
        acknowledgement.acknowledgement,
        DurableAcknowledgement::Durable
    );
    assert_eq!(
        acknowledgement.lifecycle_state,
        ScreenshotProjectedState::Retained
    );
    coordinator.ingest(
        IngestRequest {
            event: events[2].clone(),
            cadence: Some(stamp(2)),
        },
        at("2026-07-13T09:00:46Z"),
    )?;
    Ok(())
}

#[test]
fn health_exposes_only_typed_study_and_retention_facts() -> Result<(), Box<dyn std::error::Error>> {
    let (temporary, mut coordinator) = coordinator()?;
    let start = at("2026-07-13T09:00:00Z");
    let end = at("2026-07-13T10:00:00Z");
    coordinator.configure_study(StudyBoundary { start, end })?;
    let events = common::fixture_events("events.jsonl")?;
    coordinator.retain_screenshot(
        &events[0],
        b"synthetic-image-bytes",
        &events[1],
        stamp(1),
        at("2026-07-13T09:00:17Z"),
        FaultInjector::none(),
    )?;

    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let service = SharedService::open(root.clone(), SqliteStore::open(root.clone())?)?;
    let request = |id: &str| -> Result<SharedServiceRequest, Box<dyn std::error::Error>> {
        Ok(SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: RequestId::new(id)?,
            store_generation: 1,
            operation: SharedServiceOperation::Health,
        })
    };
    let response = service.execute(request("study-health-active")?, at("2026-07-13T09:30:00Z"))?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    assert_eq!(health.study.state, StudyHealthState::Active);
    assert_eq!(health.study.start, Some(start));
    assert_eq!(health.study.end, Some(end));
    assert_eq!(health.screenshot_retention.retained, 1);
    assert_eq!(
        health.screenshot_retention.next_expiry_at,
        Some(at("2026-07-14T09:00:16Z"))
    );
    let json = serde_json::to_string(&health)?;
    for forbidden in [
        "Synthetic note",
        "Quarterly notes",
        "com.example.writer",
        "screenshots/",
        "img-001",
    ] {
        assert!(!json.contains(forbidden), "health disclosed {forbidden}");
    }

    let config_before_health = root.read("config.json")?;
    let response = service.execute(request("study-health-expired")?, end)?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    assert_eq!(health.study.state, StudyHealthState::Expired);
    assert_eq!(health.study.expired_at, None);
    assert_eq!(root.read("config.json")?, config_before_health);
    assert!(coordinator.capture_allowed(at("2026-07-13T09:30:00Z"))?);
    let latched_at = at("2026-07-13T11:00:00Z");
    assert!(!coordinator.capture_allowed(latched_at)?);
    let response = service.execute(request("study-health-latched")?, latched_at)?;
    let SharedServiceResult::Health(health) = response.result else {
        return Err("expected health response".into());
    };
    assert_eq!(health.study.state, StudyHealthState::Expired);
    assert_eq!(health.study.expired_at, Some(latched_at));
    assert!(!coordinator.capture_allowed(at("2026-07-13T09:30:00Z"))?);
    Ok(())
}

#[test]
fn expired_study_blocks_pixels_before_provisional_persistence()
-> Result<(), Box<dyn std::error::Error>> {
    let (temporary, mut coordinator) = coordinator()?;
    coordinator.configure_study(StudyBoundary {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T10:00:00Z"),
    })?;
    let events = common::fixture_events("events.jsonl")?;
    assert!(matches!(
        coordinator.retain_screenshot(
            &events[0],
            b"synthetic-image-bytes",
            &events[1],
            stamp(1),
            at("2026-07-13T10:00:00Z"),
            FaultInjector::none(),
        ),
        Err(EngineError::StudyExpired)
    ));
    let screenshot_directory = temporary.path().join("store/screenshots/2026-07-13");
    assert!(!screenshot_directory.exists());
    Ok(())
}

#[test]
fn generic_ingest_cannot_acknowledge_an_uncoordinated_image_intent()
-> Result<(), Box<dyn std::error::Error>> {
    let (_temporary, mut coordinator) = coordinator()?;
    let events = common::fixture_events("events.jsonl")?;
    assert!(matches!(
        coordinator.ingest(
            IngestRequest {
                event: events[0].clone(),
                cadence: Some(stamp(1)),
            },
            at("2026-07-13T09:00:17Z"),
        ),
        Err(EngineError::Configuration(message))
            if message.contains("retain_screenshot")
    ));
    Ok(())
}

#[test]
fn screenshot_event_time_cannot_bypass_the_study_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let (_temporary, mut coordinator) = coordinator()?;
    coordinator.configure_study(StudyBoundary {
        start: at("2026-07-13T09:01:00Z"),
        end: at("2026-07-13T10:00:00Z"),
    })?;
    let events = common::fixture_events("events.jsonl")?;
    assert!(matches!(
        coordinator.retain_screenshot(
            &events[0],
            b"synthetic-image-bytes",
            &events[1],
            stamp(1),
            at("2026-07-13T09:01:01Z"),
            FaultInjector::none(),
        ),
        Err(EngineError::StudyNotStarted)
    ));
    Ok(())
}
