mod common;

use std::error::Error;

use chronicle_domain::{
    ActivityFilter, ChunkRevision, EventEnvelope, EvidenceState, QueryResponse, UtcRange,
};
use chronicle_store::{CanonicalJournal, FaultInjector, StoreError, StoreQueries};

#[test]
fn typed_queries_return_current_facts_and_matching_evidence_ids() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = read_events("ae4-ten-scheduled-events.jsonl")?;
    let journal = CanonicalJournal::new(root);
    for event in &events {
        let record = journal.append_event(event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let chunk = read_chunk("ae4-ten-scheduled-chunk.json")?;
    let record = journal.append_chunk(&chunk, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    let queries = StoreQueries::new(sqlite);
    let range = UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:05:00Z".parse()?,
    };
    let returned_events = queries.events_in_range(&range)?;
    assert_eq!(returned_events.len(), 10);
    let chunks = queries.current_chunks_in_range(&range)?;
    assert_eq!(chunks, vec![chunk.clone()]);
    let supporting = queries.supporting_events(&chunk.chunk_id, false)?;
    assert_eq!(
        supporting
            .iter()
            .map(|event| &event.event_id)
            .collect::<Vec<_>>(),
        chunk.supporting_event_ids.iter().collect::<Vec<_>>()
    );
    assert!(chunks[0]
        .duration_estimates
        .iter()
        .all(|estimate| estimate.dimension != chronicle_domain::DimensionKind::AuthorizedDomain));
    Ok(())
}

#[test]
fn query_projection_omits_managed_paths_and_can_redact_ocr_payload() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let source = read_events("events.jsonl")?;
    let event = source.first().cloned().ok_or("fixture missing")?;
    let lifecycle = source.get(1).cloned().ok_or("lifecycle fixture missing")?;
    let journal = CanonicalJournal::new(root);
    for record_event in [&event, &lifecycle] {
        let record = journal.append_event(record_event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let query = StoreQueries::new(sqlite)
        .event(&event.event_id, false)?
        .ok_or("event query missing")?;
    let json = serde_json::to_string(&query)?;
    assert!(!json.contains("managed_relative_path"));
    assert!(!json.contains("ignore previous instructions"));
    assert!(!json.contains("screenshots/"));
    assert!(json.contains("2026-07-14T09:00:16Z"));

    let packet: serde_json::Value = serde_json::from_str(&fixture("queries.json")?)?;
    let mut response = packet["exchanges"][0]["response"].clone();
    response["result"]["data"]["events"][0]["payload"]["data"]["content"]["data"]["ocr"] =
        serde_json::Value::Null;
    QueryResponse::parse(&serde_json::to_string(&response)?)?;
    Ok(())
}

#[test]
fn shared_chunk_pages_apply_keyset_and_limit_before_materialization() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let chunks = common::chunks()?;
    let mut later = chunks.last().cloned().ok_or("chunk fixture missing")?;
    let shift = chrono::Duration::minutes(5);
    later.chunk_id = chronicle_domain::ChunkId::new("chunk-page-later")?;
    later.revision_id = chronicle_domain::ChunkRevisionId::new("chunk-page-later-rev")?;
    later.prior_revision_id = None;
    later.supersedes_revision_id = None;
    later.window.start += shift;
    later.window.end += shift;
    later.generated_at += shift;
    later.input_digest = "chunk-page-later-input".to_owned();
    for gap in &mut later.gaps {
        gap.start += shift;
        gap.end += shift;
    }
    for transition in &mut later.transitions {
        transition.at += shift;
    }
    let record = CanonicalJournal::new(root).append_chunk(&later, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;
    let range = UtcRange {
        start: chunks
            .iter()
            .map(|chunk| chunk.window.start)
            .min()
            .ok_or("chunk fixture missing")?,
        end: chunks
            .iter()
            .map(|chunk| chunk.window.end)
            .max()
            .ok_or("chunk fixture missing")?
            + shift,
    };
    let queries = StoreQueries::new(sqlite).snapshot()?;
    let filter = ActivityFilter {
        range,
        application_bundle_id: None,
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    };
    let (first, truncated) = queries.current_chunk_page(&filter, None, 1)?;
    assert_eq!(first.len(), 1);
    assert!(truncated);
    let (second, _) = queries.current_chunk_page(&filter, Some(first[0].chunk_id.as_str()), 1)?;
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].chunk_id, second[0].chunk_id);
    Ok(())
}

#[test]
fn stable_high_water_excludes_chunk_revisions_projected_after_snapshot()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let journal = CanonicalJournal::new(root);
    for event in common::events()? {
        let record = journal.append_event(&event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let chunks = common::chunks()?;
    let first = chunks.first().cloned().ok_or("first chunk missing")?;
    let later = chunks.last().cloned().ok_or("later chunk missing")?;
    let record = journal.append_chunk(&first, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    let snapshot_queries = StoreQueries::new(sqlite.clone()).snapshot()?;
    let high_water = snapshot_queries.projection_high_water()?;
    let record = journal.append_chunk(&later, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    let range = UtcRange {
        start: first.window.start,
        end: first.window.end,
    };
    let cutoff = "2026-07-13T09:07:00Z".parse()?;
    let stable =
        StoreQueries::new(sqlite.clone()).chunks_in_range_at_cutoff(&range, cutoff, high_water)?;
    assert_eq!(stable, vec![first.clone()]);
    assert!(
        StoreQueries::new(sqlite.clone())
            .chunk_revision_at_snapshot(&later.revision_id, cutoff, high_water)?
            .is_none()
    );
    assert_eq!(
        StoreQueries::new(sqlite.clone())
            .current_chunk(&later.chunk_id)?
            .ok_or("current chunk missing")?,
        later
    );
    let filter = ActivityFilter {
        range,
        application_bundle_id: None,
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    };
    let (page, truncated) = StoreQueries::new(sqlite)
        .chunk_page_at_cutoff(&filter, cutoff, high_water, false, None, 10)?;
    assert!(!truncated);
    assert_eq!(page, vec![first]);
    Ok(())
}

#[test]
fn stable_chunk_filters_correlate_dimensions_and_evidence_on_one_observation()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let queries = StoreQueries::new(sqlite).snapshot()?;
    let high_water = queries.projection_high_water()?;
    let range = UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:05:00Z".parse()?,
    };
    let cutoff = "2026-07-13T09:07:00Z".parse()?;

    // The chunk has writer observations and a different protected observation,
    // but no single observation is both writer and protected.
    let crossed = ActivityFilter {
        range: range.clone(),
        application_bundle_id: Some("com.example.writer".to_owned()),
        window_text: None,
        authorized_domain: None,
        evidence_states: vec![EvidenceState::Protected],
    };
    let (page, _) = queries.chunk_page_at_cutoff(&crossed, cutoff, high_water, false, None, 10)?;
    assert!(page.is_empty());

    // Missing-observation is a chunk gap branch, while the selected dimension
    // must still be present on some supporting observation.
    let missing_writer = ActivityFilter {
        range: range.clone(),
        application_bundle_id: Some("com.example.writer".to_owned()),
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    };
    let (page, _) =
        queries.chunk_page_at_cutoff(&missing_writer, cutoff, high_water, true, None, 10)?;
    assert_eq!(page.len(), 1);
    let missing_unrelated = ActivityFilter {
        range,
        application_bundle_id: Some("com.example.does-not-exist".to_owned()),
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    };
    let (page, _) =
        queries.chunk_page_at_cutoff(&missing_unrelated, cutoff, high_water, true, None, 10)?;
    assert!(page.is_empty());
    Ok(())
}

#[test]
fn stable_chunk_and_artifact_cursors_must_belong_to_the_exact_query_scope()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_canonical(&root, &projector)?;
    let queries = StoreQueries::new(sqlite).snapshot()?;
    let high_water = queries.projection_high_water()?;
    let range = UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:05:00Z".parse()?,
    };
    let cutoff = "2026-07-13T09:10:00Z".parse()?;

    let excluded_chunk_filter = ActivityFilter {
        range: range.clone(),
        application_bundle_id: Some("com.example.does-not-exist".to_owned()),
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    };
    let chunk_cursor = common::chunks()?
        .last()
        .ok_or("chunk fixture missing")?
        .chunk_id
        .to_string();
    let chunk_error = queries
        .chunk_page_at_cutoff(
            &excluded_chunk_filter,
            cutoff,
            high_water,
            false,
            Some(&chunk_cursor),
            10,
        )
        .expect_err("out-of-filter chunk cursor must fail");
    assert!(matches!(chunk_error, StoreError::CursorScopeMismatch));

    let artifact_cursor = common::artifact()?.artifact_id.to_string();
    let artifact_error = queries
        .artifact_page_at_cutoff(&range, cutoff, high_water, Some(&artifact_cursor), 10)
        .expect_err("out-of-range artifact cursor must fail");
    assert!(matches!(artifact_error, StoreError::CursorScopeMismatch));
    Ok(())
}

fn fixture(name: &str) -> Result<String, Box<dyn Error>> {
    Ok(std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1")
            .join(name),
    )?)
}

fn read_events(name: &str) -> Result<Vec<EventEnvelope>, Box<dyn Error>> {
    fixture(name)?
        .lines()
        .map(|line| EventEnvelope::parse(line).map_err(Into::into))
        .collect()
}

fn read_chunk(name: &str) -> Result<ChunkRevision, Box<dyn Error>> {
    Ok(ChunkRevision::parse(fixture(name)?.trim())?)
}
