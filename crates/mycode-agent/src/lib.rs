use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{LlmClient, ProviderError, ProviderEvent, ProviderRequest, ToolDefinition};
use mycode_tools::context::ToolContext;
use mycode_tools::runtime::ToolExecutor;
use std::collections::VecDeque;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;

use tools::{PendingToolCall, PermissionResponse, authorize_tool, execute_plan};

mod config;
mod events;
mod tools;

pub use config::AgentConfig;
pub use events::AgentEvent;

pub struct Agent {
    provider: Box<dyn LlmClient>,
    executor: ToolExecutor,
    context: ToolContext,
    session_store: SessionStore,
    session_id: SessionId,
    config: AgentConfig,
}

pub struct AgentRun {
    pub events: mpsc::Receiver<AgentEvent>,
    permission_sender: mpsc::Sender<PermissionResponse>,
}

impl Agent {
    pub fn new(
        provider: Box<dyn LlmClient>,
        executor: ToolExecutor,
        context: ToolContext,
        session_store: SessionStore,
        session_id: SessionId,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            executor,
            context,
            session_store,
            session_id,
            config,
        }
    }

    pub fn run(self, prompt: impl Into<String> + Send + 'static) -> AgentRun {
        let (event_sender, event_receiver) = mpsc::channel(64);
        let (permission_sender, permission_receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            self.run_inner(prompt.into(), event_sender, permission_receiver)
                .await;
        });
        AgentRun {
            events: event_receiver,
            permission_sender,
        }
    }

    async fn run_inner(
        self,
        prompt: String,
        event_sender: mpsc::Sender<AgentEvent>,
        mut permission_receiver: mpsc::Receiver<PermissionResponse>,
    ) {
        let Self {
            provider,
            executor,
            context,
            session_store,
            session_id,
            config,
        } = self;
        let mut conversation = match session_store.load(&session_id) {
            Ok(Some(messages)) => {
                let mut conversation = Conversation::new();
                for message in messages {
                    conversation.push(message);
                }
                conversation
            }
            Ok(None) => Conversation::new(),
            Err(error) => {
                let _ = event_sender
                    .send(AgentEvent::RunError {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }
        };

        conversation.push(ConversationMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: prompt }],
            timestamp_unix_seconds: current_timestamp(),
        });
        let user_message = conversation
            .messages()
            .last()
            .cloned()
            .expect("user message was just pushed");
        if let Err(error) = session_store.append(&session_id, &user_message) {
            let _ = event_sender
                .send(AgentEvent::RunError {
                    message: error.to_string(),
                })
                .await;
            return;
        }

        if config.max_iterations == 0 {
            let _ = event_sender
                .send(AgentEvent::MaxIterationsReached {
                    limit: config.max_iterations,
                })
                .await;
            return;
        }

        let tools: Vec<ToolDefinition> = executor
            .registry()
            .list()
            .into_iter()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool
                    .schema()
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
            })
            .collect();
        let mut iteration = 0;
        loop {
            iteration += 1;
            let request = ProviderRequest {
                system_prompt: config.system_prompt.clone(),
                conversation: conversation.clone(),
                tools: tools.clone(),
            };
            let mut provider_stream = match provider
                .stream(request, context.cancellation().clone())
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    send_provider_error(&event_sender, error).await;
                    return;
                }
            };

            let mut text = String::new();
            let mut tool_calls: Vec<PendingToolCall> = Vec::new();
            while let Some(result) = provider_stream.recv().await {
                match result {
                    Ok(ProviderEvent::TextDelta { text: delta }) => {
                        text.push_str(&delta);
                        if event_sender
                            .send(AgentEvent::TextDelta { text: delta })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(ProviderEvent::ToolCallComplete {
                        tool_id,
                        tool_name,
                        arguments,
                    }) => {
                        tool_calls.push(PendingToolCall {
                            tool_id: tool_id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: arguments.clone(),
                        });
                        if event_sender
                            .send(AgentEvent::ToolCall {
                                tool_id,
                                tool_name,
                                arguments,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(ProviderEvent::StreamEnd { .. }) => break,
                    Ok(_) => {}
                    Err(error) => {
                        send_provider_error(&event_sender, error).await;
                        return;
                    }
                }
            }

            let mut assistant_content = Vec::new();
            if !text.is_empty() {
                assistant_content.push(ContentBlock::Text { text: text.clone() });
            }
            for tool_call in &tool_calls {
                assistant_content.push(ContentBlock::ToolUse {
                    tool_use_id: tool_call.tool_id.clone(),
                    tool_name: tool_call.tool_name.clone(),
                    arguments: tool_call.arguments.clone(),
                });
            }
            let assistant_message = ConversationMessage {
                role: MessageRole::Assistant,
                content: assistant_content,
                timestamp_unix_seconds: current_timestamp(),
            };
            if let Err(error) = session_store.append(&session_id, &assistant_message) {
                send_run_error(&event_sender, error.to_string()).await;
                return;
            }
            conversation.push(assistant_message);

            if tool_calls.is_empty() {
                let _ = event_sender
                    .send(AgentEvent::RunCompleted { final_text: text })
                    .await;
                return;
            }

            let mut plans = Vec::new();
            for tool_call in tool_calls {
                let Some(planned) = authorize_tool(
                    &executor,
                    &context,
                    &tool_call,
                    &event_sender,
                    &mut permission_receiver,
                )
                .await
                else {
                    let _ = event_sender.send(AgentEvent::RunCancelled).await;
                    return;
                };
                plans.push(planned);
            }
            let tool_metadata = plans
                .iter()
                .map(|plan| {
                    (
                        plan.tool_call.tool_id.clone(),
                        plan.tool_call.tool_name.clone(),
                    )
                })
                .collect::<Vec<_>>();

            let mut pending_plans = plans.into_iter().enumerate().collect::<VecDeque<_>>();
            let mut completed_results = vec![None; pending_plans.len()];
            let mut executions = FuturesUnordered::new();
            while executions.len() < config.tool_concurrency.max(1)
                && let Some((index, plan)) = pending_plans.pop_front()
            {
                executions.push(execute_plan(&executor, &context, index, plan));
            }
            while !executions.is_empty() {
                let next = tokio::select! {
                    _ = context.cancellation().cancelled() => {
                        break;
                    }
                    next = executions.next() => next,
                };
                let Some((index, result)) = next else {
                    break;
                };
                if !context.cancellation().is_cancelled() {
                    completed_results[index] = Some(result);
                    while !context.cancellation().is_cancelled()
                        && executions.len() < config.tool_concurrency.max(1)
                        && let Some((index, plan)) = pending_plans.pop_front()
                    {
                        executions.push(execute_plan(&executor, &context, index, plan));
                    }
                }
            }

            let mut tool_results = Vec::new();
            for (index, result) in completed_results.into_iter().enumerate() {
                let Some(result) = result else {
                    continue;
                };
                let (tool_id, tool_name) = tool_metadata[index].clone();
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_id.clone(),
                    content: result.output.clone(),
                    is_error: result.is_error,
                });
                if event_sender
                    .send(AgentEvent::ToolResult {
                        tool_id,
                        tool_name,
                        content: result.output,
                        is_error: result.is_error,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }

            if tool_results.is_empty() && context.cancellation().is_cancelled() {
                let _ = event_sender.send(AgentEvent::RunCancelled).await;
                return;
            }

            let tool_result_message = ConversationMessage {
                role: MessageRole::User,
                content: tool_results,
                timestamp_unix_seconds: current_timestamp(),
            };
            if let Err(error) = session_store.append(&session_id, &tool_result_message) {
                if context.cancellation().is_cancelled() {
                    let _ = event_sender.send(AgentEvent::RunCancelled).await;
                } else {
                    send_run_error(&event_sender, error.to_string()).await;
                }
                return;
            }
            conversation.push(tool_result_message);

            if context.cancellation().is_cancelled() {
                let _ = event_sender.send(AgentEvent::RunCancelled).await;
                return;
            }

            if iteration >= config.max_iterations {
                let _ = event_sender
                    .send(AgentEvent::MaxIterationsReached {
                        limit: config.max_iterations,
                    })
                    .await;
                return;
            }
        }
    }
}

impl AgentRun {
    pub fn respond(&self, request_id: impl Into<String>, allowed: bool) {
        let _ = self.permission_sender.try_send(PermissionResponse {
            request_id: request_id.into(),
            allowed,
        });
    }
}

async fn send_run_error(event_sender: &mpsc::Sender<AgentEvent>, message: String) {
    let _ = event_sender.send(AgentEvent::RunError { message }).await;
}

async fn send_provider_error(event_sender: &mpsc::Sender<AgentEvent>, error: ProviderError) {
    let event = if error == ProviderError::Cancelled {
        AgentEvent::RunCancelled
    } else {
        AgentEvent::RunError {
            message: error.to_string(),
        }
    };
    let _ = event_sender.send(event).await;
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
