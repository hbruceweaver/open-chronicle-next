mod common;

use std::error::Error;
use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn bundled_binary_initializes_lists_and_serves_over_real_stdio() -> Result<(), Box<dyn Error>>
{
    let fixture = common::fixture_server()?;
    let root = fixture._temporary.path().join("store");
    let command =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chronicle-mcp")).configure(|command| {
            command
                .arg("--managed-root")
                .arg(root)
                .arg("--client-id")
                .arg("client-codex-synthetic")
                .arg("--grant-id")
                .arg("grant-synthetic");
        });
    let (transport, stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = stderr.ok_or("stderr pipe missing")?;
    let stderr_task = tokio::spawn(async move {
        let mut output = String::new();
        stderr.read_to_string(&mut output).await?;
        Ok::<_, std::io::Error>(output)
    });

    let client = tokio::time::timeout(Duration::from_secs(10), ().serve(transport)).await??;
    let tools = tokio::time::timeout(Duration::from_secs(10), client.list_all_tools()).await??;
    assert_eq!(tools.len(), 16);
    assert!(tools.iter().any(|tool| tool.name == "chronicle_status"));
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "chronicle_create_artifact")
    );

    let resources =
        tokio::time::timeout(Duration::from_secs(10), client.list_all_resources()).await??;
    assert_eq!(resources.len(), 5);
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == "chronicle://status/v1")
    );
    let schema = tokio::time::timeout(
        Duration::from_secs(10),
        client.read_resource(ReadResourceRequestParams::new(
            "chronicle://schemas/event/v1",
        )),
    )
    .await??;
    assert_eq!(schema.contents.len(), 1);

    let status = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(CallToolRequestParams::new("chronicle_status")),
    )
    .await??;
    assert_eq!(status.is_error, Some(false));
    assert!(status.structured_content.is_some());

    client.cancel().await?;
    let stderr_output = tokio::time::timeout(Duration::from_secs(10), stderr_task).await???;
    assert!(
        stderr_output.is_empty(),
        "unexpected stderr: {stderr_output}"
    );
    Ok(())
}
