use mycode_tools::context::ToolContext;
use mycode_tools::read_file::ReadFileTool;
use mycode_tools::registry::ToolRegistry;
use mycode_tools::tool::{Tool, ToolCategory};
use serde_json::json;

#[tokio::test]
async fn read_file_returns_numbered_content_and_registers_deterministically() {
    let work = tempfile::tempdir().expect("work tempdir");
    let file = work.path().join("notes.txt");
    std::fs::write(&file, "first\nsecond").expect("write file");

    let context = mycode_tools::context::ToolContext::new(work.path(), "session-1")
        .expect("create tool context");
    let tool = ReadFileTool::new();
    let result = tool
        .execute(&context, json!({ "file_path": "notes.txt" }))
        .await;

    assert!(!result.is_error, "{}", result.output);
    assert_eq!(result.output, "1\tfirst\n2\tsecond");
    assert_eq!(tool.name(), "ReadFile");
    assert_eq!(tool.category(), ToolCategory::Read);
    assert_eq!(tool.schema()["name"], "ReadFile");

    let registry = ToolRegistry::new();
    registry.register(tool).expect("register ReadFile");
    let names: Vec<_> = registry.list().iter().map(|tool| tool.name()).collect();
    assert_eq!(names, ["ReadFile"]);
}

#[tokio::test]
async fn read_file_rejects_invalid_arguments_and_missing_files() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "session-1").expect("create tool context");
    let tool = ReadFileTool::new();

    let invalid = tool.execute(&context, json!({ "file_path": 42 })).await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("file_path"), "{}", invalid.output);

    let missing = tool
        .execute(&context, json!({ "file_path": "missing.txt" }))
        .await;
    assert!(missing.is_error);
    assert!(missing.output.contains("missing.txt"));
}
