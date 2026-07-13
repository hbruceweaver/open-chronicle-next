use chronicle_domain::{
    ChunkRevision, ContentClass, DisclosureGrant, EventEnvelope, EventPayload, EvidenceState,
    GrantState, ObservationContent, QueryExchange, QueryRequest, QueryResponse,
    ScreenshotDeletionCause, ScreenshotLifecycleAction, ScreenshotProjectedState,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(root().join(path))?)
}

fn values(path: &str) -> Result<Vec<Value>, Box<dyn Error>> {
    read(path)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn events(path: &str) -> Result<Vec<EventEnvelope>, Box<dyn Error>> {
    read(path)?
        .lines()
        .map(|line| EventEnvelope::parse(line).map_err(Into::into))
        .collect()
}

#[test]
fn ae4_ten_interval_centered_attempts_cover_exactly_five_minutes() -> Result<(), Box<dyn Error>> {
    let attempts = events("fixtures/synthetic/session-v1/ae4-ten-scheduled-events.jsonl")?;
    assert_eq!(attempts.len(), 10);
    let start: DateTime<Utc> = "2026-07-13T09:00:15Z".parse()?;
    for (index, event) in attempts.iter().enumerate() {
        let expected = start + Duration::seconds(i64::try_from(index)? * 30);
        assert_eq!(event.scheduled_at, Some(expected));
        assert_eq!(event.observed_at, expected);
        let EventPayload::ObservationAttempt(attempt) = &event.payload else {
            return Err("AE4 fixture contains a non-attempt".into());
        };
        assert_eq!(attempt.evidence_state, EvidenceState::CapturedNew);
    }

    let chunk = ChunkRevision::parse(&read(
        "fixtures/synthetic/session-v1/ae4-ten-scheduled-chunk.json",
    )?)?;
    assert_eq!(chunk.evidence_seconds.captured, 300);
    assert_eq!(chunk.evidence_seconds.total(), 300);
    assert_eq!(chunk.presence_seconds.total(), 300);
    let event_ids: HashSet<_> = attempts.iter().map(|event| &event.event_id).collect();
    let chunk_ids: HashSet<_> = chunk.supporting_event_ids.iter().collect();
    assert_eq!(event_ids, chunk_ids);
    Ok(())
}

#[test]
fn ae13_ten_unchanged_attempts_reuse_one_factual_artifact() -> Result<(), Box<dyn Error>> {
    let seed_records = events("fixtures/synthetic/session-v1/ae13-seed-events.jsonl")?;
    let seed = seed_records
        .iter()
        .find(|event| event.event_id.as_str() == "ae13-seed-event")
        .ok_or("AE13 changed seed is missing")?;
    let EventPayload::ObservationAttempt(seed_attempt) = &seed.payload else {
        return Err("AE13 seed is not an observation".into());
    };
    let ObservationContent::Captured(seed_content) = &seed_attempt.content else {
        return Err("AE13 seed is not changed captured content".into());
    };
    let seed_image = seed_content
        .image
        .as_ref()
        .ok_or("AE13 seed image is missing")?;
    let retained = seed_records.iter().find_map(|event| {
        let EventPayload::ScreenshotLifecycle(lifecycle) = &event.payload else {
            return None;
        };
        (lifecycle.action == ScreenshotLifecycleAction::WriteCompleted
            && lifecycle.projected_state == ScreenshotProjectedState::Retained
            && lifecycle.source_event_id == seed.event_id
            && lifecycle.artifact_id == seed_image.artifact_id)
            .then_some(lifecycle)
    });
    assert!(retained.is_some(), "AE13 seed image was never retained");

    let attempts = events("fixtures/synthetic/session-v1/ae13-ten-unchanged-events.jsonl")?;
    assert_eq!(attempts.len(), 10);
    let mut hashes = HashSet::new();
    let mut contexts = HashSet::new();
    let mut artifacts = HashSet::new();
    let mut prior_events = HashSet::new();
    for event in &attempts {
        let EventPayload::ObservationAttempt(attempt) = &event.payload else {
            return Err("AE13 fixture contains a non-attempt".into());
        };
        assert_eq!(attempt.evidence_state, EvidenceState::CapturedUnchanged);
        let ObservationContent::Unchanged(content) = &attempt.content else {
            return Err("AE13 fixture contains changed content".into());
        };
        hashes.insert(content.content_hash.as_str());
        contexts.insert((
            content.context.application_bundle_id.as_str(),
            content.context.process_name.as_str(),
            content.context.window_title.as_deref(),
        ));
        artifacts.insert(content.image_artifact_id.as_ref());
        prior_events.insert(&content.previous_event_id);
        assert_eq!(content.previous_event_id, seed.event_id);
        assert_eq!(content.context, seed_content.context);
        assert_eq!(content.content_hash, seed_content.content_hash);
        assert_eq!(
            content.image_artifact_id.as_ref(),
            Some(&seed_image.artifact_id)
        );
    }
    assert_eq!(hashes.len(), 1);
    assert_eq!(contexts.len(), 1);
    assert_eq!(artifacts.len(), 1);
    assert!(artifacts.iter().all(|artifact| artifact.is_some()));
    assert_eq!(prior_events.len(), 1);

    let chunk = ChunkRevision::parse(&read(
        "fixtures/synthetic/session-v1/ae13-ten-unchanged-chunk.json",
    )?)?;
    assert_eq!(chunk.evidence_seconds.captured, 300);
    assert_eq!(chunk.evidence_seconds.total(), 300);
    assert_eq!(chunk.presence_seconds.total(), 300);
    Ok(())
}

#[test]
fn unchanged_fixture_references_preserve_context_and_hash() -> Result<(), Box<dyn Error>> {
    let records = events("fixtures/synthetic/session-v1/events.jsonl")?;
    for event in &records {
        let EventPayload::ObservationAttempt(attempt) = &event.payload else {
            continue;
        };
        let ObservationContent::Unchanged(unchanged) = &attempt.content else {
            continue;
        };
        let previous = records
            .iter()
            .find(|candidate| candidate.event_id == unchanged.previous_event_id)
            .ok_or("unchanged reference is missing")?;
        let EventPayload::ObservationAttempt(previous_attempt) = &previous.payload else {
            return Err("unchanged reference is not an observation".into());
        };
        let ObservationContent::Captured(captured) = &previous_attempt.content else {
            return Err("unchanged reference is not captured content".into());
        };
        assert_eq!(unchanged.context, captured.context);
        assert_eq!(unchanged.content_hash, captured.content_hash);
        assert_eq!(
            unchanged.image_artifact_id.as_ref(),
            captured.image.as_ref().map(|image| &image.artifact_id)
        );
    }
    Ok(())
}

#[test]
fn screenshot_expiry_and_user_deletion_are_both_two_phase_and_cause_preserving()
-> Result<(), Box<dyn Error>> {
    let records = events("fixtures/synthetic/session-v1/events.jsonl")?;
    for (cause, final_state) in [
        (
            ScreenshotDeletionCause::RetentionExpired,
            ScreenshotProjectedState::Expired,
        ),
        (
            ScreenshotDeletionCause::UserRequested,
            ScreenshotProjectedState::UserDeleted,
        ),
    ] {
        let requested = records.iter().find_map(|event| {
            let EventPayload::ScreenshotLifecycle(lifecycle) = &event.payload else {
                return None;
            };
            (lifecycle.action == ScreenshotLifecycleAction::DeleteRequested
                && lifecycle.deletion_cause == Some(cause))
            .then_some(lifecycle)
        });
        let completed = records.iter().find_map(|event| {
            let EventPayload::ScreenshotLifecycle(lifecycle) = &event.payload else {
                return None;
            };
            (lifecycle.action == ScreenshotLifecycleAction::DeleteCompleted
                && lifecycle.deletion_cause == Some(cause))
            .then_some(lifecycle)
        });
        let requested = requested.ok_or("delete request fixture missing")?;
        let completed = completed.ok_or("delete completion fixture missing")?;
        assert_eq!(requested.artifact_id, completed.artifact_id);
        assert_eq!(requested.requested_at, completed.requested_at);
        assert_eq!(
            requested.projected_state,
            ScreenshotProjectedState::DeletePending
        );
        assert_eq!(completed.projected_state, final_state);
    }
    Ok(())
}

#[test]
fn grants_are_authorized_at_a_time_and_fail_closed_when_expired_or_revoked()
-> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let grant = DisclosureGrant::parse(&serde_json::to_string(
        packet.get("grant").ok_or("grant fixture missing")?,
    )?)?;
    let active_at: DateTime<Utc> = "2026-07-13T09:00:00Z".parse()?;
    assert!(grant.is_active_at(active_at));
    assert!(grant.allows_full_ocr_at(active_at));
    assert!(grant.allows_content_at(ContentClass::Metadata, active_at));
    assert!(!grant.is_active_at(grant.expires_at));
    assert!(!grant.allows_full_ocr_at(grant.expires_at));

    let mut revoked = grant.clone();
    revoked.state = GrantState::Revoked;
    assert!(!revoked.is_active_at(active_at));
    assert!(!revoked.allows_full_ocr_at(active_at));

    let mut expired = grant;
    expired.state = GrantState::Expired;
    assert!(!expired.is_active_at(active_at));
    assert!(!expired.allows_full_ocr_at(active_at));
    Ok(())
}

#[test]
fn query_fixture_exchanges_are_explicitly_paired_and_mismatches_fail() -> Result<(), Box<dyn Error>>
{
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let exchanges = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .ok_or("query exchanges missing")?;
    assert_eq!(exchanges.len(), 3);
    for value in exchanges {
        let exchange: QueryExchange = serde_json::from_value(value.clone())?;
        exchange.validate()?;

        let mut mismatched = exchange.clone();
        mismatched.response.request_id = chronicle_domain::RequestId::new("other-request")?;
        assert!(mismatched.validate().is_err());
    }

    let mut wrong_event = exchanges[1].clone();
    wrong_event["response"]["result"]["data"]["event"]["event_id"] = json!("other-event");
    let wrong_event: QueryExchange = serde_json::from_value(wrong_event)?;
    assert!(wrong_event.validate().is_err());

    let mut wrong_artifact = exchanges[2].clone();
    wrong_artifact["response"]["result"]["data"]["artifact"]["revision_id"] =
        json!("other-revision");
    let wrong_artifact: QueryExchange = serde_json::from_value(wrong_artifact)?;
    assert!(wrong_artifact.validate().is_err());

    let chunks = read("fixtures/synthetic/session-v1/chunks.jsonl")?;
    let chunk: Value = serde_json::from_str(chunks.lines().next().ok_or("chunk fixture missing")?)?;
    let chunk_id = chunk["chunk_id"].clone();
    let mut read_chunk = exchanges[1].clone();
    read_chunk["request"]["operation"] =
        json!({"type": "read-chunk", "data": {"chunk_id": chunk_id}});
    read_chunk["response"]["operation"] = json!("read-chunk");
    read_chunk["response"]["scope"]["requested_ranges"][0]["end"] = json!("2026-07-13T09:05:00Z");
    read_chunk["response"]["scope"]["effective_ranges"][0]["end"] = json!("2026-07-13T09:05:00Z");
    read_chunk["response"]["result"] = json!({
        "type": "chunk",
        "data": {"chunk": chunk, "images": []}
    });
    let read_chunk: QueryExchange = serde_json::from_value(read_chunk)?;
    read_chunk.validate()?;
    let mut wrong_chunk = read_chunk;
    if let chronicle_domain::QueryOperation::ReadChunk { chunk_id } =
        &mut wrong_chunk.request.operation
    {
        *chunk_id = chronicle_domain::ChunkId::new("other-chunk")?;
    } else {
        return Err("read-chunk fixture operation was not preserved".into());
    }
    assert!(wrong_chunk.validate().is_err());
    Ok(())
}

#[test]
fn query_results_enforce_ranges_content_classes_and_exact_page_counts() -> Result<(), Box<dyn Error>>
{
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let exchanges = packet["exchanges"]
        .as_array()
        .ok_or("query exchanges missing")?;
    let search = &exchanges[0];

    let mut out_of_range_event = search["response"].clone();
    out_of_range_event["result"]["data"]["events"][0]["scheduled_at"] =
        json!("2026-07-13T08:59:45Z");
    out_of_range_event["result"]["data"]["events"][0]["observed_at"] =
        json!("2026-07-13T08:59:45Z");
    assert!(
        QueryResponse::parse(&serde_json::to_string(&out_of_range_event)?).is_err(),
        "accepted an event outside the effective range"
    );

    let mut unauthorized_metadata = search["response"].clone();
    unauthorized_metadata["scope"]["content_classes"] = json!(["ocr"]);
    assert!(
        QueryResponse::parse(&serde_json::to_string(&unauthorized_metadata)?).is_err(),
        "accepted factual event metadata outside the response content scope"
    );

    let mut false_count = search["response"].clone();
    false_count["page"]["returned_items"] = json!(0);
    assert!(
        QueryResponse::parse(&serde_json::to_string(&false_count)?).is_err(),
        "accepted a false returned_items count"
    );

    let returned_event = search["response"]["result"]["data"]["events"][0].clone();
    let mut over_grant_page = search["response"].clone();
    over_grant_page["result"]["data"]["events"]
        .as_array_mut()
        .ok_or("search results missing")?
        .push(returned_event.clone());
    over_grant_page["page"]["returned_items"] = json!(2);
    over_grant_page["grant"]["limits"]["max_page_items"] = json!(1);
    assert!(
        QueryResponse::parse(&serde_json::to_string(&over_grant_page)?).is_err(),
        "accepted a page larger than its disclosure grant"
    );

    let mut over_request_page = search.clone();
    over_request_page["request"]["operation"]["data"]["page"]["limit"] = json!(1);
    over_request_page["response"]["result"]["data"]["events"]
        .as_array_mut()
        .ok_or("search results missing")?
        .push(returned_event);
    over_request_page["response"]["page"]["returned_items"] = json!(2);
    let exchange: QueryExchange = serde_json::from_value(over_request_page)?;
    assert!(
        exchange.validate().is_err(),
        "accepted a page larger than its paired request"
    );

    let chunks = read("fixtures/synthetic/session-v1/chunks.jsonl")?;
    let returned_chunk: Value = serde_json::from_str(
        chunks
            .lines()
            .nth(1)
            .ok_or("second chunk fixture missing")?,
    )?;
    let mut out_of_range_chunk = exchanges[1]["response"].clone();
    out_of_range_chunk["operation"] = json!("read-chunk");
    out_of_range_chunk["result"] = json!({
        "type": "chunk",
        "data": {"chunk": returned_chunk, "images": []}
    });
    assert!(
        QueryResponse::parse(&serde_json::to_string(&out_of_range_chunk)?).is_err(),
        "accepted a chunk outside the effective range"
    );

    let mut out_of_range_artifact = exchanges[2]["response"].clone();
    out_of_range_artifact["scope"]["requested_ranges"][0]["end"] = json!("2026-07-13T09:05:00Z");
    out_of_range_artifact["scope"]["effective_ranges"][0]["end"] = json!("2026-07-13T09:05:00Z");
    assert!(
        QueryResponse::parse(&serde_json::to_string(&out_of_range_artifact)?).is_err(),
        "accepted an artifact created outside the effective range"
    );
    Ok(())
}

#[test]
fn query_coverage_gap_intervals_must_reconcile_every_noncaptured_state()
-> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let base = packet["exchanges"][0]["response"].clone();

    let mut missing_protected = base.clone();
    missing_protected["coverage"]["gaps"]
        .as_array_mut()
        .ok_or("coverage gaps missing")?
        .remove(0);
    assert!(
        QueryResponse::parse(&serde_json::to_string(&missing_protected)?).is_err(),
        "accepted missing protected gap seconds"
    );

    let mut misclassified = base.clone();
    misclassified["coverage"]["gaps"][0]["kind"] = json!("unavailable");
    assert!(
        QueryResponse::parse(&serde_json::to_string(&misclassified)?).is_err(),
        "accepted a gap mapped to the wrong evidence state"
    );

    let mut wrong_duration = base;
    wrong_duration["coverage"]["gaps"][0]["end"] = json!("2026-07-13T09:01:29Z");
    assert!(
        QueryResponse::parse(&serde_json::to_string(&wrong_duration)?).is_err(),
        "accepted gap duration totals that do not reconcile"
    );
    Ok(())
}

#[test]
fn chunk_revision_interval_and_reference_invariants_fail_closed() -> Result<(), Box<dyn Error>> {
    let base: Value = serde_json::from_str(
        read("fixtures/synthetic/session-v1/chunks.jsonl")?
            .lines()
            .next()
            .ok_or("chunk fixture missing")?,
    )?;
    for mutation in [
        ("prior-link", json!("some-other-revision")),
        ("duplicate-support", json!("evt-090015")),
        ("outside-transition", json!("2026-07-13T09:05:00Z")),
        ("unknown-reference", json!("evt-not-in-chunk")),
        ("missing-protected-gap", Value::Null),
        ("misclassified-gap", json!("unavailable")),
        ("overlapping-gap", json!("2026-07-13T09:02:15Z")),
        ("wrong-gap-duration", json!("2026-07-13T09:01:29Z")),
    ] {
        let mut value = base.clone();
        match mutation.0 {
            "prior-link" => value["prior_revision_id"] = mutation.1,
            "duplicate-support" => value["supporting_event_ids"]
                .as_array_mut()
                .ok_or("supporting IDs missing")?
                .push(mutation.1),
            "outside-transition" => value["transitions"][0]["at"] = mutation.1,
            "unknown-reference" => {
                value["duration_estimates"][0]["supporting_event_ids"][0] = mutation.1
            }
            "missing-protected-gap" => {
                value["gaps"]
                    .as_array_mut()
                    .ok_or("chunk gaps missing")?
                    .remove(0);
            }
            "misclassified-gap" => value["gaps"][0]["kind"] = mutation.1,
            "overlapping-gap" => {
                value["gaps"][1]["end"] = mutation.1;
                value["gaps"][1]["start"] = json!("2026-07-13T09:01:15Z");
            }
            "wrong-gap-duration" => value["gaps"][0]["end"] = mutation.1,
            _ => return Err("unknown test mutation".into()),
        }
        assert!(
            ChunkRevision::parse(&serde_json::to_string(&value)?).is_err(),
            "accepted {}",
            mutation.0
        );
    }
    Ok(())
}

#[test]
fn draft_2020_12_schemas_and_rust_readers_have_differential_acceptance_parity()
-> Result<(), Box<dyn Error>> {
    let event_schema: Value = serde_json::from_str(&read("contracts/event-v1.schema.json")?)?;
    let event_validator = jsonschema::validator_for(&event_schema)?;
    let mut event_cases = Vec::new();
    let base_event = values("fixtures/synthetic/session-v1/events.jsonl")?
        .into_iter()
        .next()
        .ok_or("event fixture missing")?;
    event_cases.push(base_event.clone());
    let mut same_major_event = base_event.clone();
    same_major_event["schema_version"] = json!("1.9");
    same_major_event["future_optional_metadata"] = json!({"producer": "future"});
    event_cases.push(same_major_event);
    let mut wrong_major_event = base_event.clone();
    wrong_major_event["schema_version"] = json!("2.0");
    event_cases.push(wrong_major_event);
    let mut traversal_event = base_event.clone();
    traversal_event["payload"]["data"]["content"]["data"]["image"]["managed_relative_path"] =
        json!("../escape.heic");
    event_cases.push(traversal_event);
    let mut bad_presence = base_event;
    bad_presence["payload"]["data"]["presence_state"] = json!("locked");
    event_cases.push(bad_presence);
    for value in event_cases {
        assert_eq!(
            event_validator.is_valid(&value),
            EventEnvelope::parse(&serde_json::to_string(&value)?).is_ok(),
            "event schema/Rust acceptance diverged for {value}"
        );
    }

    let chunk_schema: Value = serde_json::from_str(&read("contracts/chunk-v1.schema.json")?)?;
    let chunk_validator = jsonschema::validator_for(&chunk_schema)?;
    let base_chunk: Value = serde_json::from_str(
        read("fixtures/synthetic/session-v1/chunks.jsonl")?
            .lines()
            .next()
            .ok_or("chunk fixture missing")?,
    )?;
    let mut chunk_cases = vec![base_chunk.clone()];
    let mut same_major_chunk = base_chunk.clone();
    same_major_chunk["schema_version"] = json!("1.8");
    same_major_chunk["future_optional_metadata"] = json!(true);
    chunk_cases.push(same_major_chunk);
    let mut zero_generation = base_chunk.clone();
    zero_generation["store_generation"] = json!(0);
    chunk_cases.push(zero_generation);
    let mut empty_provenance = base_chunk;
    empty_provenance["aggregator_version"] = json!("");
    chunk_cases.push(empty_provenance);
    for value in chunk_cases {
        assert_eq!(
            chunk_validator.is_valid(&value),
            ChunkRevision::parse(&serde_json::to_string(&value)?).is_ok(),
            "chunk schema/Rust acceptance diverged for {value}"
        );
    }

    let query_schema: Value = serde_json::from_str(&read("contracts/query-v1.schema.json")?)?;
    let query_validator = jsonschema::validator_for(&query_schema)?;
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let exchange = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .and_then(|exchanges| exchanges.first())
        .ok_or("query exchange missing")?;
    let request = exchange.get("request").ok_or("request missing")?.clone();
    let response = exchange.get("response").ok_or("response missing")?.clone();
    assert!(query_validator.is_valid(&request));
    assert!(QueryRequest::parse(&serde_json::to_string(&request)?).is_ok());
    assert!(query_validator.is_valid(&response));
    assert!(QueryResponse::parse(&serde_json::to_string(&response)?).is_ok());

    let mut mismatched_result = response.clone();
    mismatched_result["operation"] = json!("get-event");
    assert_eq!(
        query_validator.is_valid(&mismatched_result),
        QueryResponse::parse(&serde_json::to_string(&mismatched_result)?).is_ok()
    );
    assert!(!query_validator.is_valid(&mismatched_result));

    let mut path_smuggle = response;
    path_smuggle["result"]["data"]["path"] = json!("screenshots/secret.heic");
    assert_eq!(
        query_validator.is_valid(&path_smuggle),
        QueryResponse::parse(&serde_json::to_string(&path_smuggle)?).is_ok()
    );
    assert!(!query_validator.is_valid(&path_smuggle));
    Ok(())
}
