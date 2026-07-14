use chronicle_domain::{
    DiagnosticHealthSnapshot, DurableAcknowledgement, HealthOperationTimes, McpHealthSummary,
    ProjectionHealth, RequestId, ScreenshotRetentionHealthSummary, SharedServiceOperation,
    SharedServiceRequest, SharedServiceResponse, SharedServiceResult, StorageHealthSummary,
    StudyHealthState, StudyHealthSummary,
};
use chrono::Utc;

fn healthy_snapshot(observed_at: chrono::DateTime<Utc>) -> DiagnosticHealthSnapshot {
    DiagnosticHealthSnapshot {
        schema_version: "1.0".to_owned(),
        observed_at,
        store_generation: 1,
        projection: ProjectionHealth::Current,
        acknowledgement: DurableAcknowledgement::Durable,
        latest: HealthOperationTimes::default(),
        aggregation_watermark: None,
        aggregation_pending_buckets: 0,
        projection_lag_seconds: 0,
        projection_pending_records: 0,
        storage: StorageHealthSummary {
            managed_bytes: 0,
            available_bytes: 1,
        },
        study: StudyHealthSummary {
            state: StudyHealthState::Personal,
            start: None,
            end: None,
            expired_at: None,
        },
        screenshot_retention: ScreenshotRetentionHealthSummary::default(),
        mcp: McpHealthSummary {
            active_grants: 0,
            revoked_grants: 0,
            expired_grants: 0,
            exhausted_grants: 0,
            stale_generation_grants: 0,
        },
        issues: Vec::new(),
    }
}

#[test]
fn shared_requests_are_versioned_bounded_and_have_no_mutation_surface() {
    let request = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("health-request").expect("valid request ID"),
        store_generation: 1,
        operation: SharedServiceOperation::Health,
    };
    let json = serde_json::to_string(&request).expect("serialize request");
    assert_eq!(
        SharedServiceRequest::parse(&json).expect("parse request"),
        request
    );

    let oversized = format!("{}{}", json, " ".repeat(128 * 1024));
    assert!(SharedServiceRequest::parse(&oversized).is_err());

    let serialized = serde_json::to_value(request).expect("serialize request value");
    let operation = serialized["operation"]["type"]
        .as_str()
        .expect("tagged operation");
    assert_eq!(operation, "health");
    assert!(!serialized.to_string().contains("delete"));
    assert!(!serialized.to_string().contains("privacy"));
    assert!(!serialized.to_string().contains("pause"));
}

#[test]
fn diagnostic_health_contract_has_no_user_content_fields() {
    let health = healthy_snapshot(Utc::now());
    health.validate().expect("valid content-free health");
    let json = serde_json::to_string(&health).expect("serialize health");
    for forbidden in [
        "ocr_text",
        "window_title",
        "managed_relative_path",
        "image_artifact_id",
        ".heic",
        "application_bundle_id",
        "/Users/",
    ] {
        assert!(!json.contains(forbidden), "health leaked {forbidden}");
    }
}

#[test]
fn shared_response_rejects_contradictory_projection_health() {
    let generated_at = Utc::now();
    let contradictions = [
        {
            let mut health = healthy_snapshot(generated_at);
            health.projection_pending_records = 1;
            health
        },
        {
            let mut health = healthy_snapshot(generated_at);
            health.projection_lag_seconds = 1;
            health
        },
        {
            let mut health = healthy_snapshot(generated_at);
            health.projection = ProjectionHealth::Lagging;
            health.acknowledgement = DurableAcknowledgement::JournalDurableProjectionPending;
            health
        },
        {
            let mut health = healthy_snapshot(generated_at);
            health.projection = ProjectionHealth::Lagging;
            health.projection_pending_records = 1;
            health
        },
        {
            let mut health = healthy_snapshot(generated_at);
            health.projection = ProjectionHealth::Blocked;
            health
        },
    ];
    for health in contradictions {
        let response = SharedServiceResponse {
            schema_version: "1.0".to_owned(),
            request_id: RequestId::new("contradictory-health").expect("request ID"),
            generated_at,
            store_generation: 1,
            result: SharedServiceResult::Health(Box::new(health)),
        };
        let json = serde_json::to_string(&response).expect("serialize contradictory health");
        assert!(SharedServiceResponse::parse(&json).is_err());
    }
}

#[test]
fn shared_transport_rejects_unknown_path_or_image_byte_fields() {
    let request = SharedServiceRequest {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("health-request").expect("valid request ID"),
        store_generation: 1,
        operation: SharedServiceOperation::Health,
    };
    let mut value = serde_json::to_value(request).expect("serialize request");
    value["future_optional"] = serde_json::json!({"path": "/private/example/secret.heic"});
    assert!(SharedServiceRequest::parse(&value.to_string()).is_err());

    value["future_optional"] = serde_json::json!({"image_bytes": "AAAA"});
    assert!(SharedServiceRequest::parse(&value.to_string()).is_err());

    let generated_at = Utc::now();
    let response = SharedServiceResponse {
        schema_version: "1.0".to_owned(),
        request_id: RequestId::new("health-response").expect("request ID"),
        generated_at,
        store_generation: 1,
        result: SharedServiceResult::Health(Box::new(healthy_snapshot(generated_at))),
    };
    let mut response = serde_json::to_value(response).expect("serialize response");
    response["future_optional"] = serde_json::json!({"managed_relative_path": "screenshots/x"});
    assert!(SharedServiceResponse::parse(&response.to_string()).is_err());
}
