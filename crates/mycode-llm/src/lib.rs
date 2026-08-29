//! Provider protocol clients for the MyCode Headless Core.
//!
//! The Provider streaming client trait is the Agent Loop integration boundary. Higher
//! layers should depend on that trait and [`MockClient`] rather than a
//! specific Provider protocol implementation.

pub mod anthropic;
pub mod client;
pub mod events;
pub mod http;
pub mod limits;
pub mod mock;
pub mod openai;
pub mod openai_compat;
pub mod provider;
pub mod sse;

pub use anthropic::AnthropicClient;
pub use client::{LlmClient, ProviderRequest, ProviderStream, ToolDefinition};
pub use events::{ProviderError, ProviderEvent, StopReason, Usage};
pub use mock::MockClient;
pub use openai::OpenAiClient;
pub use openai_compat::OpenAiCompatClient;
pub use provider::{ProviderClient, build_provider_client, build_provider_client_with};
