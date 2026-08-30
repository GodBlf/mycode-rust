use std::sync::Arc;
use std::sync::Mutex;

use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::config::PermissionMode;
use mycode_core::conversation::{ContentBlock, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{
    LlmClient, ProviderError, ProviderEvent, ProviderRequest, ProviderStream, StopReason,
};
use mycode_tools::context::ToolContext;
use mycode_tools::registry::ToolRegistry;
use mycode_tools::runtime::{ToolExecutor, default_checker};
use mycode_tools::tool::{PermissionSubject, Tool, ToolCategory, ToolResult};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct FailingTool;

type ScriptedCall = Vec<Result<ProviderEvent, ProviderError>>;
type SharedScriptedCalls = Arc<Mutex<Vec<ScriptedCall>>>;
type SharedProviderRequests = Arc<Mutex<Vec<ProviderRequest>>>;

#[async_trait::async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &'static str {
        "FailingTool"
    }

    fn description(&self) -> &'static str {
        "A Tool that always fails"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn schema(&self) -> Value {
        json!({"name": self.name(), "description": self.description(), "input_schema": {}})
    }

    fn permission_subject(&self, _arguments: &Value) -> Option<PermissionSubject> {
        None
    }

    async fn execute(&self, _context: &ToolContext, _arguments: Value) -> ToolResult {
        ToolResult::error("tool failed")
    }
}

#[derive(Default, Clone)]
struct ScriptedProvider {
    calls: SharedScriptedCalls,
    requests: SharedProviderRequests,
}

impl ScriptedProvider {
    fn new(calls: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(
                calls
                    .into_iter()
                    .map(|events| events.into_iter().map(Ok).collect())
                    .collect(),
            )),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<ProviderRequest> {
        self.requests
            .lock()
            .expect("request lock should not be poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl LlmClient for ScriptedProvider {
    async fn stream(
        &self,
        request: ProviderRequest,
        _cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .expect("request lock should not be poisoned")
            .push(request);
        let results = self
            .calls
            .lock()
            .expect("call lock should not be poisoned")
            .remove(0);
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            for result in results {
                if sender.send(result).await.is_err() {
                    return;
                }
            }
        });
        Ok(receiver)
    }
}

#[tokio::test]
async fn tool_errors_are_returned_to_the_provider_and_persisted() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let provider = ScriptedProvider::new(vec![
        vec![failing_tool_call(), stream_end(StopReason::ToolUse)],
        vec![
            ProviderEvent::TextDelta {
                text: "recovered".into(),
            },
            stream_end(StopReason::EndTurn),
        ],
    ]);
    let (agent, provider) = build_agent(workspace.path(), provider, AgentConfig::default()).await;
    let mut events = Vec::new();
    let mut events_receiver = agent.run("fail").events;
    while let Some(event) = events_receiver.recv().await {
        events.push(event);
    }

    assert!(events.contains(&AgentEvent::ToolResult {
        tool_id: "call-fail".into(),
        tool_name: "FailingTool".into(),
        content: "tool failed".into(),
        is_error: true,
    }));
    assert!(events.contains(&AgentEvent::RunCompleted {
        final_text: "recovered".into(),
    }));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].tools[0].parameters,
        serde_json::json!({}),
        "Provider Tools should receive input_schema, not the full Tool descriptor"
    );
    let second_conversation = &requests[1].conversation;
    assert!(second_conversation.messages().iter().any(|message| {
        message.role == MessageRole::User
            && message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { is_error, .. } if *is_error
                )
            })
    }));
}

#[tokio::test]
async fn reaching_max_iterations_reports_a_terminal_event_after_tool_results() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let provider = ScriptedProvider::new(vec![vec![
        failing_tool_call(),
        stream_end(StopReason::ToolUse),
    ]]);
    let (agent, _requests) = build_agent(
        workspace.path(),
        provider,
        AgentConfig {
            max_iterations: 1,
            ..AgentConfig::default()
        },
    )
    .await;
    let mut events = Vec::new();
    let mut events_receiver = agent.run("fail once").events;
    while let Some(event) = events_receiver.recv().await {
        events.push(event);
    }

    assert_eq!(
        events.last(),
        Some(&AgentEvent::MaxIterationsReached { limit: 1 })
    );
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].role, MessageRole::User);
}

async fn build_agent(
    work_dir: &std::path::Path,
    provider: ScriptedProvider,
    config: AgentConfig,
) -> (Agent, ScriptedProvider) {
    let registry = ToolRegistry::new();
    registry
        .register(FailingTool)
        .expect("tool should register");
    let checker = default_checker(PermissionMode::BypassPermissions, work_dir, work_dir)
        .expect("checker should create");
    let context = ToolContext::with_cancellation(work_dir, "session", CancellationToken::new())
        .expect("context should create");
    let provider_for_test = provider.clone();
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(work_dir),
        SessionId::new("session").unwrap(),
        config,
    );
    (agent, provider_for_test)
}

fn failing_tool_call() -> ProviderEvent {
    ProviderEvent::ToolCallComplete {
        tool_id: "call-fail".into(),
        tool_name: "FailingTool".into(),
        arguments: serde_json::Value::Null,
    }
}

fn stream_end(stop_reason: StopReason) -> ProviderEvent {
    ProviderEvent::StreamEnd {
        stop_reason,
        usage: Default::default(),
    }
}
