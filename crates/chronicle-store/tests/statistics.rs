mod common;

use std::error::Error;

use chronicle_domain::{ChunkRevision, DimensionKind, UtcRange};
use chronicle_store::{CanonicalJournal, FactualStatistics, FaultInjector, StoreQueries};
use chrono::Duration;

#[test]
fn statistics_are_typed_factual_and_fill_unobserved_buckets_as_gaps() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1/ae4-ten-scheduled-chunk.json"),
    )?;
    let chunk = ChunkRevision::parse(text.trim())?;
    let record = CanonicalJournal::new(root).append_chunk(&chunk, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;
    let report = FactualStatistics::new(StoreQueries::new(sqlite)).range(&UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:10:00Z".parse()?,
    })?;
    assert_eq!(report.coverage.evidence_seconds.captured, 300);
    assert_eq!(report.coverage.evidence_seconds.gap, 300);
    assert_eq!(report.coverage.presence_seconds.active, 300);
    let application = report
        .factual_totals
        .iter()
        .find(|total| total.dimension == DimensionKind::Application)
        .ok_or("application total missing")?;
    assert_eq!(application.estimated_seconds, 300);
    assert_eq!(application.supporting_chunk_ids, vec![chunk.chunk_id]);
    assert!(
        report
            .factual_totals
            .iter()
            .all(|total| total.dimension != DimensionKind::AuthorizedDomain)
    );
    assert_eq!(report.coverage.gaps.len(), 1);
    Ok(())
}

#[test]
fn statistics_reject_over_budget_ranges_and_coalesce_missing_buckets() -> Result<(), Box<dyn Error>>
{
    let (_temporary, _root, sqlite, _projector) = common::store()?;
    let statistics = FactualStatistics::new(StoreQueries::new(sqlite));
    let start = "2026-01-01T00:00:00Z".parse()?;
    let coalesced = statistics.range(&UtcRange {
        start,
        end: start + Duration::minutes(15),
    })?;
    assert_eq!(coalesced.coverage.gaps.len(), 1);
    assert_eq!(coalesced.coverage.gaps[0].start, start);
    assert_eq!(
        coalesced.coverage.gaps[0].end,
        start + Duration::minutes(15)
    );

    assert!(
        statistics
            .range(&UtcRange {
                start,
                end: start + Duration::days(366) + Duration::minutes(5),
            })
            .is_err()
    );
    Ok(())
}
