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
    let shared_schema: Value =
        serde_json::from_str(&read("contracts/shared-service-v1.schema.json")?)?;

    for path in [
        "contracts/event-v1.schema.json",
        "contracts/chunk-v1.schema.json",
        "contracts/derived-artifact-v1.schema.json",
        "contracts/query-v1.schema.json",
        "contracts/shared-service-v1.schema.json",
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

    let query = &query_schema;
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

    let registry = referencing::Registry::new()
        .add(
            "https://openchronicle.dev/contracts/query-v1.schema.json",
            query_schema.clone(),
        )?
        .add(
            "https://openchronicle.dev/contracts/chunk-v1.schema.json",
            chunk_schema.clone(),
        )?
        .add(
            "https://openchronicle.dev/contracts/derived-artifact-v1.schema.json",
            artifact_schema.clone(),
        )?
        .prepare()?;
    let shared_validator = jsonschema::options()
        .with_registry(&registry)
        .build(&shared_schema)?;
    let first_exchange = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .and_then(|exchanges| exchanges.first())
        .ok_or("query exchange missing")?;
    let request = first_exchange
        .get("request")
        .ok_or("query request missing")?
        .clone();
    let response = first_exchange
        .get("response")
        .ok_or("query response missing")?
        .clone();
    let shared_query_request = serde_json::json!({
        "schema_version": "1.0",
        "request_id": request["request_id"],
        "store_generation": request["store_generation"],
        "operation": {"type": "query", "data": request}
    });
    let shared_query_response = serde_json::json!({
        "schema_version": "1.0",
        "request_id": response["request_id"],
        "generated_at": response["generated_at"],
        "store_generation": response["store_generation"],
        "result": {"type": "query", "data": response}
    });
    let artifact_revision = packet
        .get("artifact")
        .ok_or("artifact fixture missing")?
        .clone();
    let mut query_artifact = artifact_revision.clone();
    query_artifact
        .as_object_mut()
        .ok_or("artifact fixture must be an object")?
        .remove("schema_version");
    query_artifact
        .as_object_mut()
        .ok_or("artifact fixture must be an object")?
        .remove("expected_prior_revision_id");
    let grant_summary = first_exchange
        .pointer("/response/grant")
        .ok_or("grant summary missing")?
        .clone();
    let coverage = first_exchange
        .pointer("/response/coverage")
        .ok_or("coverage missing")?
        .clone();
    let export_range = coverage
        .get("range")
        .ok_or("coverage range missing")?
        .clone();
    let shared_write_request = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "write-schema-fixture",
        "store_generation": 1,
        "operation": {"type": "write-derived", "data": {
            "schema_version": "1.0",
            "request_id": "write-schema-fixture",
            "client_id": "client-codex-synthetic",
            "grant_id": "grant-synthetic",
            "store_generation": 1,
            "revision": artifact_revision
        }}
    });
    let shared_write_response = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "write-schema-fixture",
        "generated_at": "2026-07-13T09:08:00Z",
        "store_generation": 1,
        "result": {"type": "derived-written", "data": {
            "schema_version": "1.0",
            "request_id": "write-schema-fixture",
            "generated_at": "2026-07-13T09:08:00Z",
            "store_generation": 1,
            "grant": grant_summary,
            "artifact": query_artifact
        }}
    });
    let shared_export_request = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "export-schema-fixture",
        "store_generation": 1,
        "operation": {"type": "export", "data": {
            "schema_version": "1.0",
            "request_id": "export-schema-fixture",
            "client_id": "client-codex-synthetic",
            "grant_id": "grant-synthetic",
            "store_generation": 1,
            "range": export_range,
            "include_ocr": false,
            "include_derived": false,
            "format": "markdown",
            "max_bytes": 4096
        }}
    });
    let shared_export_response = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "export-schema-fixture",
        "generated_at": "2026-07-13T09:07:00Z",
        "store_generation": 1,
        "result": {"type": "export", "data": {
            "schema_version": "1.0",
            "request_id": "export-schema-fixture",
            "generated_at": "2026-07-13T09:07:00Z",
            "store_generation": 1,
            "grant": first_exchange["response"]["grant"],
            "manifest": {
                "schema_version": "1.0",
                "range": coverage["range"],
                "stable_cutoff": "2026-07-13T09:07:00Z",
                "store_generation": 1,
                "included_counts": {"events": 0, "chunks": 0, "artifacts": 0},
                "available_counts": {"events": 0, "chunks": 0, "artifacts": 0},
                "included_content_classes": ["metadata"],
                "excluded_content_classes": ["ocr", "derived", "screenshots"],
                "journal_cutoffs": [],
                "checksums": [{"component": "document", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],
                "coverage": coverage,
                "truncated": false
            },
            "payload": {"type": "markdown", "data": {"document": "# Synthetic export"}}
        }}
    });
    let shared_health_response = serde_json::json!({
        "schema_version": "1.0",
        "request_id": "health-schema-fixture",
        "generated_at": "2026-07-13T09:07:00Z",
        "store_generation": 1,
        "result": {"type": "health", "data": {
            "schema_version": "1.0",
            "observed_at": "2026-07-13T09:07:00Z",
            "store_generation": 1,
            "projection": "current",
            "acknowledgement": "durable",
            "latest": {"last_scheduled_attempt_at": null, "last_successful_capture_at": null, "last_successful_ocr_at": null, "last_journal_at": null, "last_projection_at": null, "last_chunk_at": null},
            "aggregation_watermark": null,
            "aggregation_pending_buckets": 0,
            "projection_lag_seconds": 0,
            "projection_pending_records": 0,
            "storage": {"managed_bytes": 0, "available_bytes": 1},
            "study": {"state": "personal", "start": null, "end": null, "expired_at": null},
            "screenshot_retention": {"write_pending": 0, "retained": 0, "delete_pending": 0, "expired": 0, "user_deleted": 0, "missing": 0, "write_failed": 0, "next_expiry_at": null},
            "mcp": {"active_grants": 0, "revoked_grants": 0, "expired_grants": 0, "exhausted_grants": 0, "stale_generation_grants": 0},
            "issues": []
        }}
    });
    for (label, value) in [
        (
            "health request",
            serde_json::json!({
                "schema_version": "1.0",
                "request_id": "health-schema-fixture",
                "store_generation": 1,
                "operation": {"type": "health"}
            }),
        ),
        ("query request", shared_query_request),
        ("query response", shared_query_response),
        ("health response", shared_health_response),
        ("derived write request", shared_write_request),
        ("derived write response", shared_write_response),
        ("export request", shared_export_request),
        ("export response", shared_export_response),
        (
            "safe MCP error",
            serde_json::json!({
                "schema_version": "1.0",
                "error": {"code": "invalid-input", "message": "The tool input is invalid."}
            }),
        ),
    ] {
        shared_validator
            .validate(&value)
            .map_err(|error| format!("shared {label} failed schema: {error}"))?;
    }
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
