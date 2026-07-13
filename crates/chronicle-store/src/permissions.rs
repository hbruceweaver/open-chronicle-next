use std::io;
use std::os::fd::AsFd;

use rustix::fs::{Mode, fchmod, fstat};
use rustix::process::geteuid;

use crate::{Result, StoreError};

pub(crate) const DIR_MODE: Mode = Mode::from_raw_mode(0o700);
pub(crate) const FILE_MODE: Mode = Mode::from_raw_mode(0o600);

pub(crate) fn io_error(error: rustix::io::Errno) -> StoreError {
    StoreError::Io(io::Error::from(error))
}

pub(crate) fn secure_directory(fd: impl AsFd, label: &str) -> Result<()> {
    let uid = geteuid();
    if uid.is_root() {
        return Err(StoreError::WrongOwner);
    }
    let stat = fstat(fd.as_fd()).map_err(io_error)?;
    if stat.st_uid != uid.as_raw() {
        return Err(StoreError::WrongOwner);
    }
    if stat.st_mode & 0o777 != 0o700 {
        fchmod(fd.as_fd(), DIR_MODE).map_err(io_error)?;
        let repaired = fstat(fd.as_fd()).map_err(io_error)?;
        if repaired.st_mode & 0o777 != 0o700 {
            return Err(StoreError::UnsafePermissions(label.to_owned()));
        }
    }
    Ok(())
}

pub(crate) fn secure_file(fd: impl AsFd, label: &str) -> Result<()> {
    let uid = geteuid();
    if uid.is_root() {
        return Err(StoreError::WrongOwner);
    }
    let stat = fstat(fd.as_fd()).map_err(io_error)?;
    if stat.st_uid != uid.as_raw() {
        return Err(StoreError::WrongOwner);
    }
    if stat.st_mode & 0o777 != 0o600 {
        fchmod(fd.as_fd(), FILE_MODE).map_err(io_error)?;
        let repaired = fstat(fd.as_fd()).map_err(io_error)?;
        if repaired.st_mode & 0o777 != 0o600 {
            return Err(StoreError::UnsafePermissions(label.to_owned()));
        }
    }
    Ok(())
}
