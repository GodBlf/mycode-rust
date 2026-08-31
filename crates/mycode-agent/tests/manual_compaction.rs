use std::sync::Arc;

use mycode_agent::{Agent, AgentConfig, CompactionConfig};
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{
    LlmClient, MockClient, ProviderError, ProviderEvent, ProviderRequest, ProviderStream,
    StopReason,
};
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct SharedMockClient {
    inner: Arc<MockClient>,
}

#[async_trait::async_trait]
impl LlmClient for SharedMockClient {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.inner.stream(request, cancellation).await
    }
}

#[tokio::test]
async fn manual_compaction_summarizes_preserves_tail_and_appends_boundary() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let session_id = SessionId::new("session").expect("session ID should be valid");
    let session_store = SessionStore::new(workspace.path());
    for text in ["old request", "old answer", "recent request"] {
        let role = if text.starts_with("old") {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        let role = if text == "recent request" {
            MessageRole::User
        } else {
            role
        };
        session_store
            .append(
                &session_id,
                &ConversationMessage {
                    role,
                    content: vec![ContentBlock::Text { text: text.into() }],
                    timestamp_unix_seconds: 42,
                },
            )
            .expect("append Session message");
    }

    let mock = Arc::new(MockClient::new(vec![
        ProviderEvent::TextDelta {
            text: "<summary>actionable summary</summary>".into(),
        },
        ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ]));
    let provider = SharedMockClient {
        inner: Arc::clone(&mock),
    };
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("permission checker should create");
    let context = mycode_tools::context::ToolContext::new(workspace.path(), session_id.as_str())
        .expect("tool context should create");
    let agent = Agent::new(
        Box::new(provider),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace.path()),
        session_id,
        AgentConfig {
            compaction: CompactionConfig {
                min_keep_messages: 1,
                keep_recent_tokens: 1,
                ..CompactionConfig::default()
            },
            ..AgentConfig::default()
        },
    );

    let outcome = agent.compact().await.expect("manual compaction");
    assert_eq!(outcome.summary, "actionable summary");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .conversation
            .first_user_text()
            .is_some_and(|text| text.contains("old request")),
        "summary request should include the older prefix"
    );

    let resumed = SessionStore::new(workspace.path())
        .load(&SessionId::new("session").expect("session ID should be valid"))
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(resumed.len(), 2);
    assert_eq!(resumed[0].first_text(), Some("actionable summary"));
    assert_eq!(resumed[1].first_text(), Some("recent request"));
}

#[tokio::test]
async fn manual_compaction_does_not_split_tool_use_and_result_pairs() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let session_id = SessionId::new("tool-pair").expect("session ID should be valid");
    let session_store = SessionStore::new(workspace.path());
    let records = [
        ConversationMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "old request".into(),
            }],
            timestamp_unix_seconds: 42,
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                tool_use_id: "tool-1".into(),
                tool_name: "ReadFile".into(),
                arguments: serde_json::json!({"file_path": "README.md"}),
            }],
            timestamp_unix_seconds: 42,
        },
        ConversationMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool-1".into(),
                content: "file contents".into(),
                is_error: false,
            }],
            timestamp_unix_seconds: 42,
        },
    ];
    for message in records {
        session_store
            .append(&session_id, &message)
            .expect("append Session message");
    }

    let mock = Arc::new(MockClient::new(vec![
        ProviderEvent::TextDelta {
            text: "<summary>summary</summary>".into(),
        },
        ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        },
    ]));
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::BypassPermissions,
        workspace.path(),
        workspace.path(),
    )
    .expect("permission checker should create");
    let context = mycode_tools::context::ToolContext::new(workspace.path(), session_id.as_str())
        .expect("tool context should create");
    let agent = Agent::new(
        Box::new(SharedMockClient {
            inner: Arc::clone(&mock),
        }),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace.path()),
        session_id.clone(),
        AgentConfig {
            compaction: CompactionConfig {
                min_keep_messages: 1,
                keep_recent_tokens: 1,
                ..CompactionConfig::default()
            },
            ..AgentConfig::default()
        },
    );

    agent.compact().await.expect("manual compaction");

    let resumed = SessionStore::new(workspace.path())
        .load(&session_id)
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(resumed.len(), 3);
    assert!(matches!(
        resumed[1].content[0],
        ContentBlock::ToolUse { .. }
    ));
    assert!(matches!(
        resumed[2].content[0],
        ContentBlock::ToolResult { .. }
    ));
}
