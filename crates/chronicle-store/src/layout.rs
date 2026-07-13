use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, Mode, OFlags, fstat, mkdirat, open, openat, renameat, unlinkat};
use rustix::process::umask;
use uuid::Uuid;

use crate::permissions::{DIR_MODE, FILE_MODE, io_error, secure_directory, secure_file};
use crate::{Result, StoreError};

const ROOT_DIRECTORIES: &[&str] = &[
    "evidence/events",
    "aggregates/chunks",
    "derived",
    "screenshots",
    "locks",
    "receipts",
    "diagnostics",
    "exports",
];

#[derive(Clone, Debug)]
pub struct ManagedRoot {
    path: Arc<PathBuf>,
    fd: Arc<OwnedFd>,
}

impl ManagedRoot {
    pub fn initialize(path: impl AsRef<Path>) -> Result<Self> {
        let _prior_umask = umask(Mode::from_raw_mode(0o077));
        let path = path.as_ref();
        std::fs::create_dir_all(path)?;
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        secure_directory(&fd, "managed root")?;
        // SQLite is the one pathname-based managed-file exception. Resolve the
        // already opened and ownership-validated root, then prove the resolved
        // pathname opens the same inode before retaining it for SQLite.
        let resolved_path = std::fs::canonicalize(path)?;
        let resolved_fd = open(
            &resolved_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        let original_stat = fstat(&fd).map_err(io_error)?;
        let resolved_stat = fstat(&resolved_fd).map_err(io_error)?;
        if original_stat.st_dev != resolved_stat.st_dev
            || original_stat.st_ino != resolved_stat.st_ino
        {
            return Err(StoreError::InvalidPath(
                "resolved SQLite root does not match anchored managed root".to_owned(),
            ));
        }
        let root = Self {
            path: Arc::new(resolved_path),
            fd: Arc::new(fd),
        };
        for directory in ROOT_DIRECTORIES {
            root.ensure_directory(directory)?;
        }
        root.open_file("locks/store.lock", true, false, false)?;
        root.sync_directory("locks")?;
        Ok(root)
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.path.join("index.sqlite3")
    }

    pub fn ensure_directory(&self, relative: &str) -> Result<()> {
        let components = validate_relative(relative)?;
        let mut current = self.duplicate_root()?;
        for component in components {
            match openat(
                &current,
                &component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(next) => {
                    secure_directory(&next, relative)?;
                    current = next;
                }
                Err(error) if error == rustix::io::Errno::NOENT => {
                    mkdirat(&current, &component, DIR_MODE).map_err(io_error)?;
                    let next = openat(
                        &current,
                        &component,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(io_error)?;
                    secure_directory(&next, relative)?;
                    File::from(current).sync_all()?;
                    current = next;
                }
                Err(error) => return Err(io_error(error)),
            }
        }
        Ok(())
    }

    pub fn open_file(
        &self,
        relative: &str,
        create: bool,
        append: bool,
        truncate: bool,
    ) -> Result<File> {
        let (parent, name) = self.open_parent(relative, create)?;
        let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if create {
            flags |= OFlags::CREATE;
        }
        if append {
            flags |= OFlags::APPEND;
        }
        if truncate {
            flags |= OFlags::TRUNC;
        }
        let fd = openat(&parent, &name, flags, FILE_MODE).map_err(io_error)?;
        secure_file(&fd, relative)?;
        Ok(File::from(fd))
    }

    pub fn read(&self, relative: &str) -> Result<Vec<u8>> {
        let mut file = self.open_file(relative, false, false, false)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn list_file_names(&self, relative_directory: &str) -> Result<Vec<String>> {
        self.ensure_directory(relative_directory)?;
        let mut names = Vec::new();
        for entry in std::fs::read_dir(self.path.join(relative_directory))? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                StoreError::InvalidPath("managed file name is not valid UTF-8".to_owned())
            })?;
            names.push(name);
        }
        names.sort();
        for name in &names {
            let relative = format!("{relative_directory}/{name}");
            let _ = self.open_file(&relative, false, false, false)?;
        }
        Ok(names)
    }

    pub fn exists(&self, relative: &str) -> Result<bool> {
        match self.open_file(relative, false, false, false) {
            Ok(_) => Ok(true),
            Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn sync_directory(&self, relative: &str) -> Result<()> {
        let components = validate_relative(relative)?;
        let mut current = self.duplicate_root()?;
        for component in components {
            current = openat(
                &current,
                &component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io_error)?;
        }
        File::from(current).sync_all()?;
        Ok(())
    }

    pub fn atomic_write(&self, relative: &str, bytes: &[u8]) -> Result<()> {
        self.atomic_write_with_boundary(relative, bytes, || Ok(()))
    }

    pub(crate) fn atomic_write_with_boundary(
        &self,
        relative: &str,
        bytes: &[u8],
        after_rename: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let (parent, name) = self.open_parent(relative, true)?;
        let temp = OsString::from(format!(
            ".{}.{}.tmp",
            name.to_string_lossy(),
            Uuid::now_v7()
        ));
        let fd = openat(
            &parent,
            &temp,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            FILE_MODE,
        )
        .map_err(io_error)?;
        secure_file(&fd, relative)?;
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        renameat(&parent, &temp, &parent, &name).map_err(io_error)?;
        after_rename()?;
        File::from(parent).sync_all()?;
        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.rename_with_boundary(from, to, || Ok(()))
    }

    pub(crate) fn rename_with_boundary(
        &self,
        from: &str,
        to: &str,
        after_rename: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let (from_parent, from_name) = self.open_parent(from, false)?;
        let (to_parent, to_name) = self.open_parent(to, true)?;
        renameat(&from_parent, &from_name, &to_parent, &to_name).map_err(io_error)?;
        after_rename()?;
        File::from(from_parent).sync_all()?;
        File::from(to_parent).sync_all()?;
        Ok(())
    }

    pub fn unlink(&self, relative: &str) -> Result<()> {
        self.unlink_with_boundary(relative, || Ok(()))
    }

    pub(crate) fn unlink_with_boundary(
        &self,
        relative: &str,
        after_unlink: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let (parent, name) = self.open_parent(relative, false)?;
        unlinkat(&parent, &name, AtFlags::empty()).map_err(io_error)?;
        after_unlink()?;
        File::from(parent).sync_all()?;
        Ok(())
    }

    fn duplicate_root(&self) -> Result<OwnedFd> {
        openat(
            self.fd.as_ref(),
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)
    }

    fn open_parent(&self, relative: &str, create: bool) -> Result<(OwnedFd, OsString)> {
        let mut components = validate_relative(relative)?;
        let name = components
            .pop()
            .ok_or_else(|| StoreError::InvalidPath(relative.to_owned()))?;
        let mut current = self.duplicate_root()?;
        for component in components {
            match openat(
                &current,
                &component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(next) => current = next,
                Err(error) if create && error == rustix::io::Errno::NOENT => {
                    mkdirat(&current, &component, DIR_MODE).map_err(io_error)?;
                    let next = openat(
                        &current,
                        &component,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(io_error)?;
                    File::from(current).sync_all()?;
                    current = next;
                }
                Err(error) => return Err(io_error(error)),
            }
            secure_directory(&current, relative)?;
        }
        Ok((current, name))
    }
}

fn validate_relative(relative: &str) -> Result<Vec<OsString>> {
    let path = Path::new(relative);
    if relative.is_empty() || path.is_absolute() || relative.contains('\\') {
        return Err(StoreError::InvalidPath(relative.to_owned()));
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_os_string()),
            _ => return Err(StoreError::InvalidPath(relative.to_owned())),
        }
    }
    if components.is_empty() {
        return Err(StoreError::InvalidPath(relative.to_owned()));
    }
    Ok(components)
}
