mod support;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{
    LlmClient, OpenAiClient, ProviderError, ProviderEvent, StopReason, ToolDefinition, Usage,
};
use tokio_util::sync::CancellationToken;

fn provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "openai".into(),
        protocol: ProviderProtocol::OpenAi,
        base_url,
        model: "gpt-4.1".into(),
        api_key: "test-key".into(),
        thinking: true,
        context_window: Some(1_000_000),
        max_output_tokens: None,
    }
}

fn request() -> mycode_llm::ProviderRequest {
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
                signature: "reasoning-id".into(),
                encrypted_content: "encrypted-reasoning".into(),
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

    mycode_llm::ProviderRequest {
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
async fn openai_client_builds_requests_and_decodes_streaming_events() {
    let response = support::sse_response(&[
        r#"{"type":"response.output_text.delta","delta":"hello"}"#,
        r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"reasoning-id","encrypted_content":"encrypted-response"}}"#,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#,
        r#"{"type":"response.reasoning_summary_text.done"}"#,
        r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call-2","name":"read_file"}}"#,
        r#"{"type":"response.function_call_arguments.delta","delta":"{\"path\":"}"#,
        r#"{"type":"response.function_call_arguments.delta","delta":"\"README.md\"}"}"#,
        r#"{"type":"response.function_call_arguments.done"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":120,"output_tokens":42,"input_tokens_details":{"cached_tokens":20}}}}"#,
    ]);
    let (base_url, request_receiver) = support::serve_once(response).await;
    let client = OpenAiClient::new(&provider(base_url)).expect("client should construct");

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
            ProviderEvent::TextDelta {
                text: "hello".into()
            },
            ProviderEvent::ThinkingDelta {
                text: "thinking".into()
            },
            ProviderEvent::ThinkingComplete {
                thinking: "thinking".into(),
                signature: "reasoning-id".into(),
                encrypted_content: "encrypted-response".into()
            },
            ProviderEvent::ToolCallStart {
                tool_id: "call-2".into(),
                tool_name: "read_file".into()
            },
            ProviderEvent::ToolCallDelta {
                text: "{\"path\":".into()
            },
            ProviderEvent::ToolCallDelta {
                text: "\"README.md\"}".into()
            },
            ProviderEvent::ToolCallComplete {
                tool_id: "call-2".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 42,
                    cache_read_tokens: 20,
                    cache_creation_tokens: 0,
                },
            },
        ]
    );

    let request = request_receiver.await.expect("request should be captured");
    let body = support::request_body(&request);
    let headers = support::request_headers(&request);
    assert!(request.starts_with("POST /responses HTTP/1.1\r\n"));
    assert!(headers.contains(&("authorization".to_string(), "Bearer test-key".to_string())));
    assert_eq!(body["model"], "gpt-4.1");
    assert_eq!(body["max_output_tokens"], 64_000);
    assert_eq!(body["instructions"], "be concise");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["input"][1],
        serde_json::json!({
            "type": "reasoning",
            "id": "reasoning-id",
            "encrypted_content": "encrypted-reasoning",
            "summary": [{"type": "summary_text", "text": "I should read the file"}]
        })
    );
    assert_eq!(
        body["input"][2],
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": "I will read it"
        })
    );
    assert_eq!(
        body["input"][3],
        serde_json::json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "read_file",
            "arguments": "{\"path\":\"README.md\"}"
        })
    );
    assert_eq!(
        body["input"][4],
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "file contents"
        })
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["strict"], false);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["reasoning"]["summary"], "detailed");
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
}

#[tokio::test]
async fn openai_client_maps_http_rate_limit_errors() {
    let response = support::http_response(429, r#"{"error":{"message":"rate limited"}}"#);
    let (base_url, _request) = support::serve_once(response).await;
    let client = OpenAiClient::new(&provider(base_url)).expect("client should construct");

    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("stream should start");
    assert!(matches!(
        stream.recv().await,
        Some(Err(ProviderError::RateLimit { .. }))
    ));
}

#[tokio::test]
async fn openai_client_cancellation_aborts_request() {
    let base_url = support::serve_hanging().await;
    let client = OpenAiClient::new(&provider(base_url)).expect("client should construct");
    let cancellation = CancellationToken::new();
    let mut stream = client
        .stream(request(), cancellation.clone())
        .await
        .expect("stream should start");

    cancellation.cancel();
    assert_eq!(stream.recv().await, Some(Err(ProviderError::Cancelled)));
}
