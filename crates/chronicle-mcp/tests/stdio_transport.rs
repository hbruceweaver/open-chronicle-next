mod common;

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use chronicle_domain::{EventEnvelope, EventId, GrantId, UtcRange};
use chronicle_engine::SharedService;
use chronicle_store::{
    ArtifactStore, CanonicalJournal, FaultInjector, JournalFamily, ManagedRoot, Projector,
    SqliteStore, StoreGeneration, StoreQueries,
};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, JsonObject, ReadResourceRequestParams};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::io::AsyncReadExt;

fn child_transport(root: &Path) -> Result<TokioChildProcess, Box<dyn Error>> {
    Ok(TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chronicle-mcp")).configure(|command| {
            command
                .arg("--managed-root")
                .arg(root)
                .arg("--client-id")
                .arg("client-codex-synthetic")
                .arg("--grant-id")
                .arg("grant-synthetic");
        }),
    )?)
}

fn arguments(value: serde_json::Value) -> Result<JsonObject, Box<dyn Error>> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "tool arguments must be an object".into())
}

#[tokio::test]
async fn bundled_binary_initializes_lists_and_serves_over_real_stdio() -> Result<(), Box<dyn Error>>
{
    let fixture = common::fixture_server()?;
    let root = fixture._temporary.path().join("store");
    let command =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_chronicle-mcp")).configure(|command| {
            command
                .arg("--managed-root")
                .arg(&root)
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
    assert_eq!(resources.len(), 6);
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == "chronicle://status/v1")
    );
    let grant_id = GrantId::new("grant-synthetic")?;
    let service = SharedService::open_path(&root)?;
    let bytes_before_schema = service.grant(&grant_id)?.disclosed_bytes;
    let schema = tokio::time::timeout(
        Duration::from_secs(10),
        client.read_resource(ReadResourceRequestParams::new(
            "chronicle://schemas/event/v1",
        )),
    )
    .await??;
    assert_eq!(schema.contents.len(), 1);
    let shared_schema = tokio::time::timeout(
        Duration::from_secs(10),
        client.read_resource(ReadResourceRequestParams::new(
            "chronicle://schemas/shared-service/v1",
        )),
    )
    .await??;
    assert_eq!(shared_schema.contents.len(), 1);
    assert_eq!(
        SharedService::open_path(&root)?
            .grant(&grant_id)?
            .disclosed_bytes,
        bytes_before_schema,
        "bundled public contract schemas must be explicitly unmetered"
    );

    let bytes_before_status_resource = SharedService::open_path(&root)?
        .grant(&grant_id)?
        .disclosed_bytes;
    let status_resource = tokio::time::timeout(
        Duration::from_secs(10),
        client.read_resource(ReadResourceRequestParams::new("chronicle://status/v1")),
    )
    .await??;
    let bytes_after_status_resource = SharedService::open_path(&root)?
        .grant(&grant_id)?
        .disclosed_bytes;
    let status_resource_bytes = serde_json::to_vec(&status_resource)?.len() as u64;
    assert!(
        status_resource_bytes <= bytes_after_status_resource - bytes_before_status_resource,
        "serialized status resource ({status_resource_bytes}) exceeded its disclosure charge ({})",
        bytes_after_status_resource - bytes_before_status_resource
    );

    let status = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(CallToolRequestParams::new("chronicle_status")),
    )
    .await??;
    assert_eq!(status.is_error, Some(false));
    assert!(status.structured_content.is_some());
    assert!(
        status.content.is_empty(),
        "structured JSON was duplicated as text"
    );
    let disclosed_bytes = status
        .structured_content
        .as_ref()
        .and_then(|value| value["grant"]["disclosed_bytes"].as_u64())
        .ok_or("status omitted disclosure accounting")?;
    let serialized_bytes = serde_json::to_vec(&status)?.len() as u64;
    assert!(
        serialized_bytes <= disclosed_bytes,
        "serialized MCP output ({serialized_bytes}) exceeded charged disclosure ({disclosed_bytes})"
    );
    assert_eq!(
        status
            .structured_content
            .as_ref()
            .and_then(|value| value["result"]["data"]["has_recorded_evidence"].as_bool()),
        Some(true)
    );
    assert!(
        status
            .structured_content
            .as_ref()
            .and_then(|value| value["result"]["data"].as_object())
            .is_some_and(|status| !status.contains_key("recording_available"))
    );

    let malformed = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(
            CallToolRequestParams::new("chronicle_list_chunks").with_arguments(arguments(
                serde_json::json!({
                    "filter": {
                        "range": {
                            "start": "2026-07-13T09:00:00Z",
                            "end": "2026-07-13T09:05:00Z"
                        }
                    },
                    "limit": 20,
                    "SECRET_OCR_FIELD": "SECRET_OCR_VALUE"
                }),
            )?),
        ),
    )
    .await??;
    assert_eq!(malformed.is_error, Some(true));
    assert!(malformed.content.is_empty());
    assert_eq!(
        malformed
            .structured_content
            .as_ref()
            .and_then(|value| value["error"]["code"].as_str()),
        Some("invalid-input")
    );
    let malformed_wire = serde_json::to_string(&malformed)?;
    for secret in ["SECRET_OCR_FIELD", "SECRET_OCR_VALUE", "unknown field"] {
        assert!(
            !malformed_wire.contains(secret),
            "invalid input leaked {secret}"
        );
    }

    let search = tokio::time::timeout(
        Duration::from_secs(10),
        client.call_tool(
            CallToolRequestParams::new("chronicle_search").with_arguments(arguments(
                serde_json::json!({
                    "filter": {
                        "range": {
                            "start": "2026-07-13T09:00:00Z",
                            "end": "2026-07-13T09:05:00Z"
                        }
                    },
                    "query": "synthetic",
                    "include_ocr": false,
                    "limit": 20
                }),
            )?),
        ),
    )
    .await??;
    assert_eq!(search.is_error, Some(false));
    assert!(search.content.is_empty());
    assert_eq!(
        search
            .structured_content
            .as_ref()
            .and_then(|value| value["scope"]["ocr_included"].as_bool()),
        Some(false),
        "include_ocr=false must suppress returned OCR, not OCR-index matching"
    );
    assert!(
        search
            .structured_content
            .as_ref()
            .and_then(|value| value["result"]["data"]["events"].as_array())
            .is_some_and(|items| !items.is_empty()),
        "OCR-index search semantics changed when returned OCR was suppressed"
    );

    client.cancel().await?;
    let stderr_output = tokio::time::timeout(Duration::from_secs(10), stderr_task).await???;
    assert!(
        stderr_output.is_empty(),
        "unexpected stderr: {stderr_output}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_real_servers_share_reads_app_projection_and_immutable_revision_conflicts()
-> Result<(), Box<dyn Error>> {
    let fixture = common::fixture_server_for_writes()?;
    let root_path = fixture._temporary.path().join("store");
    let initial_root = ManagedRoot::initialize(&root_path)?;
    let initial_sqlite = SqliteStore::open(initial_root)?;
    let range = UtcRange {
        start: "2026-07-13T09:00:00Z".parse()?,
        end: "2026-07-13T09:05:00Z".parse()?,
    };
    let initial_chunks = StoreQueries::new(initial_sqlite)
        .current_chunks_in_range(&range)?
        .len();
    let client_a = tokio::time::timeout(
        Duration::from_secs(10),
        ().serve(child_transport(&root_path)?),
    )
    .await??;
    let client_b = tokio::time::timeout(
        Duration::from_secs(10),
        ().serve(child_transport(&root_path)?),
    )
    .await??;

    let list_arguments = arguments(serde_json::json!({
        "filter": {
            "range": {
                "start": "2026-07-13T09:00:00Z",
                "end": "2026-07-13T09:05:00Z"
            }
        },
        "limit": 20
    }))?;
    let writer_root = root_path.clone();
    let app_writer = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let root = ManagedRoot::initialize(&writer_root).map_err(|error| error.to_string())?;
        let sqlite = SqliteStore::open(root.clone()).map_err(|error| error.to_string())?;
        let projector = Projector::new(sqlite);
        let source = common::fixture("events.jsonl").map_err(|error| error.to_string())?;
        let line = source
            .lines()
            .find(|line| line.contains("evt-gap-sleep"))
            .ok_or_else(|| "gap fixture missing".to_owned())?;
        let mut event = EventEnvelope::parse(line).map_err(|error| error.to_string())?;
        event.event_id =
            EventId::new("evt-concurrent-app-writer").map_err(|error| error.to_string())?;
        let record = CanonicalJournal::new(root)
            .append_event(&event, FaultInjector::none())
            .map_err(|error| error.to_string())?;
        projector
            .project_record(&record, FaultInjector::none())
            .map_err(|error| error.to_string())?;
        Ok(())
    });
    let (read_a, read_b, write) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            client_a.call_tool(
                CallToolRequestParams::new("chronicle_list_chunks")
                    .with_arguments(list_arguments.clone()),
            ),
            client_b.call_tool(
                CallToolRequestParams::new("chronicle_list_chunks").with_arguments(list_arguments),
            ),
            app_writer
        )
    })
    .await?;
    let read_a = read_a?;
    let read_b = read_b?;
    write??;
    assert_eq!(read_a.is_error, Some(false));
    assert_eq!(read_b.is_error, Some(false));
    assert_eq!(
        read_a
            .structured_content
            .as_ref()
            .map(|value| &value["result"]),
        read_b
            .structured_content
            .as_ref()
            .map(|value| &value["result"])
    );

    let create = client_a
        .call_tool(
            CallToolRequestParams::new("chronicle_create_artifact").with_arguments(arguments(
                serde_json::json!({
                    "request_id": "stdio-race-create",
                    "artifact_id": "stdio-race-artifact",
                    "revision_id": "stdio-race-base",
                    "artifact_type": "hypothesis",
                    "author": {
                        "kind": "model",
                        "display_name": "Stdio analyst",
                        "model": "synthetic-model"
                    },
                    "payload": {"claim": "base"},
                    "evidence": {"event_ids": ["evt-090015"]},
                    "confidence": 0.5
                }),
            )?),
        )
        .await?;
    assert_eq!(create.is_error, Some(false));

    let revision_a = arguments(serde_json::json!({
        "request_id": "stdio-race-request-a",
        "artifact_id": "stdio-race-artifact",
        "revision_id": "stdio-race-child-a",
        "expected_prior_revision_id": "stdio-race-base",
        "artifact_type": "hypothesis",
        "author": {
            "kind": "model",
            "display_name": "Stdio analyst",
            "model": "synthetic-model"
        },
        "status": "accepted",
        "payload": {"claim": "child a"},
        "evidence": {"event_ids": ["evt-090015"]},
        "confidence": 0.75
    }))?;
    let revision_b = arguments(serde_json::json!({
        "request_id": "stdio-race-request-b",
        "artifact_id": "stdio-race-artifact",
        "revision_id": "stdio-race-child-b",
        "expected_prior_revision_id": "stdio-race-base",
        "artifact_type": "hypothesis",
        "author": {
            "kind": "model",
            "display_name": "Stdio analyst",
            "model": "synthetic-model"
        },
        "status": "accepted",
        "payload": {"claim": "child b"},
        "evidence": {"event_ids": ["evt-090015"]},
        "confidence": 0.75
    }))?;
    let (revision_a, revision_b) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            client_a.call_tool(
                CallToolRequestParams::new("chronicle_revise_artifact").with_arguments(revision_a),
            ),
            client_b.call_tool(
                CallToolRequestParams::new("chronicle_revise_artifact").with_arguments(revision_b),
            )
        )
    })
    .await?;
    let revisions = [revision_a?, revision_b?];
    assert_eq!(
        revisions
            .iter()
            .filter(|result| result.is_error == Some(false))
            .count(),
        1
    );
    let conflict = revisions
        .iter()
        .find(|result| result.is_error == Some(true))
        .and_then(|result| result.structured_content.as_ref())
        .ok_or("missing conflict")?;
    assert_eq!(conflict["error"]["code"], "artifact-conflict");

    let root = ManagedRoot::initialize(&root_path)?;
    StoreGeneration::load(&root)?.increment(&root)?;
    let stale = client_a
        .call_tool(CallToolRequestParams::new("chronicle_status"))
        .await?;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(
        stale
            .structured_content
            .as_ref()
            .and_then(|value| value["error"]["code"].as_str()),
        Some("stale-generation")
    );

    client_a.cancel().await?;
    client_b.cancel().await?;

    let sqlite = SqliteStore::open(root.clone())?;
    assert_eq!(
        StoreQueries::new(sqlite.clone())
            .current_chunks_in_range(&range)?
            .len(),
        initial_chunks,
        "MCP reads and writes must not create factual chunks"
    );
    let canonical_event_records = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Events, false)?
        .records;
    let canonical_chunk_records = CanonicalJournal::new(root.clone())
        .scan_all(JournalFamily::Chunks, false)?
        .records;
    let artifact_store = ArtifactStore::new(root, Projector::new(sqlite.clone()));
    let canonical_artifacts = artifact_store.scan_all()?;
    let canonical_tip = artifact_store
        .current_revision(&chronicle_domain::ArtifactId::new("stdio-race-artifact")?)?
        .ok_or("canonical artifact tip missing")?;
    assert_eq!(canonical_artifacts.len(), 2);

    let connection = sqlite.connection()?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    assert_eq!(integrity, "ok", "SQLite integrity check failed after race");
    let foreign_key_violations: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    assert_eq!(foreign_key_violations, 0);
    for (table, canonical_count) in [
        ("events", canonical_event_records.len()),
        ("chunk_revisions", canonical_chunk_records.len()),
        ("artifact_revisions", canonical_artifacts.len()),
    ] {
        let projected_count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        assert_eq!(
            projected_count as usize, canonical_count,
            "canonical and projected {table} counts diverged"
        );
    }
    let current_artifacts: i64 = connection.query_row(
        "SELECT COUNT(*) FROM current_artifacts WHERE artifact_id='stdio-race-artifact'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(current_artifacts, 1, "artifact chain did not have one tip");
    let projected_tip: String = connection.query_row(
        "SELECT revision_id FROM current_artifacts WHERE artifact_id='stdio-race-artifact'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(projected_tip, canonical_tip.revision_id.as_str());

    let projected_ids =
        |table: &str, id_column: &str| -> Result<BTreeSet<String>, Box<dyn Error>> {
            let mut statement = connection.prepare(&format!("SELECT {id_column} FROM {table}"))?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<Result<BTreeSet<_>, _>>()?)
        };
    let canonical_event_ids = canonical_event_records
        .iter()
        .map(|record| record.stable_id().to_owned())
        .collect::<BTreeSet<_>>();
    let canonical_chunk_ids = canonical_chunk_records
        .iter()
        .map(|record| record.stable_id().to_owned())
        .collect::<BTreeSet<_>>();
    let canonical_artifact_ids = canonical_artifacts
        .iter()
        .map(|artifact| artifact.revision_id.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(projected_ids("events", "event_id")?, canonical_event_ids);
    assert_eq!(
        projected_ids("chunk_revisions", "revision_id")?,
        canonical_chunk_ids
    );
    assert_eq!(
        projected_ids("artifact_revisions", "revision_id")?,
        canonical_artifact_ids
    );
    Ok(())
}
