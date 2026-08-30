use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{MockClient, ProviderEvent, StopReason};
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn agent_sends_budgeted_results_while_keeping_raw_session_history() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let session_id = SessionId::new("session").expect("session ID should be valid");
    let session_store = SessionStore::new(workspace.path());
    session_store
        .append(
            &session_id,
            &ConversationMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: "abcdefghij".into(),
                    is_error: false,
                }],
                timestamp_unix_seconds: 42,
            },
        )
        .expect("append raw Tool Result");

    let provider = MockClient::new(vec![
        ProviderEvent::TextDelta {
            text: "done".into(),
        },
        ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ]);
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("permission checker should create");
    let cancellation = CancellationToken::new();
    let context = mycode_tools::context::ToolContext::with_cancellation(
        workspace.path(),
        session_id.as_str(),
        cancellation,
    )
    .expect("tool context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(registry, checker),
        context,
        session_store,
        session_id,
        AgentConfig {
            tool_result_budget: mycode_agent::tool_result::ToolResultBudgetConfig {
                single_result_limit_chars: 5,
                message_aggregate_limit_chars: 200,
                old_result_snip_chars: 1_000,
                keep_recent_turns: 5,
            },
            ..AgentConfig::default()
        },
    );

    let run = agent.run("continue");
    let mut events = run.events;
    while let Some(event) = events.recv().await {
        assert!(
            !matches!(event, AgentEvent::RunError { .. }),
            "Agent run should not fail"
        );
    }

    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").expect("session ID should be valid"))
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(
        messages[0].content[0],
        ContentBlock::ToolResult {
            tool_use_id: "tool-1".into(),
            content: "abcdefghij".into(),
            is_error: false,
        }
    );
}
