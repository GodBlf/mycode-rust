use std::{fs, path::PathBuf};

use crate::context::normalize_path;

#[derive(Debug)]
pub struct PathSandbox {
    workspace_root: PathBuf,
}

impl PathSandbox {
    pub fn new(workspace_root: impl AsRef<std::path::Path>) -> Self {
        let root = workspace_root.as_ref();
        let workspace_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self { workspace_root }
    }

    pub fn check(&self, path: &str) -> Result<(), String> {
        let normalized = normalize_path(std::path::Path::new(path), &self.workspace_root);
        if normalized.strip_prefix(&self.workspace_root).is_ok() {
            Ok(())
        } else {
            Err(format!(
                "path {path} is outside the workspace {}",
                self.workspace_root.display()
            ))
        }
    }
}
