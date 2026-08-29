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
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}"#,
                r#"{"type":"message_stop"}"#,
            ]),
        ),
        (
            ProviderProtocol::OpenAi,
            support::sse_response(&[
                r#"{"type":"response.output_text.delta","delta":"hello"}"#,
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":20}}}"#,
            ]),
        ),
        (
            ProviderProtocol::OpenAiCompat,
            support::sse_response(&[
                r#"{"choices":[{"delta":{"content":"hello"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#,
            ]),
        ),
    ];

    for (protocol, response) in cases {
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
            events,
            vec![
                ProviderEvent::TextDelta {
                    text: "hello".into()
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
