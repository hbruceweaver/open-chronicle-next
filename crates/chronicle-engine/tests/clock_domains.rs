mod common;

use std::error::Error;

use chronicle_engine::{
    CadenceGuard, CadenceStamp, ChunkBuild, ChunkerConfig, build_chunk, chunk_id,
};
use chrono::{DateTime, Utc};

#[test]
fn utc_identity_survives_display_timezone_dst_and_travel() -> Result<(), Box<dyn Error>> {
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    let config = ChunkerConfig {
        aggregator_version: "clock-test-1".to_owned(),
        max_cadence_seconds: 30,
    };
    let first = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &config,
    })?
    .ok_or("chunk missing")?;
    let mut travelled = events.clone();
    for event in &mut travelled {
        event.display_timezone = "America/Los_Angeles".to_owned();
    }
    let after_travel = build_chunk(ChunkBuild {
        events: &travelled,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &config,
    })?
    .ok_or("travelled chunk missing")?;
    assert_eq!(first.chunk_id, after_travel.chunk_id);
    assert_ne!(first.display_timezone, after_travel.display_timezone);

    for utc_boundary in [
        "2026-03-29T00:00:00Z",
        "2026-10-25T00:00:00Z",
        "2026-07-13T22:00:00Z",
    ] {
        let boundary: DateTime<Utc> = utc_boundary.parse()?;
        assert_eq!(boundary.timestamp().rem_euclid(300), 0);
        let delta = boundary - start;
        let mut shifted = events.clone();
        for event in &mut shifted {
            event.scheduled_at = event.scheduled_at.map(|at| at + delta);
            event.observed_at += delta;
            event.recorded_at += delta;
        }
        let shifted_chunk = build_chunk(ChunkBuild {
            events: &shifted,
            bucket_start: boundary,
            prior: None,
            store_generation: 1,
            revision_generated_at: None,
            config: &config,
        })?
        .ok_or("shifted chunk missing")?;
        assert_eq!(shifted_chunk.evidence_seconds.captured, 300);
        assert_eq!(
            shifted_chunk.chunk_id,
            chunk_id(&events[0].device_id, boundary)?
        );
    }
    Ok(())
}

#[test]
fn monotonic_cadence_is_independent_of_wall_clock_rollback_and_boot_change()
-> Result<(), Box<dyn Error>> {
    let mut guard = CadenceGuard::default();
    guard.observe(&CadenceStamp {
        boot_sequence: "boot-a".to_owned(),
        monotonic_tick: 100,
    })?;
    guard.observe(&CadenceStamp {
        boot_sequence: "boot-a".to_owned(),
        monotonic_tick: 101,
    })?;
    assert!(
        guard
            .observe(&CadenceStamp {
                boot_sequence: "boot-a".to_owned(),
                monotonic_tick: 101,
            })
            .is_err()
    );
    guard.observe(&CadenceStamp {
        boot_sequence: "boot-b".to_owned(),
        monotonic_tick: 1,
    })?;
    assert!(
        guard
            .observe(&CadenceStamp {
                boot_sequence: "boot-a".to_owned(),
                monotonic_tick: 102,
            })
            .is_err(),
        "a retired boot sequence must not become active again"
    );
    Ok(())
}

#[test]
fn cadence_change_keeps_utc_partition_and_uses_max_finalize_delay() -> Result<(), Box<dyn Error>> {
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    for event in &mut events[5..] {
        let chronicle_domain::EventPayload::ObservationAttempt(attempt) = &mut event.payload else {
            return Err("fixture contains non-attempt".into());
        };
        attempt.cadence_seconds = 60;
    }
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    let chunk = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &ChunkerConfig {
            aggregator_version: "cadence-change-1".to_owned(),
            max_cadence_seconds: 60,
        },
    })?
    .ok_or("chunk missing")?;
    assert_eq!(chunk.window.start, start);
    assert_eq!(
        chunk.window.end.timestamp() - chunk.window.start.timestamp(),
        300
    );
    assert_eq!(chunk.finalization_cadence_seconds, 60);
    assert!(chunk.generated_at >= chunk.window.end + chrono::Duration::seconds(60));
    let protected = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &ChunkerConfig {
            aggregator_version: "cadence-change-1".to_owned(),
            max_cadence_seconds: 30,
        },
    })?
    .ok_or("protected cadence chunk missing")?;
    assert_eq!(protected.finalization_cadence_seconds, 60);
    Ok(())
}
