use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};

pub fn message(role: MessageRole, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage {
        role,
        content,
        timestamp_unix_seconds: 42,
    }
}
