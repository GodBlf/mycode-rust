use std::collections::BTreeMap;

use mycode_core::config::ProviderConfig;
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use reqwest::Client;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, ProviderRequest, ProviderStream, ToolDefinition};
use crate::events::{ProviderError, ProviderEvent, StopReason, Usage};
use crate::http::{SseDecoder, spawn_sse_stream};
use crate::limits::max_output_tokens;
use crate::sse::SseEvent;

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    http: Client,
    api_key: String,
    base_url: String,
    model: String,
    max_output_tokens: u64,
}

impl OpenAiCompatClient {
    pub fn new(provider: &ProviderConfig) -> Result<Self, ProviderError> {
        let api_key =
            provider
                .resolve_api_key_from_env()
                .ok_or_else(|| {
                    ProviderError::Authentication {
                message:
                    "OpenAI-compatible API key not found; set provider api_key or OPENAI_API_KEY"
                        .into(),
            }
                })?;

        Ok(Self {
            http: Client::new(),
            api_key,
            base_url: provider.base_url.trim_end_matches('/').to_string(),
            model: provider.model.clone(),
            max_output_tokens: max_output_tokens(provider),
        })
    }

    fn request_body(&self, request: &ProviderRequest) -> Value {
        let messages =
            chat_completion_messages(&request.system_prompt, request.conversation.messages());
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_output_tokens,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(chat_completion_tool).collect());
        }
        body
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAiCompatClient {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = self.request_body(&request);
        let endpoint = self.endpoint();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let state = ChatCompletionsStreamState::default();

        Ok(spawn_sse_stream(
            http,
            endpoint,
            vec![("authorization", format!("Bearer {api_key}"))],
            body,
            cancellation,
            state,
        ))
    }
}

fn chat_completion_messages(
    system_prompt: &str,
    conversation: &[ConversationMessage],
) -> Vec<Value> {
    let mut messages = Vec::new();
    if !system_prompt.is_empty() {
        messages.push(json!({"role": "system", "content": system_prompt}));
    }

    for message in conversation {
        match message.role {
            MessageRole::System => {
                let text = text_blocks(&message.content);
                if !text.is_empty() {
                    messages.push(json!({"role": "system", "content": text}));
                }
            }
            MessageRole::Assistant => {
                let text = text_blocks(&message.content);
                let tool_calls: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse {
                            tool_use_id,
                            tool_name,
                            arguments,
                        } => Some(json!({
                            "id": tool_use_id,
                            "type": "function",
                            "function": {
                                "name": tool_name,
                                "arguments": arguments.to_string(),
                            }
                        })),
                        _ => None,
                    })
                    .collect();

                if tool_calls.is_empty() {
                    if !text.is_empty() {
                        messages.push(json!({"role": "assistant", "content": text}));
                    }
                } else {
                    let mut assistant = json!({"role": "assistant", "tool_calls": tool_calls});
                    if !text.is_empty() {
                        assistant["content"] = Value::String(text);
                    }
                    messages.push(assistant);
                }
            }
            MessageRole::User => {
                let text = text_blocks(&message.content);
                if !text.is_empty() {
                    messages.push(json!({"role": "user", "content": text}));
                }
                for block in &message.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        let _ = is_error;
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        }));
                    }
                }
            }
        }
    }

    messages
}

fn text_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn chat_completion_tool(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
            "strict": false,
        }
    })
}

#[derive(Default)]
struct ChatCompletionsStreamState {
    usage: Usage,
    stop_reason: Option<StopReason>,
    stream_end_emitted: bool,
    tool_calls: BTreeMap<u64, ChatToolCall>,
}

#[derive(Default)]
struct ChatToolCall {
    id: String,
    name: String,
    arguments: String,
    started: bool,
    completed: bool,
}

impl SseDecoder for ChatCompletionsStreamState {
    fn decode(&mut self, event: &SseEvent) -> Result<Vec<ProviderEvent>, ProviderError> {
        if event.data.trim() == "[DONE]" {
            return Ok(self.stream_end());
        }

        let data: Value =
            serde_json::from_str(&event.data).map_err(|error| ProviderError::InvalidResponse {
                message: format!("invalid OpenAI-compatible SSE JSON: {error}"),
            })?;
        let mut events = Vec::new();

        if let Some(usage) = data.get("usage").filter(|usage| {
            usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .is_some_and(|tokens| tokens > 0)
        }) {
            let cached = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            self.usage = Usage {
                input_tokens: prompt_tokens.saturating_sub(cached),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                cache_read_tokens: cached,
                cache_creation_tokens: 0,
            };
        }

        let Some(choice) = data
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            if self.usage.input_tokens > 0 || self.usage.output_tokens > 0 {
                events.extend(self.stream_end());
            }
            return Ok(events);
        };

        if let Some(content) = choice.pointer("/delta/content").and_then(Value::as_str)
            && !content.is_empty()
        {
            events.push(ProviderEvent::TextDelta {
                text: content.to_string(),
            });
        }

        if let Some(tool_calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for tool_call in tool_calls {
                self.decode_tool_call(tool_call, &mut events)?;
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(chat_stop_reason(finish_reason));
            for tool_call in self.tool_calls.values_mut() {
                if tool_call.completed {
                    continue;
                }
                tool_call.completed = true;
                let arguments = if tool_call.arguments.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&tool_call.arguments).map_err(|error| {
                        ProviderError::InvalidResponse {
                            message: format!("invalid OpenAI-compatible Tool arguments: {error}"),
                        }
                    })?
                };
                events.push(ProviderEvent::ToolCallComplete {
                    tool_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    arguments,
                });
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<ProviderEvent>, ProviderError> {
        Ok(self.stream_end())
    }
}

impl ChatCompletionsStreamState {
    fn stream_end(&mut self) -> Vec<ProviderEvent> {
        if self.stream_end_emitted {
            return Vec::new();
        }
        self.stream_end_emitted = true;
        vec![ProviderEvent::StreamEnd {
            stop_reason: self.stop_reason.clone().unwrap_or(StopReason::EndTurn),
            usage: self.usage,
        }]
    }

    fn decode_tool_call(
        &mut self,
        tool_call: &Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        let index = tool_call
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| ProviderError::InvalidResponse {
                message: "OpenAI-compatible tool call is missing index".into(),
            })?;
        let state = self.tool_calls.entry(index).or_default();

        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
            state.id = id.to_string();
        }
        if let Some(name) = tool_call.pointer("/function/name").and_then(Value::as_str) {
            state.name = name.to_string();
        }
        if !state.started && !state.name.is_empty() {
            state.started = true;
            events.push(ProviderEvent::ToolCallStart {
                tool_id: state.id.clone(),
                tool_name: state.name.clone(),
            });
        }

        if let Some(arguments) = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            state.arguments.push_str(arguments);
            events.push(ProviderEvent::ToolCallDelta {
                text: arguments.to_string(),
            });
        }
        Ok(())
    }
}

fn chat_stop_reason(value: &str) -> StopReason {
    match value {
        "tool_calls" => StopReason::ToolUse,
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        other => StopReason::Other(other.to_string()),
    }
}
