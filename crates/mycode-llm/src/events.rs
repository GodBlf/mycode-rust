use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Other(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ThinkingComplete {
        thinking: String,
        signature: String,
    },
    ToolCallStart {
        tool_id: String,
        tool_name: String,
    },
    ToolCallDelta {
        text: String,
    },
    ToolCallComplete {
        tool_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    StreamEnd {
        stop_reason: StopReason,
        usage: Usage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LlmError {
    #[error("provider authentication failed: {message}")]
    Authentication { message: String },
    #[error("provider rate limit exceeded: {message}")]
    RateLimit {
        message: String,
        retry_after: Option<String>,
    },
    #[error("provider network request failed: {message}")]
    Network { message: String },
    #[error("provider context is too long: {message}")]
    ContextTooLong { message: String },
    #[error("provider returned an invalid response: {message}")]
    InvalidResponse { message: String },
    #[error("provider request was cancelled")]
    Cancelled,
}
