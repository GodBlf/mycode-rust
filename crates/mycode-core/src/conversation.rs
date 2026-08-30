use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversationError {
    #[error("conversation could not be serialized")]
    Serialization(#[source] serde_json::Error),
    #[error("conversation JSON was invalid")]
    Deserialization(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        encrypted_content: String,
    },
    ToolUse {
        tool_use_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub timestamp_unix_seconds: u64,
}

impl ConversationMessage {
    pub fn first_text(&self) -> Option<&str> {
        self.content.iter().find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }
}

pub(crate) fn first_user_text(messages: &[ConversationMessage]) -> Option<&str> {
    messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .and_then(ConversationMessage::first_text)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    messages: Vec<ConversationMessage>,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, message: ConversationMessage) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    pub fn first_user_text(&self) -> Option<&str> {
        first_user_text(&self.messages)
    }

    pub fn into_messages(self) -> Vec<ConversationMessage> {
        self.messages
    }

    pub fn to_json(&self) -> Result<String, ConversationError> {
        serde_json::to_string(self).map_err(ConversationError::Serialization)
    }

    pub fn from_json(json: &str) -> Result<Self, ConversationError> {
        serde_json::from_str(json).map_err(ConversationError::Deserialization)
    }
}
