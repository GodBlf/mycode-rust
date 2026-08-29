use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use mycode_core::conversation::Conversation;

use crate::events::{LlmError, LlmEvent};

pub type LlmStream = mpsc::Receiver<Result<LlmEvent, LlmError>>;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub system_prompt: String,
    pub conversation: Conversation,
    pub tools: Vec<ToolDefinition>,
}

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn stream(
        &self,
        request: LlmRequest,
        cancellation: CancellationToken,
    ) -> Result<LlmStream, LlmError>;
}
