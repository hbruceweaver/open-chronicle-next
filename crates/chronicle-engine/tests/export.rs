mod common;

use std::error::Error;

use chronicle_domain::{
    ActivityFilter, ClientId, ContentClass, DisclosureGrant, DisclosureLimits, EventId,
    EventPayload, EvidenceState, ExportFormat, ExportPayload, ExportRequest, GrantId, GrantState,
    GrantTimeScope, ObservationContent, QueryOperation, QueryRequest, ReceiptId, RequestId,
    SharedServiceOperation, SharedServiceRequest, SharedServiceResult, UtcRange,
};
use chronicle_engine::{SharedService, SharedServiceError};
use chronicle_store::{CanonicalJournal, FaultInjector};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid timestamp")
}

fn range() -> UtcRange {
    UtcRange {
        start: at("2026-07-13T09:00:00Z"),
        end: at("2026-07-13T09:05:00Z"),
    }
}

fn grant() -> DisclosureGrant {
    DisclosureGrant {
        schema_version: "1.0".to_owned(),
        grant_id: GrantId::new("export-grant").expect("grant ID"),
        client_id: ClientId::new("client-codex").expect("client ID"),
        receipt_id: ReceiptId::new("export-receipt").expect("receipt ID"),
        time_scope: GrantTimeScope::Absolute { range: range() },
        content_classes: vec![
            ContentClass::Metadata,
            ContentClass::Ocr,
            ContentClass::Derived,
        ],
        created_at: at("2026-07-13T08:00:00Z"),
        expires_at: at("2026-07-14T08:00:00Z"),
        state: GrantState::Active,
        limits: DisclosureLimits {
            max_page_items: 50,
            max_response_bytes: 256 * 1024,
            max_cumulative_bytes: 1024 * 1024,
        },
        disclosed_bytes: 0,
        store_generation: 1,
    }
}

#[test]
fn context_packet_is_bounded_manifested_and_path_safe() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant())?;
    let request_id = RequestId::new("context-packet")?;
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: request_id.clone(),
            store_generation: 1,
            operation: SharedServiceOperation::Query(Box::new(QueryRequest {
                schema_version: "1.0".to_owned(),
                request_id,
                client_id: ClientId::new("client-codex")?,
                grant_id: GrantId::new("export-grant")?,
                store_generation: 1,
                operation: QueryOperation::BuildContextPacket {
                    filter: ActivityFilter {
                        range: range(),
                        application_bundle_id: None,
                        window_text: None,
                        authorized_domain: None,
                        evidence_states: vec![EvidenceState::CapturedNew],
                    },
                    include_ocr: true,
                    max_bytes: 32 * 1024,
                },
            })),
        },
        at("2026-07-13T09:05:30Z"),
    )?;
    let encoded = serde_json::to_string(&response)?;
    assert!(encoded.len() <= 32 * 1024);
    assert!(!encoded.contains("managed_relative_path"));
    assert!(!encoded.contains("screenshots/"));
    let SharedServiceResult::Query(query) = response.result else {
        return Err("expected query response".into());
    };
    let chronicle_domain::QueryResult::ContextPacket {
        manifest,
        chunks,
        events,
    } = query.result
    else {
        return Err("expected context packet".into());
    };
    assert_eq!(manifest.included_counts.chunks, chunks.len() as u64);
    assert_eq!(manifest.included_counts.events, events.len() as u64);
    assert!(!manifest.journal_cutoffs.is_empty());
    assert!(!manifest.content_sha256.is_empty());
    Ok(())
}

#[test]
fn context_packet_shrinks_payload_manifest_and_provenance_to_complete_response_bound()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(
        &root,
        &projector,
        &common::fixture_events("ae13-seed-events.jsonl")?,
    )?;
    let original = common::fixture_events("ae13-ten-unchanged-events.jsonl")?;
    common::seed_events(&root, &projector, &original)?;
    let mut clones = Vec::new();
    let mut chunk = common::fixture_chunk("ae13-ten-unchanged-chunk.json")?;
    let template = original.first().ok_or("event fixture missing")?;
    for index in 0..600 {
        let mut event = template.clone();
        event.event_id = EventId::new(format!("context-{index:04}-{}", "x".repeat(130)))?;
        chunk.supporting_event_ids.push(event.event_id.clone());
        clones.push(event);
    }
    common::seed_events(&root, &projector, &clones)?;
    let record = CanonicalJournal::new(root.clone()).append_chunk(&chunk, FaultInjector::none())?;
    projector.project_record(&record, FaultInjector::none())?;

    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant())?;
    let max_bytes = 256 * 1024;
    let request_id = RequestId::new("context-many-small-events")?;
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: request_id.clone(),
            store_generation: 1,
            operation: SharedServiceOperation::Query(Box::new(QueryRequest {
                schema_version: "1.0".to_owned(),
                request_id,
                client_id: ClientId::new("client-codex")?,
                grant_id: GrantId::new("export-grant")?,
                store_generation: 1,
                operation: QueryOperation::BuildContextPacket {
                    filter: ActivityFilter {
                        range: range(),
                        application_bundle_id: None,
                        window_text: None,
                        authorized_domain: None,
                        evidence_states: Vec::new(),
                    },
                    include_ocr: false,
                    max_bytes,
                },
            })),
        },
        at("2026-07-13T09:05:30Z"),
    )?;
    assert!(serde_json::to_vec(&response)?.len() <= max_bytes as usize);
    let SharedServiceResult::Query(query) = response.result else {
        return Err("expected query response".into());
    };
    let chronicle_domain::QueryResult::ContextPacket {
        manifest,
        chunks,
        events,
    } = query.result
    else {
        return Err("expected context packet".into());
    };
    assert!(manifest.truncated);
    assert_eq!(manifest.included_counts.chunks, chunks.len() as u64);
    assert_eq!(manifest.included_counts.events, events.len() as u64);
    assert_eq!(query.provenance.source_event_ids.len(), events.len());
    assert_eq!(
        query.provenance.source_chunk_revision_ids.len(),
        chunks.len()
    );
    Ok(())
}

#[test]
fn stable_cutoff_export_reports_cutoffs_checksums_gaps_and_excludes_screenshots()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant())?;
    let request_id = RequestId::new("stable-export")?;
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: request_id.clone(),
            store_generation: 1,
            operation: SharedServiceOperation::Export(Box::new(ExportRequest {
                schema_version: "1.0".to_owned(),
                request_id,
                client_id: ClientId::new("client-codex")?,
                grant_id: GrantId::new("export-grant")?,
                store_generation: 1,
                range: range(),
                include_ocr: true,
                include_derived: true,
                format: ExportFormat::Json,
                max_bytes: 128 * 1024,
            })),
        },
        at("2026-07-13T09:05:30Z"),
    )?;
    let encoded = serde_json::to_string(&response)?;
    assert!(!encoded.contains("managed_relative_path"));
    assert!(!encoded.contains("screenshots/"));
    let SharedServiceResult::Export(export) = response.result else {
        return Err("expected export response".into());
    };
    assert!(!export.manifest.journal_cutoffs.is_empty());
    assert!(!export.manifest.checksums.is_empty());
    assert!(!export.manifest.coverage.gaps.is_empty());
    assert!(
        export
            .manifest
            .excluded_content_classes
            .contains(&"screenshots".to_owned())
    );
    assert_eq!(export.manifest.stable_cutoff, response.generated_at);
    assert!(encoded.len() <= 128 * 1024);
    Ok(())
}

#[test]
fn markdown_export_uses_indented_untrusted_json_and_fits_complete_response_bound()
-> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let mut events = common::fixture_events("events.jsonl")?;
    let first = events.first_mut().ok_or("event fixture missing")?;
    if let EventPayload::ObservationAttempt(attempt) = &mut first.payload
        && let ObservationContent::Captured(content) = &mut attempt.content
        && let Some(ocr) = &mut content.ocr
    {
        ocr.text
            .push_str(" literal `backticks` remain untrusted data");
    } else {
        return Err("captured OCR fixture missing".into());
    }
    common::seed_events(&root, &projector, &events)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant())?;
    let request_id = RequestId::new("markdown-export")?;
    let max_bytes = 12 * 1024;
    let response = service.execute(
        SharedServiceRequest {
            schema_version: "1.0".to_owned(),
            request_id: request_id.clone(),
            store_generation: 1,
            operation: SharedServiceOperation::Export(Box::new(ExportRequest {
                schema_version: "1.0".to_owned(),
                request_id,
                client_id: ClientId::new("client-codex")?,
                grant_id: GrantId::new("export-grant")?,
                store_generation: 1,
                range: range(),
                include_ocr: true,
                include_derived: false,
                format: ExportFormat::Markdown,
                max_bytes,
            })),
        },
        at("2026-07-13T09:05:30Z"),
    )?;
    assert!(serde_json::to_vec(&response)?.len() <= max_bytes as usize);
    let SharedServiceResult::Export(export) = response.result else {
        return Err("expected export response".into());
    };
    let ExportPayload::Markdown { document } = export.payload else {
        return Err("expected Markdown payload".into());
    };
    assert!(document.contains("`backticks`"));
    assert!(document.lines().any(|line| line.starts_with("    {")));
    assert!(!document.contains("- `{"));
    assert!(export.manifest.truncated);
    Ok(())
}

#[test]
fn tiny_context_and_export_bounds_fail_without_charging() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    common::seed_events(&root, &projector, &common::fixture_events("events.jsonl")?)?;
    common::seed_chunks(&root, &projector)?;
    let service = SharedService::open(root, sqlite)?;
    service.install_grant(grant())?;

    let context_id = RequestId::new("tiny-context")?;
    let context = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: context_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Query(Box::new(QueryRequest {
            schema_version: "1.0".to_owned(),
            request_id: context_id,
            client_id: ClientId::new("client-codex")?,
            grant_id: GrantId::new("export-grant")?,
            store_generation: 1,
            operation: QueryOperation::BuildContextPacket {
                filter: ActivityFilter {
                    range: range(),
                    application_bundle_id: None,
                    window_text: None,
                    authorized_domain: None,
                    evidence_states: Vec::new(),
                },
                include_ocr: false,
                max_bytes: 1,
            },
        })),
    };
    assert!(matches!(
        service.execute(context, at("2026-07-13T09:05:30Z")),
        Err(SharedServiceError::ResponseByteLimit)
    ));

    let export_id = RequestId::new("tiny-export")?;
    let export = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: export_id.clone(),
        store_generation: 1,
        operation: SharedServiceOperation::Export(Box::new(ExportRequest {
            schema_version: "1.0".to_owned(),
            request_id: export_id,
            client_id: ClientId::new("client-codex")?,
            grant_id: GrantId::new("export-grant")?,
            store_generation: 1,
            range: range(),
            include_ocr: false,
            include_derived: false,
            format: ExportFormat::Json,
            max_bytes: 1,
        })),
    };
    assert!(matches!(
        service.execute(export, at("2026-07-13T09:05:30Z")),
        Err(SharedServiceError::ResponseByteLimit)
    ));
    assert_eq!(
        service
            .grant(&GrantId::new("export-grant")?)?
            .disclosed_bytes,
        0
    );
    Ok(())
}
