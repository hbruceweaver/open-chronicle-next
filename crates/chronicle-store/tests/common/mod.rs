#![allow(dead_code)]

use chronicle_domain::{ChunkRevision, DerivedArtifactRevision, EventEnvelope};
use chronicle_store::{
    CanonicalJournal, FaultInjector, ManagedRoot, Projector, Result, SqliteStore,
};

pub fn store() -> Result<(tempfile::TempDir, ManagedRoot, SqliteStore, Projector)> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let sqlite = SqliteStore::open(root.clone())?;
    let projector = Projector::new(sqlite.clone());
    Ok((temporary, root, sqlite, projector))
}

pub fn events() -> Result<Vec<EventEnvelope>> {
    include_str!("../../../../fixtures/synthetic/session-v1/events.jsonl")
        .lines()
        .filter(|line| !line.is_empty())
        .map(EventEnvelope::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn chunks() -> Result<Vec<ChunkRevision>> {
    include_str!("../../../../fixtures/synthetic/session-v1/chunks.jsonl")
        .lines()
        .filter(|line| !line.is_empty())
        .map(ChunkRevision::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn artifact() -> Result<DerivedArtifactRevision> {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/synthetic/session-v1/queries.json"
    ))?;
    DerivedArtifactRevision::parse(&serde_json::to_string(&value["artifact"])?).map_err(Into::into)
}

pub fn seed_canonical(root: &ManagedRoot, projector: &Projector) -> Result<()> {
    let journal = CanonicalJournal::new(root.clone());
    for event in events()? {
        let record = journal.append_event(&event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    for chunk in chunks()? {
        let record = journal.append_chunk(&chunk, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    chronicle_store::ArtifactStore::new(root.clone(), projector.clone())
        .write_revision(&artifact()?, FaultInjector::none())
}
