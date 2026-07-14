mod common;

use std::error::Error;

use chronicle_domain::{DimensionKind, EventEnvelope, PresenceState};
use chronicle_engine::{ChunkBuild, ChunkerConfig, build_chunk};
use chronicle_store::checksum::canonical_json;
use chrono::{DateTime, Utc};
use serde_json::json;

fn build(
    events: &[EventEnvelope],
    version: &str,
) -> chronicle_engine::Result<chronicle_domain::ChunkRevision> {
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse().map_err(|error| {
        chronicle_engine::EngineError::Aggregation(format!("fixture time failed: {error}"))
    })?;
    build_chunk(ChunkBuild {
        events,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &ChunkerConfig {
            aggregator_version: version.to_owned(),
            max_cadence_seconds: 30,
        },
    })?
    .ok_or_else(|| chronicle_engine::EngineError::Aggregation("missing chunk".to_owned()))
}

#[test]
fn ae4_and_ae13_goldens_define_exact_interval_centered_coverage() -> Result<(), Box<dyn Error>> {
    for (events_name, chunk_name, version) in [
        (
            "ae4-ten-scheduled-events.jsonl",
            "ae4-ten-scheduled-chunk.json",
            "synthetic-ae4",
        ),
        (
            "ae13-ten-unchanged-events.jsonl",
            "ae13-ten-unchanged-chunk.json",
            "synthetic-ae13",
        ),
    ] {
        let events = common::fixture_events(events_name)?;
        let expected = common::fixture_chunk(chunk_name)?;
        let actual = build(&events, version)?;
        assert_eq!(actual.evidence_seconds, expected.evidence_seconds);
        assert_eq!(actual.presence_seconds, expected.presence_seconds);
        assert_eq!(actual.supporting_event_ids, expected.supporting_event_ids);
        let application = actual
            .duration_estimates
            .iter()
            .find(|estimate| estimate.dimension == DimensionKind::Application)
            .ok_or("application estimate missing")?;
        assert_eq!(application.estimated_seconds, 300);
        assert_eq!(
            application.supporting_event_ids,
            expected.supporting_event_ids
        );

        let repeated = build(&events, version)?;
        assert_eq!(canonical_json(&actual)?, canonical_json(&repeated)?);
    }
    Ok(())
}

#[test]
fn protected_gap_and_idle_states_stay_factual_and_separate() -> Result<(), Box<dyn Error>> {
    let source = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let mut protected = Vec::new();
    for event in &source {
        let mut value = serde_json::to_value(event)?;
        value["payload"]["data"]["attempt_status"] = json!("skipped");
        value["payload"]["data"]["evidence_state"] = json!("protected");
        value["payload"]["data"]["ocr_state"] = json!("not-run");
        value["payload"]["data"]["content"] = json!({
            "type": "protected",
            "data": {"reason": "application-excluded", "privacy_policy_version": "test-1"}
        });
        protected.push(EventEnvelope::parse(&serde_json::to_string(&value)?)?);
    }
    let chunk = build(&protected, "protected-1")?;
    assert_eq!(chunk.evidence_seconds.protected, 300);
    assert_eq!(chunk.presence_seconds.total(), 0);
    assert!(chunk.duration_estimates.is_empty());

    let mut mixed = Vec::new();
    for (index, event) in source.iter().enumerate() {
        let mut value = serde_json::to_value(event)?;
        if index % 2 == 1 {
            value["payload"]["data"]["presence_state"] = json!("idle");
            value["payload"]["data"]["idle_seconds"] = json!(60);
        }
        mixed.push(EventEnvelope::parse(&serde_json::to_string(&value)?)?);
    }
    let chunk = build(&mixed, "mixed-idle-1")?;
    assert_eq!(chunk.presence_seconds.active, 150);
    assert_eq!(chunk.presence_seconds.idle, 150);
    assert_eq!(
        chunk
            .duration_estimates
            .iter()
            .filter(|estimate| estimate.dimension == DimensionKind::Application)
            .map(|estimate| estimate.estimated_seconds)
            .sum::<u32>(),
        150,
        "idle coverage must not count as application duration"
    );
    assert!(
        chunk
            .duration_estimates
            .iter()
            .all(|estimate| { estimate.dimension != DimensionKind::AuthorizedDomain })
    );
    assert_eq!(mixed[1].payload.clone(), mixed[1].payload);
    let _ = PresenceState::Idle;
    Ok(())
}

#[test]
fn prompt_like_ocr_is_bounded_inert_evidence() -> Result<(), Box<dyn Error>> {
    let events = common::fixture_events("events.jsonl")?;
    let chunk = build(&events, "prompt-inert-1")?;
    let extract = chunk
        .ocr_extracts
        .iter()
        .find(|extract| extract.text.contains("ignore previous instructions"))
        .ok_or("prompt-like evidence missing")?;
    assert!(extract.untrusted_evidence == chronicle_domain::UntrustedEvidenceMarker);
    assert!(extract.text.chars().count() <= 512);
    Ok(())
}

#[test]
fn all_gap_and_permission_outage_are_never_reported_as_inactivity() -> Result<(), Box<dyn Error>> {
    let gap = common::fixture_events("events.jsonl")?
        .into_iter()
        .find(|event| {
            matches!(
                event.payload,
                chronicle_domain::EventPayload::RecordingGap(_)
            )
        })
        .ok_or("gap fixture missing")?;
    let all_gap = build(std::slice::from_ref(&gap), "all-gap-1")?;
    assert_eq!(all_gap.evidence_seconds.gap, 300);
    assert_eq!(all_gap.presence_seconds.total(), 0);
    assert!(all_gap.duration_estimates.is_empty());

    let mut permission = gap;
    permission.event_id = chronicle_domain::EventId::new("permission-gap-3m")?;
    permission.observed_at = "2026-07-13T09:04:00Z".parse()?;
    permission.recorded_at = "2026-07-13T09:04:01Z".parse()?;
    let chronicle_domain::EventPayload::RecordingGap(payload) = &mut permission.payload else {
        return Err("fixture is not a gap".into());
    };
    payload.start = "2026-07-13T09:01:00Z".parse()?;
    payload.end = "2026-07-13T09:04:00Z".parse()?;
    payload.reason = chronicle_domain::GapReason::PermissionLoss;
    let unavailable = build(&[permission], "permission-gap-1")?;
    assert_eq!(unavailable.evidence_seconds.unavailable, 180);
    assert_eq!(unavailable.evidence_seconds.gap, 120);
    assert_eq!(unavailable.presence_seconds.total(), 0);
    assert!(unavailable.duration_estimates.is_empty());
    Ok(())
}

#[test]
fn scheduled_centers_absorb_observation_jitter_at_bucket_edges() -> Result<(), Box<dyn Error>> {
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    for (index, event) in events.iter_mut().enumerate() {
        let jitter = if index == 0 {
            7
        } else if index == 9 {
            6
        } else if index % 2 == 0 {
            4
        } else {
            3
        };
        event.observed_at += chrono::Duration::seconds(jitter);
        event.recorded_at = event.observed_at + chrono::Duration::seconds(1);
        event.validate()?;
    }
    let chunk = build(&events, "scheduled-jitter-1")?;
    assert_eq!(chunk.evidence_seconds.captured, 300);
    assert_eq!(chunk.evidence_seconds.gap, 0);
    assert_eq!(chunk.presence_seconds.active, 300);
    Ok(())
}
