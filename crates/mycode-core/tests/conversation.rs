use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use serde_json::json;

fn message(role: MessageRole, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage {
        role,
        content,
        timestamp_unix_seconds: 42,
    }
}

#[test]
fn conversation_round_trips_every_content_block_kind_in_order() {
    let mut conversation = Conversation::new();
    conversation.push(message(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "Inspect this repository".into(),
        }],
    ));
    conversation.push(message(
        MessageRole::Assistant,
        vec![
            ContentBlock::Thinking {
                thinking: "I will inspect the files".into(),
                signature: "signature".into(),
            },
            ContentBlock::ToolUse {
                tool_use_id: "tool-1".into(),
                tool_name: "read_file".into(),
                arguments: json!({ "path": "README.md" }),
            },
        ],
    ));
    conversation.push(message(MessageRole::System, Vec::new()));
    conversation.push(message(
        MessageRole::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool-1".into(),
            content: "file contents".into(),
            is_error: false,
        }],
    ));

    let json = conversation
        .to_json()
        .expect("serialization should succeed");
    let restored = Conversation::from_json(&json).expect("deserialization should succeed");

    assert_eq!(restored, conversation);
    assert_eq!(restored.messages().len(), 4);
    assert_eq!(restored.messages()[2].role, MessageRole::System);
}

#[test]
fn conversation_deserialization_tolerates_unknown_fields() {
    let json = r#"{
      "messages": [
        {
          "role": "user",
          "content": [
            {
              "type": "text",
              "text": "hello",
              "future_field": "ignored"
            }
          ],
          "timestamp_unix_seconds": 7,
          "another_future_field": 12
        }
      ],
      "schema_version": "future"
    }"#;

    let conversation = Conversation::from_json(json).expect("unknown fields should be ignored");
    let message = &conversation.messages()[0];

    assert_eq!(message.role, MessageRole::User);
    assert_eq!(
        message.content,
        vec![ContentBlock::Text {
            text: "hello".into()
        }]
    );
    assert_eq!(message.timestamp_unix_seconds, 7);
}
