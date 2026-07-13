mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use chronicle_domain::{ArtifactRevisionId, ArtifactStatus};
use chronicle_store::{
    ArtifactStore, FaultInjector, LockManager, ManagedRoot, Projector, SqliteStore, StoreError,
    StoreGeneration,
};

#[test]
fn shared_requests_block_exclusive_maintenance_with_bounded_wait() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let shared_manager = LockManager::new(root.clone(), Duration::from_millis(100));
    let shared = shared_manager.shared_request()?;
    let exclusive_manager = LockManager::new(root, Duration::from_millis(25));
    assert!(matches!(
        exclusive_manager.exclusive_maintenance(),
        Err(StoreError::LockTimeout(_))
    ));
    drop(shared);
    let _exclusive = exclusive_manager.exclusive_maintenance()?;
    Ok(())
}

#[test]
fn generation_change_invalidates_stale_handles() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let old = StoreGeneration::load(&root)?;
    let new = old.increment(&root)?;
    assert_eq!(new.generation, old.generation + 1);
    assert!(matches!(
        old.ensure_current(&root),
        Err(StoreError::StaleGeneration { .. })
    ));
    Ok(())
}

#[test]
fn two_writer_processes_from_one_prior_have_one_typed_winner() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path)?;
    let sqlite = SqliteStore::open(root.clone())?;
    let projector = Projector::new(sqlite.clone());
    let base = common::artifact()?;
    ArtifactStore::new(root.clone(), projector).write_revision(&base, FaultInjector::none())?;

    let gate = temporary.path().join("artifact-gate");
    let exe = std::env::current_exe()?;
    let mut children = Vec::new();
    let mut ready_paths = Vec::new();
    let mut result_paths = Vec::new();
    for revision in ["artifact-process-left", "artifact-process-right"] {
        let ready = temporary.path().join(format!("{revision}.ready"));
        let result = temporary.path().join(format!("{revision}.result"));
        children.push(
            std::process::Command::new(&exe)
                .arg("--exact")
                .arg("artifact_writer_process_child")
                .arg("--nocapture")
                .env("CHRONICLE_ARTIFACT_ROOT", &root_path)
                .env("CHRONICLE_ARTIFACT_REVISION", revision)
                .env("CHRONICLE_PROCESS_READY", &ready)
                .env("CHRONICLE_PROCESS_GATE", &gate)
                .env("CHRONICLE_PROCESS_RESULT", &result)
                .spawn()?,
        );
        ready_paths.push(ready);
        result_paths.push(result);
    }
    wait_for_paths(&ready_paths)?;
    std::fs::write(&gate, b"go")?;
    for mut child in children {
        assert!(child.wait()?.success());
    }
    let results = result_paths
        .iter()
        .map(std::fs::read_to_string)
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(
        results.iter().filter(|value| *value == "success").count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|value| *value == "artifact-conflict")
            .count(),
        1
    );
    let revisions = ArtifactStore::new(root, Projector::new(sqlite.clone())).scan_all()?;
    assert_eq!(revisions.len(), 2);
    assert_eq!(sqlite.snapshot_ids()?.artifact_revision_ids.len(), 2);
    Ok(())
}

#[test]
fn artifact_writer_process_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_ARTIFACT_ROOT") else {
        return Ok(());
    };
    let revision = std::env::var("CHRONICLE_ARTIFACT_REVISION")
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    let ready = required_env_path("CHRONICLE_PROCESS_READY")?;
    let gate = required_env_path("CHRONICLE_PROCESS_GATE")?;
    let result = required_env_path("CHRONICLE_PROCESS_RESULT")?;
    let root = ManagedRoot::initialize(root_path)?;
    let sqlite = SqliteStore::open(root.clone())?;
    let base = common::artifact()?;
    let mut child = base.clone();
    child.revision_id = ArtifactRevisionId::new(revision)
        .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
    child.prior_revision_id = Some(base.revision_id.clone());
    child.expected_prior_revision_id = Some(base.revision_id);
    child.status = ArtifactStatus::Accepted;
    std::fs::write(ready, b"ready")?;
    wait_for_path(&gate)?;
    let outcome = ArtifactStore::new(root, Projector::new(sqlite))
        .write_revision(&child, FaultInjector::none());
    match outcome {
        Ok(()) => std::fs::write(result, b"success")?,
        Err(StoreError::ArtifactConflict) => std::fs::write(result, b"artifact-conflict")?,
        Err(error) => return Err(error),
    }
    Ok(())
}

#[test]
fn process_opened_before_generation_change_is_rejected() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path)?;
    let ready = temporary.path().join("generation.ready");
    let gate = temporary.path().join("generation.gate");
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("stale_generation_process_child")
        .arg("--nocapture")
        .env("CHRONICLE_GENERATION_ROOT", &root_path)
        .env("CHRONICLE_PROCESS_READY", &ready)
        .env("CHRONICLE_PROCESS_GATE", &gate)
        .spawn()?;
    wait_for_path(&ready)?;
    StoreGeneration::load(&root)?.increment(&root)?;
    std::fs::write(&gate, b"go")?;
    assert!(child.wait()?.success());
    Ok(())
}

#[test]
fn stale_generation_process_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_GENERATION_ROOT") else {
        return Ok(());
    };
    let ready = required_env_path("CHRONICLE_PROCESS_READY")?;
    let gate = required_env_path("CHRONICLE_PROCESS_GATE")?;
    let root = ManagedRoot::initialize(root_path)?;
    let sqlite = SqliteStore::open(root)?;
    std::fs::write(ready, b"ready")?;
    wait_for_path(&gate)?;
    match sqlite.snapshot_ids() {
        Err(StoreError::StaleGeneration { .. }) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(StoreError::InvalidPath(
            "stale process retained projection access".to_owned(),
        )),
    }
}

#[test]
fn process_shared_lock_blocks_exclusive_with_bounded_wait() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path)?;
    let ready = temporary.path().join("lock.ready");
    let gate = temporary.path().join("lock.gate");
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("shared_lock_process_child")
        .arg("--nocapture")
        .env("CHRONICLE_LOCK_ROOT", &root_path)
        .env("CHRONICLE_PROCESS_READY", &ready)
        .env("CHRONICLE_PROCESS_GATE", &gate)
        .spawn()?;
    wait_for_path(&ready)?;
    let manager = LockManager::new(root, Duration::from_millis(25));
    assert!(matches!(
        manager.exclusive_maintenance(),
        Err(StoreError::LockTimeout(_))
    ));
    std::fs::write(&gate, b"release")?;
    assert!(child.wait()?.success());
    let _exclusive = manager.exclusive_maintenance()?;
    Ok(())
}

#[test]
fn shared_lock_process_child() -> chronicle_store::Result<()> {
    let Some(root_path) = std::env::var_os("CHRONICLE_LOCK_ROOT") else {
        return Ok(());
    };
    let ready = required_env_path("CHRONICLE_PROCESS_READY")?;
    let gate = required_env_path("CHRONICLE_PROCESS_GATE")?;
    let root = ManagedRoot::initialize(root_path)?;
    let manager = LockManager::new(root, Duration::from_secs(1));
    let _shared = manager.shared_request()?;
    std::fs::write(ready, b"ready")?;
    wait_for_path(&gate)
}

fn required_env_path(name: &str) -> chronicle_store::Result<std::path::PathBuf> {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| StoreError::InvalidPath(format!("missing {name}")))
}

fn wait_for_paths(paths: &[std::path::PathBuf]) -> chronicle_store::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !paths.iter().all(|path| path.exists()) {
        if Instant::now() >= deadline {
            return Err(StoreError::LockTimeout(
                "child processes did not reach the race gate".to_owned(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn wait_for_path(path: &Path) -> chronicle_store::Result<()> {
    wait_for_paths(&[path.to_path_buf()])
}
