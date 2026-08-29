use mycode_core::config::ProviderConfig;
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, LlmRequest, LlmStream, ToolDefinition};
use crate::events::{LlmError, LlmEvent, StopReason, Usage};
use crate::http::{SseDecoder, run_sse_stream};
use crate::limits::max_output_tokens;
use crate::sse::SseEvent;

#[derive(Debug, Clone)]
pub struct OpenAiClient {
    http: Client,
    api_key: String,
    base_url: String,
    model: String,
    thinking: bool,
    instructions: String,
    max_output_tokens: u64,
}

impl OpenAiClient {
    pub fn new(
        provider: &ProviderConfig,
        instructions: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key =
            provider
                .resolve_api_key_from_env()
                .ok_or_else(|| LlmError::Authentication {
                    message: "OpenAI API key not found; set provider api_key or OPENAI_API_KEY"
                        .into(),
                })?;

        Ok(Self {
            http: Client::new(),
            api_key,
            base_url: provider.base_url.trim_end_matches('/').to_string(),
            model: provider.model.clone(),
            thinking: provider.thinking,
            instructions: instructions.into(),
            max_output_tokens: max_output_tokens(provider),
        })
    }

    fn request_body(&self, request: &LlmRequest) -> Value {
        let input: Vec<Value> = request
            .conversation
            .messages()
            .iter()
            .flat_map(openai_input_items)
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_output_tokens": self.max_output_tokens,
            "instructions": self.instructions,
            "input": input,
            "stream": true,
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(openai_tool).collect());
        }
        if self.thinking {
            body["reasoning"] = json!({"effort": "high", "summary": "detailed"});
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        body
    }

    fn endpoint(&self) -> String {
        format!("{}/responses", self.base_url)
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAiClient {
    async fn stream(
        &self,
        request: LlmRequest,
        cancellation: CancellationToken,
    ) -> Result<LlmStream, LlmError> {
        let body = self.request_body(&request);
        let endpoint = self.endpoint();
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let (sender, receiver) = mpsc::channel(64);
        let state = OpenAiResponsesStreamState::default();

        tokio::spawn(async move {
            run_sse_stream(
                http,
                endpoint,
                vec![("authorization", format!("Bearer {api_key}"))],
                body,
                cancellation,
                sender,
                state,
            )
            .await;
        });

        Ok(receiver)
    }
}

fn openai_input_items(message: &ConversationMessage) -> Vec<Value> {
    match message.role {
        MessageRole::Assistant => message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => json!({
                    "type": "message",
                    "role": "assistant",
                    "content": text,
                }),
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => json!({
                    "type": "reasoning",
                    "id": signature,
                    "summary": [{"type": "summary_text", "text": thinking}],
                }),
                ContentBlock::ToolUse {
                    tool_use_id,
                    tool_name,
                    arguments,
                } => json!({
                    "type": "function_call",
                    "call_id": tool_use_id,
                    "name": tool_name,
                    "arguments": arguments.to_string(),
                }),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                }),
            })
            .collect(),
        MessageRole::User | MessageRole::System => message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => json!({
                    "type": "message",
                    "role": "user",
                    "content": text,
                }),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                }),
                ContentBlock::Thinking { .. } | ContentBlock::ToolUse { .. } => Value::Null,
            })
            .filter(|value| !value.is_null())
            .collect(),
    }
}

fn openai_tool(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
        "strict": false,
    })
}

#[derive(Default)]
struct OpenAiResponsesStreamState {
    usage: Usage,
    saw_tool_call: bool,
    reasoning_id: String,
    reasoning_text: String,
    current_tool_id: String,
    current_tool_name: String,
    tool_arguments: String,
}

impl SseDecoder for OpenAiResponsesStreamState {
    fn decode(&mut self, event: &SseEvent) -> Result<Vec<LlmEvent>, LlmError> {
        let data: Value =
            serde_json::from_str(&event.data).map_err(|error| LlmError::InvalidResponse {
                message: format!("invalid OpenAI SSE JSON: {error}"),
            })?;
        let event_type = data
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or_default();

        match event_type {
            "response.output_text.delta" => Ok(vec![LlmEvent::TextDelta {
                text: required_string(&data, "delta")?,
            }]),
            "response.output_item.added" => {
                let item = data.get("item").ok_or_else(invalid_response)?;
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        self.current_tool_id = required_string(item, "call_id")?;
                        self.current_tool_name = required_string(item, "name")?;
                        self.tool_arguments.clear();
                        self.saw_tool_call = true;
                        Ok(vec![LlmEvent::ToolCallStart {
                            tool_id: self.current_tool_id.clone(),
                            tool_name: self.current_tool_name.clone(),
                        }])
                    }
                    Some("reasoning") => {
                        self.reasoning_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.reasoning_text.clear();
                        Ok(Vec::new())
                    }
                    _ => Ok(Vec::new()),
                }
            }
            "response.reasoning_summary_text.delta" => {
                let text = required_string(&data, "delta")?;
                self.reasoning_text.push_str(&text);
                Ok(vec![LlmEvent::ThinkingDelta { text }])
            }
            "response.reasoning_summary_text.done" => Ok(vec![LlmEvent::ThinkingComplete {
                thinking: self.reasoning_text.clone(),
                signature: self.reasoning_id.clone(),
            }]),
            "response.function_call_arguments.delta" => {
                let text = required_string(&data, "delta")?;
                self.tool_arguments.push_str(&text);
                Ok(vec![LlmEvent::ToolCallDelta { text }])
            }
            "response.function_call_arguments.done" => {
                let arguments = if self.tool_arguments.is_empty() {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&self.tool_arguments).map_err(|error| {
                        LlmError::InvalidResponse {
                            message: format!("invalid OpenAI Tool arguments: {error}"),
                        }
                    })?
                };
                Ok(vec![LlmEvent::ToolCallComplete {
                    tool_id: self.current_tool_id.clone(),
                    tool_name: self.current_tool_name.clone(),
                    arguments,
                }])
            }
            "response.completed" => {
                if let Some(usage) = data.pointer("/response/usage") {
                    let cached = usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    let input = usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    self.usage = Usage {
                        input_tokens: input.saturating_sub(cached),
                        output_tokens: usage
                            .get("output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                        cache_read_tokens: cached,
                        cache_creation_tokens: 0,
                    };
                }
                Ok(vec![LlmEvent::StreamEnd {
                    stop_reason: if self.saw_tool_call {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    },
                    usage: self.usage,
                }])
            }
            "error" => {
                let message = data
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown OpenAI stream error");
                Err(LlmError::InvalidResponse {
                    message: message.to_string(),
                })
            }
            _ => Ok(Vec::new()),
        }
    }
}

fn required_string(object: &Value, field: &str) -> Result<String, LlmError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| LlmError::InvalidResponse {
            message: format!("OpenAI response is missing string field {field:?}"),
        })
}

fn invalid_response() -> LlmError {
    LlmError::InvalidResponse {
        message: "OpenAI response was missing an expected field".into(),
    }
}
