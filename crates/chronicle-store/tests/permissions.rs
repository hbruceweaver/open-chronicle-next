mod common;

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use chronicle_store::{CanonicalJournal, FaultInjector, ManagedRoot, StoreError};

#[test]
fn layout_is_private_and_repairs_broad_modes() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("store");
    std::fs::create_dir_all(&root_path)?;
    std::fs::set_permissions(&root_path, std::fs::Permissions::from_mode(0o755))?;
    let root = ManagedRoot::initialize(&root_path)?;
    assert_eq!(std::fs::metadata(&root_path)?.mode() & 0o777, 0o700);
    let file = root.open_file("config.json", true, false, false)?;
    assert_eq!(file.metadata()?.mode() & 0o777, 0o600);
    assert_eq!(file.metadata()?.uid(), std::fs::metadata(&root_path)?.uid());
    Ok(())
}

#[test]
fn nofollow_component_walk_rejects_symlink_escape() -> chronicle_store::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = ManagedRoot::initialize(temporary.path().join("store"))?;
    let outside = temporary.path().join("outside");
    std::fs::create_dir_all(&outside)?;
    symlink(&outside, root.path().join("derived/escape"))?;
    assert!(matches!(
        root.atomic_write("derived/escape/payload.json", b"{}"),
        Err(StoreError::Io(_))
    ));
    assert!(!outside.join("payload.json").exists());
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn real_filesystem_write_denial_is_not_acknowledged_as_durable() -> chronicle_store::Result<()> {
    let (_temporary, root, _sqlite, _projector) = common::store()?;
    let events_directory = root.path().join("evidence/events");
    let protected = std::process::Command::new("/usr/bin/chflags")
        .args(["uchg"])
        .arg(&events_directory)
        .status()?;
    assert!(protected.success(), "failed to make fixture immutable");
    let write_result = CanonicalJournal::new(root)
        .append_event(&common::events()?.remove(2), FaultInjector::none());
    let unprotected = std::process::Command::new("/usr/bin/chflags")
        .args(["nouchg"])
        .arg(&events_directory)
        .status()?;
    assert!(unprotected.success(), "failed to restore fixture flags");
    assert!(
        matches!(write_result, Err(StoreError::Io(_))),
        "immutable journal directory unexpectedly accepted a write"
    );
    let health = chronicle_store::critical_storage_health(chrono::Utc::now());
    assert_eq!(
        health.acknowledgement,
        Some(chronicle_domain::DurableAcknowledgement::NotDurable)
    );
    assert_eq!(health.severity, chronicle_domain::HealthSeverity::Critical);
    assert_eq!(
        health.projection,
        chronicle_domain::ProjectionHealth::Blocked
    );
    Ok(())
}
