use std::{
    collections::BTreeMap,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FileHistoryError {
    #[error("failed to create the file history directory")]
    CreateDirectory(#[source] std::io::Error),
    #[error("failed to back up {path}")]
    Backup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to roll back the file history backup")]
    Rollback(#[source] std::io::Error),
}

#[derive(Debug)]
pub struct PendingFileBackup {
    source: PathBuf,
    destination: Option<PathBuf>,
    version: u32,
}

#[derive(Debug)]
pub struct FileHistory {
    session_dir: PathBuf,
    versions: Mutex<BTreeMap<PathBuf, u32>>,
}

impl FileHistory {
    pub fn new(session_dir: impl Into<PathBuf>) -> Self {
        Self {
            session_dir: session_dir.into(),
            versions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn capture(&self, path: &Path) -> Result<PendingFileBackup, FileHistoryError> {
        let source = absolute_path(path);
        fs::create_dir_all(&self.session_dir).map_err(FileHistoryError::CreateDirectory)?;
        let version = {
            let mut versions = self
                .versions
                .lock()
                .expect("file history lock should not be poisoned");
            let version = versions.get(&source).copied().unwrap_or(0) + 1;
            versions.insert(source.clone(), version);
            version
        };

        if !source.exists() {
            return Ok(PendingFileBackup {
                source,
                destination: None,
                version,
            });
        }

        let destination = self.session_dir.join(backup_name(&source, version));
        let contents = fs::read(&source).map_err(|read_error| FileHistoryError::Backup {
            path: source.clone(),
            source: read_error,
        })?;
        if let Err(write_error) = fs::write(&destination, contents) {
            self.remove_version(&source, version);
            return Err(FileHistoryError::Backup {
                path: source.clone(),
                source: write_error,
            });
        }

        Ok(PendingFileBackup {
            source,
            destination: Some(destination),
            version,
        })
    }

    pub fn commit(&self, _backup: PendingFileBackup) {}

    pub fn rollback(&self, backup: PendingFileBackup) -> Result<(), FileHistoryError> {
        if let Some(destination) = &backup.destination
            && destination.exists()
        {
            fs::remove_file(destination).map_err(FileHistoryError::Rollback)?;
        }
        self.remove_version(&backup.source, backup.version);
        Ok(())
    }

    fn remove_version(&self, source: &Path, version: u32) {
        let mut versions = self
            .versions
            .lock()
            .expect("file history lock should not be poisoned");
        if versions.get(source).copied() == Some(version) {
            if version == 1 {
                versions.remove(source);
            } else {
                versions.insert(source.to_path_buf(), version - 1);
            }
        }
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match env::current_dir() {
        Ok(current_dir) => current_dir.join(path),
        Err(_) => path.to_path_buf(),
    }
}

fn backup_name(path: &Path, version: u32) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}-v{version}", hasher.finish())
}
