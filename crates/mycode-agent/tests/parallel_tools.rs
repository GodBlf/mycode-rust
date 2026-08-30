use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[derive(Clone, Default)]
struct ExecutionLog {
    events: Arc<Mutex<Vec<&'static str>>>,
}

struct DelayTool {
    name: &'static str,
    delay: Duration,
    log: ExecutionLog,
}

#[async_trait::async_trait]
impl Tool for DelayTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Test tool with an observable delay"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn schema(&self) -> Value {
        json!({"name": self.name, "description": self.description(), "input_schema": {}})
    }

    fn permission_subject(&self, _arguments: &Value) -> Option<PermissionSubject> {
        None
    }

    async fn execute(&self, _context: &ToolContext, _arguments: Value) -> ToolResult {
        self.log
            .events
            .lock()
            .expect("log lock should not be poisoned")
            .push(self.name);
        tokio::time::sleep(self.delay).await;
        self.log
            .events
            .lock()
            .expect("log lock should not be poisoned")
            .push(self.name);
        ToolResult::success(format!("finished {}", self.name))
    }
}

#[derive(Default)]
struct ScriptedProvider {
    calls: Mutex<Vec<Vec<Result<ProviderEvent, ProviderError>>>>,
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
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for ScriptedProvider {
    async fn stream(
        &self,
        _request: ProviderRequest,
        _cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
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
async fn multiple_tools_run_bounded_and_results_preserve_request_order() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let log = ExecutionLog::default();
    let registry = ToolRegistry::new();
    registry
        .register(DelayTool {
            name: "SlowOne",
            delay: Duration::from_millis(80),
            log: log.clone(),
        })
        .expect("tool should register");
    registry
        .register(DelayTool {
            name: "SlowTwo",
            delay: Duration::from_millis(50),
            log: log.clone(),
        })
        .expect("tool should register");
    registry
        .register(DelayTool {
            name: "Fast",
            delay: Duration::from_millis(1),
            log: log.clone(),
        })
        .expect("tool should register");
    let provider = ScriptedProvider::new(vec![
        vec![
            tool_call("call-1", "SlowOne"),
            tool_call("call-2", "SlowTwo"),
            tool_call("call-3", "Fast"),
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "all done".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ],
    ]);
    let checker = default_checker(
        PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("checker should create");
    let context =
        ToolContext::with_cancellation(workspace.path(), "session", CancellationToken::new())
            .expect("context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(Arc::new(registry), checker),
        context,
        SessionStore::new(workspace.path()),
        SessionId::new("session").unwrap(),
        AgentConfig {
            tool_concurrency: 2,
            ..AgentConfig::default()
        },
    );

    let mut run = agent.run("run tools");
    let mut events = Vec::new();
    while let Some(event) = run.events.recv().await {
        events.push(event);
    }

    let tool_results = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { tool_id, .. } => Some(tool_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results, ["call-1", "call-2", "call-3"]);
    assert_eq!(
        log.events.lock().expect("log lock should not be poisoned")[..2],
        ["SlowOne", "SlowTwo"]
    );

    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    let assistant = &messages[1];
    assert_eq!(assistant.role, MessageRole::Assistant);
    let assistant_tool_ids = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(assistant_tool_ids, ["call-1", "call-2", "call-3"]);
    let results = &messages[2];
    assert_eq!(results.role, MessageRole::User);
    let result_ids = results
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(result_ids, ["call-1", "call-2", "call-3"]);
}

fn tool_call(tool_id: &str, tool_name: &str) -> ProviderEvent {
    ProviderEvent::ToolCallComplete {
        tool_id: tool_id.into(),
        tool_name: tool_name.into(),
        arguments: serde_json::Value::Null,
    }
}
