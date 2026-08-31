use std::sync::{Arc, Mutex};

use mycode_agent::{
    Agent, AgentConfig, CompactionConfig, CompactionTracker, CompactionTrigger, estimate_tokens,
};
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::{
    LlmClient, ProviderError, ProviderEvent, ProviderRequest, ProviderStream, StopReason, Usage,
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

fn agent(
    workspace: &std::path::Path,
    provider: Arc<SequenceMockClient>,
    session_id: SessionId,
    config: AgentConfig,
) -> Agent {
    let registry = default_registry();
    let checker = default_checker(
        mycode_core::config::PermissionMode::BypassPermissions,
        workspace,
        workspace,
    )
    .expect("permission checker should create");
    let context = mycode_tools::context::ToolContext::new(workspace, session_id.as_str())
        .expect("tool context should create");
    Agent::new(
        Box::new(SharedSequenceMockClient { inner: provider }),
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(workspace),
        session_id,
        config,
    )
}

fn small_compaction_config() -> CompactionConfig {
    CompactionConfig {
        min_keep_messages: 1,
        keep_recent_tokens: 1,
        recovery_token_budget: 1_000,
        ..CompactionConfig::default()
    }
}

#[tokio::test]
async fn automatic_compaction_triggers_before_the_provider_call_and_persists_boundary() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let session_id = SessionId::new("auto-session").expect("session ID should be valid");
    let old_text = "x".repeat(1_000);
    SessionStore::new(workspace.path())
        .append(
            &session_id,
            &ConversationMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: old_text.clone(),
                }],
                timestamp_unix_seconds: 42,
            },
        )
        .expect("append old message");

    let provider = SequenceMockClient::new(vec![
        vec![
            ProviderEvent::TextDelta {
                text: "<summary>old work summarized</summary>".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "continued".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ],
    ]);
    let config = AgentConfig {
        compaction: CompactionConfig {
            context_window_tokens: 100,
            max_output_tokens: 5,
            auto_safety_margin_tokens: 0,
            manual_safety_margin_tokens: 0,
            recovery_token_budget: 1,
            ..small_compaction_config()
        },
        ..AgentConfig::default()
    };
    let run = agent(
        workspace.path(),
        Arc::clone(&provider),
        session_id.clone(),
        config,
    )
    .run("continue");
    let mut events = run.events;
    let mut compacted = false;
    while let Some(event) = events.recv().await {
        if matches!(event, mycode_agent::AgentEvent::Compacted { .. }) {
            compacted = true;
        }
        assert!(
            !matches!(event, mycode_agent::AgentEvent::RunError { .. }),
            "automatic run should not fail"
        );
    }
    assert!(compacted);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .conversation
            .first_user_text()
            .is_some_and(|text| text.contains(&old_text))
    );
    assert!(
        requests[1]
            .conversation
            .first_user_text()
            .is_some_and(|text| text.contains("old work summarized"))
    );

    let resumed = SessionStore::new(workspace.path())
        .load(&session_id)
        .expect("session should load")
        .expect("session should exist");
    assert_eq!(resumed.len(), 3);
    assert!(
        resumed[0]
            .first_text()
            .is_some_and(|text| text.contains("old work summarized"))
    );
    assert_eq!(resumed[2].first_text(), Some("continued"));
}

#[tokio::test]
async fn automatic_compaction_recovery_includes_recent_file_read_and_tools() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let file_content = format!("recover this file content\n{}", "z".repeat(1_000));
    std::fs::write(workspace.path().join("notes.txt"), &file_content).expect("file should write");
    let session_id = SessionId::new("recovery-session").expect("session ID should be valid");
    let provider = SequenceMockClient::new(vec![
        vec![
            ProviderEvent::ToolCallComplete {
                tool_id: "read-1".into(),
                tool_name: "ReadFile".into(),
                arguments: serde_json::json!({"file_path": "notes.txt"}),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 70,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
        ],
        vec![
            ProviderEvent::TextDelta {
                text: "<summary>file read summarized</summary>".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
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
        compaction: CompactionConfig {
            context_window_tokens: 110,
            max_output_tokens: 5,
            auto_safety_margin_tokens: 0,
            manual_safety_margin_tokens: 0,
            recovery_token_budget: 80,
            ..small_compaction_config()
        },
        ..AgentConfig::default()
    };
    let run = agent(workspace.path(), Arc::clone(&provider), session_id, config).run("read notes");
    let mut events = run.events;
    while let Some(event) = events.recv().await {
        assert!(
            !matches!(event, mycode_agent::AgentEvent::RunError { .. }),
            "recovery run should not fail"
        );
    }

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let compacted_request = &requests[2];
    let continuation = compacted_request
        .conversation
        .first_user_text()
        .expect("compacted conversation should have text");
    assert!(continuation.contains("notes.txt"));
    assert!(continuation.contains("recover this file content"));
    assert!(continuation.contains("Available tools:"));
    assert!(!continuation.contains(&"z".repeat(1_000)));
}

#[tokio::test]
async fn soft_compaction_failures_are_circuit_broken_within_one_agent_run() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    std::fs::write(workspace.path().join("notes.txt"), "content\n").expect("file should write");
    let session_id = SessionId::new("circuit-breaker").expect("session ID should be valid");
    SessionStore::new(workspace.path())
        .append(
            &session_id,
            &ConversationMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "x".repeat(280),
                }],
                timestamp_unix_seconds: 42,
            },
        )
        .expect("append initial message");

    let read_file = || {
        vec![
            ProviderEvent::ToolCallComplete {
                tool_id: format!("read-{}", uuid_like()),
                tool_name: "ReadFile".into(),
                arguments: serde_json::json!({"file_path": "notes.txt"}),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 70,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
        ]
    };
    let empty_summary = || {
        vec![ProviderEvent::StreamEnd {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        }]
    };
    let final_stream = || {
        vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::StreamEnd {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ]
    };
    let provider = SequenceMockClient::new(vec![
        empty_summary(),
        read_file(),
        empty_summary(),
        read_file(),
        final_stream(),
    ]);
    let config = AgentConfig {
        max_iterations: 4,
        compaction: CompactionConfig {
            context_window_tokens: 100,
            max_output_tokens: 5,
            auto_safety_margin_tokens: 20,
            manual_safety_margin_tokens: 0,
            max_consecutive_auto_failures: 2,
            ..small_compaction_config()
        },
        ..AgentConfig::default()
    };
    let run = agent(workspace.path(), Arc::clone(&provider), session_id, config).run("continue");
    let mut events = run.events;
    while let Some(event) = events.recv().await {
        assert!(
            !matches!(event, mycode_agent::AgentEvent::RunError { .. }),
            "circuit-broken run should finish: {event:?}"
        );
    }

    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.system_prompt.contains("summaries"))
            .count(),
        2
    );
}

#[tokio::test]
async fn hard_threshold_compaction_failure_terminates_the_agent_run() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let session_id = SessionId::new("hard-threshold").expect("session ID should be valid");
    SessionStore::new(workspace.path())
        .append(
            &session_id,
            &ConversationMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "x".repeat(1_000),
                }],
                timestamp_unix_seconds: 42,
            },
        )
        .expect("append initial message");
    let provider = SequenceMockClient::new(vec![vec![ProviderEvent::StreamEnd {
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    }]]);
    let config = AgentConfig {
        compaction: CompactionConfig {
            context_window_tokens: 100,
            max_output_tokens: 5,
            auto_safety_margin_tokens: 0,
            manual_safety_margin_tokens: 0,
            ..small_compaction_config()
        },
        ..AgentConfig::default()
    };
    let run = agent(workspace.path(), Arc::clone(&provider), session_id, config).run("continue");
    let mut events = run.events;
    let mut terminal = None;
    while let Some(event) = events.recv().await {
        terminal = Some(event);
    }

    assert!(matches!(
        terminal,
        Some(mycode_agent::AgentEvent::RunError { .. })
    ));
    assert_eq!(provider.requests().len(), 1);
}

#[test]
fn usage_anchor_and_failure_tracker_drive_thresholds() {
    let mut tracker = CompactionTracker::default();
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            timestamp_unix_seconds: 42,
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp_unix_seconds: 42,
        },
    ];
    tracker.record_usage(
        Usage {
            input_tokens: 90,
            output_tokens: 5,
            cache_read_tokens: 2,
            cache_creation_tokens: 3,
        },
        messages.len(),
    );
    let config = CompactionConfig {
        context_window_tokens: 110,
        max_output_tokens: 5,
        auto_safety_margin_tokens: 11,
        manual_safety_margin_tokens: 4,
        max_consecutive_auto_failures: 2,
        ..CompactionConfig::default()
    };
    assert_eq!(tracker.used_tokens(&messages), 100);
    assert_eq!(
        tracker.should_compact(&messages, &config),
        CompactionTrigger::Soft
    );

    assert!(tracker.record_auto_failure(&config));
    assert!(!tracker.record_auto_failure(&config));
    assert_eq!(
        tracker.should_compact(&messages, &config),
        CompactionTrigger::None
    );

    tracker.record_auto_success();
    assert_eq!(
        tracker.should_compact(&messages, &config),
        CompactionTrigger::Soft
    );
}

#[test]
fn zero_output_limit_still_reserves_summary_output() {
    let tracker = CompactionTracker::default();
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "x".repeat(350),
        }],
        timestamp_unix_seconds: 42,
    }];
    let config = CompactionConfig {
        context_window_tokens: 110,
        max_output_tokens: 0,
        summary_output_reserve_tokens: 5,
        auto_safety_margin_tokens: 11,
        manual_safety_margin_tokens: 0,
        max_consecutive_auto_failures: 2,
        ..CompactionConfig::default()
    };

    assert_eq!(
        tracker.should_compact(&messages, &config),
        CompactionTrigger::Soft
    );
}

#[test]
fn token_estimation_uses_utf8_bytes_for_multibyte_text() {
    let cjk = ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "你好".into(),
        }],
        timestamp_unix_seconds: 42,
    };
    let ascii = ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "abcdef".into(),
        }],
        timestamp_unix_seconds: 42,
    };

    assert_eq!(
        estimate_tokens(std::slice::from_ref(&cjk)),
        estimate_tokens(std::slice::from_ref(&ascii))
    );
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
    format!("read-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}
