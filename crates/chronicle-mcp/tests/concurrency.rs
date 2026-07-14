mod common;

use std::error::Error;

use chronicle_mcp::{
    ArtifactAuthorKindParam, ArtifactAuthorParams, ArtifactStatusParam, ArtifactTypeParam,
    ChronicleMcp, CreateArtifactParams, EvidenceReferenceParams, ReviseArtifactParams,
    ServerConfig,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;

fn author() -> ArtifactAuthorParams {
    ArtifactAuthorParams {
        kind: ArtifactAuthorKindParam::Model,
        display_name: Some("Concurrent analyst".to_owned()),
        model: Some("synthetic-model".to_owned()),
    }
}

fn evidence() -> EvidenceReferenceParams {
    EvidenceReferenceParams {
        event_ids: vec!["evt-090015".to_owned()],
        chunk_ids: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_servers_revising_one_expected_prior_have_exactly_one_winner()
-> Result<(), Box<dyn Error>> {
    let fixture = common::fixture_server_for_writes()?;
    let created = fixture
        .server
        .create_artifact(Parameters(CreateArtifactParams {
            request_id: "race-create-request".to_owned(),
            artifact_id: "race-artifact".to_owned(),
            revision_id: "race-base".to_owned(),
            artifact_type: ArtifactTypeParam::Hypothesis,
            author: author(),
            payload: json!({"claim": "base"}),
            evidence: evidence(),
            confidence: Some(0.5),
        }))
        .await?;
    assert_eq!(created.is_error, Some(false));

    let second = ChronicleMcp::new(ServerConfig::new(
        fixture._temporary.path().join("store"),
        "client-codex-synthetic",
        "grant-synthetic",
    )?);
    let first_revision = ReviseArtifactParams {
        request_id: "race-child-request-a".to_owned(),
        artifact_id: "race-artifact".to_owned(),
        revision_id: "race-child-a".to_owned(),
        expected_prior_revision_id: "race-base".to_owned(),
        artifact_type: ArtifactTypeParam::Hypothesis,
        author: author(),
        status: ArtifactStatusParam::Accepted,
        payload: json!({"claim": "child a"}),
        evidence: evidence(),
        confidence: Some(0.7),
    };
    let second_revision = ReviseArtifactParams {
        request_id: "race-child-request-b".to_owned(),
        revision_id: "race-child-b".to_owned(),
        payload: json!({"claim": "child b"}),
        ..first_revision.clone()
    };
    let (first, second_result) = tokio::join!(
        fixture.server.revise_artifact(Parameters(first_revision)),
        second.revise_artifact(Parameters(second_revision))
    );
    let results = [first?, second_result?];
    assert_eq!(
        results
            .iter()
            .filter(|result| result.is_error == Some(false))
            .count(),
        1
    );
    let conflict = results
        .iter()
        .find(|result| result.is_error == Some(true))
        .and_then(|result| result.structured_content.as_ref())
        .ok_or("missing conflict response")?;
    assert_eq!(conflict["error"]["code"], "artifact-conflict");
    Ok(())
}
