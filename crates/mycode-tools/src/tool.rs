use serde_json::Value;

use crate::context::ToolContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Read,
    Write,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionSubject {
    Command(String),
    Path(String),
    Search {
        pattern: String,
        path: Option<String>,
    },
    Query(String),
}

impl PermissionSubject {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Command(value) | Self::Path(value) | Self::Query(value) => value,
            Self::Search { pattern, .. } => pattern,
        }
    }
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn category(&self) -> ToolCategory;

    fn schema(&self) -> Value;

    fn permission_subject(&self, arguments: &Value) -> Option<PermissionSubject>;

    fn is_deferred(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult;
}
