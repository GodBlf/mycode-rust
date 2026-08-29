use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FileStateError {
    #[error("file has not been read yet; read it before changing it")]
    NotRead,
    #[error("file was modified after it was last read; read it again")]
    ModifiedSinceRead,
    #[error("file was deleted after it was last read; read it again before recreating it")]
    DeletedSinceRead,
    #[error("failed to inspect file state for {path}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStateEntry {
    modified: SystemTime,
    content_hash: u64,
}

#[derive(Debug, Default)]
pub struct FileStateCache {
    entries: Mutex<HashMap<PathBuf, FileStateEntry>>,
}

impl FileStateCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, path: &Path, contents: &[u8]) -> Result<(), FileStateError> {
        let metadata = fs::metadata(path).map_err(|source| FileStateError::Inspect {
            path: path.to_path_buf(),
            source,
        })?;
        let modified = metadata
            .modified()
            .map_err(|source| FileStateError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
        self.store(path, contents, modified);
        Ok(())
    }

    pub fn update(&self, path: &Path, contents: &[u8]) -> Result<(), FileStateError> {
        self.record(path, contents)
    }

    pub fn check(&self, path: &Path) -> Result<(), FileStateError> {
        let entry = {
            let entries = self
                .entries
                .lock()
                .expect("file state lock should not be poisoned");
            entries.get(path).copied()
        };

        let Some(entry) = entry else {
            return Err(FileStateError::NotRead);
        };
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(source) => {
                return Err(FileStateError::Inspect {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let contents = fs::read(path).map_err(|source| FileStateError::Inspect {
            path: path.to_path_buf(),
            source,
        })?;

        let modified = metadata
            .modified()
            .map_err(|source| FileStateError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
        if modified != entry.modified || content_hash(&contents) != entry.content_hash {
            Err(FileStateError::ModifiedSinceRead)
        } else {
            Ok(())
        }
    }

    pub fn check_for_write(&self, path: &Path) -> Result<(), FileStateError> {
        if path.exists() {
            return self.check(path);
        }

        let was_read = {
            let entries = self
                .entries
                .lock()
                .expect("file state lock should not be poisoned");
            entries.contains_key(path)
        };
        if was_read {
            Err(FileStateError::DeletedSinceRead)
        } else {
            Ok(())
        }
    }

    fn store(&self, path: &Path, contents: &[u8], modified: SystemTime) {
        let entry = FileStateEntry {
            modified,
            content_hash: content_hash(contents),
        };
        let mut entries = self
            .entries
            .lock()
            .expect("file state lock should not be poisoned");
        entries.insert(path.to_path_buf(), entry);
    }
}

fn content_hash(contents: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    contents.hash(&mut hasher);
    hasher.finish()
}
