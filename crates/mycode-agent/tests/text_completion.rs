use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::conversation::MessageRole;
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{MockClient, ProviderEvent, StopReason, Usage};
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn text_completion_streams_events_and_persists_the_session() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let provider = MockClient::new(vec![
        ProviderEvent::TextDelta {
            text: "hello".into(),
        },
        ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        },
    ]);
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::Default,
        workspace.path(),
        workspace.path(),
    )
    .expect("permission checker should create");
    let cancellation = CancellationToken::new();
    let context = mycode_tools::context::ToolContext::with_cancellation(
        workspace.path(),
        "session",
        cancellation,
    )
    .expect("tool context should create");
    let session_store = SessionStore::new(workspace.path());
    let session_id = SessionId::new("session").expect("session ID should be valid");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(registry, checker),
        context,
        session_store,
        session_id,
        AgentConfig::default(),
    );

    let mut run = agent.run("hi");
    let mut events = Vec::new();
    while let Some(event) = run.events.recv().await {
        events.push(event);
    }

    assert_eq!(
        events,
        vec![
            AgentEvent::TextDelta {
                text: "hello".into()
            },
            AgentEvent::RunCompleted {
                final_text: "hello".into()
            },
        ]
    );
    let messages = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").unwrap())
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
}
