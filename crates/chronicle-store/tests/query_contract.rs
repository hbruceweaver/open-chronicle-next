mod common;

use std::error::Error;

use chronicle_domain::{ActivityFilter, ChunkRevision, EventEnvelope, QueryResponse, UtcRange};
use chronicle_store::{CanonicalJournal, FaultInjector, StoreQueries};

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
