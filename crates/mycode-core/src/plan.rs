use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::workspace::{WorkspacePaths, unique_slug};

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("failed to create the plans directory")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write plan {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read plan {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct PlanFileManager {
    work_dir: PathBuf,
    selected_path: Option<PathBuf>,
}

impl PlanFileManager {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            selected_path: None,
        }
    }

    pub fn select_new_path(&mut self) -> Result<PathBuf, PlanError> {
        let directory = self.plans_dir();
        fs::create_dir_all(&directory).map_err(|source| PlanError::CreateDirectory {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join(format!("plan-{}.md", unique_slug()));
        self.selected_path = Some(path.clone());
        Ok(path)
    }

    fn select_path(&mut self) -> Result<PathBuf, PlanError> {
        if let Some(path) = &self.selected_path {
            return Ok(path.clone());
        }
        self.select_new_path()
    }

    pub fn save(&mut self, content: &str) -> Result<PathBuf, PlanError> {
        let path = match self.selected_path.clone() {
            Some(path) => path,
            None => self.select_path()?,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| PlanError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&path, content).map_err(|source| PlanError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    pub fn load(&self) -> Result<Option<String>, PlanError> {
        let Some(path) = &self.selected_path else {
            return Ok(None);
        };
        fs::read_to_string(path)
            .map(Some)
            .map_err(|source| PlanError::Read {
                path: path.clone(),
                source,
            })
    }

    pub fn reset(&mut self) {
        self.selected_path = None;
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_path.as_deref()
    }

    fn plans_dir(&self) -> PathBuf {
        WorkspacePaths::new(&self.work_dir).plans_dir()
    }
}
