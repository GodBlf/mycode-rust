use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{LlmClient, LlmError, LlmEvent, MockClient, StopReason, ToolDefinition, Usage};
use tokio_util::sync::CancellationToken;

fn request() -> mycode_llm::LlmRequest {
    let mut conversation = Conversation::new();
    conversation.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "hello".into(),
        }],
        timestamp_unix_seconds: 1,
    });

    mycode_llm::LlmRequest {
        system_prompt: "system".into(),
        conversation,
        tools: vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }],
    }
}

#[tokio::test]
async fn mock_client_replays_events_and_captures_the_request() {
    let client = MockClient::new(vec![
        LlmEvent::TextDelta {
            text: "hello".into(),
        },
        LlmEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: 30,
                cache_creation_tokens: 40,
            },
        },
    ]);

    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("mock stream should start");

    assert_eq!(
        stream.recv().await,
        Some(Ok(LlmEvent::TextDelta {
            text: "hello".into()
        }))
    );
    assert_eq!(
        stream.recv().await,
        Some(Ok(LlmEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: 30,
                cache_creation_tokens: 40,
            },
        }))
    );
    assert_eq!(stream.recv().await, None);

    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system_prompt, "system");
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].conversation.first_user_text(), Some("hello"));
}

#[tokio::test]
async fn mock_client_replays_errors_and_cancellation() {
    let client = MockClient::with_results(vec![
        Ok(LlmEvent::TextDelta {
            text: "partial".into(),
        }),
        Err(LlmError::Authentication {
            message: "invalid key".into(),
        }),
    ]);

    let cancellation = CancellationToken::new();
    let mut stream = client
        .stream(request(), cancellation.clone())
        .await
        .expect("mock stream should start");

    assert_eq!(
        stream.recv().await,
        Some(Ok(LlmEvent::TextDelta {
            text: "partial".into()
        }))
    );
    cancellation.cancel();
    assert_eq!(stream.recv().await, Some(Err(LlmError::Cancelled)));
}
