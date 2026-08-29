use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::{ToolContext, resolve_workspace_path},
    search::walk_files,
    tool::PermissionSubject,
    tool::{Tool, ToolCategory, ToolResult},
};

const DEFAULT_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
struct GlobRequest {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug)]
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "Glob"
    }

    fn description(&self) -> &'static str {
        "Finds workspace files with glob patterns, including recursive double-star patterns."
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
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern, for example **/*.rs"
                    },
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative base directory",
                        "default": "."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches",
                        "default": DEFAULT_LIMIT
                    }
                },
                "required": ["pattern"]
            }
        })
    }

    fn permission_subject(&self, arguments: &Value) -> Option<PermissionSubject> {
        arguments
            .get("pattern")?
            .as_str()
            .map(|pattern| PermissionSubject::Search {
                pattern: pattern.to_string(),
                path: arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, GlobRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid Glob arguments: {source}"));
            }
        };
        let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return ToolResult::error("Glob limit must be greater than zero");
        }
        let pattern = match globset::Glob::new(&request.pattern) {
            Ok(pattern) => pattern.compile_matcher(),
            Err(source) => {
                return ToolResult::error(format!(
                    "invalid Glob pattern {}: {source}",
                    request.pattern
                ));
            }
        };
        let base = match resolve_workspace_path(
            request.path.as_deref().unwrap_or("."),
            context.workspace_root(),
        ) {
            Ok(base) => base,
            Err(message) => return ToolResult::error(message),
        };
        if !base.is_dir() {
            return ToolResult::error(format!(
                "Glob path is not a directory: {}",
                request.path.as_deref().unwrap_or(".")
            ));
        }

        let files = match walk_files(&base) {
            Ok(files) => files,
            Err(source) => return ToolResult::error(format!("failed to walk files: {source}")),
        };
        let matches = files
            .iter()
            .filter_map(|path| path.strip_prefix(&base).ok())
            .filter(|path| {
                path.to_str()
                    .is_some_and(|relative| pattern.is_match(relative))
            })
            .take(limit)
            .filter_map(|path| path.to_str().map(str::to_string))
            .collect::<Vec<_>>();

        if matches.is_empty() {
            ToolResult::success("No files matched the pattern.")
        } else {
            ToolResult::success(matches.join("\n"))
        }
    }
}
