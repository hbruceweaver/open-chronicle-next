use std::process::Command;
use std::time::Duration;

use chronicle_store::{LockManager, ManagedRoot, StoreError};

const CHILD_ROOT: &str = "OPEN_CHRONICLE_CAPTURE_OWNER_CHILD_ROOT";

#[test]
fn capture_owner_child_helper() {
    let Ok(path) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let root = ManagedRoot::initialize(path).expect("open child managed root");
    let error = LockManager::new(root, Duration::from_secs(1))
        .try_capture_owner()
        .expect_err("parent process must retain capture ownership");
    assert!(matches!(error, StoreError::CaptureOwnerActive));
}

#[test]
fn capture_owner_excludes_same_process_and_releases_on_drop() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = ManagedRoot::initialize(temporary.path().join("store")).expect("managed root");
    let manager = LockManager::new(root, Duration::from_secs(1));

    let owner = manager.try_capture_owner().expect("first capture owner");
    assert!(matches!(
        manager.try_capture_owner(),
        Err(StoreError::CaptureOwnerActive)
    ));
    drop(owner);
    manager
        .try_capture_owner()
        .expect("capture ownership released on drop");
}

#[test]
fn capture_owner_excludes_a_real_second_process() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root_path = temporary.path().join("store");
    let root = ManagedRoot::initialize(&root_path).expect("managed root");
    let owner = LockManager::new(root, Duration::from_secs(1))
        .try_capture_owner()
        .expect("parent capture owner");

    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "capture_owner_child_helper", "--nocapture"])
        .env(CHILD_ROOT, root_path)
        .status()
        .expect("spawn capture-owner child");
    assert!(status.success(), "child did not observe the active owner");
    drop(owner);
}
