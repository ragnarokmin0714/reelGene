//! One reelgene per data folder.
//!
//! The scheduler keeps the queue in memory, so two processes on one database
//! would overwrite each other's view of it. An OS file lock settles it: it is
//! released when the process exits, crashes included, so there is no stale
//! lock file to clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

pub struct Instance {
    _lock: File,
}

impl Instance {
    pub fn acquire(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let path = data_dir.join("reelgene.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _lock: file }),
            Err(TryLockError::WouldBlock) => Err(
                "reelgene is already running (window or another command); close it first".into(),
            ),
            Err(TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_is_refused_until_the_first_is_gone() {
        let dir = std::env::temp_dir().join(format!("reelgene-lock-{}", std::process::id()));
        let first = Instance::acquire(&dir).unwrap();
        let second = Instance::acquire(&dir);
        assert!(second.is_err_and(|e| e.contains("already running")));
        drop(first);
        assert!(Instance::acquire(&dir).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
