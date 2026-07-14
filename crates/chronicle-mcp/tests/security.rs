mod common;

use std::error::Error;

use chronicle_domain::{ContentClass, GrantId, GrantTimeScope, UtcRange};
use chronicle_engine::SharedService;
use chronicle_mcp::{
    ActivityFilterParams, ArtifactAuthorKindParam, ArtifactAuthorParams, ArtifactTypeParam,
    CreateArtifactParams, EventParams, EvidenceReferenceParams, RangeParams, SearchParams,
};
use chronicle_store::{ManagedRoot, StoreGeneration};
use chrono::{Duration, Utc};
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

fn filter() -> ActivityFilterParams {
    ActivityFilterParams {
        range: RangeParams {
            start: "2026-07-13T09:00:00Z".to_owned(),
            end: "2026-07-13T09:05:00Z".to_owned(),
        },
        application_bundle_id: None,
        window_text: None,
        authorized_domain: None,
        evidence_states: Vec::new(),
    }
}

fn error_code(result: &rmcp::model::CallToolResult) -> Option<&str> {
    result
        .structured_content
        .as_ref()?
        .get("error")?
        .get("code")?
        .as_str()
}

#[tokio::test]
async fn derived_write_without_grant_fails_closed_without_echoing_payload_or_registration()
-> Result<(), Box<dyn Error>> {
    let fixture = common::empty_server("client-write-secret", "grant-write-missing")?;
    let result = fixture
        .server
        .create_artifact(common::parameters(CreateArtifactParams {
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
        .await;
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

#[tokio::test]
async fn expired_and_revoked_grants_fail_on_the_next_request() -> Result<(), Box<dyn Error>> {
    let mut expired = common::fixture_grant()?;
    expired.expires_at = Utc::now() - Duration::seconds(1);
    let expired = common::seeded_fixture_server(expired)?;
    let result = expired.server.status().await;
    assert_eq!(error_code(&result), Some("grant-inactive"));

    let revoked = common::fixture_server()?;
    let service = SharedService::open_path(revoked._temporary.path().join("store"))?;
    service.revoke_grant(&GrantId::new("grant-synthetic")?, Utc::now())?;
    let result = revoked.server.status().await;
    assert_eq!(error_code(&result), Some("grant-inactive"));
    Ok(())
}

#[tokio::test]
async fn metadata_only_search_is_denied_without_disclosure_charge() -> Result<(), Box<dyn Error>> {
    let mut grant = common::fixture_grant()?;
    grant
        .content_classes
        .retain(|class| *class != ContentClass::Ocr);
    let fixture = common::seeded_fixture_server(grant)?;
    let result = fixture
        .server
        .search(common::parameters(SearchParams {
            filter: filter(),
            query: "synthetic".to_owned(),
            include_ocr: false,
            cursor: None,
            limit: 20,
        }))
        .await;
    assert_eq!(error_code(&result), Some("content-denied"));
    let service = SharedService::open_path(fixture._temporary.path().join("store"))?;
    assert_eq!(
        service
            .grant(&GrantId::new("grant-synthetic")?)?
            .disclosed_bytes,
        0
    );
    Ok(())
}

#[tokio::test]
async fn direct_out_of_scope_id_and_cursor_escape_reveal_no_evidence() -> Result<(), Box<dyn Error>>
{
    let mut grant = common::fixture_grant()?;
    grant.time_scope = GrantTimeScope::Absolute {
        range: UtcRange {
            start: "2026-07-13T09:00:00Z".parse()?,
            end: "2026-07-13T09:05:00Z".parse()?,
        },
    };
    let fixture = common::seeded_fixture_server(grant)?;
    let direct = fixture
        .server
        .get_event(common::parameters(EventParams {
            event_id: "evt-img-missing".to_owned(),
        }))
        .await;
    assert_eq!(error_code(&direct), Some("not-found"));
    let encoded = direct
        .structured_content
        .ok_or("missing error")?
        .to_string();
    assert!(!encoded.contains("evt-img-missing"));

    let first = fixture
        .server
        .search(common::parameters(SearchParams {
            filter: filter(),
            query: "synthetic".to_owned(),
            include_ocr: false,
            cursor: None,
            limit: 1,
        }))
        .await;
    assert_eq!(first.is_error, Some(false));
    let cursor = first
        .structured_content
        .as_ref()
        .and_then(|value| value["page"]["next_cursor"].as_str())
        .ok_or("fixture search did not paginate")?
        .to_owned();
    assert!(!cursor.contains("evt-"));
    let mut changed_filter = filter();
    changed_filter.window_text = Some("different scope".to_owned());
    let escaped = fixture
        .server
        .search(common::parameters(SearchParams {
            filter: changed_filter,
            query: "synthetic".to_owned(),
            include_ocr: false,
            cursor: Some(cursor),
            limit: 1,
        }))
        .await;
    assert_eq!(error_code(&escaped), Some("cursor-denied"));
    Ok(())
}

#[tokio::test]
async fn response_and_cumulative_caps_fail_without_committing_usage() -> Result<(), Box<dyn Error>>
{
    let mut grant = common::fixture_grant()?;
    grant.limits.max_response_bytes = 128;
    grant.limits.max_cumulative_bytes = 128;
    let fixture = common::seeded_fixture_server(grant)?;
    let result = fixture.server.status().await;
    assert_eq!(error_code(&result), Some("response-limit"));
    let service = SharedService::open_path(fixture._temporary.path().join("store"))?;
    assert_eq!(
        service
            .grant(&GrantId::new("grant-synthetic")?)?
            .disclosed_bytes,
        0
    );
    Ok(())
}

#[tokio::test]
async fn wrong_client_and_replaced_store_generation_fail_closed() -> Result<(), Box<dyn Error>> {
    let fixture = common::fixture_server()?;
    let root_path = fixture._temporary.path().join("store");
    let wrong_client = chronicle_mcp::ChronicleMcp::new(chronicle_mcp::ServerConfig::new(
        root_path.clone(),
        "client-not-on-receipt",
        "grant-synthetic",
    )?);
    let result = wrong_client.status().await;
    assert_eq!(error_code(&result), Some("grant-client-mismatch"));
    let encoded = result
        .structured_content
        .ok_or("missing wrong-client body")?
        .to_string();
    assert!(!encoded.contains("client-not-on-receipt"));

    let root = ManagedRoot::initialize(root_path)?;
    StoreGeneration::load(&root)?.increment(&root)?;
    let result = fixture.server.status().await;
    assert_eq!(error_code(&result), Some("stale-generation"));
    Ok(())
}
