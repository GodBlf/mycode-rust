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
struct GrepRequest {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug)]
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "Grep"
    }

    fn description(&self) -> &'static str {
        "Searches workspace file lines with a regular expression."
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
                        "description": "Regular expression to match"
                    },
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative base directory",
                        "default": "."
                    },
                    "include": {
                        "type": "string",
                        "description": "Filename glob filter, for example *.rs"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matching lines",
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
        let request = match serde_path_to_error::deserialize::<_, GrepRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid Grep arguments: {source}"));
            }
        };
        let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return ToolResult::error("Grep limit must be greater than zero");
        }
        let regex = match regex::Regex::new(&request.pattern) {
            Ok(regex) => regex,
            Err(source) => {
                return ToolResult::error(format!(
                    "invalid Grep regular expression {}: {source}",
                    request.pattern
                ));
            }
        };
        let include = match request.include.as_deref() {
            Some(include) => match globset::Glob::new(include) {
                Ok(include) => Some(include.compile_matcher()),
                Err(source) => {
                    return ToolResult::error(format!(
                        "invalid Grep include pattern {include}: {source}"
                    ));
                }
            },
            None => None,
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
                "Grep path is not a directory: {}",
                request.path.as_deref().unwrap_or(".")
            ));
        }

        let files = match walk_files(&base) {
            Ok(files) => files,
            Err(source) => return ToolResult::error(format!("failed to walk files: {source}")),
        };
        let mut matches = Vec::new();
        for file in files {
            let Some(file_name) = file.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if include
                .as_ref()
                .is_some_and(|include| !include.is_match(file_name))
            {
                continue;
            }
            let contents = match std::fs::read_to_string(&file) {
                Ok(contents) => contents,
                Err(source) => {
                    return ToolResult::error(format!(
                        "failed to read search file {}: {source}",
                        file.display()
                    ));
                }
            };
            for (index, line) in contents.lines().enumerate() {
                if regex.is_match(line) {
                    let relative = file
                        .strip_prefix(&base)
                        .unwrap_or(&file)
                        .display()
                        .to_string();
                    matches.push(format!("{}:{}:{}", relative, index + 1, line));
                    if matches.len() == limit {
                        return ToolResult::success(matches.join("\n"));
                    }
                }
            }
        }

        if matches.is_empty() {
            ToolResult::success("No matches found.")
        } else {
            ToolResult::success(matches.join("\n"))
        }
    }
}
