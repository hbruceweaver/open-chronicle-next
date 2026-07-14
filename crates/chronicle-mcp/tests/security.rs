mod common;

use std::error::Error;

use chronicle_mcp::{
    ArtifactAuthorKindParam, ArtifactAuthorParams, ArtifactTypeParam, CreateArtifactParams,
    EvidenceReferenceParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

#[tokio::test]
async fn missing_grant_fails_closed_without_leaking_registration_or_paths()
-> Result<(), Box<dyn Error>> {
    let fixture = common::empty_server("client-secret-shaped", "grant-missing")?;
    let result = fixture.server.status().await;
    assert_eq!(result.is_error, Some(true));
    let value = result.structured_content.ok_or("missing error body")?;
    assert_eq!(value["error"]["code"], "grant-not-found");
    let encoded = value.to_string();
    assert!(!encoded.contains("client-secret-shaped"));
    assert!(!encoded.contains("grant-missing"));
    assert!(!encoded.contains(fixture._temporary.path().to_string_lossy().as_ref()));
    Ok(())
}

#[tokio::test]
async fn derived_write_without_grant_fails_closed_without_echoing_payload_or_registration()
-> Result<(), Box<dyn Error>> {
    let fixture = common::empty_server("client-write-secret", "grant-write-missing")?;
    let result = fixture
        .server
        .create_artifact(Parameters(CreateArtifactParams {
            request_id: "write-without-grant".to_owned(),
            artifact_id: "artifact-without-grant".to_owned(),
            revision_id: "artifact-without-grant-revision".to_owned(),
            artifact_type: ArtifactTypeParam::Annotation,
            author: ArtifactAuthorParams {
                kind: ArtifactAuthorKindParam::McpClient,
                display_name: None,
                model: None,
            },
            payload: json!({"secret": "must-not-be-echoed"}),
            evidence: EvidenceReferenceParams {
                event_ids: vec!["evt-secret".to_owned()],
                chunk_ids: Vec::new(),
            },
            confidence: None,
        }))
        .await?;
    assert_eq!(result.is_error, Some(true));
    let encoded = result
        .structured_content
        .ok_or("missing error body")?
        .to_string();
    assert!(encoded.contains("grant-not-found"));
    for forbidden in [
        "client-write-secret",
        "grant-write-missing",
        "must-not-be-echoed",
        fixture._temporary.path().to_string_lossy().as_ref(),
    ] {
        assert!(!encoded.contains(forbidden), "leaked {forbidden}");
    }
    Ok(())
}
