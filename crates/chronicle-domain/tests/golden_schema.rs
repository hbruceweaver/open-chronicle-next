use chronicle_domain::{
    ArtifactType, AttemptStatus, ChunkRevision, ContentClass, DerivedArtifactRevision,
    DisclosureGrant, DurableAcknowledgement, EventEnvelope, EventKind, EvidenceState, GrantState,
    OcrState, PresenceState, ProjectionHealth, QueryCapability, QueryExchange, QueryRequest,
    QueryResponse, ScreenshotProjectedState,
};
use serde::Serialize;
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(root().join(path))?)
}

fn json_value<T: Serialize>(value: T) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::to_value(value)?)
}

#[test]
fn event_and_chunk_jsonl_round_trip_byte_stably() -> Result<(), Box<dyn Error>> {
    for path in [
        "fixtures/synthetic/session-v1/events.jsonl",
        "fixtures/synthetic/session-v1/ae4-ten-scheduled-events.jsonl",
        "fixtures/synthetic/session-v1/ae13-seed-events.jsonl",
        "fixtures/synthetic/session-v1/ae13-ten-unchanged-events.jsonl",
    ] {
        for line in read(path)?.lines() {
            let event = EventEnvelope::parse(line)?;
            assert_eq!(serde_json::to_string(&event)?, line);
        }
    }

    for path in [
        "fixtures/synthetic/session-v1/chunks.jsonl",
        "fixtures/synthetic/session-v1/ae4-ten-scheduled-chunk.json",
        "fixtures/synthetic/session-v1/ae13-ten-unchanged-chunk.json",
    ] {
        for line in read(path)?.lines() {
            let chunk = ChunkRevision::parse(line)?;
            assert_eq!(serde_json::to_string(&chunk)?, line);
        }
    }
    Ok(())
}

#[test]
fn query_grant_response_and_artifact_goldens_match_typed_contracts() -> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;

    let grant_value = packet.get("grant").ok_or("grant fixture missing")?.clone();
    let grant = DisclosureGrant::parse(&serde_json::to_string(&grant_value)?)?;
    assert_eq!(json_value(&grant)?, grant_value);

    for value in packet
        .get("exchanges")
        .and_then(Value::as_array)
        .ok_or("query exchanges missing")?
    {
        let request_value = value.get("request").ok_or("paired request missing")?;
        let response_value = value.get("response").ok_or("paired response missing")?;
        let request = QueryRequest::parse(&serde_json::to_string(request_value)?)?;
        let response = QueryResponse::parse(&serde_json::to_string(response_value)?)?;
        assert_eq!(json_value(&request)?, *request_value);
        assert_eq!(json_value(&response)?, *response_value);

        let exchange = QueryExchange { request, response };
        exchange.validate()?;
    }

    let artifact_value = packet
        .get("artifact")
        .ok_or("artifact fixture missing")?
        .clone();
    let artifact = DerivedArtifactRevision::parse(&serde_json::to_string(&artifact_value)?)?;
    assert_eq!(json_value(artifact)?, artifact_value);
    Ok(())
}

#[test]
fn contract_schemas_are_draft_2020_12_valid_and_validate_every_fixture()
-> Result<(), Box<dyn Error>> {
    let event_schema: Value = serde_json::from_str(&read("contracts/event-v1.schema.json")?)?;
    let chunk_schema: Value = serde_json::from_str(&read("contracts/chunk-v1.schema.json")?)?;
    let artifact_schema: Value =
        serde_json::from_str(&read("contracts/derived-artifact-v1.schema.json")?)?;
    let query_schema: Value = serde_json::from_str(&read("contracts/query-v1.schema.json")?)?;

    for path in [
        "contracts/event-v1.schema.json",
        "contracts/chunk-v1.schema.json",
        "contracts/derived-artifact-v1.schema.json",
        "contracts/query-v1.schema.json",
    ] {
        let schema: Value = serde_json::from_str(&read(path)?)?;
        assert_eq!(
            schema.get("$schema").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
        assert!(
            schema
                .get("$defs")
                .and_then(Value::as_object)
                .is_some_and(|defs| !defs.is_empty())
        );
        jsonschema::meta::validate(&schema)
            .map_err(|error| format!("{path} failed meta-validation: {error}"))?;
    }

    let event_validator = jsonschema::validator_for(&event_schema)?;
    for path in [
        "fixtures/synthetic/session-v1/events.jsonl",
        "fixtures/synthetic/session-v1/ae4-ten-scheduled-events.jsonl",
        "fixtures/synthetic/session-v1/ae13-seed-events.jsonl",
        "fixtures/synthetic/session-v1/ae13-ten-unchanged-events.jsonl",
    ] {
        for (index, line) in read(path)?.lines().enumerate() {
            let value: Value = serde_json::from_str(line)?;
            event_validator
                .validate(&value)
                .map_err(|error| format!("{path}:{} failed schema: {error}", index + 1))?;
        }
    }

    let chunk_validator = jsonschema::validator_for(&chunk_schema)?;
    for path in [
        "fixtures/synthetic/session-v1/chunks.jsonl",
        "fixtures/synthetic/session-v1/ae4-ten-scheduled-chunk.json",
        "fixtures/synthetic/session-v1/ae13-ten-unchanged-chunk.json",
    ] {
        for (index, line) in read(path)?.lines().enumerate() {
            let value: Value = serde_json::from_str(line)?;
            chunk_validator
                .validate(&value)
                .map_err(|error| format!("{path}:{} failed schema: {error}", index + 1))?;
        }
    }

    let packet: Value = serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    let artifact_validator = jsonschema::validator_for(&artifact_schema)?;
    artifact_validator
        .validate(packet.get("artifact").ok_or("artifact fixture missing")?)
        .map_err(|error| format!("artifact fixture failed schema: {error}"))?;
    let query_validator = jsonschema::validator_for(&query_schema)?;
    query_validator
        .validate(packet.get("grant").ok_or("grant fixture missing")?)
        .map_err(|error| format!("grant fixture failed schema: {error}"))?;
    for exchange in packet
        .get("exchanges")
        .and_then(Value::as_array)
        .ok_or("query exchanges missing")?
    {
        for side in ["request", "response"] {
            query_validator
                .validate(exchange.get(side).ok_or("exchange side missing")?)
                .map_err(|error| format!("query {side} failed schema: {error}"))?;
        }
    }

    let query = query_schema;
    assert!(
        query
            .pointer("/$defs/operation/oneOf")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() == 12)
    );
    assert!(
        query
            .pointer("/$defs/queryResult/oneOf")
            .and_then(Value::as_array)
            .is_some_and(|items| items.len() == 13)
    );
    assert!(!read("contracts/query-v1.schema.json")?.contains("managed_relative_path"));
    Ok(())
}

#[test]
fn every_serialized_enum_token_is_kebab_case() -> Result<(), Box<dyn Error>> {
    let values = [
        json_value(EventKind::ObservationAttempt)?,
        json_value(AttemptStatus::Completed)?,
        json_value(EvidenceState::CapturedUnchanged)?,
        json_value(PresenceState::Unknown)?,
        json_value(OcrState::NotRun)?,
        json_value(DurableAcknowledgement::JournalDurableProjectionPending)?,
        json_value(ProjectionHealth::Rebuilding)?,
        json_value(ScreenshotProjectedState::DeletePending)?,
        json_value(ArtifactType::Hypothesis)?,
        json_value(ContentClass::Derived)?,
        json_value(GrantState::Revoked)?,
        json_value(QueryCapability::DerivedWrite)?,
    ];
    for token in values {
        let token = token.as_str().ok_or("enum did not serialize as a string")?;
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        );
        assert!(!token.contains('_'));
    }
    Ok(())
}

#[test]
fn manifest_matches_the_synthetic_corpus() -> Result<(), Box<dyn Error>> {
    let manifest: Value =
        serde_json::from_str(&read("fixtures/synthetic/session-v1/manifest.json")?)?;
    let events = read("fixtures/synthetic/session-v1/events.jsonl")?;
    let chunks = read("fixtures/synthetic/session-v1/chunks.jsonl")?;
    assert_eq!(manifest.get("synthetic"), Some(&Value::Bool(true)));
    assert_eq!(
        manifest.get("event_records").and_then(Value::as_u64),
        Some(events.lines().count() as u64)
    );
    assert_eq!(
        manifest.get("chunk_revisions").and_then(Value::as_u64),
        Some(chunks.lines().count() as u64)
    );
    let queries: Value =
        serde_json::from_str(&read("fixtures/synthetic/session-v1/queries.json")?)?;
    assert_eq!(
        manifest.get("query_exchanges").and_then(Value::as_u64),
        queries
            .get("exchanges")
            .and_then(Value::as_array)
            .map(|exchanges| exchanges.len() as u64)
    );
    for (manifest_key, path) in [
        (
            "ae4_interval_centered_attempts",
            "fixtures/synthetic/session-v1/ae4-ten-scheduled-events.jsonl",
        ),
        (
            "ae13_unchanged_attempts",
            "fixtures/synthetic/session-v1/ae13-ten-unchanged-events.jsonl",
        ),
        (
            "ae13_seed_records",
            "fixtures/synthetic/session-v1/ae13-seed-events.jsonl",
        ),
    ] {
        assert_eq!(
            manifest.get(manifest_key).and_then(Value::as_u64),
            Some(read(path)?.lines().count() as u64)
        );
    }
    Ok(())
}
