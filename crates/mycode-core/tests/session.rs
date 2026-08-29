use std::fs;
use std::thread;
use std::time::Duration;

use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use mycode_core::session::{SessionId, SessionStore};
use serde_json::json;

fn message(role: MessageRole, content: Vec<ContentBlock>) -> ConversationMessage {
    ConversationMessage {
        role,
        content,
        timestamp_unix_seconds: 42,
    }
}

fn tool_result() -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: "tool-1".into(),
        content: "contents".into(),
        is_error: true,
    }
}

#[test]
fn session_append_and_load_round_trip_typed_messages_in_order() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let session_id = SessionId::new("session-1").expect("valid session ID");

    store
        .append(
            &session_id,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            ),
        )
        .expect("append user message");
    store
        .append(
            &session_id,
            &message(
                MessageRole::Assistant,
                vec![
                    ContentBlock::Thinking {
                        thinking: "thinking".into(),
                        signature: "signature".into(),
                    },
                    ContentBlock::ToolUse {
                        tool_use_id: "tool-1".into(),
                        tool_name: "read_file".into(),
                        arguments: json!({ "path": "README.md" }),
                    },
                ],
            ),
        )
        .expect("append assistant message");
    store
        .append(
            &session_id,
            &message(MessageRole::User, vec![tool_result()]),
        )
        .expect("append tool result");

    let messages = store
        .load(&session_id)
        .expect("load session")
        .expect("session exists");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].content.len(), 2);
    assert_eq!(messages[2].content[0], tool_result());
}

#[test]
fn session_list_reports_metadata_newest_first() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let older = SessionId::new("older").expect("valid session ID");
    let newer = SessionId::new("newer").expect("valid session ID");

    store
        .append(
            &older,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "older prompt".into(),
                }],
            ),
        )
        .expect("append older session");
    thread::sleep(Duration::from_millis(10));
    store
        .append(
            &newer,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "newer prompt".into(),
                }],
            ),
        )
        .expect("append newer session");

    let sessions = store.list().expect("list sessions");

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, newer);
    assert_eq!(
        sessions[0].first_user_message.as_deref(),
        Some("newer prompt")
    );
    assert_eq!(sessions[0].message_count, 1);
    assert_eq!(sessions[1].id, older);
    assert!(sessions[0].modified_at > sessions[1].modified_at);
}

#[test]
fn loading_a_missing_session_is_none_and_malformed_jsonl_fails() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let missing = SessionId::new("missing").expect("valid session ID");

    assert!(
        store
            .load(&missing)
            .expect("missing session lookup should succeed")
            .is_none()
    );

    let malformed = SessionId::new("malformed").expect("valid session ID");
    let path = work.path().join(".mycode/sessions/malformed.jsonl");
    fs::create_dir_all(path.parent().expect("session dir")).expect("create session dir");
    fs::write(&path, "{not json}\n").expect("write malformed session");

    let error = store
        .load(&malformed)
        .expect_err("malformed JSONL should fail")
        .to_string();
    assert!(
        error.contains("line 1"),
        "error should identify the line: {error}"
    );
}

#[test]
fn session_ids_reject_path_traversal() {
    let error = SessionId::new("../outside")
        .expect_err("path traversal should fail")
        .to_string();
    assert!(
        error.contains("session ID"),
        "error should identify the field: {error}"
    );
}
