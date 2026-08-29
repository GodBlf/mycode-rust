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

    fn permission_argument(&self, arguments: &Value) -> Option<String>;

    fn is_deferred(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult;
}
