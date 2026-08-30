use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use mycode_core::conversation::Conversation;

use crate::events::{ProviderError, ProviderEvent};

pub type ProviderStream = mpsc::Receiver<Result<ProviderEvent, ProviderError>>;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequest {
    pub system_prompt: String,
    pub conversation: Conversation,
    pub tools: Vec<ToolDefinition>,
}

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError>;
}
