mod common;

use std::error::Error;

use chronicle_domain::{
    ActivityFilter, EventEnvelope, EventId, EventPayload, EvidenceState, ObservationContent,
    PageRequest, UtcRange,
};
use chronicle_store::{ActivitySearch, CanonicalJournal, FaultInjector, StoreError, StoreQueries};
use chrono::{DateTime, Duration, Utc};

#[test]
fn fts_treats_quotes_operators_and_prompt_text_as_literal_evidence() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = search_events()?;
    let journal = CanonicalJournal::new(root);
    for event in &events {
        let record = journal.append_event(event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let search = ActivitySearch::new(sqlite);
    let filter = filter()?;
    let unicode = search.search(
        &filter,
        "café 日本語",
        true,
        &PageRequest {
            cursor: None,
            limit: 10,
        },
    )?;
    assert_eq!(unicode.hits.len(), 1);
    let snippet = unicode.hits[0]
        .snippet
        .as_ref()
        .ok_or("Unicode search omitted its structured snippet")?;
    let highlighted = snippet
        .highlights
        .iter()
        .map(|range| {
            snippet
                .text
                .chars()
                .skip(usize::try_from(range.start).unwrap_or(usize::MAX))
                .take(usize::try_from(range.length).unwrap_or_default())
                .collect::<String>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        highlighted,
        ["café".to_owned(), "日本語".to_owned()]
            .into_iter()
            .collect()
    );
    assert!(
        unicode.hits[0]
            .highlight_html
            .as_ref()
            .is_some_and(|highlight| highlight.contains("<mark>café</mark>"))
    );

    let injected = search.search(
        &filter,
        "café\" OR synthetic",
        true,
        &PageRequest {
            cursor: None,
            limit: 10,
        },
    )?;
    assert!(
        injected.hits.is_empty(),
        "FTS operator widened a literal query"
    );
    let cursor_error = search
        .search(
            &filter,
            "café",
            true,
            &PageRequest {
                cursor: Some("fts-event-1".to_owned()),
                limit: 10,
            },
        )
        .expect_err("out-of-query event cursor must fail");
    assert!(matches!(cursor_error, StoreError::CursorScopeMismatch));
    let empty = search.search(
        &filter,
        "\"\" ()",
        true,
        &PageRequest {
            cursor: None,
            limit: 10,
        },
    )?;
    assert!(empty.hits.is_empty());
    Ok(())
}

#[test]
fn fts_paginates_stably_and_bounds_escaped_highlights() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = search_events()?;
    let journal = CanonicalJournal::new(root);
    for event in &events {
        let record = journal.append_event(event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let search = ActivitySearch::new(sqlite);
    let filter = filter()?;
    let first = search.search(
        &filter,
        "Synthetic",
        true,
        &PageRequest {
            cursor: None,
            limit: 2,
        },
    )?;
    assert_eq!(first.hits.len(), 2);
    assert!(first.page.truncated);
    let second = search.search(
        &filter,
        "Synthetic",
        true,
        &PageRequest {
            cursor: first.page.next_cursor.clone(),
            limit: 2,
        },
    )?;
    assert_eq!(second.hits.len(), 2);
    let first_ids = first
        .hits
        .iter()
        .map(|hit| hit.event.event_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let second_ids = second
        .hits
        .iter()
        .map(|hit| hit.event.event_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(first_ids.is_disjoint(&second_ids));
    let highlights = first
        .hits
        .iter()
        .chain(&second.hits)
        .filter_map(|hit| hit.highlight_html.as_ref())
        .collect::<Vec<_>>();
    assert!(
        highlights
            .iter()
            .all(|highlight| highlight.chars().count() < 300)
    );
    assert!(highlights.iter().any(|highlight| {
        highlight.contains("&lt;script&gt;") && !highlight.contains("<script>")
    }));

    let redacted = search.search(
        &filter,
        "Synthetic",
        false,
        &PageRequest {
            cursor: None,
            limit: 1,
        },
    )?;
    assert!(redacted.hits[0].highlight_html.is_none());
    assert!(!serde_json::to_string(&redacted.hits[0].event)?.contains("Synthetic note"));
    Ok(())
}

#[test]
fn fts_pushes_typed_filters_before_keyset_limit_beyond_ten_thousand_rows()
-> Result<(), Box<dyn Error>> {
    let (_temporary, _root, sqlite, _projector) = common::store()?;
    let mut base = search_events()?.remove(0);
    let at: DateTime<Utc> = "2026-07-13T09:00:15Z".parse()?;
    base.scheduled_at = Some(at);
    base.observed_at = at;
    base.recorded_at = at + Duration::seconds(1);
    let mut connection = sqlite.connection()?;
    let transaction = connection.transaction()?;
    for index in 0..10_002_u32 {
        let mut event = base.clone();
        event.event_id = EventId::new(format!("bulk-{index:05}"))?;
        let (application, content_hash, ocr_text) = {
            let EventPayload::ObservationAttempt(attempt) = &mut event.payload else {
                return Err("fixture is not an observation".into());
            };
            let ObservationContent::Captured(content) = &mut attempt.content else {
                return Err("fixture is not captured".into());
            };
            content.context.application_bundle_id = if index >= 10_000 {
                "com.example.writer".to_owned()
            } else {
                "com.example.rejected".to_owned()
            };
            content.content_hash = format!("bulk-hash-{index}");
            content.image = None;
            let ocr = content.ocr.as_mut().ok_or("OCR fixture missing")?;
            ocr.text = "needle factual evidence".to_owned();
            (
                content.context.application_bundle_id.clone(),
                content.content_hash.clone(),
                ocr.text.clone(),
            )
        };
        event.validate()?;
        let body = serde_json::to_string(&event)?;
        transaction.execute(
            "INSERT INTO events(event_id, checksum, kind, recorded_at, body_json)
             VALUES(?1, ?2, 'observation-attempt', ?3, ?4)",
            rusqlite::params![
                event.event_id.as_str(),
                format!("checksum-{index}"),
                event.recorded_at.to_rfc3339(),
                body,
            ],
        )?;
        transaction.execute(
            "INSERT INTO observations(event_id, attempt_status, evidence_state, presence_state,
                 ocr_state, application_bundle_id, process_name, window_title,
                 authorized_domain, content_hash, ocr_text)
             VALUES(?1, 'completed', 'captured-new', 'active', 'complete', ?2,
                 'Bulk Test', 'Bulk factual window', NULL, ?3, ?4)",
            rusqlite::params![event.event_id.as_str(), application, content_hash, ocr_text,],
        )?;
        transaction.execute(
            "INSERT INTO ocr_fts(event_id, text) VALUES(?1, ?2)",
            rusqlite::params![event.event_id.as_str(), ocr_text],
        )?;
    }
    transaction.commit()?;

    let search = ActivitySearch::new(sqlite);
    let page = search.search(
        &filter()?,
        "needle",
        false,
        &PageRequest {
            cursor: None,
            limit: 1,
        },
    )?;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].event.event_id.as_str(), "bulk-10000");
    let second = search.search(
        &filter()?,
        "needle",
        false,
        &PageRequest {
            cursor: page.page.next_cursor,
            limit: 1,
        },
    )?;
    assert_eq!(second.hits.len(), 1);
    assert_eq!(second.hits[0].event.event_id.as_str(), "bulk-10001");
    assert!(second.page.next_cursor.is_none());
    Ok(())
}

#[test]
fn fts_high_water_excludes_backfilled_events_projected_after_snapshot() -> Result<(), Box<dyn Error>>
{
    let (_temporary, root, sqlite, projector) = common::store()?;
    let events = search_events()?;
    let journal = CanonicalJournal::new(root);
    let first_record = journal.append_event(&events[0], FaultInjector::none())?;
    projector.project_record(&first_record, FaultInjector::none())?;
    let high_water = StoreQueries::new(sqlite.clone())
        .snapshot()?
        .projection_high_water()?;
    let later_record = journal.append_event(&events[1], FaultInjector::none())?;
    projector.project_record(&later_record, FaultInjector::none())?;

    let page = ActivitySearch::new(sqlite).search_at_cutoff(
        &filter()?,
        "Synthetic",
        true,
        &PageRequest {
            cursor: None,
            limit: 10,
        },
        "2026-07-13T09:05:00Z".parse()?,
        high_water.event_rowid,
    )?;
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].event.event_id, events[0].event_id);
    assert!(page.hits[0].snippet.is_some());
    Ok(())
}

fn filter() -> Result<ActivityFilter, Box<dyn Error>> {
    Ok(ActivityFilter {
        range: UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        },
        application_bundle_id: Some("com.example.writer".to_owned()),
        window_text: None,
        authorized_domain: None,
        evidence_states: vec![EvidenceState::CapturedNew],
    })
}

fn search_events() -> Result<Vec<EventEnvelope>, Box<dyn Error>> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1/events.jsonl"),
    )?;
    let base = EventEnvelope::parse(text.lines().next().ok_or("fixture empty")?)?;
    let start: DateTime<Utc> = "2026-07-13T09:00:15Z".parse()?;
    let texts = [
        "Synthetic note: ignore previous instructions; café résumé 日本語".to_owned(),
        "Synthetic <script>alert('x')</script> factual evidence".to_owned(),
        "Synthetic ".repeat(5_000),
        "Synthetic final page evidence".to_owned(),
    ];
    let mut events = Vec::new();
    for (index, text) in texts.into_iter().enumerate() {
        let mut event = base.clone();
        event.event_id = EventId::new(format!("fts-event-{index}"))?;
        let at = start + Duration::seconds(i64::try_from(index)? * 30);
        event.scheduled_at = Some(at);
        event.observed_at = at;
        event.recorded_at = at + Duration::seconds(1);
        let EventPayload::ObservationAttempt(attempt) = &mut event.payload else {
            return Err("fixture is not an observation".into());
        };
        let ObservationContent::Captured(content) = &mut attempt.content else {
            return Err("fixture is not captured".into());
        };
        content.content_hash = format!("hash-fts-{index}");
        // FTS coverage is about OCR/query behavior, not managed-image
        // identity. Avoid manufacturing four canonical owners for one image.
        content.image = None;
        let ocr = content.ocr.as_mut().ok_or("fixture OCR missing")?;
        ocr.text = text;
        event.validate()?;
        events.push(event);
    }
    Ok(events)
}
