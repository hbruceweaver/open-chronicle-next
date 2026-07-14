mod common;

use std::error::Error;

use chronicle_mcp::{ActivityFilterParams, RangeParams, SearchParams};
use rmcp::handler::server::wrapper::Parameters;

#[tokio::test]
async fn search_result_matches_the_language_neutral_u2_golden() -> Result<(), Box<dyn Error>> {
    let fixture = common::fixture_server()?;
    let result = fixture
        .server
        .search(Parameters(SearchParams {
            filter: ActivityFilterParams {
                range: RangeParams {
                    start: "2026-07-13T09:00:00Z".to_owned(),
                    end: "2026-07-13T09:05:00Z".to_owned(),
                },
                application_bundle_id: None,
                window_text: None,
                authorized_domain: None,
                evidence_states: vec!["captured-new".to_owned(), "captured-unchanged".to_owned()],
            },
            query: "café 日本語".to_owned(),
            include_ocr: true,
            cursor: None,
            limit: 20,
        }))
        .await?;
    assert_eq!(result.is_error, Some(false));
    let actual = result
        .structured_content
        .ok_or("missing structured result")?;
    let packet: serde_json::Value = serde_json::from_str(&common::fixture("queries.json")?)?;
    let mut expected = packet["exchanges"][0]["response"]["result"].clone();
    expected["data"]["events"][0]["payload"]["data"]["content"]["data"]["image"]["state"] =
        serde_json::json!("expired");
    assert_eq!(actual["result"], expected);
    assert_eq!(actual["scope"]["ocr_included"], true);
    let encoded = actual.to_string();
    assert!(!encoded.contains("managed_relative_path"));
    assert!(!encoded.contains("screenshot_bytes"));
    Ok(())
}
