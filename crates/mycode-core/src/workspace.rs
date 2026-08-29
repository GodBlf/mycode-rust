use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    work_dir: PathBuf,
}

impl WorkspacePaths {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }

    pub fn home_config_file(home_dir: &Path) -> PathBuf {
        home_dir.join(".mycode/config.yaml")
    }

    pub fn config_file(&self) -> PathBuf {
        self.work_dir.join(".mycode/config.yaml")
    }

    pub fn local_config_file(&self) -> PathBuf {
        self.work_dir.join(".mycode/config.local.yaml")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.work_dir.join(".mycode/sessions")
    }

    pub fn plans_dir(&self) -> PathBuf {
        self.work_dir.join(".mycode/plans")
    }
}
