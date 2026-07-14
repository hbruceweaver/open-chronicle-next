#![allow(dead_code)]

use std::error::Error;

use chronicle_domain::{ChunkRevision, EventEnvelope};
use chronicle_store::{CanonicalJournal, FaultInjector, ManagedRoot, Projector, SqliteStore};

pub fn fixture_events(name: &str) -> Result<Vec<EventEnvelope>, Box<dyn Error>> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1")
            .join(name),
    )?;
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| EventEnvelope::parse(line).map_err(Into::into))
        .collect()
}

pub fn fixture_chunk(name: &str) -> Result<ChunkRevision, Box<dyn Error>> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/session-v1")
            .join(name),
    )?;
    Ok(ChunkRevision::parse(text.trim())?)
}

pub fn store() -> Result<(tempfile::TempDir, ManagedRoot, SqliteStore, Projector), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let sqlite = SqliteStore::open(root.clone())?;
    let projector = Projector::new(sqlite.clone());
    Ok((temporary, root, sqlite, projector))
}

pub fn seed_events(
    root: &ManagedRoot,
    projector: &Projector,
    events: &[EventEnvelope],
) -> Result<(), Box<dyn Error>> {
    let journal = CanonicalJournal::new(root.clone());
    for event in events {
        let record = journal.append_event(event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    Ok(())
}
