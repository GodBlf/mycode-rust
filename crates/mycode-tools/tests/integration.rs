use std::sync::Arc;

use mycode_core::config::Config;
use mycode_tools::{
    context::ToolContext,
    runtime::{ToolExecutor, default_checker, default_registry},
    tool::{Tool, ToolCategory, ToolResult},
};
use serde_json::{Value, json};

#[derive(Debug)]
struct DeferredIntegrationTool;

#[async_trait::async_trait]
impl Tool for DeferredIntegrationTool {
    fn name(&self) -> &'static str {
        "DeferredIntegration"
    }

    fn description(&self) -> &'static str {
        "Integration deferred tool"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": { "type": "object", "properties": {} }
        })
    }

    fn permission_argument(&self, _arguments: &Value) -> Option<String> {
        None
    }

    fn is_deferred(&self) -> bool {
        true
    }

    async fn execute(&self, _context: &ToolContext, _arguments: Value) -> ToolResult {
        ToolResult::success("integration deferred tool")
    }
}

#[tokio::test]
async fn stage_two_tools_permissions_and_persistence_work_together() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    let config_path = work.path().join(".mycode/config.yaml");
    std::fs::create_dir_all(config_path.parent().expect("config parent"))
        .expect("create config directory");
    std::fs::write(
        &config_path,
        r#"
providers:
  - name: integration-provider
    protocol: anthropic
    base_url: https://provider.example.test
    model: integration-model
permission_mode: acceptEdits
"#,
    )
    .expect("write config");

    let config = Config::load(home.path(), work.path()).expect("load config");
    let context =
        ToolContext::new(work.path(), "integration-session").expect("create tool context");
    let registry = default_registry();
    registry
        .register(DeferredIntegrationTool)
        .expect("register deferred integration tool");
    let checker = default_checker(config.permission_mode, home.path(), work.path())
        .expect("create permission checker");
    let executor = ToolExecutor::new(Arc::clone(&registry), checker);

    std::fs::write(work.path().join("notes.md"), "integration original\n")
        .expect("write initial file");
    let read = executor
        .execute(&context, "ReadFile", json!({ "file_path": "notes.md" }))
        .await;
    assert!(!read.is_error, "{}", read.output);
    assert!(read.output.contains("integration original"));

    let grep = executor
        .execute(
            &context,
            "Grep",
            json!({ "pattern": "integration", "include": "*.md" }),
        )
        .await;
    assert!(!grep.is_error, "{}", grep.output);
    assert!(grep.output.contains("notes.md:1:integration original"));

    let write = executor
        .execute(
            &context,
            "WriteFile",
            json!({ "file_path": "notes.md", "content": "alpha\nbeta\n" }),
        )
        .await;
    assert!(!write.is_error, "{}", write.output);
    assert_eq!(
        std::fs::read_to_string(work.path().join("notes.md")).expect("read written file"),
        "alpha\nbeta\n"
    );

    let edit = executor
        .execute(
            &context,
            "EditFile",
            json!({
                "file_path": "notes.md",
                "old_string": "beta",
                "new_string": "gamma"
            }),
        )
        .await;
    assert!(!edit.is_error, "{}", edit.output);
    assert_eq!(
        std::fs::read_to_string(work.path().join("notes.md")).expect("read edited file"),
        "alpha\ngamma\n"
    );

    let bash = executor
        .execute(
            &context,
            "Bash",
            json!({ "command": "echo stage-two", "timeout_seconds": 5 }),
        )
        .await;
    assert!(!bash.is_error, "{}", bash.output);
    assert!(bash.output.contains("stage-two"));

    assert!(
        !registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredIntegration")
    );
    let tool_search = executor
        .execute(
            &context,
            "ToolSearch",
            json!({ "query": "select:DeferredIntegration" }),
        )
        .await;
    assert!(!tool_search.is_error, "{}", tool_search.output);
    assert!(
        registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredIntegration")
    );

    assert!(
        work.path()
            .join(".mycode/file-history/integration-session")
            .exists()
    );
}
