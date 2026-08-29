mod support;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_llm::{ProviderError, build_provider_client, build_provider_client_with};
use tokio_util::sync::CancellationToken;

fn provider(
    protocol: ProviderProtocol,
    base_url: String,
    model: &str,
    thinking: bool,
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
) -> ProviderConfig {
    ProviderConfig {
        name: "test-provider".into(),
        protocol,
        base_url,
        model: model.into(),
        api_key: "test-key".into(),
        thinking,
        context_window,
        max_output_tokens,
    }
}

fn minimal_request() -> mycode_llm::ProviderRequest {
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
        tools: Vec::new(),
    }
}

#[tokio::test]
async fn anthropic_model_metadata_is_fetched_and_used() {
    let (base_url, request_receiver) = support::serve_once(support::http_response(
        200,
        r#"{"id":"claude-sonnet-4-6","max_input_tokens":555000}"#,
    ))
    .await;

    let built = build_provider_client(&provider(
        ProviderProtocol::Anthropic,
        base_url,
        "claude-sonnet-4-6",
        false,
        None,
        None,
    ))
    .await
    .expect("provider should build");

    assert_eq!(built.context_window, 555_000);
    assert_eq!(built.max_output_tokens, 8_192);
    let request = request_receiver
        .await
        .expect("metadata request should be captured");
    assert!(request.starts_with("GET /v1/models/claude-sonnet-4-6 HTTP/1.1\r\n"));
    let headers = support::request_headers(&request);
    assert!(headers.contains(&("x-api-key".to_string(), "test-key".to_string())));
    assert!(headers.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())));
}

#[tokio::test]
async fn configured_api_key_takes_precedence_over_environment_lookup() {
    let (base_url, request_receiver) = support::serve_once(support::sse_response(&[
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}"#,
    ]))
    .await;
    let built = build_provider_client_with(
        &provider(
            ProviderProtocol::OpenAi,
            base_url,
            "gpt-4.1",
            false,
            Some(128_000),
            None,
        ),
        |_| Some("environment-key".into()),
    )
    .await
    .expect("provider should build");

    let mut stream = built
        .client
        .stream(minimal_request(), CancellationToken::new())
        .await
        .expect("stream should start");
    while stream.recv().await.is_some() {}

    let request = request_receiver.await.expect("request should be captured");
    let headers = support::request_headers(&request);
    assert!(headers.contains(&("authorization".to_string(), "Bearer test-key".to_string())));
}

#[tokio::test]
async fn explicit_context_window_skips_anthropic_metadata_fetch() {
    let base_url = support::serve_hanging().await;
    let built = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        build_provider_client(&provider(
            ProviderProtocol::Anthropic,
            base_url,
            "claude-sonnet-4-6",
            false,
            Some(4_096),
            None,
        )),
    )
    .await
    .expect("explicit context window should not fetch")
    .expect("provider should build");

    assert_eq!(built.context_window, 4_096);
}

#[tokio::test]
async fn anthropic_metadata_failure_falls_back_to_builtin_mapping() {
    let (base_url, _request) =
        support::serve_once(support::http_response(500, r#"{"error":"unavailable"}"#)).await;

    let built = build_provider_client(&provider(
        ProviderProtocol::Anthropic,
        base_url,
        "claude-sonnet-4-6",
        false,
        None,
        None,
    ))
    .await
    .expect("provider should build despite metadata failure");

    assert_eq!(built.context_window, 200_000);
}

#[tokio::test]
async fn non_anthropic_providers_do_not_fetch_model_metadata() {
    let base_url = support::serve_hanging().await;
    let built = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        build_provider_client(&provider(
            ProviderProtocol::OpenAi,
            base_url,
            "gpt-4.1",
            false,
            None,
            None,
        )),
    )
    .await
    .expect("OpenAI provider should not fetch Anthropic metadata")
    .expect("provider should build");

    assert_eq!(built.context_window, 1_000_000);
    assert_eq!(built.max_output_tokens, 8_192);
}

#[tokio::test]
async fn token_limits_use_explicit_and_thinking_defaults() {
    let explicit = build_provider_client(&provider(
        ProviderProtocol::OpenAiCompat,
        "https://provider.example.test".into(),
        "unknown-model",
        false,
        Some(123_456),
        Some(2_048),
    ))
    .await
    .expect("provider should build");
    assert_eq!(explicit.context_window, 123_456);
    assert_eq!(explicit.max_output_tokens, 2_048);

    let thinking = build_provider_client(&provider(
        ProviderProtocol::OpenAiCompat,
        "https://provider.example.test".into(),
        "unknown-model",
        true,
        None,
        None,
    ))
    .await
    .expect("provider should build");
    assert_eq!(thinking.context_window, 128_000);
    assert_eq!(thinking.max_output_tokens, 64_000);
}

#[tokio::test]
async fn missing_api_keys_fail_before_requests() {
    let protocols = [
        ProviderProtocol::Anthropic,
        ProviderProtocol::OpenAi,
        ProviderProtocol::OpenAiCompat,
    ];
    for protocol in protocols {
        let mut missing_key = provider(
            protocol,
            "https://provider.example.test".into(),
            "model",
            false,
            Some(128_000),
            None,
        );
        missing_key.api_key.clear();
        let result = build_provider_client_with(&missing_key, |_| None).await;
        assert!(
            matches!(result, Err(ProviderError::Authentication { .. })),
            "protocol {protocol:?} should reject a missing API key"
        );
    }
}
