mod support;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{
    LlmClient, LlmError, LlmEvent, OpenAiCompatClient, StopReason, ToolDefinition, Usage,
};
use tokio_util::sync::CancellationToken;

fn provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "openai-compatible".into(),
        protocol: ProviderProtocol::OpenAiCompat,
        base_url,
        model: "compatible-model".into(),
        api_key: "test-key".into(),
        thinking: false,
        context_window: None,
        max_output_tokens: Some(4_096),
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
                thinking: "unsupported in chat completions".into(),
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
async fn openai_compat_client_builds_requests_and_assembles_tool_calls() {
    let response = support::sse_response(&[
        r#"{"choices":[{"delta":{"content":"hello"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-2","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call-3","function":{"name":"search","arguments":"{\"query\":\"docs\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":42,"prompt_tokens_details":{"cached_tokens":20}}}"#,
    ]);
    let (base_url, request_receiver) = support::serve_once(response).await;
    let client = OpenAiCompatClient::new(&provider(base_url), "be concise")
        .expect("client should construct");

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
            LlmEvent::ToolCallStart {
                tool_id: "call-2".into(),
                tool_name: "read_file".into()
            },
            LlmEvent::ToolCallDelta {
                text: "{\"path\":".into()
            },
            LlmEvent::ToolCallDelta {
                text: "\"README.md\"}".into()
            },
            LlmEvent::ToolCallStart {
                tool_id: "call-3".into(),
                tool_name: "search".into()
            },
            LlmEvent::ToolCallDelta {
                text: "{\"query\":\"docs\"}".into()
            },
            LlmEvent::ToolCallComplete {
                tool_id: "call-2".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            },
            LlmEvent::ToolCallComplete {
                tool_id: "call-3".into(),
                tool_name: "search".into(),
                arguments: serde_json::json!({"query": "docs"}),
            },
            LlmEvent::StreamEnd {
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
    assert!(request.starts_with("POST /chat/completions HTTP/1.1\r\n"));
    assert!(headers.contains(&("authorization".to_string(), "Bearer test-key".to_string())));
    assert_eq!(body["model"], "compatible-model");
    assert_eq!(body["max_tokens"], 4_096);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "system", "content": "be concise"})
    );
    assert_eq!(
        body["messages"][2],
        serde_json::json!({
            "role": "assistant",
            "content": "I will read it",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            }]
        })
    );
    assert_eq!(
        body["messages"][3],
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "call-1",
            "content": "file contents"
        })
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["strict"], false);
}

#[tokio::test]
async fn openai_compat_client_maps_context_length_errors() {
    let response = support::http_response(
        400,
        r#"{"error":{"message":"This model's maximum context length is 4096 tokens"}}"#,
    );
    let (base_url, _request) = support::serve_once(response).await;
    let client =
        OpenAiCompatClient::new(&provider(base_url), "system").expect("client should construct");

    let mut stream = client
        .stream(request(), CancellationToken::new())
        .await
        .expect("stream should start");
    assert!(matches!(
        stream.recv().await,
        Some(Err(LlmError::ContextTooLong { .. }))
    ));
}

#[tokio::test]
async fn openai_compat_client_cancellation_aborts_request() {
    let base_url = support::serve_hanging().await;
    let client =
        OpenAiCompatClient::new(&provider(base_url), "system").expect("client should construct");
    let cancellation = CancellationToken::new();
    let mut stream = client
        .stream(request(), cancellation.clone())
        .await
        .expect("stream should start");

    cancellation.cancel();
    assert_eq!(stream.recv().await, Some(Err(LlmError::Cancelled)));
}
