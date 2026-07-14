mod common;

use std::error::Error;

use chronicle_domain::QueryOperation;
use chronicle_mcp::{ChronicleMcp, CurrentContextParams, ListChunksParams};
use rmcp::ServerHandler;

#[test]
fn initialization_and_tool_inventory_are_restrictive() -> Result<(), Box<dyn Error>> {
    let fixture = common::empty_server("client-protocol", "grant-protocol")?;
    let info = fixture.server.get_info();
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());
    assert!(info.capabilities.prompts.is_none());
    assert!(info.capabilities.logging.is_none());
    assert!(
        info.instructions
            .as_deref()
            .is_some_and(|value| value.contains("untrusted evidence"))
    );

    let mut tools = ChronicleMcp::tool_router().list_all();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "chronicle_compare_periods",
            "chronicle_context_packet",
            "chronicle_create_artifact",
            "chronicle_get_artifact",
            "chronicle_get_chunk",
            "chronicle_get_current_context",
            "chronicle_get_event",
            "chronicle_inspect_moment",
            "chronicle_list_artifacts",
            "chronicle_list_chunks",
            "chronicle_revise_artifact",
            "chronicle_search",
            "chronicle_set_artifact_status",
            "chronicle_statistics",
            "chronicle_status",
            "chronicle_supporting_evidence",
        ]
    );
    let retry_safe_writes = ["chronicle_create_artifact", "chronicle_revise_artifact"];
    let status_write = "chronicle_set_artifact_status";
    for tool in &tools {
        let annotations = tool.annotations.as_ref().ok_or("missing annotations")?;
        let write = retry_safe_writes.contains(&tool.name.as_ref()) || tool.name == status_write;
        assert_eq!(annotations.read_only_hint, Some(!write));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(
            annotations.idempotent_hint,
            Some(retry_safe_writes.contains(&tool.name.as_ref()))
        );
        assert_eq!(annotations.open_world_hint, Some(false));
    }
    let serialized = serde_json::to_string(&tools)?;
    for forbidden in [
        "screenshot_bytes",
        "managed_relative_path",
        "raw_sql",
        "delete_evidence",
        "factory_reset",
        "capture_screen",
        "pause_capture",
    ] {
        assert!(!serialized.contains(forbidden), "exposed {forbidden}");
    }
    Ok(())
}

#[test]
fn tool_inputs_reject_unknown_fields_and_out_of_bounds_pages() -> Result<(), Box<dyn Error>> {
    let unknown = serde_json::json!({
        "filter": {
            "range": {
                "start": "2026-07-13T09:00:00Z",
                "end": "2026-07-13T09:05:00Z"
            },
            "unexpected_scope": "deny me"
        },
        "limit": 20
    });
    assert!(serde_json::from_value::<ListChunksParams>(unknown).is_err());

    let oversized: ListChunksParams = serde_json::from_value(serde_json::json!({
        "filter": {
            "range": {
                "start": "2026-07-13T09:00:00Z",
                "end": "2026-07-13T09:05:00Z"
            }
        },
        "limit": 101
    }))?;
    assert_eq!(
        oversized.operation().expect_err("limit must fail").code(),
        "invalid-input"
    );
    Ok(())
}

#[test]
fn current_context_uses_the_last_fully_completed_five_minute_bucket() -> Result<(), Box<dyn Error>>
{
    let operation = CurrentContextParams {
        include_ocr: false,
        max_bytes: 4096,
    }
    .operation("2026-07-13T09:07:43Z".parse()?)?;
    let QueryOperation::BuildContextPacket {
        filter,
        include_ocr,
        max_bytes,
    } = operation
    else {
        return Err("expected context packet operation".into());
    };
    assert_eq!(filter.range.start.to_rfc3339(), "2026-07-13T09:00:00+00:00");
    assert_eq!(filter.range.end.to_rfc3339(), "2026-07-13T09:05:00+00:00");
    assert!(!include_ocr);
    assert_eq!(max_bytes, 4096);
    Ok(())
}
