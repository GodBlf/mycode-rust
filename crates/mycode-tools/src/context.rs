use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use mycode_core::{
    session::{SessionError, SessionId},
    workspace::WorkspacePaths,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{file_history::FileHistory, file_state::FileStateCache};

#[derive(Debug, Error)]
pub enum ToolContextError {
    #[error("the workspace directory could not be resolved")]
    WorkspaceResolution(#[source] std::io::Error),
    #[error(
        "invalid session ID: session IDs may contain ASCII letters, numbers, hyphens, and underscores"
    )]
    InvalidSessionId(#[from] SessionError),
    #[error("failed to create the file history directory")]
    CreateFileHistoryDirectory(#[source] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    workspace_root: PathBuf,
    file_state: Arc<FileStateCache>,
    file_history: Arc<FileHistory>,
    cancellation: CancellationToken,
}

impl ToolContext {
    pub fn new(
        work_dir: impl AsRef<Path>,
        session_id: impl AsRef<str>,
    ) -> Result<Self, ToolContextError> {
        let workspace_root =
            fs::canonicalize(work_dir).map_err(ToolContextError::WorkspaceResolution)?;
        let session_id = SessionId::new(session_id.as_ref().to_string())?;
        let history_dir = WorkspacePaths::new(&workspace_root)
            .file_history_dir()
            .join(session_id.as_str());
        fs::create_dir_all(&history_dir).map_err(ToolContextError::CreateFileHistoryDirectory)?;

        Ok(Self {
            workspace_root,
            file_state: Arc::new(FileStateCache::new()),
            file_history: Arc::new(FileHistory::new(history_dir)),
            cancellation: CancellationToken::new(),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn file_state(&self) -> Arc<FileStateCache> {
        Arc::clone(&self.file_state)
    }

    pub fn file_history(&self) -> Arc<FileHistory> {
        Arc::clone(&self.file_history)
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

pub(crate) fn normalize_path(path: &Path, workspace_root: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let cleaned = clean_components(&absolute);
    canonicalize_longest_existing_ancestor(cleaned)
}

pub(crate) fn resolve_workspace_path(path: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    let normalized = normalize_path(Path::new(path), workspace_root);
    if normalized.strip_prefix(workspace_root).is_ok() {
        Ok(normalized)
    } else {
        Err(format!(
            "path {path} is outside the workspace {}",
            workspace_root.display()
        ))
    }
}

fn clean_components(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            component => cleaned.push(component),
        }
    }
    cleaned
}

fn canonicalize_longest_existing_ancestor(path: PathBuf) -> PathBuf {
    let mut existing = path.clone();
    while !existing.exists() {
        if !existing.pop() {
            return path;
        }
    }

    match fs::canonicalize(&existing) {
        Ok(canonical) => {
            let suffix = path
                .strip_prefix(&existing)
                .expect("existing is an ancestor of path");
            if suffix.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(suffix)
            }
        }
        Err(_) => path,
    }
}
