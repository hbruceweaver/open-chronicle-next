mod common;

use std::error::Error;

use chronicle_domain::DimensionKind;
use chronicle_engine::{ChunkBuild, ChunkerConfig, build_chunk};
use chrono::{DateTime, Utc};

#[test]
fn sparse_samples_are_capped_and_never_bridge_missing_time() -> Result<(), Box<dyn Error>> {
    let all = common::fixture_events("ae4-ten-scheduled-events.jsonl")?;
    let sparse = vec![all[0].clone(), all[5].clone()];
    let start: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    let chunk = build_chunk(ChunkBuild {
        events: &sparse,
        bucket_start: start,
        prior: None,
        store_generation: 1,
        revision_generated_at: None,
        config: &ChunkerConfig {
            aggregator_version: "duration-test-1".to_owned(),
            max_cadence_seconds: 30,
        },
    })?
    .ok_or("chunk missing")?;
    assert_eq!(chunk.evidence_seconds.captured, 67);
    assert_eq!(chunk.evidence_seconds.gap, 233);
    let application_seconds = chunk
        .duration_estimates
        .iter()
        .filter(|estimate| estimate.dimension == DimensionKind::Application)
        .map(|estimate| estimate.estimated_seconds)
        .sum::<u32>();
    assert_eq!(application_seconds, 67);
    assert!(application_seconds <= chunk.evidence_seconds.captured);
    Ok(())
}
