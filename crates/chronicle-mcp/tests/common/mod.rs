#![allow(dead_code)]

use std::error::Error;

use chronicle_domain::{ChunkRevision, DisclosureGrant, EventEnvelope};
use chronicle_engine::SharedService;
use chronicle_mcp::{ChronicleMcp, ServerConfig};
use chronicle_store::{CanonicalJournal, FaultInjector, ManagedRoot, Projector, SqliteStore};

pub struct TestServer {
    pub _temporary: tempfile::TempDir,
    pub server: ChronicleMcp,
}

pub fn empty_server(client: &str, grant: &str) -> Result<TestServer, Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    ManagedRoot::initialize(&root_path)?;
    let config = ServerConfig::new(root_path, client, grant)?;
    Ok(TestServer {
        _temporary: temporary,
        server: ChronicleMcp::new(config),
    })
}

pub fn fixture_server() -> Result<TestServer, Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path)?;
    let sqlite = SqliteStore::open(root.clone())?;
    let projector = Projector::new(sqlite.clone());
    let journal = CanonicalJournal::new(root.clone());
    for line in fixture("events.jsonl")?
        .lines()
        .filter(|line| !line.is_empty())
    {
        let event = EventEnvelope::parse(line)?;
        let record = journal.append_event(&event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    for line in fixture("chunks.jsonl")?
        .lines()
        .filter(|line| !line.is_empty())
    {
        let chunk = ChunkRevision::parse(line)?;
        let record = journal.append_chunk(&chunk, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let packet: serde_json::Value = serde_json::from_str(&fixture("queries.json")?)?;
    let grant: DisclosureGrant = serde_json::from_value(packet["grant"].clone())?;
    SharedService::open(root, sqlite)?.install_grant(grant)?;
    let config = ServerConfig::new(root_path, "client-codex-synthetic", "grant-synthetic")?;
    Ok(TestServer {
        _temporary: temporary,
        server: ChronicleMcp::new(config),
    })
}

pub fn fixture(name: &str) -> Result<String, Box<dyn Error>> {
    Ok(std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1")
            .join(name),
    )?)
}
