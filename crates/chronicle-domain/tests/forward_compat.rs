use chronicle_domain::{
    ChunkRevision, ContractError, DerivedArtifactRevision, DisclosureGrant, EventEnvelope,
    ManagedRelativePath, QueryRequest, QueryResponse,
};
use serde_json::{Value, json};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

type ContractReader = Box<dyn Fn(&str) -> bool>;

fn first_event_value() -> Result<Value, Box<dyn Error>> {
    let events = fs::read_to_string(root().join("fixtures/synthetic/session-v1/events.jsonl"))?;
    let line = events.lines().next().ok_or("event fixture is empty")?;
    Ok(serde_json::from_str(line)?)
}

#[test]
fn same_major_optional_envelope_fields_are_ignored_by_v1_reader() -> Result<(), Box<dyn Error>> {
    let mut value = first_event_value()?;
    value["schema_version"] = json!("1.1");
    value["future_optional_metadata"] = json!({"producer": "synthetic-v1.1"});
    let event = EventEnvelope::parse(&serde_json::to_string(&value)?)?;
    let serialized = serde_json::to_value(event)?;
    assert_eq!(
        serialized.get("schema_version").and_then(Value::as_str),
        Some("1.1")
    );
    assert!(serialized.get("future_optional_metadata").is_none());
    Ok(())
}

#[test]
fn same_major_extensions_are_schema_and_reader_compatible_for_every_contract()
-> Result<(), Box<dyn Error>> {
    let query_packet: Value = serde_json::from_str(&fs::read_to_string(
        root().join("fixtures/synthetic/session-v1/queries.json"),
    )?)?;
    let chunk_line = fs::read_to_string(root().join("fixtures/synthetic/session-v1/chunks.jsonl"))?;
    let contracts: Vec<(&str, Value, ContractReader)> = vec![
        (
            "contracts/chunk-v1.schema.json",
            serde_json::from_str(chunk_line.lines().next().ok_or("chunk fixture missing")?)?,
            Box::new(|json| ChunkRevision::parse(json).is_ok()),
        ),
        (
            "contracts/derived-artifact-v1.schema.json",
            query_packet
                .get("artifact")
                .ok_or("artifact fixture missing")?
                .clone(),
            Box::new(|json| DerivedArtifactRevision::parse(json).is_ok()),
        ),
        (
            "contracts/query-v1.schema.json",
            query_packet
                .get("grant")
                .ok_or("grant fixture missing")?
                .clone(),
            Box::new(|json| DisclosureGrant::parse(json).is_ok()),
        ),
        (
            "contracts/query-v1.schema.json",
            query_packet
                .pointer("/exchanges/0/request")
                .ok_or("request fixture missing")?
                .clone(),
            Box::new(|json| QueryRequest::parse(json).is_ok()),
        ),
        (
            "contracts/query-v1.schema.json",
            query_packet
                .pointer("/exchanges/0/response")
                .ok_or("response fixture missing")?
                .clone(),
            Box::new(|json| QueryResponse::parse(json).is_ok()),
        ),
    ];

    for (schema_path, mut value, rust_accepts) in contracts {
        value["schema_version"] = json!("1.42");
        value["future_optional_metadata"] = json!({"producer": "synthetic-future"});
        let schema: Value = serde_json::from_str(&fs::read_to_string(root().join(schema_path))?)?;
        let validator = jsonschema::validator_for(&schema)?;
        assert!(validator.is_valid(&value), "schema rejected {schema_path}");
        assert!(
            rust_accepts(&serde_json::to_string(&value)?),
            "Rust reader rejected {schema_path}"
        );
    }
    Ok(())
}

#[test]
fn unknown_major_versions_fail_with_typed_error() -> Result<(), Box<dyn Error>> {
    let mut event = first_event_value()?;
    event["schema_version"] = json!("2.0");
    assert!(matches!(
        EventEnvelope::parse(&serde_json::to_string(&event)?),
        Err(ContractError::UnsupportedMajorVersion {
            expected: 1,
            actual: 2
        })
    ));

    let chunks = fs::read_to_string(root().join("fixtures/synthetic/session-v1/chunks.jsonl"))?;
    let line = chunks.lines().next().ok_or("chunk fixture is empty")?;
    let mut chunk: Value = serde_json::from_str(line)?;
    chunk["schema_version"] = json!("9.1");
    assert!(matches!(
        ChunkRevision::parse(&serde_json::to_string(&chunk)?),
        Err(ContractError::UnsupportedMajorVersion {
            expected: 1,
            actual: 9
        })
    ));
    Ok(())
}

#[test]
fn malformed_and_missing_versions_fail_before_contract_use() {
    assert!(matches!(
        EventEnvelope::parse("{}"),
        Err(ContractError::MissingSchemaVersion)
    ));
    assert!(matches!(
        EventEnvelope::parse(r#"{"schema_version":"banana"}"#),
        Err(ContractError::InvalidSchemaVersion(_))
    ));
    assert!(matches!(
        EventEnvelope::parse(r#"{"schema_version":"1.future"}"#),
        Err(ContractError::InvalidSchemaVersion(_))
    ));
}

#[test]
fn managed_relative_paths_reject_absolute_and_traversal_inputs() {
    assert!(ManagedRelativePath::new("screenshots/2026-07-13/img.heic").is_ok());
    for invalid in [
        "/synthetic-root/evidence.heic",
        "../evidence.heic",
        "screenshots/../evidence.heic",
        r"screenshots\evidence.heic",
        "",
    ] {
        assert!(
            ManagedRelativePath::new(invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn query_filters_reject_untyped_evidence_state_tokens() -> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&fs::read_to_string(
        root().join("fixtures/synthetic/session-v1/queries.json"),
    )?)?;
    let mut request = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .and_then(|exchanges| exchanges.first())
        .and_then(|exchange| exchange.get("request"))
        .ok_or("query request fixture missing")?
        .clone();
    request["operation"]["data"]["filter"]["evidence_states"] = json!(["CapturED_New"]);
    assert!(QueryRequest::parse(&serde_json::to_string(&request)?).is_err());
    Ok(())
}
