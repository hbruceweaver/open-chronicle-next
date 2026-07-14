mod common;

use std::error::Error;

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
