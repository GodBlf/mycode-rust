use std::collections::BTreeMap;

use mycode_core::config::ProviderConfig;
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use reqwest::Client;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, ProviderRequest, ProviderStream, ToolDefinition};
use crate::events::{ProviderError, ProviderEvent, StopReason, Usage};
use crate::http::{SseDecoder, spawn_sse_stream};
use crate::limits::max_output_tokens;
use crate::sse::SseEvent;

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: Client,
    api_key: String,
    base_url: String,
    model: String,
    thinking: bool,
    max_output_tokens: u64,
}

impl AnthropicClient {
    pub fn new(provider: &ProviderConfig) -> Result<Self, ProviderError> {
        let api_key =
            provider
                .resolve_api_key_from_env()
                .ok_or_else(|| ProviderError::Authentication {
                    message:
                        "Anthropic API key not found; set provider api_key or ANTHROPIC_API_KEY"
                            .into(),
                })?;

        Ok(Self {
            http: Client::new(),
            api_key,
            base_url: provider.base_url.trim_end_matches('/').to_string(),
            model: provider.model.clone(),
            thinking: provider.thinking,
            max_output_tokens: max_output_tokens(provider),
        })
    }

    fn request_body(&self, request: &ProviderRequest) -> Value {
        let mut messages = Vec::new();
        for message in request.conversation.messages() {
            append_anthropic_message(&mut messages, message);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_output_tokens,
            "stream": true,
            "system": [{
                "type": "text",
                "text": request.system_prompt,
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": messages,
        });

        if !request.tools.is_empty() {
            let mut tools: Vec<Value> = request.tools.iter().map(anthropic_tool).collect();
            if let Some(last_tool) = tools.last_mut().and_then(Value::as_object_mut) {
                last_tool.insert("cache_control".into(), json!({"type": "ephemeral"}));
            }
            body["tools"] = Value::Array(tools);
        }

        if self.thinking {
            body["thinking"] = if supports_adaptive_thinking(&self.model) {
                json!({"type": "adaptive"})
            } else {
                json!({
                    "type": "enabled",
                    "budget_tokens": self.max_output_tokens.saturating_sub(1).max(1),
                })
            };
        }

        body
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

#[async_trait::async_trait]
impl LlmClient for AnthropicClient {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = self.request_body(&request);
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let endpoint = self.endpoint();
        let state = AnthropicStreamState::default();

        Ok(spawn_sse_stream(
            http,
            endpoint,
            vec![
                ("x-api-key", api_key),
                ("anthropic-version", "2023-06-01".to_string()),
            ],
            body,
            cancellation,
            state,
        ))
    }
}

fn append_anthropic_message(messages: &mut Vec<Value>, message: &ConversationMessage) {
    match message.role {
        MessageRole::Assistant => {
            let content: Vec<Value> = message
                .content
                .iter()
                .map(anthropic_content_block)
                .collect();
            messages.push(json!({"role": "assistant", "content": content}));
        }
        MessageRole::User | MessageRole::System => {
            let mut content = Vec::new();
            for block in &message.content {
                if let ContentBlock::Text { text } = block {
                    content.push(json!({"type": "text", "text": text}));
                } else {
                    content.push(anthropic_content_block(block));
                }
            }
            if content.is_empty() {
                content.push(json!({"type": "text", "text": ""}));
            }

            let can_merge = messages
                .last()
                .and_then(Value::as_object)
                .is_some_and(|previous| {
                    previous.get("role").and_then(Value::as_str) == Some("user")
                        && previous
                            .get("content")
                            .and_then(Value::as_array)
                            .is_some_and(|blocks| {
                                blocks
                                    .first()
                                    .and_then(Value::as_object)
                                    .and_then(|block| block.get("type").and_then(Value::as_str))
                                    != Some("tool_result")
                            })
                });

            if can_merge {
                if let Some(previous) = messages
                    .last_mut()
                    .and_then(Value::as_object_mut)
                    .and_then(|previous| previous.get_mut("content"))
                    .and_then(Value::as_array_mut)
                {
                    previous.extend(content);
                }
            } else {
                messages.push(json!({"role": "user", "content": content}));
            }
        }
    }
}

fn anthropic_content_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Thinking {
            thinking,
            signature,
            ..
        } => {
            json!({"type": "thinking", "thinking": thinking, "signature": signature})
        }
        ContentBlock::ToolUse {
            tool_use_id,
            tool_name,
            arguments,
        } => json!({
            "type": "tool_use",
            "id": tool_use_id,
            "name": tool_name,
            "input": arguments,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": [{"type": "text", "text": content}],
            "is_error": is_error,
        }),
    }
}

fn anthropic_tool(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.parameters,
    })
}

fn supports_adaptive_thinking(model: &str) -> bool {
    ["claude-opus-4-", "claude-sonnet-4-"].iter().any(|family| {
        model
            .strip_prefix(family)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|version| ('6'..='9').contains(&version))
    })
}

#[derive(Default)]
struct AnthropicStreamState {
    usage: Usage,
    stop_reason: Option<StopReason>,
    blocks: BTreeMap<u64, AnthropicBlockState>,
}

#[derive(Default)]
enum AnthropicBlockState {
    #[default]
    Text,
    Thinking {
        text: String,
        signature: String,
    },
    ToolUse {
        tool_id: String,
        tool_name: String,
        arguments: String,
    },
}

impl SseDecoder for AnthropicStreamState {
    fn decode(&mut self, event: &SseEvent) -> Result<Vec<ProviderEvent>, ProviderError> {
        let data: Value =
            serde_json::from_str(&event.data).map_err(|error| ProviderError::InvalidResponse {
                message: format!("invalid Anthropic SSE JSON: {error}"),
            })?;
        let event_type = data
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or_default();

        match event_type {
            "message_start" => {
                if let Some(usage) = data.pointer("/message/usage") {
                    self.usage.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    self.usage.cache_read_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    self.usage.cache_creation_tokens = usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                }
                Ok(Vec::new())
            }
            "content_block_start" => {
                let index = required_u64(&data, "index")?;
                let block = data.get("content_block").ok_or_else(invalid_response)?;
                let state = match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => AnthropicBlockState::Thinking {
                        text: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                    Some("tool_use") => AnthropicBlockState::ToolUse {
                        tool_id: required_string(block, "id")?,
                        tool_name: required_string(block, "name")?,
                        arguments: String::new(),
                    },
                    _ => AnthropicBlockState::Text,
                };
                let events = if let AnthropicBlockState::ToolUse {
                    tool_id, tool_name, ..
                } = &state
                {
                    vec![ProviderEvent::ToolCallStart {
                        tool_id: tool_id.clone(),
                        tool_name: tool_name.clone(),
                    }]
                } else {
                    Vec::new()
                };
                self.blocks.insert(index, state);
                Ok(events)
            }
            "content_block_delta" => {
                let index = required_u64(&data, "index")?;
                let delta = data.get("delta").ok_or_else(invalid_response)?;
                let delta_type = delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match delta_type {
                    "text_delta" => Ok(vec![ProviderEvent::TextDelta {
                        text: required_string(delta, "text")?,
                    }]),
                    "thinking_delta" => {
                        let text = required_string(delta, "thinking")?;
                        if let Some(AnthropicBlockState::Thinking {
                            text: accumulated, ..
                        }) = self.blocks.get_mut(&index)
                        {
                            accumulated.push_str(&text);
                        }
                        Ok(vec![ProviderEvent::ThinkingDelta { text }])
                    }
                    "signature_delta" => {
                        let signature = required_string(delta, "signature")?;
                        if let Some(AnthropicBlockState::Thinking {
                            signature: accumulated,
                            ..
                        }) = self.blocks.get_mut(&index)
                        {
                            *accumulated = signature;
                        }
                        Ok(Vec::new())
                    }
                    "input_json_delta" => {
                        let text = required_string(delta, "partial_json")?;
                        if let Some(AnthropicBlockState::ToolUse { arguments, .. }) =
                            self.blocks.get_mut(&index)
                        {
                            arguments.push_str(&text);
                        }
                        Ok(vec![ProviderEvent::ToolCallDelta { text }])
                    }
                    _ => Ok(Vec::new()),
                }
            }
            "content_block_stop" => {
                let index = required_u64(&data, "index")?;
                match self.blocks.remove(&index) {
                    Some(AnthropicBlockState::Thinking { text, signature }) => {
                        Ok(vec![ProviderEvent::ThinkingComplete {
                            thinking: text,
                            signature,
                            encrypted_content: String::new(),
                        }])
                    }
                    Some(AnthropicBlockState::ToolUse {
                        tool_id,
                        tool_name,
                        arguments,
                    }) => {
                        let arguments = if arguments.is_empty() {
                            Value::Object(Map::new())
                        } else {
                            serde_json::from_str(&arguments).map_err(|error| {
                                ProviderError::InvalidResponse {
                                    message: format!("invalid Anthropic Tool arguments: {error}"),
                                }
                            })?
                        };
                        Ok(vec![ProviderEvent::ToolCallComplete {
                            tool_id,
                            tool_name,
                            arguments,
                        }])
                    }
                    Some(AnthropicBlockState::Text) | None => Ok(Vec::new()),
                }
            }
            "message_delta" => {
                if let Some(stop_reason) =
                    data.pointer("/delta/stop_reason").and_then(Value::as_str)
                {
                    self.stop_reason = Some(anthropic_stop_reason(stop_reason));
                }
                if let Some(output_tokens) =
                    data.pointer("/usage/output_tokens").and_then(Value::as_u64)
                {
                    self.usage.output_tokens = output_tokens;
                }
                Ok(Vec::new())
            }
            "message_stop" => Ok(vec![ProviderEvent::StreamEnd {
                stop_reason: self.stop_reason.clone().unwrap_or(StopReason::EndTurn),
                usage: self.usage,
            }]),
            "error" => {
                let message = data
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown Anthropic stream error");
                Err(ProviderError::InvalidResponse {
                    message: message.to_string(),
                })
            }
            _ => Ok(Vec::new()),
        }
    }
}

fn anthropic_stop_reason(value: &str) -> StopReason {
    match value {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_string()),
    }
}

fn required_string(object: &Value, field: &str) -> Result<String, ProviderError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| ProviderError::InvalidResponse {
            message: format!("Anthropic response is missing string field {field:?}"),
        })
}

fn required_u64(object: &Value, field: &str) -> Result<u64, ProviderError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| ProviderError::InvalidResponse {
            message: format!("Anthropic response is missing integer field {field:?}"),
        })
}

fn invalid_response() -> ProviderError {
    ProviderError::InvalidResponse {
        message: "Anthropic response was missing an expected field".into(),
    }
}
