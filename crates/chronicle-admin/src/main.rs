use std::error::Error;
use std::path::{Path, PathBuf};

use chronicle_domain::{ChunkRevision, DerivedArtifactRevision, DeviceId, EventEnvelope};
use chronicle_store::{
    CanonicalJournal, FaultInjector, JournalFamily, ManagedRoot, Projector, RecoveryManager,
    RepairConfirmation, SqliteStore,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("chronicle-admin: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or(
            "usage: chronicle-admin <verify-journals|rebuild-index|repair-journal|replay> ...",
        )?;
    match command.as_str() {
        "verify-journals" => {
            let root = required_path(arguments.next(), "managed root")?;
            ensure_no_more(arguments)?;
            let root = ManagedRoot::initialize(root)?;
            let report = RecoveryManager::new(root).verify_journals(true)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "rebuild-index" => {
            let root = required_path(arguments.next(), "managed root")?;
            ensure_no_more(arguments)?;
            let root = ManagedRoot::initialize(root)?;
            let (report, snapshot) = RecoveryManager::new(root).rebuild_index()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "report": report,
                    "snapshot": snapshot,
                }))?
            );
        }
        "replay" => {
            let fixture = required_path(arguments.next(), "synthetic fixture directory")?;
            let root = required_path(arguments.next(), "managed root")?;
            ensure_no_more(arguments)?;
            replay(&fixture, &root)?;
        }
        "repair-journal" => {
            let root = required_path(arguments.next(), "managed root")?;
            let family = required_string(arguments.next(), "journal family")?;
            let shard = required_string(arguments.next(), "shard name")?;
            let device_id = DeviceId::new(required_string(arguments.next(), "device ID")?)?;
            let confirmation = RepairConfirmation::confirm(&required_string(
                arguments.next(),
                "exact repair confirmation phrase",
            )?)?;
            ensure_no_more(arguments)?;
            let family = match family.as_str() {
                "events" => JournalFamily::Events,
                "chunks" => JournalFamily::Chunks,
                _ => return Err("journal family must be events or chunks".into()),
            };
            let root = ManagedRoot::initialize(root)?;
            let manager = RecoveryManager::new(root);
            let repair = manager.repair_journal(family, &shard, device_id, confirmation)?;
            let (rebuild, snapshot) = manager.rebuild_index()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "repair": repair,
                    "rebuild": rebuild,
                    "snapshot": snapshot,
                }))?
            );
        }
        _ => return Err(format!("unknown command: {command}").into()),
    }
    Ok(())
}

fn replay(fixture: &Path, root_path: &Path) -> Result<(), Box<dyn Error>> {
    let root = ManagedRoot::initialize(root_path)?;
    let sqlite = SqliteStore::open(root.clone())?;
    let projector = Projector::new(sqlite.clone());
    let journal = CanonicalJournal::new(root.clone());
    for line in nonempty_lines(&fixture.join("events.jsonl"))? {
        let event = EventEnvelope::parse(&line)?;
        let record = journal.append_event(&event, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    for line in nonempty_lines(&fixture.join("chunks.jsonl"))? {
        let chunk = ChunkRevision::parse(&line)?;
        let record = journal.append_chunk(&chunk, FaultInjector::none())?;
        projector.project_record(&record, FaultInjector::none())?;
    }
    let queries: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.join("queries.json"))?)?;
    if let Some(artifact) = queries.get("artifact") {
        let artifact = DerivedArtifactRevision::parse(&serde_json::to_string(artifact)?)?;
        chronicle_store::ArtifactStore::new(root.clone(), projector)
            .write_revision(&artifact, FaultInjector::none())?;
    }
    let snapshot = sqlite.snapshot_ids()?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

fn nonempty_lines(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn required_path(
    value: Option<std::ffi::OsString>,
    label: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {label}").into())
}

fn required_string(
    value: Option<std::ffi::OsString>,
    label: &str,
) -> Result<String, Box<dyn Error>> {
    value
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| format!("missing or invalid {label}").into())
}

fn ensure_no_more(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    if arguments.next().is_some() {
        Err("unexpected extra arguments".into())
    } else {
        Ok(())
    }
}
