use std::sync::{Arc, Mutex};

use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{
    LlmClient, ProviderError, ProviderEvent, ProviderRequest, ProviderStream, StopReason,
};
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct SequenceMockClient {
    streams: Mutex<Vec<Vec<ProviderEvent>>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

#[derive(Clone)]
struct SharedSequenceMockClient {
    inner: Arc<SequenceMockClient>,
}

impl SequenceMockClient {
    fn new(streams: Vec<Vec<ProviderEvent>>) -> Arc<Self> {
        Arc::new(Self {
            streams: Mutex::new(streams),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ProviderRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

#[async_trait::async_trait]
impl LlmClient for SharedSequenceMockClient {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.inner
            .requests
            .lock()
            .expect("request lock")
            .push(request.clone());
        let events = self
            .inner
            .streams
            .lock()
            .expect("stream lock")
            .first()
            .cloned()
            .ok_or_else(|| ProviderError::InvalidResponse {
                message: "no mock stream remains".into(),
            })?;
        self.inner.streams.lock().expect("stream lock").remove(0);

        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            if cancellation.is_cancelled() {
                let _ = sender.send(Err(ProviderError::Cancelled)).await;
                return;
            }
            for event in events {
                if sender.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });
        Ok(receiver)
    }
}

#[tokio::test]
async fn tool_result_replacement_is_stable_across_turns_and_restart() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    std::fs::write(workspace.path().join("large.txt"), "x".repeat(60_000))
        .expect("file should write");
    let session_id = SessionId::new("restart").expect("session ID should be valid");
    let first_provider = SequenceMockClient::new(vec![
        vec![
            ProviderEvent::ToolCallComplete {
                tool_id: "read-1".into(),
                tool_name: "ReadFile".into(),
                arguments: serde_json::json!({"file_path": "large.txt"}),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Default::default(),
            },
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ],
    ]);
    let config = AgentConfig {
        tool_result_budget: mycode_agent::tool_result::ToolResultBudgetConfig {
            single_result_limit_chars: 50_000,
            message_aggregate_limit_chars: 200_000,
            old_result_snip_chars: 2_000,
            keep_recent_turns: 10,
        },
        ..AgentConfig::default()
    };
    run_agent(
        workspace.path(),
        Arc::clone(&first_provider),
        &session_id,
        config.clone(),
    )
    .await;

    let first_requests = first_provider.requests();
    assert_eq!(first_requests.len(), 2);
    let first_preview = tool_result_content(&first_requests[1], "read-1")
        .expect("first run should replace the Tool Result");
    assert!(first_preview.starts_with("[Result of 60002 chars saved to"));

    let second_provider = SequenceMockClient::new(vec![vec![
        ProviderEvent::TextDelta {
            text: "done".into(),
        },
        ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ]]);
    run_agent(
        workspace.path(),
        Arc::clone(&second_provider),
        &session_id,
        config,
    )
    .await;

    let second_requests = second_provider.requests();
    let restarted_preview = tool_result_content(&second_requests[0], "read-1")
        .expect("restart should replace the Tool Result");
    assert_eq!(restarted_preview, first_preview);

    let session = SessionStore::new(workspace.path())
        .load(&session_id)
        .expect("session should load")
        .expect("session should exist");
    assert!(
        session
            .iter()
            .any(|message| message.content.iter().any(|block| matches!(block,
                mycode_core::conversation::ContentBlock::ToolResult { content, .. }
                    if content.len() == 60_002
            )))
    );
}

async fn run_agent(
    workspace: &std::path::Path,
    provider: Arc<SequenceMockClient>,
    session_id: &SessionId,
    config: AgentConfig,
) {
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::BypassPermissions,
        workspace,
        workspace,
    )
    .expect("permission checker should create");
    let context = mycode_tools::context::ToolContext::new(workspace, session_id.as_str())
        .expect("tool context should create");
    let agent = Agent::new(
        Box::new(SharedSequenceMockClient { inner: provider }),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace),
        session_id.clone(),
        config,
    );
    let run = agent.run("continue");
    let mut events = run.events;
    while let Some(event) = events.recv().await {
        assert!(
            !matches!(event, AgentEvent::RunError { .. }),
            "run should not fail"
        );
    }
}

fn tool_result_content(request: &ProviderRequest, tool_use_id: &str) -> Option<String> {
    request.conversation.messages().iter().find_map(|message| {
        message.content.iter().find_map(|block| match block {
            mycode_core::conversation::ContentBlock::ToolResult {
                tool_use_id: id,
                content,
                ..
            } if id == tool_use_id => Some(content.clone()),
            _ => None,
        })
    })
}
