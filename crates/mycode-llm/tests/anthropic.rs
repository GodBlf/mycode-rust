mod support;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{
    AnthropicClient, LlmClient, LlmError, LlmEvent, StopReason, ToolDefinition, Usage,
};
use tokio_util::sync::CancellationToken;

fn provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "anthropic".into(),
        protocol: ProviderProtocol::Anthropic,
        base_url,
        model: "claude-sonnet-4-5".into(),
        api_key: "test-key".into(),
        thinking: true,
        context_window: Some(200_000),
        max_output_tokens: None,
    }
}

fn adaptive_provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "anthropic".into(),
        protocol: ProviderProtocol::Anthropic,
        base_url,
        model: "claude-sonnet-4-6".into(),
        api_key: "test-key".into(),
        thinking: true,
        context_window: Some(200_000),
        max_output_tokens: None,
    }
}

fn request() -> mycode_llm::LlmRequest {
    let mut conversation = Conversation::new();
    conversation.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "inspect this".into(),
        }],
        timestamp_unix_seconds: 1,
    });
    conversation.push(ConversationMessage {
        role: MessageRole::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "I should read the file".into(),
                signature: "signature".into(),
            },
            ContentBlock::Text {
                text: "I will read it".into(),
            },
            ContentBlock::ToolUse {
                tool_use_id: "call-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            },
        ],
        timestamp_unix_seconds: 2,
    });
    conversation.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call-1".into(),
            content: "file contents".into(),
            is_error: false,
        }],
        timestamp_unix_seconds: 3,
    });

    mycode_llm::LlmRequest {
        system_prompt: "be concise".into(),
        conversation,
        tools: vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }],
    }
}

#[tokio::test]
async fn anthropic_adaptive_thinking_models_use_adaptive_mode() {
    let (base_url, request_receiver) =
        support::serve_once(support::sse_response(&[r#"{"type":"message_stop"}"#])).await;
    let client = AnthropicClient::new(&adaptive_provider(base_url), "system")
        .expect("client should construct");
    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("stream should start");
    while stream.recv().await.is_some() {}

    let request = request_receiver.await.expect("request should be captured");
    let body = support::request_body(&request);
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert!(body["thinking"].get("budget_tokens").is_none());
}

#[tokio::test]
async fn anthropic_client_builds_requests_and_decodes_streaming_events() {
    let response = support::sse_response(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":120,"output_tokens":1,"cache_read_input_tokens":5000,"cache_creation_input_tokens":200}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"signature"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-2","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"README.md\"}"}}"#,
        r#"{"type":"content_block_stop","index":2}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let (base_url, request_receiver) = support::serve_once(response).await;
    let client =
        AnthropicClient::new(&provider(base_url), "be concise").expect("client should construct");

    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("stream should start");
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event.expect("stream should not fail"));
    }

    assert_eq!(
        events,
        vec![
            LlmEvent::TextDelta {
                text: "hello".into()
            },
            LlmEvent::ThinkingDelta {
                text: "thinking".into()
            },
            LlmEvent::ThinkingComplete {
                thinking: "thinking".into(),
                signature: "signature".into()
            },
            LlmEvent::ToolCallDelta {
                text: "{\"path\":".into()
            },
            LlmEvent::ToolCallDelta {
                text: "\"README.md\"}".into()
            },
            LlmEvent::ToolCallComplete {
                tool_id: "call-2".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            },
            LlmEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 120,
                    output_tokens: 42,
                    cache_read_tokens: 5000,
                    cache_creation_tokens: 200,
                },
            },
        ]
    );

    let request = request_receiver.await.expect("request should be captured");
    let body = support::request_body(&request);
    let headers = support::request_headers(&request);
    assert!(request.starts_with("POST /v1/messages HTTP/1.1\r\n"));
    assert!(headers.contains(&("x-api-key".to_string(), "test-key".to_string())));
    assert!(headers.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())));
    assert_eq!(body["model"], "claude-sonnet-4-5");
    assert_eq!(body["max_tokens"], 64_000);
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"][0]["text"], "be concise");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(
        body["messages"][1]["content"][0],
        serde_json::json!({
            "type": "thinking",
            "thinking": "I should read the file",
            "signature": "signature"
        })
    );
    assert_eq!(
        body["messages"][1]["content"][2],
        serde_json::json!({
            "type": "tool_use",
            "id": "call-1",
            "name": "read_file",
            "input": {"path": "README.md"}
        })
    );
    assert_eq!(
        body["messages"][2]["content"][0],
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "call-1",
            "content": [{"type": "text", "text": "file contents"}],
            "is_error": false
        })
    );
    assert_eq!(
        body["tools"][0],
        serde_json::json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            },
            "cache_control": {"type": "ephemeral"}
        })
    );
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 63_999);
}

#[tokio::test]
async fn anthropic_client_maps_http_authentication_errors() {
    let (base_url, _request) = support::serve_once(support::http_response(
        401,
        r#"{"error":{"message":"invalid api key"}}"#,
    ))
    .await;
    let client =
        AnthropicClient::new(&provider(base_url), "system").expect("client should construct");

    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("stream should start");
    assert!(matches!(
        stream.recv().await,
        Some(Err(LlmError::Authentication { .. }))
    ));
}

#[tokio::test]
async fn anthropic_client_cancellation_aborts_request() {
    let base_url = support::serve_hanging().await;
    let client =
        AnthropicClient::new(&provider(base_url), "system").expect("client should construct");
    let cancellation = CancellationToken::new();
    let mut stream = client
        .stream(request(), cancellation.clone())
        .await
        .expect("stream should start");

    cancellation.cancel();
    assert_eq!(stream.recv().await, Some(Err(LlmError::Cancelled)));
}
