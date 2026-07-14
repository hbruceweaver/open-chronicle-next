mod common;

use chronicle_domain::{
    AttemptStatus, CaptureCadence, DeviceId, DurableAcknowledgement, EventEnvelope, EventId,
    EventPayload, EvidenceState, GapReason, NoEvidenceContent, NoEvidenceReason,
    ObservationContent, OcrState, PresenceState, SharedServiceRequest,
};
use chronicle_engine::{
    CadenceStamp, CaptureAdmissionReason, ChunkerConfig, EngineError, IngestRequest,
    RecordingCoordinator, RuntimeFaultInjector, StartupReconcileRequest,
};
use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, JournalFamily, ManagedRoot, StoreError,
};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC test timestamp")
}

fn open(root: ManagedRoot, now: DateTime<Utc>) -> RecordingCoordinator {
    RecordingCoordinator::open_at(
        root,
        ChunkerConfig {
            aggregator_version: "runtime-test-1".to_owned(),
            max_cadence_seconds: 60,
        },
        now,
    )
    .expect("open recording coordinator")
}

fn startup(session_id: &str, now: DateTime<Utc>) -> StartupReconcileRequest {
    StartupReconcileRequest {
        session_id: session_id.to_owned(),
        device_id: DeviceId::new("dev-runtime-test").expect("device ID"),
        display_timezone: "Europe/Zurich".to_owned(),
        now,
    }
}

fn stamp(tick: u64) -> CadenceStamp {
    CadenceStamp {
        boot_sequence: "runtime-test-boot".to_owned(),
        monotonic_tick: tick,
    }
}

fn gap_events(root: &ManagedRoot) -> Vec<EventEnvelope> {
    let scan = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)
        .expect("read canonical events");
    let mut events = scan
        .records
        .iter()
        .filter_map(|record| {
            EventEnvelope::parse(std::str::from_utf8(record.body_bytes()).ok()?).ok()
        })
        .filter(|event| matches!(event.payload, EventPayload::RecordingGap(_)))
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.observed_at);
    events
}

fn paused_event(mut event: EventEnvelope, event_id: &str) -> EventEnvelope {
    event.event_id = EventId::new(event_id).expect("paused event ID");
    let EventPayload::ObservationAttempt(attempt) = &mut event.payload else {
        panic!("fixture must be an observation attempt");
    };
    attempt.attempt_status = AttemptStatus::Skipped;
    attempt.evidence_state = EvidenceState::Paused;
    attempt.presence_state = PresenceState::Active;
    attempt.idle_seconds = None;
    attempt.ocr_state = OcrState::NotRun;
    attempt.content = ObservationContent::NoEvidence(NoEvidenceContent {
        reason: NoEvidenceReason::UserPaused,
    });
    event.validate().expect("valid paused event");
    event
}

fn update_config(root: &ManagedRoot, update: impl FnOnce(&mut serde_json::Value)) {
    let mut document: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read config"))
            .expect("decode config");
    update(&mut document);
    root.atomic_write(
        "config.json",
        &serde_json::to_vec(&document).expect("encode config"),
    )
    .expect("write config");
}

#[test]
fn runtime_config_round_trips_and_preserves_unknown_top_level_keys() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    root.atomic_write(
        "config.json",
        br#"{"future_policy":{"keep":true},"recording_preference":false}"#,
    )
    .expect("seed config");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));

    coordinator
        .set_recording_preference(true)
        .expect("enable recording preference");
    coordinator
        .set_cadence(CaptureCadence::ThirtySeconds)
        .expect("set cadence");
    let state = coordinator
        .runtime_state(at("2026-07-13T09:00:01Z"))
        .expect("runtime state");
    assert!(state.recording_preference);
    assert_eq!(state.cadence, CaptureCadence::ThirtySeconds);

    let document: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read config"))
            .expect("decode config");
    assert_eq!(document["future_policy"]["keep"], true);
    assert_eq!(document["recording_preference"], true);
    assert_eq!(document["capture_cadence"], "thirty-seconds");
}

#[test]
fn admission_and_failure_before_journal_do_not_advance_durable_heartbeat() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
    coordinator
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    coordinator
        .set_recording_preference(true)
        .expect("enable recording");
    assert!(
        coordinator
            .capture_admission(at("2026-07-13T09:05:00Z"))
            .expect("admission")
            .allowed
    );
    let event = common::fixture_events("events.jsonl").expect("events")[2].clone();
    let failed = coordinator.ingest_with_faults(
        IngestRequest {
            event,
            cadence: None,
        },
        at("2026-07-13T09:06:00Z"),
        FaultInjector::none(),
        FaultInjector::none(),
    );
    assert!(matches!(
        failed,
        Err(EngineError::Cadence(message))
            if message.contains("cadence stamp")
    ));
    drop(coordinator);

    let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
    reopened
        .startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z")))
        .expect("reconcile crash gap");
    let gaps = gap_events(&root);
    let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
        unreachable!();
    };
    assert_eq!(gap.start, at("2026-07-13T09:00:00Z"));
    assert_eq!(gap.end, at("2026-07-13T09:10:00Z"));
}

#[test]
fn successful_durable_observation_advances_heartbeat_after_acknowledgement() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
    coordinator
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    coordinator
        .set_recording_preference(true)
        .expect("enable recording");
    let event = common::fixture_events("events.jsonl").expect("events")[2].clone();
    let outcome = coordinator
        .ingest(
            IngestRequest {
                event,
                cadence: Some(stamp(1)),
            },
            at("2026-07-13T09:07:00Z"),
        )
        .expect("durable observation");
    assert!(matches!(
        outcome.acknowledgement,
        DurableAcknowledgement::Durable | DurableAcknowledgement::JournalDurableProjectionPending
    ));
    drop(coordinator);

    let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
    reopened
        .startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z")))
        .expect("reconcile crash gap");
    let gaps = gap_events(&root);
    let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
        unreachable!();
    };
    assert_eq!(gap.start, at("2026-07-13T09:07:00Z"));
}

#[test]
fn startup_recovers_canonical_event_heartbeat_intent_without_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
    coordinator
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    coordinator
        .set_recording_preference(true)
        .expect("enable recording");
    let event = common::fixture_events("events.jsonl").expect("events")[2].clone();
    let request = IngestRequest {
        event: event.clone(),
        cadence: Some(stamp(1)),
    };
    assert!(matches!(
        coordinator.ingest_with_runtime_faults(
            request,
            at("2026-07-13T09:07:00Z"),
            FaultInjector::none(),
            FaultInjector::none(),
            RuntimeFaultInjector::before_checkpoint_write(),
        ),
        Err(EngineError::Configuration(message))
            if message.contains("runtime checkpoint write")
    ));
    let records = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)
        .expect("scan events")
        .records;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.stable_id() == event.event_id.as_str())
            .count(),
        1
    );
    update_config(&root, |document| {
        document["heartbeat_acknowledgement_intent"]["future_intent"] =
            serde_json::json!({"keep": true});
        document["heartbeat_acknowledgement_intent"]["proofs"][0]["future_proof"] =
            serde_json::json!({"keep": true});
    });
    coordinator
        .set_cadence(CaptureCadence::ThirtySeconds)
        .expect("rewrite config while heartbeat intent is unresolved");
    let document: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read config"))
            .expect("decode config");
    assert_eq!(
        document["heartbeat_acknowledgement_intent"]["future_intent"]["keep"],
        true
    );
    assert_eq!(
        document["heartbeat_acknowledgement_intent"]["proofs"][0]["future_proof"]["keep"],
        true
    );
    drop(coordinator);

    let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
    reopened
        .startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z")))
        .expect("reconcile crash gap");
    let gaps = gap_events(&root);
    let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
        unreachable!();
    };
    assert_eq!(gap.start, at("2026-07-13T09:07:00Z"));
    let document: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read config"))
            .expect("decode config");
    assert!(document.get("heartbeat_acknowledgement_intent").is_none());
}

#[test]
fn heartbeat_intent_rejects_empty_duplicate_unordered_unbounded_and_invalid_proofs() {
    let proof = |event_id: &str, heartbeat_at: &str| {
        serde_json::json!({
            "event_id": event_id,
            "heartbeat_at": heartbeat_at,
        })
    };
    let cases = [
        ("empty", serde_json::json!({"proofs": []})),
        (
            "duplicate",
            serde_json::json!({"proofs": [
                proof("proof-one", "2026-07-13T09:00:01Z"),
                proof("proof-one", "2026-07-13T09:00:02Z"),
            ]}),
        ),
        (
            "unordered",
            serde_json::json!({"proofs": [
                proof("proof-one", "2026-07-13T09:00:02Z"),
                proof("proof-two", "2026-07-13T09:00:01Z"),
            ]}),
        ),
        (
            "unbounded",
            serde_json::json!({"proofs": [
                proof("proof-one", "2026-07-13T09:00:01Z"),
                proof("proof-two", "2026-07-13T09:00:02Z"),
                proof("proof-three", "2026-07-13T09:00:03Z"),
                proof("proof-four", "2026-07-13T09:00:04Z"),
                proof("proof-five", "2026-07-13T09:00:05Z"),
            ]}),
        ),
        (
            "invalid",
            serde_json::json!({"proofs": [
                proof("invalid/proof", "2026-07-13T09:00:01Z"),
            ]}),
        ),
    ];

    for (label, invalid_intent) in cases {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
        let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
        coordinator
            .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
            .expect("start session");
        update_config(&root, |document| {
            document["heartbeat_acknowledgement_intent"] = invalid_intent.clone();
        });

        assert!(
            matches!(
                coordinator.startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z"))),
                Err(EngineError::Configuration(_))
            ),
            "invalid proof case {label} must be rejected"
        );
        let document: serde_json::Value =
            serde_json::from_slice(&root.read("config.json").expect("read rejected config"))
                .expect("decode rejected config");
        assert_eq!(
            document["heartbeat_acknowledgement_intent"], invalid_intent,
            "rejection must not mutate the invalid intent for {label}"
        );
    }
}

#[test]
fn startup_recovers_canonical_screenshot_heartbeat_intent_without_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
    coordinator
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    coordinator
        .set_recording_preference(true)
        .expect("enable recording");
    let events = common::fixture_events("events.jsonl").expect("events");
    assert!(matches!(
        coordinator.retain_screenshot_with_runtime_faults(
            &events[0],
            b"synthetic-image",
            &events[1],
            stamp(1),
            at("2026-07-13T09:02:00Z"),
            FaultInjector::none(),
            RuntimeFaultInjector::before_checkpoint_write(),
        ),
        Err(EngineError::Configuration(message))
            if message.contains("runtime checkpoint write")
    ));
    assert!(
        root.exists("screenshots/2026-07-13/img-001.heic")
            .expect("final screenshot existence")
    );
    drop(coordinator);

    let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
    reopened
        .startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z")))
        .expect("reconcile crash gap");
    let gaps = gap_events(&root);
    let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
        unreachable!();
    };
    assert_eq!(gap.start, at("2026-07-13T09:02:00Z"));
}

#[test]
fn screenshot_fault_matrix_recovers_the_strongest_canonical_heartbeat_without_retry() {
    let cases = [
        (
            FaultPoint::AfterProvisionalImageSync,
            at("2026-07-13T09:00:00Z"),
            false,
            false,
        ),
        (
            FaultPoint::AfterJournalAppend,
            at("2026-07-13T09:00:16Z"),
            true,
            false,
        ),
        (
            FaultPoint::AfterJournalSync,
            at("2026-07-13T09:00:16Z"),
            true,
            false,
        ),
        (
            FaultPoint::AfterObservationAppend,
            at("2026-07-13T09:00:16Z"),
            true,
            false,
        ),
        (
            FaultPoint::AfterImagePromotion,
            at("2026-07-13T09:00:16Z"),
            true,
            false,
        ),
        (
            FaultPoint::AfterImagePromotionDirectorySync,
            at("2026-07-13T09:00:16Z"),
            true,
            false,
        ),
        (
            FaultPoint::AfterLifecycleCompletion,
            at("2026-07-13T09:02:00Z"),
            true,
            true,
        ),
    ];

    for (point, expected_gap_start, observation_survives, completion_survives) in cases {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
        let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
        coordinator
            .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
            .expect("start session");
        coordinator
            .set_recording_preference(true)
            .expect("enable recording");
        let events = common::fixture_events("events.jsonl").expect("events");
        assert!(matches!(
            coordinator.retain_screenshot_with_runtime_faults(
                &events[0],
                b"synthetic-image",
                &events[1],
                stamp(1),
                at("2026-07-13T09:02:00Z"),
                FaultInjector::at(point),
                RuntimeFaultInjector::none(),
            ),
            Err(EngineError::Store(StoreError::InjectedFault(actual))) if actual == point
        ));
        drop(coordinator);

        let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
        reopened
            .startup_reconcile(startup("session-two", at("2026-07-13T09:10:00Z")))
            .expect("recover screenshot and reconcile crash gap without retry");
        let gaps = gap_events(&root);
        let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
            unreachable!();
        };
        assert_eq!(gap.start, expected_gap_start, "fault boundary {point:?}");

        let records = CanonicalJournal::new(root.clone())
            .scan_all(JournalFamily::Events, false)
            .expect("scan recovered events")
            .records;
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stable_id() == events[0].event_id.as_str())
                .count(),
            usize::from(observation_survives),
            "observation count after {point:?}"
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stable_id() == events[1].event_id.as_str())
                .count(),
            usize::from(completion_survives),
            "supplied completion count after {point:?}"
        );
    }
}

#[test]
fn clean_termination_records_exactly_one_next_launch_quit_gap() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut first = open(root.clone(), at("2026-07-13T09:00:00Z"));
    first
        .startup_reconcile(startup("session-clean", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    first
        .prepare_termination("session-clean", at("2026-07-13T09:05:00Z"))
        .expect("close session");
    drop(first);

    let mut reopened = open(root.clone(), at("2026-07-13T09:10:00Z"));
    let request = startup("session-next", at("2026-07-13T09:10:00Z"));
    let first = reopened
        .startup_reconcile(request.clone())
        .expect("start after clean termination");
    let repeated = reopened.startup_reconcile(request).expect("repeat startup");
    assert_eq!(first.gap_event_ids, repeated.gap_event_ids);
    assert_eq!(first.gap_event_ids.len(), 1);
    let gaps = gap_events(&root);
    assert_eq!(gaps.len(), 1);
    let EventPayload::RecordingGap(gap) = &gaps[0].payload else {
        unreachable!();
    };
    assert_eq!(gap.start, at("2026-07-13T09:05:00Z"));
    assert_eq!(gap.end, at("2026-07-13T09:10:00Z"));
    assert_eq!(gap.reason, GapReason::Quit);
}

#[test]
fn pending_replacement_crashes_accumulate_contiguous_immutable_gap_segments() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut first = open(root.clone(), at("2026-07-13T09:00:00Z"));
    first
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start first session");
    drop(first);

    for (session, now) in [
        ("session-two", "2026-07-13T09:10:00Z"),
        ("session-three", "2026-07-13T09:11:00Z"),
    ] {
        let mut replacement = open(root.clone(), at(now));
        let failed = replacement.startup_reconcile_with_faults(
            startup(session, at(now)),
            FaultInjector::at(FaultPoint::AfterJournalAppend),
        );
        assert!(
            failed.is_err(),
            "replacement startup must hit injected fault"
        );
        drop(replacement);
    }

    let mut final_process = open(root.clone(), at("2026-07-13T09:12:00Z"));
    let request = startup("session-four", at("2026-07-13T09:12:00Z"));
    let completed = final_process
        .startup_reconcile(request.clone())
        .expect("complete pending reconciliation");
    let repeated = final_process
        .startup_reconcile(request)
        .expect("repeat completed reconciliation");
    assert_eq!(completed.gap_event_ids, repeated.gap_event_ids);
    assert_eq!(completed.gap_event_ids.len(), 3);

    let gaps = gap_events(&root);
    let ranges = gaps
        .iter()
        .map(|event| {
            let EventPayload::RecordingGap(gap) = &event.payload else {
                unreachable!();
            };
            (gap.start, gap.end)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ranges,
        vec![
            (at("2026-07-13T09:00:00Z"), at("2026-07-13T09:10:00Z")),
            (at("2026-07-13T09:10:00Z"), at("2026-07-13T09:11:00Z")),
            (at("2026-07-13T09:11:00Z"), at("2026-07-13T09:12:00Z")),
        ]
    );
}

#[test]
fn pending_replacement_survives_wall_clock_rollback_without_mutating_prior_gap() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut first = open(root.clone(), at("2026-07-13T09:00:00Z"));
    first
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start first session");
    drop(first);

    let mut failed_start = open(root.clone(), at("2026-07-13T09:10:00Z"));
    assert!(
        failed_start
            .startup_reconcile_with_faults(
                startup("session-two", at("2026-07-13T09:10:00Z")),
                FaultInjector::at(FaultPoint::AfterJournalAppend),
            )
            .is_err()
    );
    drop(failed_start);
    let pending_before: serde_json::Value = serde_json::from_slice(
        &root
            .read("config.json")
            .expect("read pending startup config"),
    )
    .expect("decode pending startup config");
    let immutable_first = pending_before["pending_startup_reconciliation"]["gap_events"][0].clone();
    let immutable_first_id = immutable_first["event_id"]
        .as_str()
        .expect("pending event ID")
        .to_owned();
    let immutable_first_bytes = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)
        .expect("read canonical event before rollback")
        .records
        .into_iter()
        .find(|record| record.stable_id() == immutable_first_id)
        .expect("canonical prior gap before rollback")
        .body_bytes()
        .to_vec();

    let mut rollback = open(root.clone(), at("2026-07-13T09:05:00Z"));
    let request = startup("session-three", at("2026-07-13T09:05:00Z"));
    let completed = rollback
        .startup_reconcile(request.clone())
        .expect("adopt pending startup after clock rollback");
    let repeated = rollback
        .startup_reconcile(request)
        .expect("repeat rollback startup");
    assert_eq!(completed.gap_event_ids, repeated.gap_event_ids);
    assert_eq!(completed.gap_event_ids.len(), 2);

    let gaps = gap_events(&root);
    let preserved = gaps
        .iter()
        .find(|event| event.event_id.as_str() == immutable_first_id)
        .expect("immutable prior gap");
    assert_eq!(
        serde_json::to_value(preserved).expect("encode preserved gap"),
        immutable_first
    );
    let preserved_bytes = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)
        .expect("read canonical event after rollback")
        .records
        .into_iter()
        .find(|record| record.stable_id() == immutable_first_id)
        .expect("canonical prior gap after rollback")
        .body_bytes()
        .to_vec();
    assert_eq!(preserved_bytes, immutable_first_bytes);
    let correction = gaps
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::RecordingGap(gap) if gap.reason == GapReason::ClockCorrection => {
                Some(gap)
            }
            EventPayload::RecordingGap(_)
            | EventPayload::ObservationAttempt(_)
            | EventPayload::ScreenshotLifecycle(_) => None,
        })
        .expect("clock correction gap");
    assert_eq!(correction.start, at("2026-07-13T09:05:00Z"));
    assert_eq!(correction.end, at("2026-07-13T09:10:00Z"));
}

#[test]
fn paused_ingest_boundary_allows_only_current_user_paused_attempts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root.clone(), at("2026-07-13T09:00:00Z"));
    coordinator
        .startup_reconcile(startup("session-paused", at("2026-07-13T09:00:00Z")))
        .expect("start session");
    let events = common::fixture_events("events.jsonl").expect("events");

    for (index, event) in [events[2].clone(), events[4].clone()]
        .into_iter()
        .enumerate()
    {
        assert!(matches!(
            coordinator.ingest(
                IngestRequest {
                    event,
                    cadence: Some(stamp(index as u64 + 1)),
                },
                at("2026-07-13T09:02:00Z"),
            ),
            Err(EngineError::Configuration(message)) if message.contains("paused")
        ));
    }
    assert!(matches!(
        coordinator.retain_screenshot(
            &events[0],
            b"synthetic-image",
            &events[1],
            stamp(3),
            at("2026-07-13T09:02:00Z"),
            FaultInjector::none(),
        ),
        Err(EngineError::Configuration(message)) if message.contains("paused")
    ));
    assert!(
        CanonicalJournal::new(root.clone())
            .scan_all(JournalFamily::Events, false)
            .expect("scan paused journal")
            .records
            .is_empty()
    );

    let paused = paused_event(events[6].clone(), "evt-user-paused-current");
    coordinator
        .ingest(
            IngestRequest {
                event: paused.clone(),
                cadence: Some(stamp(4)),
            },
            at("2026-07-13T09:03:17Z"),
        )
        .expect("durable paused outcome");
    coordinator
        .set_recording_preference(true)
        .expect("resume recording");
    let stale = paused_event(paused, "evt-user-paused-stale");
    assert!(matches!(
        coordinator.ingest(
            IngestRequest {
                event: stale,
                cadence: Some(stamp(5)),
            },
            at("2026-07-13T09:03:18Z"),
        ),
        Err(EngineError::Configuration(message)) if message.contains("not paused")
    ));
}

#[test]
fn capture_admission_fails_closed_without_an_active_runtime_session() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut coordinator = open(root, at("2026-07-13T09:00:00Z"));
    coordinator
        .set_recording_preference(true)
        .expect("enable preference");
    let before = coordinator
        .capture_admission(at("2026-07-13T09:00:01Z"))
        .expect("pre-start admission");
    assert!(!before.allowed);
    assert_eq!(before.reason, CaptureAdmissionReason::RuntimeInactive);

    coordinator
        .startup_reconcile(startup("session-active", at("2026-07-13T09:00:02Z")))
        .expect("start session");
    coordinator
        .prepare_termination("session-active", at("2026-07-13T09:00:03Z"))
        .expect("terminate session");
    let after = coordinator
        .capture_admission(at("2026-07-13T09:00:04Z"))
        .expect("post-termination admission");
    assert!(!after.allowed);
    assert_eq!(after.reason, CaptureAdmissionReason::RuntimeInactive);
}

#[test]
fn runtime_owned_nested_extensions_survive_session_pending_and_last_rewrites() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let mut first = open(root.clone(), at("2026-07-13T09:00:00Z"));
    first
        .startup_reconcile(startup("session-one", at("2026-07-13T09:00:00Z")))
        .expect("start first session");
    drop(first);

    let mut second = open(root.clone(), at("2026-07-13T09:10:00Z"));
    assert!(
        second
            .startup_reconcile_with_faults(
                startup("session-two", at("2026-07-13T09:10:00Z")),
                FaultInjector::at(FaultPoint::AfterJournalAppend),
            )
            .is_err()
    );
    drop(second);
    update_config(&root, |document| {
        document["pending_startup_reconciliation"]["future_pending"] =
            serde_json::json!({"keep": true});
        document["pending_startup_reconciliation"]["new_session"]["future_session"] =
            serde_json::json!({"keep": true});
    });

    let mut third = open(root.clone(), at("2026-07-13T09:11:00Z"));
    assert!(
        third
            .startup_reconcile_with_faults(
                startup("session-three", at("2026-07-13T09:11:00Z")),
                FaultInjector::at(FaultPoint::AfterJournalAppend),
            )
            .is_err()
    );
    drop(third);
    let pending: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read pending config"))
            .expect("decode pending config");
    assert_eq!(
        pending["pending_startup_reconciliation"]["future_pending"]["keep"],
        true
    );
    assert_eq!(
        pending["pending_startup_reconciliation"]["new_session"]["future_session"]["keep"],
        true
    );

    let mut committed = open(root.clone(), at("2026-07-13T09:11:00Z"));
    committed
        .startup_reconcile(startup("session-three", at("2026-07-13T09:11:00Z")))
        .expect("commit adopted startup");
    update_config(&root, |document| {
        document["lifecycle_checkpoint"]["future_session"] = serde_json::json!({"keep": true});
        document["last_startup_reconciliation"]["future_last"] = serde_json::json!({"keep": true});
    });
    committed
        .prepare_termination("session-three", at("2026-07-13T09:12:00Z"))
        .expect("terminate third session");
    let after_termination: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read termination config"))
            .expect("decode termination config");
    assert_eq!(
        after_termination["lifecycle_checkpoint"]["future_session"]["keep"],
        true
    );
    drop(committed);

    let mut fourth = open(root.clone(), at("2026-07-13T09:13:00Z"));
    fourth
        .startup_reconcile(startup("session-four", at("2026-07-13T09:13:00Z")))
        .expect("commit fourth startup");
    let final_config: serde_json::Value =
        serde_json::from_slice(&root.read("config.json").expect("read final config"))
            .expect("decode final config");
    assert_eq!(
        final_config["last_startup_reconciliation"]["future_last"]["keep"],
        true
    );
}

#[test]
fn shared_service_contract_has_no_app_control_operation() {
    let encoded = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "req-control-must-stay-app-only",
        "store_generation": 1,
        "operation": { "type": "runtime-state" }
    })
    .to_string();
    assert!(SharedServiceRequest::parse(&encoded).is_err());
}
