use compaction::compact_conversation;
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_core::time::current_timestamp;
use mycode_llm::{LlmClient, ProviderError, ProviderEvent, ProviderRequest, ToolDefinition, Usage};
use mycode_tools::context::ToolContext;
use mycode_tools::runtime::ToolExecutor;
use std::collections::VecDeque;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use tools::{PendingToolCall, PermissionResponse, authorize_tool, execute_plan};

mod compaction;
mod config;
mod events;
pub mod tool_result;
mod tools;

pub use compaction::{
    CompactionConfig, CompactionError, CompactionOutcome, CompactionTracker, CompactionTrigger,
    RecoveryState, estimate_tokens,
};
pub use config::AgentConfig;
pub use events::AgentEvent;
pub use tool_result::{ToolResultBudget, ToolResultBudgetConfig, ToolResultBudgetError};

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

struct ProviderTurn {
    text: String,
    tool_calls: Vec<PendingToolCall>,
    usage: Option<Usage>,
}

enum ProviderTurnError {
    Provider(ProviderError),
    EventStreamClosed,
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

    pub async fn compact(&self) -> Result<CompactionOutcome, CompactionError> {
        let messages = self
            .session_store
            .load(&self.session_id)?
            .ok_or(CompactionError::SessionMissing)?;
        let mut conversation = Conversation::new();
        for message in messages {
            conversation.push(message);
        }
        compact_conversation(
            self.provider.as_ref(),
            compaction::CompactionTarget {
                session_store: &self.session_store,
                session_id: &self.session_id,
            },
            &conversation,
            &self.config.compaction,
            compaction::CompactionAttachments {
                cancellation: self.context.cancellation().clone(),
                recovery: None,
                tools: &[],
            },
        )
        .await
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
        let (mut conversation, mut tool_result_budget) = match initialize_run(
            &session_store,
            context.workspace_root(),
            &session_id,
            &config,
            prompt,
            &event_sender,
        )
        .await
        {
            Ok(run) => run,
            Err(()) => return,
        };

        if config.max_iterations == 0 {
            let _ = event_sender
                .send(AgentEvent::MaxIterationsReached {
                    limit: config.max_iterations,
                })
                .await;
            return;
        }

        let tools = tool_definitions(&executor);
        let mut compaction_tracker = CompactionTracker::default();
        let mut recovery_state = RecoveryState::default();
        let mut iteration = 0;
        loop {
            iteration += 1;
            let trigger =
                compaction_tracker.should_compact(conversation.messages(), &config.compaction);
            if trigger != compaction::CompactionTrigger::None {
                match compact_conversation(
                    provider.as_ref(),
                    compaction::CompactionTarget {
                        session_store: &session_store,
                        session_id: &session_id,
                    },
                    &conversation,
                    &config.compaction,
                    compaction::CompactionAttachments {
                        cancellation: context.cancellation().clone(),
                        recovery: Some(&recovery_state),
                        tools: &tools,
                    },
                )
                .await
                {
                    Ok(outcome) if !outcome.summary.is_empty() => {
                        conversation = outcome.compacted_conversation;
                        compaction_tracker.reset_after_compaction();
                        let event = AgentEvent::Compacted {
                            message: format!(
                                "Compacted: {} → {} estimated tokens",
                                outcome.estimated_tokens_before, outcome.estimated_tokens_after
                            ),
                        };
                        if event_sender.send(event).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => compaction_tracker.record_auto_success(),
                    Err(error) => {
                        if trigger == compaction::CompactionTrigger::Hard {
                            send_run_error(&event_sender, error.to_string()).await;
                            return;
                        }
                        compaction_tracker.record_auto_failure(&config.compaction);
                    }
                }
            }

            let request = ProviderRequest {
                system_prompt: config.system_prompt.clone(),
                conversation: tool_result_budget.apply(&conversation),
                tools: tools.clone(),
            };
            let turn = match stream_provider_turn(
                provider.as_ref(),
                request,
                context.cancellation().clone(),
                &event_sender,
            )
            .await
            {
                Ok(turn) => turn,
                Err(ProviderTurnError::Provider(error)) => {
                    send_provider_error(&event_sender, error).await;
                    return;
                }
                Err(ProviderTurnError::EventStreamClosed) => return,
            };
            let ProviderTurn {
                text,
                tool_calls,
                usage,
            } = turn;

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
            if let Some(usage) = usage {
                compaction_tracker.record_usage(usage, conversation.messages().len());
            }

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
                        plan.tool_call.arguments.clone(),
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
                let (tool_id, tool_name, arguments) = tool_metadata[index].clone();
                if tool_name == "ReadFile"
                    && let Some(path) = arguments
                        .get("file_path")
                        .and_then(serde_json::Value::as_str)
                {
                    recovery_state.record_file_read(path, &result.output);
                }
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

async fn initialize_run(
    session_store: &SessionStore,
    workspace_root: &std::path::Path,
    session_id: &SessionId,
    config: &AgentConfig,
    prompt: String,
    event_sender: &mpsc::Sender<AgentEvent>,
) -> Result<(Conversation, ToolResultBudget), ()> {
    let mut conversation = match session_store.load(session_id) {
        Ok(Some(messages)) => conversation_from_messages(messages),
        Ok(None) => Conversation::new(),
        Err(error) => {
            send_run_error(event_sender, error.to_string()).await;
            return Err(());
        }
    };
    let mut tool_result_budget =
        match ToolResultBudget::resume(workspace_root, session_id, config.tool_result_budget) {
            Ok(budget) => budget,
            Err(error) => {
                send_run_error(event_sender, error.to_string()).await;
                return Err(());
            }
        };
    tool_result_budget.reconstruct(&conversation);

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
    if let Err(error) = session_store.append(session_id, &user_message) {
        send_run_error(event_sender, error.to_string()).await;
        return Err(());
    }

    Ok((conversation, tool_result_budget))
}

fn conversation_from_messages(messages: Vec<ConversationMessage>) -> Conversation {
    let mut conversation = Conversation::new();
    for message in messages {
        conversation.push(message);
    }
    conversation
}

fn tool_definitions(executor: &ToolExecutor) -> Vec<ToolDefinition> {
    executor
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
        .collect()
}

async fn stream_provider_turn(
    provider: &dyn LlmClient,
    request: ProviderRequest,
    cancellation: CancellationToken,
    event_sender: &mpsc::Sender<AgentEvent>,
) -> Result<ProviderTurn, ProviderTurnError> {
    let mut provider_stream = provider
        .stream(request, cancellation)
        .await
        .map_err(ProviderTurnError::Provider)?;
    let mut turn = ProviderTurn {
        text: String::new(),
        tool_calls: Vec::new(),
        usage: None,
    };

    while let Some(result) = provider_stream.recv().await {
        match result {
            Ok(ProviderEvent::TextDelta { text: delta }) => {
                turn.text.push_str(&delta);
                if event_sender
                    .send(AgentEvent::TextDelta { text: delta })
                    .await
                    .is_err()
                {
                    return Err(ProviderTurnError::EventStreamClosed);
                }
            }
            Ok(ProviderEvent::ToolCallComplete {
                tool_id,
                tool_name,
                arguments,
            }) => {
                turn.tool_calls.push(PendingToolCall {
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
                    return Err(ProviderTurnError::EventStreamClosed);
                }
            }
            Ok(ProviderEvent::StreamEnd {
                usage: reported, ..
            }) => {
                turn.usage = Some(reported);
                break;
            }
            Ok(_) => {}
            Err(error) => return Err(ProviderTurnError::Provider(error)),
        }
    }

    Ok(turn)
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
