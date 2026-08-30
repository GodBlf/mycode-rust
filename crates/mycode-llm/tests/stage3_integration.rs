mod support;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{
    ProviderError, ProviderEvent, StopReason, ToolDefinition, Usage, build_provider_client_with,
};
use tokio_util::sync::CancellationToken;

fn provider(protocol: ProviderProtocol, base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: format!("{protocol:?}-provider"),
        protocol,
        base_url,
        model: "test-model".into(),
        api_key: "test-key".into(),
        thinking: false,
        context_window: Some(128_000),
        max_output_tokens: Some(1_024),
    }
}

fn request() -> mycode_llm::ProviderRequest {
    let mut conversation = Conversation::new();
    conversation.push(ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "hello".into(),
        }],
        timestamp_unix_seconds: 1,
    });

    mycode_llm::ProviderRequest {
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
async fn provider_factory_streams_equivalent_events_for_all_protocols() {
    let cases = [
        (
            ProviderProtocol::Anthropic,
            support::sse_response(&[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"thinking"}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"signature"}}"#,
                r#"{"type":"content_block_stop","index":1}"#,
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-2","name":"read_file"}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"README.md\"}"}}"#,
                r#"{"type":"content_block_stop","index":2}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}"#,
                r#"{"type":"message_stop"}"#,
            ]),
            vec![
                ProviderEvent::TextDelta {
                    text: "hello".into(),
                },
                ProviderEvent::ThinkingDelta {
                    text: "thinking".into(),
                },
                ProviderEvent::ThinkingComplete {
                    thinking: "thinking".into(),
                    signature: "signature".into(),
                    encrypted_content: String::new(),
                },
                ProviderEvent::ToolCallStart {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                },
                ProviderEvent::ToolCallDelta {
                    text: "{\"path\":\"README.md\"}".into(),
                },
                ProviderEvent::ToolCallComplete {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                    arguments: serde_json::json!({"path": "README.md"}),
                },
                ProviderEvent::StreamEnd {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 20,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                    },
                },
            ],
        ),
        (
            ProviderProtocol::OpenAi,
            support::sse_response(&[
                r#"{"type":"response.output_text.delta","delta":"hello"}"#,
                r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"reasoning-id","encrypted_content":"encrypted-response"}}"#,
                r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#,
                r#"{"type":"response.reasoning_summary_text.done"}"#,
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call-2","name":"read_file"}}"#,
                r#"{"type":"response.function_call_arguments.delta","delta":"{\"path\":\"README.md\"}"}"#,
                r#"{"type":"response.function_call_arguments.done"}"#,
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":20}}}"#,
            ]),
            vec![
                ProviderEvent::TextDelta {
                    text: "hello".into(),
                },
                ProviderEvent::ThinkingDelta {
                    text: "thinking".into(),
                },
                ProviderEvent::ThinkingComplete {
                    thinking: "thinking".into(),
                    signature: "reasoning-id".into(),
                    encrypted_content: "encrypted-response".into(),
                },
                ProviderEvent::ToolCallStart {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                },
                ProviderEvent::ToolCallDelta {
                    text: "{\"path\":\"README.md\"}".into(),
                },
                ProviderEvent::ToolCallComplete {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                    arguments: serde_json::json!({"path": "README.md"}),
                },
                ProviderEvent::StreamEnd {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 20,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                    },
                },
            ],
        ),
        (
            ProviderProtocol::OpenAiCompat,
            support::sse_response(&[
                r#"{"choices":[{"delta":{"content":"hello"}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-2","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#,
                "[DONE]",
            ]),
            vec![
                ProviderEvent::TextDelta {
                    text: "hello".into(),
                },
                ProviderEvent::ToolCallStart {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                },
                ProviderEvent::ToolCallDelta {
                    text: "{\"path\":\"README.md\"}".into(),
                },
                ProviderEvent::ToolCallComplete {
                    tool_id: "call-2".into(),
                    tool_name: "read_file".into(),
                    arguments: serde_json::json!({"path": "README.md"}),
                },
                ProviderEvent::StreamEnd {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 20,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                    },
                },
            ],
        ),
    ];

    for (protocol, response, expected) in cases {
        let (base_url, request_receiver) = support::serve_once(response).await;
        let built = build_provider_client_with(&provider(protocol, base_url), |_| {
            Some("environment-key".into())
        })
        .await
        .expect("provider should build");

        let mut stream = built
            .client
            .stream(request(), CancellationToken::new())
            .await
            .expect("stream should start");
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event.expect("stream should not fail"));
        }
        let _ = request_receiver.await;

        assert_eq!(
            events, expected,
            "protocol {protocol:?} should emit provider-neutral events"
        );
        assert_eq!(built.context_window, 128_000);
        assert_eq!(built.max_output_tokens, 1_024);
    }
}

#[tokio::test]
async fn cancellation_is_consistent_for_every_protocol() {
    for protocol in [
        ProviderProtocol::Anthropic,
        ProviderProtocol::OpenAi,
        ProviderProtocol::OpenAiCompat,
    ] {
        let base_url = support::serve_hanging().await;
        let built = build_provider_client_with(&provider(protocol, base_url), |_| {
            Some("environment-key".into())
        })
        .await
        .expect("provider should build");
        let cancellation = CancellationToken::new();
        let mut stream = built
            .client
            .stream(request(), cancellation.clone())
            .await
            .expect("stream should start");

        cancellation.cancel();
        assert_eq!(
            stream.recv().await,
            Some(Err(ProviderError::Cancelled)),
            "protocol {protocol:?} should honour cancellation"
        );
    }
}

#[tokio::test]
async fn malformed_sse_is_reported_as_invalid_response() {
    for protocol in [
        ProviderProtocol::Anthropic,
        ProviderProtocol::OpenAi,
        ProviderProtocol::OpenAiCompat,
    ] {
        let (base_url, _request) = support::serve_once(support::sse_response(&["not-json"])).await;
        let built = build_provider_client_with(&provider(protocol, base_url), |_| {
            Some("environment-key".into())
        })
        .await
        .expect("provider should build");

        let mut stream = built
            .client
            .stream(request(), CancellationToken::new())
            .await
            .expect("stream should start");
        assert!(
            matches!(
                stream.recv().await,
                Some(Err(ProviderError::InvalidResponse { .. }))
            ),
            "protocol {protocol:?} should report malformed SSE"
        );
    }
}

#[tokio::test]
async fn connection_failures_are_reported_as_network_errors() {
    for protocol in [
        ProviderProtocol::Anthropic,
        ProviderProtocol::OpenAi,
        ProviderProtocol::OpenAiCompat,
    ] {
        let built =
            build_provider_client_with(&provider(protocol, "http://127.0.0.1:1".into()), |_| {
                Some("environment-key".into())
            })
            .await
            .expect("provider should build");

        let mut stream = built
            .client
            .stream(request(), CancellationToken::new())
            .await
            .expect("stream should start");
        assert!(
            matches!(
                stream.recv().await,
                Some(Err(ProviderError::Network { .. }))
            ),
            "protocol {protocol:?} should report connection failure"
        );
    }
}
