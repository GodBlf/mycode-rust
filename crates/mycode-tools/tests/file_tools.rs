use mycode_tools::{
    context::ToolContext, edit_file::EditFileTool, read_file::ReadFileTool, tool::Tool,
    write_file::WriteFileTool,
};
use serde_json::json;

fn history_files(work: &std::path::Path, session: &str) -> Vec<std::path::PathBuf> {
    let history_dir = work.join(format!(".mycode/file-history/{session}"));
    std::fs::read_dir(history_dir)
        .expect("history directory exists")
        .filter_map(std::io::Result::ok)
        .map(|entry| entry.path())
        .collect()
}

#[tokio::test]
async fn write_file_creates_new_files_and_backs_up_rewrites() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "write-session").expect("create tool context");
    let read = ReadFileTool::new();
    let write = WriteFileTool::new();

    let created = write
        .execute(
            &context,
            json!({ "file_path": "nested/new.txt", "content": "new content" }),
        )
        .await;
    assert!(!created.is_error, "{}", created.output);
    assert_eq!(
        std::fs::read_to_string(work.path().join("nested/new.txt")).expect("read new file"),
        "new content"
    );
    assert!(history_files(work.path(), "write-session").is_empty());

    read.execute(&context, json!({ "file_path": "nested/new.txt" }))
        .await;
    let rewritten = write
        .execute(
            &context,
            json!({ "file_path": "nested/new.txt", "content": "replacement" }),
        )
        .await;
    assert!(!rewritten.is_error, "{}", rewritten.output);
    assert_eq!(
        std::fs::read_to_string(work.path().join("nested/new.txt")).expect("read rewrite"),
        "replacement"
    );
    assert_eq!(history_files(work.path(), "write-session").len(), 1);
}

#[tokio::test]
async fn write_file_rejects_unread_and_stale_files_without_history() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "write-guard").expect("create tool context");
    let read = ReadFileTool::new();
    let write = WriteFileTool::new();
    let path = work.path().join("guarded.txt");
    std::fs::write(&path, "original").expect("write original");

    let unread = write
        .execute(
            &context,
            json!({ "file_path": "guarded.txt", "content": "changed" }),
        )
        .await;
    assert!(unread.is_error);
    assert!(unread.output.contains("has not been read"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read unchanged"),
        "original"
    );
    assert!(history_files(work.path(), "write-guard").is_empty());

    read.execute(&context, json!({ "file_path": "guarded.txt" }))
        .await;
    std::fs::write(&path, "external change").expect("modify externally");
    let stale = write
        .execute(
            &context,
            json!({ "file_path": "guarded.txt", "content": "changed" }),
        )
        .await;
    assert!(stale.is_error);
    assert!(stale.output.contains("modified"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read externally changed"),
        "external change"
    );
    assert!(history_files(work.path(), "write-guard").is_empty());
}

#[tokio::test]
async fn write_file_rejects_invalid_arguments_and_outside_paths() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "write-invalid").expect("create tool context");
    let write = WriteFileTool::new();

    let invalid = write
        .execute(&context, json!({ "file_path": "file.txt", "content": 7 }))
        .await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("content"));

    let outside = write
        .execute(
            &context,
            json!({ "file_path": "../outside.txt", "content": "no" }),
        )
        .await;
    assert!(outside.is_error);
    assert!(outside.output.contains("outside the workspace"));
}

#[tokio::test]
async fn write_file_rejects_a_file_deleted_after_it_was_read() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "write-deleted").expect("create tool context");
    let path = work.path().join("deleted.txt");
    std::fs::write(&path, "original").expect("write original");
    let read = ReadFileTool::new();
    let write = WriteFileTool::new();
    read.execute(&context, json!({ "file_path": "deleted.txt" }))
        .await;
    std::fs::remove_file(&path).expect("delete file");

    let result = write
        .execute(
            &context,
            json!({ "file_path": "deleted.txt", "content": "replacement" }),
        )
        .await;

    assert!(result.is_error);
    assert!(result.output.contains("deleted after"));
    assert!(!path.exists());
    assert!(history_files(work.path(), "write-deleted").is_empty());
}

#[tokio::test]
async fn edit_file_replaces_one_unique_match_and_updates_history() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "edit-session").expect("create tool context");
    let path = work.path().join("source.txt");
    std::fs::write(&path, "alpha\nbeta\n").expect("write source");
    let read = ReadFileTool::new();
    let edit = EditFileTool::new();
    read.execute(&context, json!({ "file_path": "source.txt" }))
        .await;

    let result = edit
        .execute(
            &context,
            json!({ "file_path": "source.txt", "old_string": "beta", "new_string": "gamma" }),
        )
        .await;
    assert!(!result.is_error, "{}", result.output);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read edited file"),
        "alpha\ngamma\n"
    );
    assert_eq!(history_files(work.path(), "edit-session").len(), 1);

    let second = edit
        .execute(
            &context,
            json!({ "file_path": "source.txt", "old_string": "gamma", "new_string": "delta" }),
        )
        .await;
    assert!(!second.is_error, "{}", second.output);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read second edit"),
        "alpha\ndelta\n"
    );
    assert_eq!(history_files(work.path(), "edit-session").len(), 2);
}

#[tokio::test]
async fn edit_file_rejects_conflicts_without_changing_file_or_history() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "edit-conflict").expect("create tool context");
    let read = ReadFileTool::new();
    let edit = EditFileTool::new();
    let path = work.path().join("conflict.txt");
    std::fs::write(&path, "one\ntwo\none\n").expect("write conflict source");
    read.execute(&context, json!({ "file_path": "conflict.txt" }))
        .await;

    let multiple = edit
        .execute(
            &context,
            json!({ "file_path": "conflict.txt", "old_string": "one", "new_string": "changed" }),
        )
        .await;
    assert!(multiple.is_error);
    assert!(multiple.output.contains("2 times"));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read unchanged"),
        "one\ntwo\none\n"
    );

    let absent = edit
        .execute(
            &context,
            json!({ "file_path": "conflict.txt", "old_string": "missing", "new_string": "changed" }),
        )
        .await;
    assert!(absent.is_error);
    assert!(absent.output.contains("not found"));
    assert!(history_files(work.path(), "edit-conflict").is_empty());
}

#[tokio::test]
async fn edit_file_rejects_missing_unread_stale_and_invalid_requests() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "edit-guards").expect("create tool context");
    let read = ReadFileTool::new();
    let edit = EditFileTool::new();

    let missing = edit
        .execute(
            &context,
            json!({ "file_path": "missing.txt", "old_string": "a", "new_string": "b" }),
        )
        .await;
    assert!(missing.is_error);
    assert!(missing.output.contains("has not been read"));

    let path = work.path().join("guarded.txt");
    std::fs::write(&path, "original").expect("write guarded");
    let unread = edit
        .execute(
            &context,
            json!({ "file_path": "guarded.txt", "old_string": "original", "new_string": "changed" }),
        )
        .await;
    assert!(unread.is_error);
    assert!(unread.output.contains("has not been read"));

    read.execute(&context, json!({ "file_path": "guarded.txt" }))
        .await;
    std::fs::write(&path, "external").expect("modify externally");
    let stale = edit
        .execute(
            &context,
            json!({ "file_path": "guarded.txt", "old_string": "external", "new_string": "changed" }),
        )
        .await;
    assert!(stale.is_error);
    assert!(stale.output.contains("modified"));

    let invalid = edit
        .execute(
            &context,
            json!({ "file_path": "guarded.txt", "old_string": 4 }),
        )
        .await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("old_string"));
    assert!(history_files(work.path(), "edit-guards").is_empty());
}
