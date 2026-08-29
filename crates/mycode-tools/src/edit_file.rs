use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::{ToolContext, resolve_workspace_path},
    tool::{PermissionSubject, Tool, ToolCategory, ToolResult},
};

#[derive(Debug, Deserialize)]
struct EditFileRequest {
    file_path: String,
    old_string: String,
    new_string: String,
}

#[derive(Debug)]
pub struct EditFileTool;

impl EditFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "EditFile"
    }

    fn description(&self) -> &'static str {
        "Replaces one unique exact string in a file that was read first."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute or workspace-relative file path"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact string to replace; it must occur once"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement string"
                    }
                },
                "required": ["file_path", "old_string", "new_string"]
            }
        })
    }

    fn permission_subject(&self, arguments: &Value) -> Option<PermissionSubject> {
        arguments
            .get("file_path")?
            .as_str()
            .map(|path| PermissionSubject::Path(path.to_string()))
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, EditFileRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid EditFile arguments: {source}"));
            }
        };
        if request.old_string.is_empty() {
            return ToolResult::error("old_string must not be empty");
        }
        let path = match resolve_workspace_path(&request.file_path, context.workspace_root()) {
            Ok(path) => path,
            Err(message) => return ToolResult::error(message),
        };
        if let Err(source) = context.file_state().check(&path) {
            return ToolResult::error(format!("cannot edit {}: {source}", request.file_path));
        }

        let text = match crate::file_io::read_utf8(&path) {
            Ok(text) => text,
            Err(crate::file_io::FileReadError::NotFound) => {
                return ToolResult::error(format!("file not found: {}", request.file_path));
            }
            Err(crate::file_io::FileReadError::Io(source)) => {
                return ToolResult::error(format!("failed to read {}: {source}", path.display()));
            }
            Err(crate::file_io::FileReadError::InvalidUtf8) => {
                return ToolResult::error(format!("file is not UTF-8 text: {}", request.file_path));
            }
        };
        let match_count = text.matches(&request.old_string).count();
        if match_count == 0 {
            return ToolResult::error("old_string was not found in the file");
        }
        if match_count > 1 {
            return ToolResult::error(format!(
                "old_string occurred {match_count} times; it must be unique"
            ));
        }

        let backup = match context.file_history().capture(&path) {
            Ok(backup) => backup,
            Err(source) => {
                return ToolResult::error(format!(
                    "failed to capture file history for {}: {source}",
                    request.file_path
                ));
            }
        };
        let new_text = text.replacen(&request.old_string, &request.new_string, 1);
        if let Err(source) = std::fs::write(&path, new_text.as_bytes()) {
            if let Err(rollback_source) = context.file_history().rollback(backup) {
                return ToolResult::error(format!(
                    "failed to edit {} ({source}) and roll back file history ({rollback_source})",
                    request.file_path
                ));
            }
            return ToolResult::error(format!("failed to write {}: {source}", request.file_path));
        }
        context.file_history().commit(backup);
        if let Err(source) = context.file_state().update(&path, new_text.as_bytes()) {
            return ToolResult::error(format!(
                "edited {}, but failed to update its state: {source}",
                request.file_path
            ));
        }

        ToolResult::success(format!("Successfully edited {}", request.file_path))
    }
}
