use chronicle_domain::{
    ChunkRevision, DisclosureGrant, EventEnvelope, ImageArtifactId, QueryResponse,
};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn event_values() -> Result<Vec<Value>, Box<dyn Error>> {
    let events = fs::read_to_string(root().join("fixtures/synthetic/session-v1/events.jsonl"))?;
    events
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(key) || object.values().any(|child| has_key(child, key))
        }
        Value::Array(array) => array.iter().any(|child| has_key(child, key)),
        _ => false,
    }
}

fn protected_or_no_evidence(values: &[Value], content_type: &str) -> Option<Value> {
    values
        .iter()
        .find(|value| {
            value
                .pointer("/payload/data/content/type")
                .and_then(Value::as_str)
                == Some(content_type)
        })
        .cloned()
}

#[test]
fn protected_and_no_evidence_payloads_reject_sensitive_or_freeform_fields()
-> Result<(), Box<dyn Error>> {
    let events = event_values()?;
    let protected =
        protected_or_no_evidence(&events, "protected").ok_or("protected fixture missing")?;
    let no_evidence =
        protected_or_no_evidence(&events, "no-evidence").ok_or("no-evidence fixture missing")?;

    for (base, field, value) in [
        (
            protected.clone(),
            "window_title",
            json!("Excluded secret title"),
        ),
        (protected.clone(), "ocr", json!({"text": "secret"})),
        (protected, "image", json!({"artifact_id": "img-secret"})),
        (
            no_evidence.clone(),
            "application_bundle_id",
            json!("com.secret.app"),
        ),
        (
            no_evidence,
            "factual_detail",
            json!("freeform data can smuggle a title"),
        ),
    ] {
        let mut mutated = base;
        let data = mutated
            .pointer_mut("/payload/data/content/data")
            .and_then(Value::as_object_mut)
            .ok_or("coarse payload data missing")?;
        data.insert(field.to_owned(), value);
        assert!(
            EventEnvelope::parse(&serde_json::to_string(&mutated)?).is_err(),
            "accepted field {field}"
        );
    }

    let mut outer_smuggle = protected_or_no_evidence(&events, "protected")
        .ok_or("protected fixture missing for outer smuggle")?;
    outer_smuggle["payload"]["data"]["window_title"] = json!("outer secret title");
    assert!(EventEnvelope::parse(&serde_json::to_string(&outer_smuggle)?).is_err());
    Ok(())
}

#[test]
fn ocr_marker_cannot_be_false_and_ocr_axes_match_text() -> Result<(), Box<dyn Error>> {
    let mut captured = event_values()?
        .into_iter()
        .next()
        .ok_or("event fixture empty")?;
    captured["payload"]["data"]["content"]["data"]["ocr"]["untrusted_evidence"] = json!(false);
    assert!(EventEnvelope::parse(&serde_json::to_string(&captured)?).is_err());

    let mut empty_with_text = event_values()?
        .into_iter()
        .find(|value| {
            value
                .pointer("/payload/data/ocr_state")
                .and_then(Value::as_str)
                == Some("empty")
        })
        .ok_or("empty OCR fixture missing")?;
    empty_with_text["payload"]["data"]["content"]["data"]["ocr"]["text"] = json!("not empty");
    assert!(EventEnvelope::parse(&serde_json::to_string(&empty_with_text)?).is_err());

    let captured = event_values()?
        .into_iter()
        .next()
        .ok_or("event fixture empty")?;
    for required in [
        "engine",
        "automatic_language_detection",
        "recognition_languages",
    ] {
        let mut missing = captured.clone();
        missing["payload"]["data"]["content"]["data"]["ocr"]
            .as_object_mut()
            .ok_or("OCR fixture is not an object")?
            .remove(required);
        assert!(
            EventEnvelope::parse(&serde_json::to_string(&missing)?).is_err(),
            "accepted OCR without required provenance field {required}"
        );
    }

    let mut empty_engine = captured.clone();
    empty_engine["payload"]["data"]["content"]["data"]["ocr"]["engine"]["adapter"] = json!("");
    assert!(EventEnvelope::parse(&serde_json::to_string(&empty_engine)?).is_err());

    let mut empty_language = captured;
    empty_language["payload"]["data"]["content"]["data"]["ocr"]["recognition_languages"] =
        json!([""]);
    assert!(EventEnvelope::parse(&serde_json::to_string(&empty_language)?).is_err());

    let captured = event_values()?
        .into_iter()
        .next()
        .ok_or("event fixture empty")?;
    let mut unknown_ocr = captured.clone();
    unknown_ocr["payload"]["data"]["content"]["data"]["ocr"]["detected_language"] = json!("en-US");
    assert!(
        EventEnvelope::parse(&serde_json::to_string(&unknown_ocr)?).is_err(),
        "accepted unknown OCR field despite closed schema"
    );

    let mut unknown_engine = captured;
    unknown_engine["payload"]["data"]["content"]["data"]["ocr"]["engine"]["model_name"] =
        json!("private-model");
    assert!(
        EventEnvelope::parse(&serde_json::to_string(&unknown_engine)?).is_err(),
        "accepted unknown OCR engine field despite closed schema"
    );
    Ok(())
}

#[test]
fn cross_axis_mismatches_are_rejected() -> Result<(), Box<dyn Error>> {
    let captured = event_values()?
        .into_iter()
        .next()
        .ok_or("event fixture empty")?;
    let mut skipped_capture = captured.clone();
    skipped_capture["payload"]["data"]["attempt_status"] = json!("skipped");
    assert!(EventEnvelope::parse(&serde_json::to_string(&skipped_capture)?).is_err());

    let mut locked_capture = captured.clone();
    locked_capture["payload"]["data"]["presence_state"] = json!("locked");
    assert!(EventEnvelope::parse(&serde_json::to_string(&locked_capture)?).is_err());

    let mut active_with_idle_seconds = captured;
    active_with_idle_seconds["payload"]["data"]["idle_seconds"] = json!(30);
    assert!(EventEnvelope::parse(&serde_json::to_string(&active_with_idle_seconds)?).is_err());

    let mut unavailable = event_values()?
        .into_iter()
        .find(|value| {
            value
                .pointer("/payload/data/evidence_state")
                .and_then(Value::as_str)
                == Some("unavailable")
        })
        .ok_or("unavailable fixture missing")?;
    unavailable["payload"]["data"]["content"]["data"]["reason"] = json!("user-paused");
    assert!(EventEnvelope::parse(&serde_json::to_string(&unavailable)?).is_err());

    let mut locked_with_unknown_presence = unavailable;
    locked_with_unknown_presence["payload"]["data"]["content"]["data"]["reason"] = json!("locked");
    assert!(EventEnvelope::parse(&serde_json::to_string(&locked_with_unknown_presence)?).is_err());

    let mut unchanged = event_values()?
        .into_iter()
        .find(|value| {
            value
                .pointer("/payload/data/evidence_state")
                .and_then(Value::as_str)
                == Some("captured-unchanged")
        })
        .ok_or("unchanged fixture missing")?;
    unchanged["payload"]["data"]["content"]["data"]["content_hash"] = json!("");
    unchanged["payload"]["data"]["content"]["data"]["context"]["process_name"] = json!("");
    assert!(EventEnvelope::parse(&serde_json::to_string(&unchanged)?).is_err());
    Ok(())
}

#[test]
fn capture_artifact_and_lifecycle_invariants_are_enforced() -> Result<(), Box<dyn Error>> {
    let captured = event_values()?
        .into_iter()
        .next()
        .ok_or("event fixture empty")?;

    let mut invalid_confidence = captured.clone();
    invalid_confidence["payload"]["data"]["content"]["data"]["ocr"]["confidence"] = json!(1.5);
    assert!(EventEnvelope::parse(&serde_json::to_string(&invalid_confidence)?).is_err());

    let mut oversized = captured.clone();
    oversized["payload"]["data"]["content"]["data"]["image"]["dimensions"]["width"] = json!(2561);
    assert!(EventEnvelope::parse(&serde_json::to_string(&oversized)?).is_err());

    let mut mismatched_hash = captured.clone();
    mismatched_hash["payload"]["data"]["content"]["data"]["image"]["content_hash"] =
        json!("sha256-other");
    assert!(EventEnvelope::parse(&serde_json::to_string(&mismatched_hash)?).is_err());

    let mut expired_before_recording = captured;
    expired_before_recording["payload"]["data"]["content"]["data"]["image"]["expires_at"] =
        json!("2026-07-13T09:00:00Z");
    assert!(EventEnvelope::parse(&serde_json::to_string(&expired_before_recording)?).is_err());

    let mut lifecycle = event_values()?
        .into_iter()
        .find(|value| value.get("kind").and_then(Value::as_str) == Some("screenshot-lifecycle"))
        .ok_or("lifecycle fixture missing")?;
    lifecycle["payload"]["data"]["projected_state"] = json!("user-deleted");
    assert!(EventEnvelope::parse(&serde_json::to_string(&lifecycle)?).is_err());
    Ok(())
}

#[test]
fn query_images_are_opaque_and_cannot_serialize_managed_paths() -> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&fs::read_to_string(
        root().join("fixtures/synthetic/session-v1/queries.json"),
    )?)?;
    let response_value = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .and_then(|exchanges| exchanges.first())
        .and_then(|exchange| exchange.get("response"))
        .ok_or("response fixture missing")?;
    let response = QueryResponse::parse(&serde_json::to_string(response_value)?)?;
    let serialized = serde_json::to_value(response)?;
    for forbidden in ["managed_relative_path", "path", "bytes", "image_bytes"] {
        assert!(
            !has_key(&serialized, forbidden),
            "query response exposed {forbidden}"
        );
    }
    assert!(has_key(&serialized, "coverage"));
    assert!(has_key(&serialized, "gaps"));

    let mut smuggled_path = response_value.clone();
    smuggled_path["result"]["data"]["managed_relative_path"] = json!("screenshots/synthetic.heic");
    assert!(QueryResponse::parse(&serde_json::to_string(&smuggled_path)?).is_err());

    let artifact_response = packet
        .get("exchanges")
        .and_then(Value::as_array)
        .and_then(|exchanges| exchanges.get(2))
        .and_then(|exchange| exchange.get("response"))
        .ok_or("artifact response fixture missing")?;
    let mut nested_path = artifact_response.clone();
    nested_path["result"]["data"]["artifact"]["payload"]["path"] =
        json!("screenshots/private.heic");
    assert!(QueryResponse::parse(&serde_json::to_string(&nested_path)?).is_err());
    Ok(())
}

#[test]
fn opaque_image_ids_reject_path_shaped_values() {
    assert!(serde_json::from_value::<ImageArtifactId>(json!("img-opaque-001")).is_ok());
    assert!(serde_json::from_value::<ImageArtifactId>(json!("/synthetic-root/img.heic")).is_err());
    assert!(serde_json::from_value::<ImageArtifactId>(json!("screenshots/img.heic")).is_err());
}

#[test]
fn metadata_only_grant_does_not_authorize_ocr() -> Result<(), Box<dyn Error>> {
    let packet: Value = serde_json::from_str(&fs::read_to_string(
        root().join("fixtures/synthetic/session-v1/queries.json"),
    )?)?;
    let mut grant_value = packet.get("grant").ok_or("grant fixture missing")?.clone();
    grant_value["content_classes"] = json!(["metadata"]);
    let grant: DisclosureGrant = serde_json::from_value(grant_value)?;
    grant.validate()?;
    let during_grant = Utc
        .with_ymd_and_hms(2026, 7, 13, 9, 0, 0)
        .single()
        .ok_or("invalid test time")?;
    assert!(!grant.allows_full_ocr_at(during_grant));
    Ok(())
}

#[test]
fn canonical_event_and_chunk_keys_do_not_smuggle_analysis_fields() -> Result<(), Box<dyn Error>> {
    let forbidden = [
        "project",
        "workflow",
        "productivity",
        "intent",
        "recommendation",
    ];
    for event in event_values()? {
        let parsed = EventEnvelope::parse(&serde_json::to_string(&event)?)?;
        assert_no_forbidden_keys(&serde_json::to_value(parsed)?, &forbidden);
    }
    let chunks = fs::read_to_string(root().join("fixtures/synthetic/session-v1/chunks.jsonl"))?;
    for line in chunks.lines() {
        let parsed = ChunkRevision::parse(line)?;
        assert_no_forbidden_keys(&serde_json::to_value(parsed)?, &forbidden);
    }
    Ok(())
}

fn assert_no_forbidden_keys(value: &Value, forbidden: &[&str]) {
    match value {
        Value::Object(object) => {
            for key in object.keys() {
                assert!(
                    !forbidden.contains(&key.as_str()),
                    "forbidden canonical key {key}"
                );
            }
            for child in object.values() {
                assert_no_forbidden_keys(child, forbidden);
            }
        }
        Value::Array(array) => {
            for child in array {
                assert_no_forbidden_keys(child, forbidden);
            }
        }
        _ => {}
    }
}
