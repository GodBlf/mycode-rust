use mycode_agent::tool_result::{ToolResultBudget, ToolResultBudgetConfig};
use mycode_core::conversation::{ContentBlock, Conversation, ConversationMessage, MessageRole};

fn tool_result_message(results: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage {
        role: MessageRole::User,
        content: results,
        timestamp_unix_seconds: 42,
    }
}

fn result(id: &str, content: impl Into<String>) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: content.into(),
        is_error: false,
    }
}

fn text_message(role: MessageRole, text: &str) -> ConversationMessage {
    ConversationMessage {
        role,
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp_unix_seconds: 42,
    }
}

fn tool_use_message(
    tool_use_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> ConversationMessage {
    ConversationMessage {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::ToolUse {
            tool_use_id: tool_use_id.into(),
            tool_name: tool_name.into(),
            arguments,
        }],
        timestamp_unix_seconds: 42,
    }
}

fn config() -> ToolResultBudgetConfig {
    ToolResultBudgetConfig {
        single_result_limit_chars: 10_000,
        message_aggregate_limit_chars: 5_000,
        old_result_snip_chars: 5,
        keep_recent_turns: 1,
    }
}

#[test]
fn single_large_tool_result_spills_and_stays_stable_after_restart() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let mut budget = ToolResultBudget::resume(
        workspace.path(),
        "session",
        ToolResultBudgetConfig {
            single_result_limit_chars: 5,
            message_aggregate_limit_chars: 200,
            old_result_snip_chars: 1_000,
            keep_recent_turns: 5,
        },
    )
    .expect("budget should resume");
    let mut conversation = Conversation::new();
    conversation.push(tool_result_message(vec![result("tool-1", "abcdefghij")]));

    let first = budget.apply(&conversation);
    let ContentBlock::ToolResult { content, .. } = &first.messages()[0].content[0] else {
        panic!("expected a Tool Result");
    };
    assert!(content.starts_with("[Result of 10 chars saved to "));
    assert!(content.ends_with(" — read with ReadFile if needed]"));

    let mut restarted = ToolResultBudget::resume(
        workspace.path(),
        "session",
        ToolResultBudgetConfig {
            single_result_limit_chars: 5,
            message_aggregate_limit_chars: 200,
            old_result_snip_chars: 1_000,
            keep_recent_turns: 5,
        },
    )
    .expect("budget should restart");
    let second = restarted.apply(&conversation);
    assert_eq!(first, second);
}

#[test]
fn aggregate_budget_spills_largest_results_while_preserving_order() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let mut budget = ToolResultBudget::resume(workspace.path(), "session", config())
        .expect("budget should resume");
    let mut conversation = Conversation::new();
    conversation.push(tool_result_message(vec![
        result("small", "1".repeat(1_000)),
        result("large", "a".repeat(3_000)),
        result("medium", "b".repeat(2_000)),
    ]));

    let api_conversation = budget.apply(&conversation);
    let ContentBlock::ToolResult { content: small, .. } =
        &api_conversation.messages()[0].content[0]
    else {
        panic!("expected the first Tool Result");
    };
    let ContentBlock::ToolResult { content: large, .. } =
        &api_conversation.messages()[0].content[1]
    else {
        panic!("expected the second Tool Result");
    };
    let ContentBlock::ToolResult {
        content: medium, ..
    } = &api_conversation.messages()[0].content[2]
    else {
        panic!("expected the third Tool Result");
    };

    assert_eq!(small.as_str(), "1".repeat(1_000).as_str());
    assert!(large.starts_with("[Result of 3000 chars saved to"));
    assert_eq!(medium.as_str(), "b".repeat(2_000).as_str());
}

#[test]
fn stale_tool_results_are_snipped_with_a_stable_preview() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let mut budget = ToolResultBudget::resume(workspace.path(), "session", config())
        .expect("budget should resume");
    let mut conversation = Conversation::new();
    conversation.push(text_message(MessageRole::Assistant, "first turn"));
    conversation.push(tool_result_message(vec![result("old", "abcdefghij")]));
    conversation.push(text_message(MessageRole::Assistant, "second turn"));

    let api_conversation = budget.apply(&conversation);
    let ContentBlock::ToolResult { content, .. } = &api_conversation.messages()[1].content[0]
    else {
        panic!("expected a Tool Result");
    };

    assert_eq!(content, "[Stale output snipped: 10 chars]");
    assert_eq!(api_conversation, budget.apply(&conversation));
}

#[test]
fn spill_readback_does_not_spill_again() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let spill_path = workspace.path().join(".mycode/tool_results/readback.txt");
    std::fs::create_dir_all(spill_path.parent().expect("spill parent")).expect("create spill dir");
    std::fs::write(&spill_path, "large readback").expect("write spill file");
    let mut budget = ToolResultBudget::resume(
        workspace.path(),
        "session",
        ToolResultBudgetConfig {
            single_result_limit_chars: 5,
            message_aggregate_limit_chars: 200,
            old_result_snip_chars: 1_000,
            keep_recent_turns: 5,
        },
    )
    .expect("budget should resume");
    let mut conversation = Conversation::new();
    conversation.push(tool_use_message(
        "readback",
        "ReadFile",
        serde_json::json!({"file_path": spill_path}),
    ));
    conversation.push(tool_result_message(vec![result(
        "readback",
        "large readback",
    )]));

    let api_conversation = budget.apply(&conversation);
    let ContentBlock::ToolResult { content, .. } = &api_conversation.messages()[1].content[0]
    else {
        panic!("expected a Tool Result");
    };

    assert_eq!(content, "large readback");
}

#[test]
fn failed_spill_freezes_the_original_result() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let spill_path = workspace.path().join(".mycode/tool_results/tool-1");
    std::fs::create_dir_all(&spill_path).expect("block spill file with a directory");
    let mut budget = ToolResultBudget::resume(
        workspace.path(),
        "session",
        ToolResultBudgetConfig {
            single_result_limit_chars: 5,
            message_aggregate_limit_chars: 200,
            old_result_snip_chars: 1_000,
            keep_recent_turns: 5,
        },
    )
    .expect("budget should resume");
    let mut conversation = Conversation::new();
    conversation.push(tool_result_message(vec![result("tool-1", "abcdefghij")]));

    let first = budget.apply(&conversation);
    let second = budget.apply(&conversation);

    assert_eq!(first, second);
    assert_eq!(
        first.messages()[0].content[0],
        result("tool-1", "abcdefghij")
    );
}

#[test]
fn restart_reconstructs_seen_but_unreplaced_results() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let config = ToolResultBudgetConfig {
        single_result_limit_chars: 10_000,
        message_aggregate_limit_chars: 20_000,
        old_result_snip_chars: 1_000,
        keep_recent_turns: 5,
    };
    let mut conversation = Conversation::new();
    conversation.push(tool_result_message(vec![result("old", "o".repeat(1_000))]));

    let mut first = ToolResultBudget::resume(workspace.path(), "session", config)
        .expect("budget should resume");
    assert_eq!(first.apply(&conversation), conversation);

    conversation.push(tool_result_message(vec![result("new", "n".repeat(24_000))]));
    let mut restarted = ToolResultBudget::resume(workspace.path(), "session", config)
        .expect("budget should restart");
    restarted.reconstruct(&conversation);
    let resumed = restarted.apply(&conversation);

    let ContentBlock::ToolResult { content, .. } = &resumed.messages()[0].content[0] else {
        panic!("expected the old Tool Result");
    };
    assert_eq!(content.as_str(), "o".repeat(1_000).as_str());
}
