mod common;

use chronicle_domain::{ActivityFilter, EventId, ExportCounts, QueryEventPayload, UtcRange};
use chronicle_store::{
    ArtifactStore, CanonicalJournal, FaultInjector, RecoveryManager, StableExportBuilder,
    StoreQueries,
};

fn range(start: &str, end: &str) -> chronicle_store::Result<UtcRange> {
    Ok(UtcRange {
        start: start.parse().map_err(|error: chrono::ParseError| {
            chronicle_store::StoreError::InvalidPath(error.to_string())
        })?,
        end: end.parse().map_err(|error: chrono::ParseError| {
            chronicle_store::StoreError::InvalidPath(error.to_string())
        })?,
    })
}

#[test]
fn stable_snapshot_excludes_projection_writes_after_its_cutoff() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let export_range = range("2026-07-13T09:00:00Z", "2026-07-13T10:00:00Z")?;
    let pinned = StableExportBuilder::new(StoreQueries::new(sqlite.clone()))?;

    let mut later = common::events()?.get(3).cloned().ok_or_else(|| {
        chronicle_store::StoreError::InvalidPath("event fixture missing".to_owned())
    })?;
    later.event_id = EventId::new("evt-after-export-cutoff")
        .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?;
    let shift = chrono::Duration::minutes(30);
    later.scheduled_at = later.scheduled_at.map(|timestamp| timestamp + shift);
    later.observed_at += shift;
    later.recorded_at += shift;
    let record = CanonicalJournal::new(root).append_event(&later, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    let pinned_selection = pinned.full_export(&export_range, false, false, u64::MAX)?;
    assert!(
        !pinned_selection
            .events
            .iter()
            .any(|event| event.event_id == later.event_id)
    );
    assert!(
        pinned_selection
            .journal_cutoffs
            .iter()
            .all(|cutoff| cutoff.byte_offset < record.end_offset() || cutoff.family != "events")
    );

    let current = StableExportBuilder::new(StoreQueries::new(sqlite))?.full_export(
        &export_range,
        false,
        false,
        u64::MAX,
    )?;
    assert!(
        current
            .events
            .iter()
            .any(|event| event.event_id == later.event_id)
    );
    assert!(
        current
            .journal_cutoffs
            .iter()
            .any(|cutoff| cutoff.family == "events" && cutoff.byte_offset == record.end_offset())
    );
    Ok(())
}

#[test]
fn export_selection_is_identical_after_projection_rebuild() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let export_range = range("2026-07-13T09:00:00Z", "2026-07-13T09:10:00Z")?;
    let before = StableExportBuilder::new(StoreQueries::new(sqlite.clone()))?.full_export(
        &export_range,
        true,
        true,
        u64::MAX,
    )?;

    let (_report, rebuilt) = RecoveryManager::new(root).rebuild_index()?;
    assert_eq!(sqlite.snapshot_ids()?, rebuilt);
    let after = StableExportBuilder::new(StoreQueries::new(sqlite))?.full_export(
        &export_range,
        true,
        true,
        u64::MAX,
    )?;

    assert_eq!(before, after);
    assert_eq!(after.available_counts.artifacts, 1);
    Ok(())
}

#[test]
fn exact_range_containment_excludes_overlapping_chunks_and_out_of_range_evidence()
-> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let exact_range = range("2026-07-13T09:01:00Z", "2026-07-13T09:02:00Z")?;
    let builder = StableExportBuilder::new(StoreQueries::new(sqlite))?;
    let packet = builder.context_packet(
        &ActivityFilter {
            range: exact_range.clone(),
            application_bundle_id: None,
            window_text: None,
            authorized_domain: None,
            evidence_states: Vec::new(),
        },
        true,
        u64::MAX,
    )?;
    assert_eq!(packet.available_counts, ExportCounts::default());
    assert!(packet.events.is_empty());
    assert!(packet.chunks.is_empty());

    let export = builder.full_export(&exact_range, true, true, u64::MAX)?;
    assert!(export.chunks.is_empty());
    assert!(export.artifacts.is_empty());
    assert!(!export.events.is_empty());
    for event in export.events {
        assert!(event.observed_at >= exact_range.start && event.observed_at < exact_range.end);
        if let Some(scheduled_at) = event.scheduled_at {
            assert!(scheduled_at >= exact_range.start && scheduled_at < exact_range.end);
        }
        if let QueryEventPayload::RecordingGap(gap) = event.payload {
            assert!(gap.start >= exact_range.start && gap.end <= exact_range.end);
        }
    }
    Ok(())
}

#[test]
fn artifact_page_cursor_is_stable_when_anchor_gets_a_new_revision() -> chronicle_store::Result<()> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let store = ArtifactStore::new(root, projector);
    for suffix in ["a", "b"] {
        let mut artifact = common::artifact()?;
        artifact.artifact_id = chronicle_domain::ArtifactId::new(format!("page-{suffix}"))
            .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?;
        artifact.revision_id =
            chronicle_domain::ArtifactRevisionId::new(format!("page-{suffix}-revision"))
                .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?;
        artifact.created_at =
            "2026-07-13T09:07:00Z"
                .parse()
                .map_err(|error: chrono::ParseError| {
                    chronicle_store::StoreError::InvalidPath(error.to_string())
                })?;
        store.write_revision(&artifact, FaultInjector::none())?;
    }
    let query_range = range("2026-07-13T09:00:00Z", "2026-07-13T09:10:00Z")?;
    let queries = StoreQueries::new(sqlite);
    let (first, truncated) = queries.current_artifact_page(&query_range, None, 1)?;
    assert!(truncated);
    assert_eq!(first[0].artifact_id.as_str(), "artifact-hypothesis-001");

    let mut revised = common::artifact()?;
    revised.revision_id =
        chronicle_domain::ArtifactRevisionId::new("artifact-anchor-revision-2")
            .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?;
    revised.prior_revision_id = Some(
        chronicle_domain::ArtifactRevisionId::new("artifact-revision-001")
            .map_err(|error| chronicle_store::StoreError::InvalidPath(error.to_string()))?,
    );
    revised.expected_prior_revision_id = revised.prior_revision_id.clone();
    revised.created_at = "2026-07-13T09:09:00Z"
        .parse()
        .map_err(|error: chrono::ParseError| {
            chronicle_store::StoreError::InvalidPath(error.to_string())
        })?;
    store.write_revision(&revised, FaultInjector::none())?;

    let (second, _) =
        queries.current_artifact_page(&query_range, Some(first[0].artifact_id.as_str()), 1)?;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].artifact_id.as_str(), "page-a");
    Ok(())
}
