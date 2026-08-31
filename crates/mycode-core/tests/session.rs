use std::fs;
use std::thread;
use std::time::Duration;

mod support;

use mycode_core::conversation::{ContentBlock, MessageRole};
use mycode_core::session::{CompactBoundary, SessionId, SessionStore};
use serde_json::json;

use support::message;

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
                        encrypted_content: String::new(),
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

#[test]
fn session_search_matches_id_and_first_user_prompt_case_insensitively() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let rust_session = SessionId::new("rust-refactor").expect("valid session ID");
    let database_session = SessionId::new("database-migration").expect("valid session ID");

    store
        .append(
            &rust_session,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "Fix the workspace lints".into(),
                }],
            ),
        )
        .expect("append Rust Session");
    store
        .append(
            &database_session,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "Add a migration".into(),
                }],
            ),
        )
        .expect("append database Session");

    assert_eq!(
        store
            .search("MIGRATION")
            .expect("search Sessions")
            .into_iter()
            .map(|summary| summary.id)
            .collect::<Vec<_>>(),
        vec![database_session]
    );
    assert_eq!(
        store
            .search("RUST")
            .expect("search Sessions")
            .into_iter()
            .map(|summary| summary.id)
            .collect::<Vec<_>>(),
        vec![rust_session]
    );
    assert_eq!(store.search("").expect("search Sessions").len(), 2);
    assert!(store.search("missing").expect("search Sessions").is_empty());
}

#[test]
fn compact_boundary_resume_replays_summary_kept_tail_and_later_messages() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let session_id = SessionId::new("compacted").expect("valid session ID");

    store
        .append(
            &session_id,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "old prompt".into(),
                }],
            ),
        )
        .expect("append old prompt");
    let kept = message(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: "kept answer".into(),
        }],
    );
    store
        .append_compact_boundary(
            &session_id,
            &CompactBoundary {
                summary: "old conversation summary".into(),
                keep: vec![kept.clone()],
            },
        )
        .expect("append compact boundary");
    store
        .append(
            &session_id,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text {
                    text: "new prompt".into(),
                }],
            ),
        )
        .expect("append post-boundary prompt");

    let resumed = store
        .load(&session_id)
        .expect("load compacted Session")
        .expect("Session exists");

    assert_eq!(resumed.len(), 3);
    assert_eq!(resumed[0].first_text(), Some("old conversation summary"));
    assert_eq!(resumed[1], kept);
    assert_eq!(resumed[2].first_text(), Some("new prompt"));
}

#[test]
fn compact_boundary_resume_uses_the_last_boundary_and_falls_back_from_corruption() {
    let work = tempfile::tempdir().expect("work tempdir");
    let store = SessionStore::new(work.path());
    let session_id = SessionId::new("multi-boundary").expect("valid session ID");
    let keep = message(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "kept".into(),
        }],
    );

    store
        .append(
            &session_id,
            &message(
                MessageRole::User,
                vec![ContentBlock::Text { text: "raw".into() }],
            ),
        )
        .expect("append raw message");
    store
        .append_compact_boundary(
            &session_id,
            &CompactBoundary {
                summary: "first summary".into(),
                keep: vec![keep.clone()],
            },
        )
        .expect("append first boundary");
    store
        .append_compact_boundary(
            &session_id,
            &CompactBoundary {
                summary: "second summary".into(),
                keep: vec![keep],
            },
        )
        .expect("append second boundary");

    let resumed = store
        .load(&session_id)
        .expect("load twice-compacted Session")
        .expect("Session exists");
    assert_eq!(resumed.len(), 2);
    assert_eq!(resumed[0].first_text(), Some("second summary"));

    let boundary_path = work.path().join(".mycode/sessions/multi-boundary.jsonl");
    let lines = std::fs::read_to_string(&boundary_path)
        .expect("read Session file")
        .lines()
        .map(String::from)
        .collect::<Vec<_>>();
    let corrupt_boundary = lines
        .last()
        .map(|line| line.replace("\"summary\":\"second summary\"", "\"summary\":42"))
        .expect("final boundary should exist");
    std::fs::write(
        &boundary_path,
        format!("{}\n{}\n{}\n", lines[0], lines[1], corrupt_boundary),
    )
    .expect("rewrite Session with corrupt boundary");

    let fallback = store
        .load(&session_id)
        .expect("load Session with corrupt final boundary")
        .expect("Session exists");
    assert_eq!(fallback.len(), 2);
    assert_eq!(fallback[0].first_text(), Some("first summary"));
    assert_eq!(fallback[1].first_text(), Some("kept"));
}
