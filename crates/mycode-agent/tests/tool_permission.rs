use std::sync::Mutex;

use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::config::PermissionMode;
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{
    LlmClient, ProviderError, ProviderEvent, ProviderRequest, ProviderStream, StopReason, Usage,
};
use mycode_tools::context::ToolContext;
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct ScriptedProvider {
    calls: Mutex<Vec<Vec<Result<ProviderEvent, ProviderError>>>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    fn new(calls: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            calls: Mutex::new(
                calls
                    .into_iter()
                    .map(|events| events.into_iter().map(Ok).collect())
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        }
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
            .expect("mock request lock should not be poisoned")
            .push(request);
        let results = self
            .calls
            .lock()
            .expect("mock call lock should not be poisoned")
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
async fn allowed_tool_call_round_trips_and_persists_results() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let provider = ScriptedProvider::new(vec![
        vec![
            ProviderEvent::ToolCallComplete {
                tool_id: "call-1".into(),
                tool_name: "WriteFile".into(),
                arguments: serde_json::json!({
                    "file_path": "created.txt",
                    "content": "created"
                }),
            },
            stream_end(StopReason::ToolUse),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            stream_end(StopReason::EndTurn),
        ],
    ]);
    let registry = default_registry();
    let checker = default_checker(PermissionMode::Default, workspace.path(), workspace.path())
        .expect("checker should create");
    let context =
        ToolContext::with_cancellation(workspace.path(), "session", CancellationToken::new())
            .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("session").unwrap(),
        AgentConfig::default(),
    );

    let mut run = agent.run("create the file");
    let mut events = Vec::new();
    while let Some(event) = run.events.recv().await {
        if let AgentEvent::PermissionRequest { request_id, .. } = &event {
            run.respond(request_id, true);
        }
        events.push(event);
    }

    assert_eq!(
        events,
        vec![
            AgentEvent::ToolCall {
                tool_id: "call-1".into(),
                tool_name: "WriteFile".into(),
                arguments: serde_json::json!({
                    "file_path": "created.txt",
                    "content": "created"
                }),
            },
            AgentEvent::PermissionRequest {
                request_id: "call-1".into(),
                tool_name: "WriteFile".into(),
                arguments: serde_json::json!({
                    "file_path": "created.txt",
                    "content": "created"
                }),
                reason: "user confirmation required".into(),
            },
            AgentEvent::PermissionDecision {
                request_id: "call-1".into(),
                allowed: true,
            },
            AgentEvent::ToolResult {
                tool_id: "call-1".into(),
                tool_name: "WriteFile".into(),
                content: "Successfully wrote to created.txt".into(),
                is_error: false,
            },
            AgentEvent::TextDelta {
                text: "done".into()
            },
            AgentEvent::RunCompleted {
                final_text: "done".into()
            },
        ]
    );
    assert!(workspace.path().join("created.txt").exists());
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn denied_tool_call_does_not_execute_and_returns_an_error_result() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let provider = ScriptedProvider::new(vec![
        vec![
            ProviderEvent::ToolCallComplete {
                tool_id: "call-deny".into(),
                tool_name: "WriteFile".into(),
                arguments: serde_json::json!({
                    "file_path": "denied.txt",
                    "content": "denied"
                }),
            },
            stream_end(StopReason::ToolUse),
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "kept going".into(),
            },
            stream_end(StopReason::EndTurn),
        ],
    ]);
    let registry = default_registry();
    let checker = default_checker(PermissionMode::Default, workspace.path(), workspace.path())
        .expect("checker should create");
    let context =
        ToolContext::with_cancellation(workspace.path(), "deny-session", CancellationToken::new())
            .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("deny-session").unwrap(),
        AgentConfig::default(),
    );

    let mut run = agent.run("try to write");
    let mut events = Vec::new();
    while let Some(event) = run.events.recv().await {
        if let AgentEvent::PermissionRequest { request_id, .. } = &event {
            run.respond(request_id, false);
        }
        events.push(event);
    }

    assert!(events.contains(&AgentEvent::PermissionDecision {
        request_id: "call-deny".into(),
        allowed: false,
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        tool_id: "call-deny".into(),
        tool_name: "WriteFile".into(),
        content: "tool denied by user: WriteFile".into(),
        is_error: true,
    }));
    assert!(events.contains(&AgentEvent::RunCompleted {
        final_text: "kept going".into(),
    }));
    assert!(!workspace.path().join("denied.txt").exists());
}

fn stream_end(stop_reason: StopReason) -> ProviderEvent {
    ProviderEvent::StreamEnd {
        stop_reason,
        usage: Usage::default(),
    }
}
