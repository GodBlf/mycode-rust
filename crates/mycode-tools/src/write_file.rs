use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::{ToolContext, resolve_workspace_path},
    tool::{Tool, ToolCategory, ToolResult},
};

#[derive(Debug, Deserialize)]
struct WriteFileRequest {
    file_path: String,
    content: String,
}

#[derive(Debug)]
pub struct WriteFileTool;

impl WriteFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "WriteFile"
    }

    fn description(&self) -> &'static str {
        "Writes complete UTF-8 file content after read-before-write validation."
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
                    "content": {
                        "type": "string",
                        "description": "Complete replacement file content"
                    }
                },
                "required": ["file_path", "content"]
            }
        })
    }

    fn permission_argument(&self, arguments: &Value) -> Option<String> {
        arguments.get("file_path")?.as_str().map(str::to_string)
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, WriteFileRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid WriteFile arguments: {source}"));
            }
        };
        let path = match resolve_workspace_path(&request.file_path, context.workspace_root()) {
            Ok(path) => path,
            Err(message) => return ToolResult::error(message),
        };

        if let Err(source) = context.file_state().check_for_write(&path) {
            return ToolResult::error(format!("cannot write {}: {source}", request.file_path));
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
        let write_result = write_file(&path, request.content.as_bytes());
        if let Err(source) = write_result {
            if let Err(rollback_source) = context.file_history().rollback(backup) {
                return ToolResult::error(format!(
                    "failed to write {} ({source}) and roll back file history ({rollback_source})",
                    request.file_path
                ));
            }
            return ToolResult::error(format!("failed to write {}: {source}", request.file_path));
        }
        context.file_history().commit(backup);
        if let Err(source) = context
            .file_state()
            .update(&path, request.content.as_bytes())
        {
            return ToolResult::error(format!(
                "wrote {}, but failed to update its state: {source}",
                request.file_path
            ));
        }

        ToolResult::success(format!("Successfully wrote to {}", request.file_path))
    }
}

fn write_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}
