use std::time::Duration;

use mycode_core::config::{ProviderConfig, ProviderProtocol};
use reqwest::Client;
use serde_json::Value;

use crate::anthropic::AnthropicClient;
use crate::client::LlmClient;
use crate::events::ProviderError;
use crate::limits::{context_window, max_output_tokens};
use crate::openai::OpenAiClient;
use crate::openai_compat::OpenAiCompatClient;

const MODEL_METADATA_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ProviderClient {
    pub client: Box<dyn LlmClient>,
    pub context_window: u64,
    pub max_output_tokens: u64,
}

pub async fn build_provider_client(
    provider: &ProviderConfig,
) -> Result<ProviderClient, ProviderError> {
    build_provider_client_with(provider, |variable| std::env::var(variable).ok()).await
}

pub async fn build_provider_client_with(
    provider: &ProviderConfig,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<ProviderClient, ProviderError> {
    let api_key = provider
        .resolve_api_key_with(lookup)
        .ok_or_else(authentication_error)?;
    let mut provider = provider.clone();
    provider.api_key = api_key.clone();
    let max_output = max_output_tokens(&provider);
    let client: Box<dyn LlmClient> = match provider.protocol {
        ProviderProtocol::Anthropic => {
            let fetched = if provider.context_window.is_some() {
                None
            } else {
                fetch_anthropic_context_window(&provider, &api_key).await
            };
            let context_window = context_window(&provider, fetched);
            return Ok(ProviderClient {
                client: Box::new(AnthropicClient::new(&provider)?),
                context_window,
                max_output_tokens: max_output,
            });
        }
        ProviderProtocol::OpenAi => Box::new(OpenAiClient::new(&provider)?),
        ProviderProtocol::OpenAiCompat => Box::new(OpenAiCompatClient::new(&provider)?),
    };

    Ok(ProviderClient {
        client,
        context_window: context_window(&provider, None),
        max_output_tokens: max_output,
    })
}

async fn fetch_anthropic_context_window(provider: &ProviderConfig, api_key: &str) -> Option<u64> {
    let endpoint = format!(
        "{}/v1/models/{}",
        provider.base_url.trim_end_matches('/'),
        provider.model
    );
    let fetch = async {
        let response = Client::new()
            .get(endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }

        let body: Value = response.json().await.ok()?;
        body.get("max_input_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0)
    };
    tokio::time::timeout(MODEL_METADATA_TIMEOUT, fetch)
        .await
        .ok()?
}

fn authentication_error() -> ProviderError {
    ProviderError::Authentication {
        message:
            "Provider API key not found; set provider api_key or its protocol environment variable"
                .into(),
    }
}
