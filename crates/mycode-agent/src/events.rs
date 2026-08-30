use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta {
        text: String,
    },
    ToolCall {
        tool_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    PermissionRequest {
        request_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        reason: String,
    },
    PermissionDecision {
        request_id: String,
        allowed: bool,
    },
    ToolResult {
        tool_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
    },
    RunCompleted {
        final_text: String,
    },
    MaxIterationsReached {
        limit: usize,
    },
    RunError {
        message: String,
    },
    RunCancelled,
}
