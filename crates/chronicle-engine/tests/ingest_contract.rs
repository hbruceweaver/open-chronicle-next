mod common;

use std::error::Error;

use chronicle_domain::{DurableAcknowledgement, ProjectionHealth, UtcRange};
use chronicle_engine::{CadenceStamp, ChunkerConfig, IngestEngine, IngestRequest};
use chronicle_store::{
    CanonicalJournal, FaultInjector, FaultPoint, JournalFamily, SqliteStore, StoreQueries,
};
use chrono::{DateTime, Utc};

fn cadence(boot: &str, tick: u64) -> Option<CadenceStamp> {
    Some(CadenceStamp {
        boot_sequence: boot.to_owned(),
        monotonic_tick: tick,
    })
}

#[test]
fn engine_open_performs_one_canonical_startup_scan_per_family() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let engine = IngestEngine::open_at(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "single-startup-scan-1".to_owned(),
            max_cadence_seconds: 30,
        },
        "2026-07-13T09:00:00Z".parse()?,
    )?;
    let probe = CanonicalJournal::new(root);
    assert_eq!(probe.directory_enumeration_count(JournalFamily::Events), 1);
    assert_eq!(probe.directory_enumeration_count(JournalFamily::Chunks), 1);
    assert_eq!(probe.index_full_scan_count(JournalFamily::Events), 1);
    assert_eq!(probe.index_full_scan_count(JournalFamily::Chunks), 1);
    drop(engine);
    Ok(())
}

#[test]
fn ingest_is_journal_first_and_finalizes_only_after_boundary_plus_cadence()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let mut engine = IngestEngine::open(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "ingest-test-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    for (index, event) in events.into_iter().enumerate() {
        let now = if index == 9 {
            "2026-07-13T09:05:30Z".parse::<DateTime<Utc>>()?
        } else {
            event.recorded_at
        };
        let outcome = engine.ingest(
            IngestRequest {
                event,
                cadence: Some(CadenceStamp {
                    boot_sequence: "boot-ingest".to_owned(),
                    monotonic_tick: u64::try_from(index + 1)?,
                }),
            },
            now,
        )?;
        assert_eq!(outcome.acknowledgement, DurableAcknowledgement::Durable);
        assert_eq!(outcome.projection, ProjectionHealth::Current);
        if index < 9 {
            assert!(
                outcome
                    .aggregation
                    .as_ref()
                    .is_some_and(|report| report.generated_revision_ids.is_empty())
            );
        }
    }
    let sqlite = SqliteStore::open(root)?;
    let chunks = StoreQueries::new(sqlite).current_chunks_in_range(&UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:05:00Z".parse()?,
    })?;
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].evidence_seconds.captured, 300);
    Ok(())
}

#[test]
fn projection_fault_reports_durable_pending_and_replays_once() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let event = events.first().cloned().ok_or("fixture empty")?;
    let mut engine = IngestEngine::open(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "ingest-fault-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    let outcome = engine.ingest_with_faults(
        IngestRequest {
            event: event.clone(),
            cadence: cadence("projection-fault", 1),
        },
        event.recorded_at,
        FaultInjector::at(FaultPoint::AfterRowInsert),
        FaultInjector::none(),
    )?;
    assert_eq!(
        outcome.acknowledgement,
        DurableAcknowledgement::JournalDurableProjectionPending
    );
    let second = events
        .get(1)
        .cloned()
        .ok_or("second fixture event missing")?;
    let recovered = engine.ingest(
        IngestRequest {
            event: second.clone(),
            cadence: cadence("projection-fault", 2),
        },
        second.recorded_at,
    )?;
    assert_eq!(recovered.acknowledgement, DurableAcknowledgement::Durable);
    let sqlite = SqliteStore::open(root)?;
    let queries = StoreQueries::new(sqlite);
    assert!(queries.event(&event.event_id, true)?.is_some());
    assert!(queries.event(&second.event_id, true)?.is_some());
    Ok(())
}

#[test]
fn stable_event_retry_is_idempotent_and_mismatched_bytes_fail_before_append()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let event = common::fixture_events("ae4-ten-scheduled-events.jsonl")?
        .into_iter()
        .next()
        .ok_or("fixture empty")?;
    let mut engine = IngestEngine::open(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "ingest-idempotent-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    engine.ingest(
        IngestRequest {
            event: event.clone(),
            cadence: cadence("stable-retry", 1),
        },
        event.recorded_at,
    )?;
    engine.ingest(
        IngestRequest {
            event: event.clone(),
            cadence: cadence("stable-retry", 1),
        },
        event.recorded_at,
    )?;
    let journal = CanonicalJournal::new(root.clone());
    assert_eq!(
        journal
            .scan_all(JournalFamily::Events, false)?
            .records
            .len(),
        1
    );

    let mut mismatched = event;
    let chronicle_domain::EventPayload::ObservationAttempt(attempt) = &mut mismatched.payload
    else {
        return Err("fixture is not an observation".into());
    };
    let chronicle_domain::ObservationContent::Captured(content) = &mut attempt.content else {
        return Err("fixture is not captured".into());
    };
    content.content_hash = "different-stable-id-bytes".to_owned();
    assert!(
        engine
            .ingest(
                IngestRequest {
                    event: mismatched,
                    cadence: cadence("stable-retry", 1),
                },
                "2026-07-13T09:00:20Z".parse()?,
            )
            .is_err()
    );
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
fn chunk_projection_failure_is_recovered_before_the_next_ingest() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let mut engine = IngestEngine::open(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "ingest-chunk-recovery-1".to_owned(),
            max_cadence_seconds: 30,
        },
    )?;
    for (index, event) in events[..9].iter().enumerate() {
        engine.ingest(
            IngestRequest {
                event: event.clone(),
                cadence: cadence("chunk-recovery", u64::try_from(index + 1)?),
            },
            event.recorded_at,
        )?;
    }
    let last = events.last().cloned().ok_or("last event missing")?;
    let due: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    let lagging = engine.ingest_with_faults(
        IngestRequest {
            event: last.clone(),
            cadence: cadence("chunk-recovery", 10),
        },
        due,
        FaultInjector::none(),
        FaultInjector::at(FaultPoint::AfterJournalSync),
    )?;
    assert_eq!(lagging.acknowledgement, DurableAcknowledgement::Durable);
    assert_eq!(lagging.projection, ProjectionHealth::Lagging);
    assert!(lagging.aggregation.is_none());
    let recovered = engine.ingest(
        IngestRequest {
            event: last,
            cadence: cadence("chunk-recovery", 10),
        },
        due,
    )?;
    assert_eq!(recovered.acknowledgement, DurableAcknowledgement::Durable);
    let journal = CanonicalJournal::new(root.clone());
    assert_eq!(
        journal
            .scan_all(JournalFamily::Chunks, false)?
            .records
            .len(),
        1
    );
    assert_eq!(
        SqliteStore::open(root)?
            .snapshot_ids()?
            .current_chunks
            .len(),
        1
    );
    Ok(())
}

#[test]
fn every_chunk_failure_preserves_event_ack_and_restart_needs_no_new_ingest()
-> Result<(), Box<dyn Error>> {
    let faults = [
        FaultPoint::AfterJournalAppend,
        FaultPoint::AfterJournalSync,
        FaultPoint::AfterRowInsert,
        FaultPoint::AfterCurrentPointerUpdate,
        FaultPoint::AfterWatermarkUpdate,
        FaultPoint::AfterCursorUpdate,
        FaultPoint::BeforeTransactionCommit,
        FaultPoint::AfterTransactionCommit,
    ];
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let due: DateTime<Utc> = "2026-07-13T09:05:30Z".parse()?;
    for fault in faults {
        let (_temporary, root, _, _) = common::store()?;
        let config = ChunkerConfig {
            aggregator_version: format!("restart-{fault:?}"),
            max_cadence_seconds: 30,
        };
        let mut engine = IngestEngine::open_at(root.clone(), config.clone(), due)?;
        for (index, event) in events[..9].iter().enumerate() {
            engine.ingest(
                IngestRequest {
                    event: event.clone(),
                    cadence: cadence("restart-proof", u64::try_from(index + 1)?),
                },
                event.recorded_at,
            )?;
        }
        let last = events[9].clone();
        let outcome = engine.ingest_with_faults(
            IngestRequest {
                event: last.clone(),
                cadence: cadence("restart-proof", 10),
            },
            due,
            FaultInjector::none(),
            FaultInjector::at(fault),
        )?;
        assert_eq!(outcome.acknowledgement, DurableAcknowledgement::Durable);
        assert_eq!(outcome.projection, ProjectionHealth::Lagging);
        assert!(
            StoreQueries::new(SqliteStore::open(root.clone())?)
                .event(&last.event_id, false)?
                .is_some()
        );
        drop(engine);

        let _reopened = IngestEngine::open_at(root.clone(), config, due)?;
        let chunks =
            StoreQueries::new(SqliteStore::open(root)?).current_chunks_in_range(&UtcRange {
                start: "2026-07-13T09:00:00Z".parse()?,
                end: "2026-07-13T09:05:00Z".parse()?,
            })?;
        assert_eq!(chunks.len(), 1, "startup did not recover {fault:?}");
    }
    Ok(())
}

#[test]
fn generic_ingest_rejects_transactional_screenshot_records_before_append()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let events = common::fixture_events("events.jsonl")?;
    let image_event = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                chronicle_domain::EventPayload::ObservationAttempt(attempt)
                    if matches!(
                        &attempt.content,
                        chronicle_domain::ObservationContent::Captured(content)
                            if content.image.is_some()
                    )
            )
        })
        .cloned()
        .ok_or("image observation missing")?;
    let lifecycle = events
        .iter()
        .find(|event| {
            matches!(
                event.payload,
                chronicle_domain::EventPayload::ScreenshotLifecycle(_)
            )
        })
        .cloned()
        .ok_or("lifecycle missing")?;
    let mut engine = IngestEngine::open_at(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "transactional-rejection-1".to_owned(),
            max_cadence_seconds: 30,
        },
        image_event.recorded_at,
    )?;
    assert!(
        engine
            .ingest(
                IngestRequest {
                    event: image_event,
                    cadence: cadence("transactional-rejection", 1),
                },
                "2026-07-13T09:00:16Z".parse()?,
            )
            .is_err()
    );
    assert!(
        engine
            .ingest(
                IngestRequest {
                    event: lifecycle,
                    cadence: None,
                },
                "2026-07-13T09:00:17Z".parse()?,
            )
            .is_err()
    );
    assert!(
        CanonicalJournal::new(root)
            .scan_all(JournalFamily::Events, false)?
            .records
            .is_empty()
    );
    Ok(())
}

#[test]
fn causal_timestamp_violation_is_rejected_before_journal_and_query_projection()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, _, _) = common::store()?;
    let mut event = common::fixture_events("ae4-ten-scheduled-events.jsonl")?.remove(0);
    event.recorded_at = event.observed_at - chrono::Duration::seconds(1);
    let mut engine = IngestEngine::open_at(
        root.clone(),
        ChunkerConfig {
            aggregator_version: "causal-time-1".to_owned(),
            max_cadence_seconds: 30,
        },
        event.observed_at,
    )?;
    assert!(
        engine
            .ingest(
                IngestRequest {
                    event,
                    cadence: cadence("causal-time", 1),
                },
                "2026-07-13T09:00:15Z".parse()?,
            )
            .is_err()
    );
    assert!(
        CanonicalJournal::new(root)
            .scan_all(JournalFamily::Events, false)?
            .records
            .is_empty()
    );
    Ok(())
}

#[test]
fn retry_after_pre_sync_append_uses_same_stamp_and_forces_durability() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, _, _) = common::store()?;
    let event = common::fixture_events("ae4-ten-scheduled-events.jsonl")?.remove(0);
    let config = ChunkerConfig {
        aggregator_version: "pre-sync-retry-1".to_owned(),
        max_cadence_seconds: 30,
    };
    let mut engine = IngestEngine::open_at(root.clone(), config.clone(), event.recorded_at)?;
    assert!(
        engine
            .ingest_with_faults(
                IngestRequest {
                    event: event.clone(),
                    cadence: cadence("pre-sync-retry", 1),
                },
                event.recorded_at,
                FaultInjector::at(FaultPoint::AfterJournalAppend),
                FaultInjector::none(),
            )
            .is_err()
    );
    let retry = engine.ingest(
        IngestRequest {
            event: event.clone(),
            cadence: cadence("pre-sync-retry", 1),
        },
        event.recorded_at,
    )?;
    assert_eq!(retry.acknowledgement, DurableAcknowledgement::Durable);
    let shared = CanonicalJournal::new(root.clone());
    assert_eq!(shared.index_full_scan_count(JournalFamily::Events), 1);
    assert_eq!(
        shared.scan_all(JournalFamily::Events, false)?.records.len(),
        1
    );
    drop(engine);

    let _restarted = IngestEngine::open_at(root.clone(), config, event.recorded_at)?;
    assert!(
        StoreQueries::new(SqliteStore::open(root)?)
            .event(&event.event_id, false)?
            .is_some()
    );
    Ok(())
}
