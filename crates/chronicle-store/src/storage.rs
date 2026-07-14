use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::{LockManager, ManagedRoot, Result, StoreError, storage_available_bytes};

pub type StorageAvailableBytesProbe = fn(&ManagedRoot) -> Result<u64>;

pub const GIB: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_STORAGE_WARNING_FREE_BYTES: u64 = 4 * GIB;
pub const DEFAULT_STORAGE_MINIMUM_FREE_BYTES: u64 = 2 * GIB;
pub const DEFAULT_MANAGED_IMAGE_QUOTA_BYTES: u64 = 20 * GIB;
pub const DEFAULT_SCREENSHOT_JOURNAL_RESERVE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenshotStorageLimits {
    pub warning_free_bytes: u64,
    pub minimum_free_bytes: u64,
    pub managed_image_quota_bytes: u64,
    pub journal_reserve_bytes: u64,
}

impl Default for ScreenshotStorageLimits {
    fn default() -> Self {
        Self {
            warning_free_bytes: DEFAULT_STORAGE_WARNING_FREE_BYTES,
            minimum_free_bytes: DEFAULT_STORAGE_MINIMUM_FREE_BYTES,
            managed_image_quota_bytes: DEFAULT_MANAGED_IMAGE_QUOTA_BYTES,
            journal_reserve_bytes: DEFAULT_SCREENSHOT_JOURNAL_RESERVE_BYTES,
        }
    }
}

impl ScreenshotStorageLimits {
    pub fn validate(self) -> Result<Self> {
        if self.warning_free_bytes < self.minimum_free_bytes {
            return Err(StoreError::InvalidPath(
                "storage warning threshold must not be below the hard floor".to_owned(),
            ));
        }
        if self.managed_image_quota_bytes == 0 {
            return Err(StoreError::InvalidPath(
                "managed screenshot quota must be positive".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotStorageState {
    Healthy,
    Warning,
    BlockedFreeSpace,
    BlockedImageQuota,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenshotStorageHealth {
    pub managed_image_bytes: u64,
    pub available_bytes: u64,
    pub warning_free_bytes: u64,
    pub minimum_free_bytes: u64,
    pub managed_image_quota_bytes: u64,
    pub journal_reserve_bytes: u64,
    pub state: ScreenshotStorageState,
}

impl ScreenshotStorageHealth {
    pub fn ensure_candidate_fits(self, candidate_bytes: u64) -> Result<()> {
        let required_available = self
            .minimum_free_bytes
            .checked_add(self.journal_reserve_bytes)
            .and_then(|required| required.checked_add(candidate_bytes))
            .ok_or(StoreError::ScreenshotFreeSpace {
                available_bytes: self.available_bytes,
                required_bytes: u64::MAX,
            })?;
        if self.available_bytes < required_available {
            return Err(StoreError::ScreenshotFreeSpace {
                available_bytes: self.available_bytes,
                required_bytes: required_available,
            });
        }
        let prospective_image_bytes = self
            .managed_image_bytes
            .checked_add(candidate_bytes)
            .ok_or(StoreError::ScreenshotImageQuota {
                managed_image_bytes: self.managed_image_bytes,
                candidate_bytes,
                quota_bytes: self.managed_image_quota_bytes,
            })?;
        if prospective_image_bytes > self.managed_image_quota_bytes {
            return Err(StoreError::ScreenshotImageQuota {
                managed_image_bytes: self.managed_image_bytes,
                candidate_bytes,
                quota_bytes: self.managed_image_quota_bytes,
            });
        }
        Ok(())
    }
}

pub fn screenshot_storage_health(
    root: &ManagedRoot,
    limits: ScreenshotStorageLimits,
) -> Result<ScreenshotStorageHealth> {
    let limits = limits.validate()?;
    let locks = LockManager::new(root.clone(), Duration::from_secs(2));
    let shared = locks.shared_request()?;
    let _screenshots = shared.screenshots()?;
    screenshot_storage_health_locked(root, limits, storage_available_bytes)
}

pub(crate) fn screenshot_storage_health_locked(
    root: &ManagedRoot,
    limits: ScreenshotStorageLimits,
    available_bytes_probe: StorageAvailableBytesProbe,
) -> Result<ScreenshotStorageHealth> {
    let limits = limits.validate()?;
    let managed_image_bytes = managed_image_bytes(root)?;
    let available_bytes = available_bytes_probe(root)?;
    evaluate_screenshot_storage(available_bytes, managed_image_bytes, limits)
}

pub fn evaluate_screenshot_storage(
    available_bytes: u64,
    managed_image_bytes: u64,
    limits: ScreenshotStorageLimits,
) -> Result<ScreenshotStorageHealth> {
    let limits = limits.validate()?;
    let state = if managed_image_bytes >= limits.managed_image_quota_bytes {
        ScreenshotStorageState::BlockedImageQuota
    } else if available_bytes < limits.minimum_free_bytes {
        ScreenshotStorageState::BlockedFreeSpace
    } else if available_bytes < limits.warning_free_bytes {
        ScreenshotStorageState::Warning
    } else {
        ScreenshotStorageState::Healthy
    };
    Ok(ScreenshotStorageHealth {
        managed_image_bytes,
        available_bytes,
        warning_free_bytes: limits.warning_free_bytes,
        minimum_free_bytes: limits.minimum_free_bytes,
        managed_image_quota_bytes: limits.managed_image_quota_bytes,
        journal_reserve_bytes: limits.journal_reserve_bytes,
        state,
    })
}

/// Counts logical bytes for every regular managed screenshot file, including
/// provisional recovery files. The exact recursive scan is intentionally the
/// MVP correctness backstop; callers serialize it with the screenshot lock.
fn managed_image_bytes(root: &ManagedRoot) -> Result<u64> {
    tree_bytes(&root.path().join("screenshots"))
}

fn tree_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(StoreError::InvalidPath(
                "managed screenshot inventory contains a symbolic link".to_owned(),
            ));
        }
        let bytes = if metadata.is_dir() {
            tree_bytes(&entry.path())?
        } else if metadata.is_file() {
            metadata.len()
        } else {
            return Err(StoreError::InvalidPath(
                "managed screenshot inventory contains a non-regular object".to_owned(),
            ));
        };
        total = total.checked_add(bytes).ok_or_else(|| {
            StoreError::InvalidPath("managed screenshot size overflow".to_owned())
        })?;
    }
    Ok(total)
}
