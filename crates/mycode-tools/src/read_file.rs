use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::{ToolContext, resolve_workspace_path},
    tool::{Tool, ToolCategory, ToolResult},
};

#[derive(Debug, Deserialize)]
struct ReadFileRequest {
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug)]
pub struct ReadFileTool;

impl ReadFileTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "ReadFile"
    }

    fn description(&self) -> &'static str {
        "Reads a UTF-8 text file with numbered lines."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
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
                    "offset": {
                        "type": "integer",
                        "description": "Zero-based starting line",
                        "default": 0
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines",
                        "default": 2000
                    }
                },
                "required": ["file_path"]
            }
        })
    }

    fn permission_argument(&self, arguments: &Value) -> Option<String> {
        arguments.get("file_path")?.as_str().map(str::to_string)
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, ReadFileRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid ReadFile arguments: {source}"));
            }
        };
        let path = match resolve_workspace_path(&request.file_path, context.workspace_root()) {
            Ok(path) => path,
            Err(message) => return ToolResult::error(message),
        };
        let contents = match std::fs::read(&path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return ToolResult::error(format!("file not found: {}", request.file_path));
            }
            Err(source) => {
                return ToolResult::error(format!("failed to read {}: {source}", path.display()));
            }
        };
        let text = match String::from_utf8(contents.clone()) {
            Ok(text) => text,
            Err(_) => {
                return ToolResult::error(format!("file is not UTF-8 text: {}", request.file_path));
            }
        };
        if let Err(source) = context.file_state().record(&path, &contents) {
            return ToolResult::error(format!(
                "failed to record file state for {}: {source}",
                request.file_path
            ));
        }

        let lines: Vec<&str> = text.lines().collect();
        let offset = request.offset.unwrap_or(0);
        let limit = request.limit.unwrap_or(2000);
        let selected = lines
            .iter()
            .enumerate()
            .skip(offset)
            .take(limit)
            .map(|(index, line)| format!("{}\t{}", index + 1, line))
            .collect::<Vec<_>>();
        ToolResult::success(selected.join("\n"))
    }
}
