mod common;

use std::error::Error;

use chronicle_engine::{ChunkBuild, ChunkerConfig, build_chunk};
use chronicle_store::{CanonicalJournal, FaultInjector, checksum::canonical_json};
use chrono::{DateTime, Utc};

#[test]
fn late_input_creates_an_immutable_superseding_revision() -> Result<(), Box<dyn Error>> {
    let mut events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let late = events.pop().ok_or("fixture missing last event")?;
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    let config = ChunkerConfig {
        aggregator_version: "late-test-1".to_owned(),
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
    .ok_or("initial chunk missing")?;
    let first_bytes = canonical_json(&first)?;

    let mut late_value = serde_json::to_value(late)?;
    late_value["recorded_at"] = serde_json::json!("2026-07-13T09:06:00Z");
    events.push(chronicle_domain::EventEnvelope::parse(
        &serde_json::to_string(&late_value)?,
    )?);
    let second = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: Some(&first),
        store_generation: 1,
        revision_generated_at: None,
        config: &config,
    })?
    .ok_or("late chunk missing")?;
    assert_ne!(second.revision_id, first.revision_id);
    assert_eq!(second.prior_revision_id.as_ref(), Some(&first.revision_id));
    assert_eq!(
        second.supersedes_revision_id.as_ref(),
        Some(&first.revision_id)
    );
    assert!(second.late_input);
    let reconciled_at: DateTime<Utc> = "2026-07-13T09:06:00Z".parse()?;
    assert!(second.generated_at <= reconciled_at);
    assert_eq!(canonical_json(&first)?, first_bytes, "old bytes mutated");

    let repeated = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: Some(&first),
        store_generation: 1,
        revision_generated_at: None,
        config: &config,
    })?
    .ok_or("repeated late chunk missing")?;
    assert_eq!(canonical_json(&second)?, canonical_json(&repeated)?);
    Ok(())
}

#[test]
fn algorithm_only_revision_is_not_labeled_late() -> Result<(), Box<dyn Error>> {
    let events = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    let v1 = ChunkerConfig {
        aggregator_version: "algorithm-v1".to_owned(),
        max_cadence_seconds: 30,
    };
    let first = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &v1,
    })?
    .ok_or("v1 chunk missing")?;
    let v2 = ChunkerConfig {
        aggregator_version: "algorithm-v2".to_owned(),
        max_cadence_seconds: 30,
    };
    let upgraded = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: Some(&first),
        store_generation: 1,
        revision_generated_at: Some("2026-07-14T10:00:00Z".parse()?),
        config: &v2,
    })?
    .ok_or("v2 chunk missing")?;
    assert_ne!(upgraded.revision_id, first.revision_id);
    assert_eq!(
        upgraded.prior_revision_id.as_ref(),
        Some(&first.revision_id)
    );
    assert!(!upgraded.late_input);
    assert_eq!(
        upgraded.generated_at,
        "2026-07-14T10:00:00Z".parse::<DateTime<Utc>>()?
    );
    let repeated = build_chunk(ChunkBuild {
        events: &events,
        bucket_start: start,
        prior: Some(&first),
        store_generation: 1,
        revision_generated_at: Some("2026-07-14T10:00:00Z".parse()?),
        config: &v2,
    })?
    .ok_or("repeated v2 chunk missing")?;
    assert_eq!(canonical_json(&upgraded)?, canonical_json(&repeated)?);
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let record = CanonicalJournal::new(root).append_chunk(&upgraded, FaultInjector::none())?;
    assert!(record.shard().starts_with("2026-07-14"));
    Ok(())
}
