use std::fs;
use std::sync::{Arc, Mutex};

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
use tokio::sync::mpsc::{self, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct ExecutionLog {
    started: Arc<Mutex<Vec<&'static str>>>,
}

struct TestTool {
    name: &'static str,
    waits_for_cancellation: bool,
    log: ExecutionLog,
    started: Option<UnboundedSender<()>>,
}

#[tokio::test]
async fn cancellation_while_waiting_for_permission_ends_the_run() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let cancellation = CancellationToken::new();
    let registry = ToolRegistry::new();
    registry
        .register(TestTool {
            name: "NeedsPermission",
            waits_for_cancellation: false,
            log: ExecutionLog::default(),
            started: None,
        })
        .expect("tool should register");
    let provider = ScriptedProvider::new(vec![ProviderCall::Events(vec![
        Ok(tool_call("call-permission", "NeedsPermission")),
        Ok(ProviderEvent::StreamEnd {
            stop_reason: StopReason::ToolUse,
            usage: Default::default(),
        }),
    ])]);
    let checker = default_checker(PermissionMode::Default, workspace.path(), workspace.path())
        .expect("checker should create");
    let context = ToolContext::with_cancellation(
        workspace.path(),
        "permission-cancel-session",
        cancellation.clone(),
    )
    .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("permission-cancel-session").unwrap(),
        AgentConfig::default(),
    );

    let mut events = Vec::new();
    let run = agent.run("wait for permission");
    let mut run_events = run.events;
    let (permission_seen_sender, mut permission_seen_receiver) = unbounded_channel();
    let collection = tokio::spawn(async move {
        while let Some(event) = run_events.recv().await {
            if matches!(event, AgentEvent::PermissionRequest { .. }) {
                let _ = permission_seen_sender.send(());
            }
            events.push(event);
        }
        events
    });
    permission_seen_receiver
        .recv()
        .await
        .expect("permission request should be emitted");
    cancellation.cancel();
    let events = collection.await.expect("event collection should finish");

    assert_eq!(events.last(), Some(&AgentEvent::RunCancelled));
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("permission-cancel-session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 2);
}

#[async_trait::async_trait]
impl Tool for TestTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Cancellation test tool"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn schema(&self) -> Value {
        json!({"name": self.name, "description": self.description(), "input_schema": {}})
    }

    fn permission_subject(&self, _arguments: &Value) -> Option<PermissionSubject> {
        None
    }

    async fn execute(&self, context: &ToolContext, _arguments: Value) -> ToolResult {
        self.log
            .started
            .lock()
            .expect("log lock should not be poisoned")
            .push(self.name);
        if let Some(started) = &self.started {
            let _ = started.send(());
        }
        if self.waits_for_cancellation {
            context.cancellation().cancelled().await;
            return ToolResult::error("tool cancelled");
        }
        ToolResult::success(format!("finished {}", self.name))
    }
}

enum ProviderCall {
    Events(Vec<Result<ProviderEvent, ProviderError>>),
    WaitCancellation(UnboundedSender<()>),
}

#[derive(Default)]
struct ScriptedProvider {
    calls: Mutex<Vec<ProviderCall>>,
}

impl ScriptedProvider {
    fn new(calls: Vec<ProviderCall>) -> Self {
        Self {
            calls: Mutex::new(calls),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for ScriptedProvider {
    async fn stream(
        &self,
        _request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let call = self
            .calls
            .lock()
            .expect("mock call lock should not be poisoned")
            .remove(0);
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            match call {
                ProviderCall::Events(results) => {
                    for result in results {
                        if sender.send(result).await.is_err() {
                            return;
                        }
                    }
                }
                ProviderCall::WaitCancellation(started) => {
                    let _ = started.send(());
                    cancellation.cancelled().await;
                    let _ = sender.send(Err(ProviderError::Cancelled)).await;
                }
            }
        });
        Ok(receiver)
    }
}

#[tokio::test]
async fn cancellation_after_a_completed_tool_result_persists_that_result() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let cancellation = CancellationToken::new();
    let (provider_started_sender, mut provider_started_receiver) = unbounded_channel();
    let registry = ToolRegistry::new();
    registry
        .register(TestTool {
            name: "Fast",
            waits_for_cancellation: false,
            log: ExecutionLog::default(),
            started: None,
        })
        .expect("tool should register");
    let provider = ScriptedProvider::new(vec![
        ProviderCall::Events(vec![
            Ok(tool_call("call-fast", "Fast")),
            Ok(ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            }),
        ]),
        ProviderCall::WaitCancellation(provider_started_sender),
    ]);
    let checker = default_checker(
        PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("checker should create");
    let context = ToolContext::with_cancellation(
        workspace.path(),
        "persisted-cancel-session",
        cancellation.clone(),
    )
    .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("persisted-cancel-session").unwrap(),
        AgentConfig::default(),
    );

    let mut events = Vec::new();
    let mut run_events = agent.run("run until cancelled").events;
    let collection = tokio::spawn(async move {
        while let Some(event) = run_events.recv().await {
            events.push(event);
        }
        events
    });
    provider_started_receiver
        .recv()
        .await
        .expect("second provider call should start");
    cancellation.cancel();
    let events = collection.await.expect("event collection should finish");

    assert_eq!(events.last(), Some(&AgentEvent::RunCancelled));
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("persisted-cancel-session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].role, MessageRole::User);
    assert!(matches!(
        &messages[2].content[0],
        ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call-fast"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_run_remains_cancelled_when_persisting_completed_results_fails() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let cancellation = CancellationToken::new();
    let registry = ToolRegistry::new();
    registry
        .register(TestTool {
            name: "Fast",
            waits_for_cancellation: false,
            log: ExecutionLog::default(),
            started: None,
        })
        .expect("tool should register");
    let (waiting_started_sender, mut waiting_started_receiver) = unbounded_channel();
    registry
        .register(TestTool {
            name: "Waiting",
            waits_for_cancellation: true,
            log: ExecutionLog::default(),
            started: Some(waiting_started_sender),
        })
        .expect("tool should register");
    let provider = ScriptedProvider::new(vec![ProviderCall::Events(vec![
        Ok(tool_call("call-fast", "Fast")),
        Ok(tool_call("call-waiting", "Waiting")),
        Ok(ProviderEvent::StreamEnd {
            stop_reason: StopReason::ToolUse,
            usage: Default::default(),
        }),
    ])]);
    let checker = default_checker(
        PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("checker should create");
    let context = ToolContext::with_cancellation(
        workspace.path(),
        "persist-failure-cancel-session",
        cancellation.clone(),
    )
    .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("persist-failure-cancel-session").unwrap(),
        AgentConfig {
            tool_concurrency: 2,
            ..AgentConfig::default()
        },
    );

    let mut events = Vec::new();
    let mut run_events = agent.run("cancel with a persistence failure").events;
    let collection = tokio::spawn(async move {
        while let Some(event) = run_events.recv().await {
            events.push(event);
        }
        events
    });
    waiting_started_receiver
        .recv()
        .await
        .expect("waiting tool should start");

    let session_path = workspace
        .path()
        .join(".mycode/sessions/persist-failure-cancel-session.jsonl");
    fs::remove_file(&session_path).expect("session file should be removable");
    fs::create_dir(&session_path).expect("session path should become a directory");
    cancellation.cancel();

    let events = collection.await.expect("event collection should finish");
    assert_eq!(events.last(), Some(&AgentEvent::RunCancelled));
}

#[tokio::test]
async fn cancellation_while_a_tool_runs_does_not_start_unsubmitted_tools() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let cancellation = CancellationToken::new();
    let log = ExecutionLog::default();
    let (tool_started_sender, mut tool_started_receiver) = unbounded_channel();
    let registry = ToolRegistry::new();
    registry
        .register(TestTool {
            name: "Waiting",
            waits_for_cancellation: true,
            log: log.clone(),
            started: Some(tool_started_sender),
        })
        .expect("tool should register");
    registry
        .register(TestTool {
            name: "Unsubmitted",
            waits_for_cancellation: false,
            log: log.clone(),
            started: None,
        })
        .expect("tool should register");
    let provider = ScriptedProvider::new(vec![ProviderCall::Events(vec![
        Ok(tool_call("call-waiting", "Waiting")),
        Ok(tool_call("call-unsubmitted", "Unsubmitted")),
        Ok(ProviderEvent::StreamEnd {
            stop_reason: StopReason::ToolUse,
            usage: Default::default(),
        }),
    ])]);
    let checker = default_checker(
        PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("checker should create");
    let context = ToolContext::with_cancellation(
        workspace.path(),
        "tool-cancel-session",
        cancellation.clone(),
    )
    .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("tool-cancel-session").unwrap(),
        AgentConfig {
            tool_concurrency: 1,
            ..AgentConfig::default()
        },
    );

    let mut events = Vec::new();
    let mut run_events = agent.run("run until cancelled").events;
    let collection = tokio::spawn(async move {
        while let Some(event) = run_events.recv().await {
            events.push(event);
        }
        events
    });
    tool_started_receiver
        .recv()
        .await
        .expect("waiting tool should start");
    cancellation.cancel();
    let events = collection.await.expect("event collection should finish");

    assert_eq!(events.last(), Some(&AgentEvent::RunCancelled));
    assert_eq!(
        log.started.lock().expect("log lock should not be poisoned")[..],
        ["Waiting"]
    );
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("tool-cancel-session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, MessageRole::Assistant);
}

fn tool_call(tool_id: &str, tool_name: &str) -> ProviderEvent {
    ProviderEvent::ToolCallComplete {
        tool_id: tool_id.into(),
        tool_name: tool_name.into(),
        arguments: serde_json::Value::Null,
    }
}
