use std::collections::HashSet;
use std::fs::File;
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};

use crate::maintenance::ensure_normal_store_access;
use crate::permissions::io_error;
use crate::{JournalFamily, ManagedRoot, Result, StoreError};

const RETRY_INTERVAL: Duration = Duration::from_millis(10);
static NAMED_PROCESS_LOCKS: OnceLock<(Mutex<HashSet<String>>, Condvar)> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct LockManager {
    root: ManagedRoot,
    timeout: Duration,
}

impl LockManager {
    pub const fn new(root: ManagedRoot, timeout: Duration) -> Self {
        Self { root, timeout }
    }

    pub fn shared_request(&self) -> Result<SharedStoreGuard> {
        let file = self
            .root
            .open_file("locks/store.lock", true, false, false)?;
        lock_bounded(&file, false, self.timeout, "shared store")?;
        ensure_normal_store_access(&self.root)?;
        Ok(SharedStoreGuard {
            file,
            root: self.root.clone(),
            timeout: self.timeout,
        })
    }

    pub fn exclusive_maintenance(&self) -> Result<ExclusiveStoreGuard> {
        let file = self
            .root
            .open_file("locks/store.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "exclusive store")?;
        Ok(ExclusiveStoreGuard { file })
    }

    pub fn journal(&self, family: JournalFamily) -> Result<JournalGuard> {
        let label = family.cursor_name();
        let relative = format!("locks/journal-{label}.lock");
        let file = self.root.open_file(&relative, true, false, false)?;
        lock_bounded(&file, true, self.timeout, "journal writer")?;
        Ok(JournalGuard { file })
    }

    pub fn grant_receipts(&self) -> Result<GrantReceiptGuard> {
        let file = self
            .root
            .open_file("locks/grant-receipts.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "grant receipts")?;
        Ok(GrantReceiptGuard { file })
    }

    /// Serializes a projection-changing operation with a multi-statement shared
    /// service read so result, coverage, and provenance observe one stable state.
    pub fn query_snapshot(&self) -> Result<QuerySnapshotGuard> {
        let file = self
            .root
            .open_file("locks/query-snapshot.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "query snapshot")?;
        Ok(QuerySnapshotGuard { file })
    }

    /// Acquires the one process-lifetime capture lease for this managed root.
    ///
    /// This deliberately does not wait: a second application instance must
    /// activate the existing owner instead of becoming another scheduler.
    pub fn try_capture_owner(&self) -> Result<CaptureOwnerGuard> {
        let process =
            try_lock_named_process(format!("{}:capture-owner", self.root.path().display()))?;
        let file = self
            .root
            .open_file("locks/capture-owner.lock", true, false, false)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(CaptureOwnerGuard {
                _process: process,
                file,
            }),
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN =>
            {
                Err(StoreError::CaptureOwnerActive)
            }
            Err(error) => Err(io_error(error)),
        }
    }
}

#[derive(Debug)]
pub struct SharedStoreGuard {
    file: File,
    root: ManagedRoot,
    timeout: Duration,
}

impl SharedStoreGuard {
    pub fn derived_revisions(&self) -> Result<DerivedRevisionGuard> {
        let process = lock_named_process(
            format!("{}:derived-revisions", self.root.path().display()),
            self.timeout,
            "derived revisions",
        )?;
        let file = self
            .root
            .open_file("locks/derived-revisions.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "derived revisions")?;
        Ok(DerivedRevisionGuard {
            _process: process,
            _file: file,
        })
    }

    pub fn artifact(&self, artifact_id: &str) -> Result<ArtifactGuard> {
        if artifact_id.is_empty()
            || !artifact_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(StoreError::InvalidPath(artifact_id.to_owned()));
        }
        let relative = format!("locks/artifact-{artifact_id}.lock");
        let file = self.root.open_file(&relative, true, false, false)?;
        lock_bounded(&file, true, self.timeout, "artifact")?;
        Ok(ArtifactGuard { file })
    }

    /// Serializes the image inventory, provisional promotion, and deletion
    /// lifecycle across both threads and processes.
    pub fn screenshots(&self) -> Result<ScreenshotGuard> {
        let process = lock_named_process(
            format!("{}:screenshots", self.root.path().display()),
            self.timeout,
            "screenshot inventory",
        )?;
        let file = self
            .root
            .open_file("locks/screenshots.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "screenshot inventory")?;
        Ok(ScreenshotGuard {
            _process: process,
            _file: file,
        })
    }

    /// Serializes authoritative config.json read-modify-write operations.
    pub fn configuration(&self) -> Result<ConfigurationGuard> {
        let process = lock_named_process(
            format!("{}:configuration", self.root.path().display()),
            self.timeout,
            "configuration",
        )?;
        let file = self
            .root
            .open_file("locks/configuration.lock", true, false, false)?;
        lock_bounded(&file, true, self.timeout, "configuration")?;
        Ok(ConfigurationGuard {
            _process: process,
            _file: file,
        })
    }
}

#[derive(Debug)]
pub struct ExclusiveStoreGuard {
    file: File,
}

#[derive(Debug)]
pub struct ArtifactGuard {
    file: File,
}

#[derive(Debug)]
pub struct DerivedRevisionGuard {
    _process: ProcessNamedGuard,
    _file: File,
}

#[derive(Debug)]
pub struct ScreenshotGuard {
    _process: ProcessNamedGuard,
    _file: File,
}

#[derive(Debug)]
pub struct ConfigurationGuard {
    _process: ProcessNamedGuard,
    _file: File,
}

#[derive(Debug)]
pub struct JournalGuard {
    file: File,
}

#[derive(Debug)]
pub struct GrantReceiptGuard {
    file: File,
}

#[derive(Debug)]
pub struct QuerySnapshotGuard {
    file: File,
}

#[derive(Debug)]
pub struct CaptureOwnerGuard {
    _process: ProcessNamedGuard,
    file: File,
}

fn lock_bounded(file: &File, exclusive: bool, timeout: Duration, label: &str) -> Result<()> {
    let started = Instant::now();
    loop {
        let operation = if exclusive {
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::NonBlockingLockShared
        };
        match flock(file, operation) {
            Ok(()) => return Ok(()),
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN =>
            {
                if started.elapsed() >= timeout {
                    return Err(StoreError::LockTimeout(label.to_owned()));
                }
                thread::sleep(RETRY_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
            }
            Err(error) => return Err(io_error(error)),
        }
    }
}

#[derive(Debug)]
struct ProcessNamedGuard {
    key: String,
}

fn lock_named_process(key: String, timeout: Duration, label: &str) -> Result<ProcessNamedGuard> {
    let (registry, wake) =
        NAMED_PROCESS_LOCKS.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()));
    let started = Instant::now();
    let mut active = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if active.insert(key.clone()) {
            return Ok(ProcessNamedGuard { key });
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(StoreError::LockTimeout(label.to_owned()));
        }
        let (next, timeout_result) = wake
            .wait_timeout(active, RETRY_INTERVAL.min(remaining))
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active = next;
        if timeout_result.timed_out() && started.elapsed() >= timeout {
            return Err(StoreError::LockTimeout(label.to_owned()));
        }
    }
}

fn try_lock_named_process(key: String) -> Result<ProcessNamedGuard> {
    let (registry, _) =
        NAMED_PROCESS_LOCKS.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()));
    let mut active = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if active.insert(key.clone()) {
        Ok(ProcessNamedGuard { key })
    } else {
        Err(StoreError::CaptureOwnerActive)
    }
}

impl Drop for ProcessNamedGuard {
    fn drop(&mut self) {
        let (registry, wake) =
            NAMED_PROCESS_LOCKS.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()));
        let mut active = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.remove(&self.key);
        wake.notify_all();
    }
}

macro_rules! unlock_on_drop {
    ($type:ty) => {
        impl Drop for $type {
            fn drop(&mut self) {
                let _ = flock(&self.file, FlockOperation::Unlock);
            }
        }
    };
}

unlock_on_drop!(SharedStoreGuard);
unlock_on_drop!(ExclusiveStoreGuard);
unlock_on_drop!(ArtifactGuard);
impl Drop for DerivedRevisionGuard {
    fn drop(&mut self) {
        let _ = flock(&self._file, FlockOperation::Unlock);
    }
}
impl Drop for ScreenshotGuard {
    fn drop(&mut self) {
        let _ = flock(&self._file, FlockOperation::Unlock);
    }
}
impl Drop for ConfigurationGuard {
    fn drop(&mut self) {
        let _ = flock(&self._file, FlockOperation::Unlock);
    }
}
unlock_on_drop!(JournalGuard);
unlock_on_drop!(GrantReceiptGuard);
unlock_on_drop!(QuerySnapshotGuard);
unlock_on_drop!(CaptureOwnerGuard);
