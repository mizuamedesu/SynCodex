use crate::storage;
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

const DIRECTORY: &str = "thread-writer-locks";
const COORDINATION: &str = ".coordination.lock";

fn lock_path(home: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        !id.is_empty() && !id.starts_with('.') && !id.contains(['/', '\\']),
        "Invalid thread id for writer lock"
    );
    let path = home.join(DIRECTORY).join(format!("{id}.lock"));
    storage::check_path(&path)?;
    Ok(path)
}

// Probe existing files only: reading lists and dry-runs never create lock files.
pub(crate) fn in_use(home: &Path, id: &str) -> Result<bool> {
    let path = lock_path(home, id)?;
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn ensure_unused(home: &Path, ids: &[String]) -> Result<()> {
    for id in ids {
        ensure!(
            !in_use(home, id)?,
            "Session {id} is in use by Codex. Close that session before editing; other sessions can stay open"
        );
    }
    Ok(())
}

// Same path and std::fs locks as Codex's WriterLockCoordinator. The native
// coordination lock prevents cleanup from unlinking a lock while we acquire it.
pub(crate) struct WriterLocks {
    directory: PathBuf,
    files: Vec<(PathBuf, File)>,
}

impl WriterLocks {
    pub fn acquire(home: &Path, ids: &[String]) -> Result<Self> {
        let directory = home.join(DIRECTORY);
        storage::check_path(&directory)?;
        storage::private_dir(&directory)?;
        let coordinator = open_coordinator(&directory)?;
        coordinator
            .try_lock()
            .context("Codex is changing session ownership; retry shortly")?;
        let mut result = Self {
            directory,
            files: Vec::new(),
        };
        for id in ids {
            let path = lock_path(home, id)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            match file.try_lock() {
                Ok(()) => result.files.push((path, file)),
                Err(TryLockError::WouldBlock) => {
                    drop(coordinator);
                    bail!(
                        "Session {id} is in use by Codex. Close that session before editing; other sessions can stay open"
                    );
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        drop(coordinator);
        Ok(result)
    }
}

fn open_coordinator(directory: &Path) -> Result<File> {
    let path = directory.join(COORDINATION);
    storage::check_path(&path)?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

impl Drop for WriterLocks {
    fn drop(&mut self) {
        let Ok(coordinator) = open_coordinator(&self.directory) else {
            return;
        };
        if coordinator.try_lock().is_err() {
            // Leave unlocked stale files for Codex's cleanup instead of unlinking
            // files while another process coordinates acquisitions.
            return;
        }
        for (path, file) in self.files.drain(..) {
            drop(file);
            let _ = fs::remove_file(path);
        }
    }
}
