use std::fs::File;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};

use crate::permissions::io_error;
use crate::{JournalFamily, ManagedRoot, Result, StoreError};

const RETRY_INTERVAL: Duration = Duration::from_millis(10);

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
}

#[derive(Debug)]
pub struct SharedStoreGuard {
    file: File,
    root: ManagedRoot,
    timeout: Duration,
}

impl SharedStoreGuard {
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
pub struct JournalGuard {
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
unlock_on_drop!(JournalGuard);
