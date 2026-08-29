use std::sync::Arc;

use mycode_tools::{
    context::ToolContext,
    registry::ToolRegistry,
    tool::{Tool, ToolCategory, ToolResult},
    tool_search::ToolSearchTool,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct DeferredExampleTool;

#[async_trait::async_trait]
impl Tool for DeferredExampleTool {
    fn name(&self) -> &'static str {
        "DeferredExample"
    }

    fn description(&self) -> &'static str {
        "Example deferred tool for discovery tests"
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
        ToolResult::success("deferred")
    }
}

#[tokio::test]
async fn tool_search_discovers_deferred_tools_and_repeats_idempotently() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context =
        ToolContext::new(work.path(), "tool-search-session").expect("create tool context");
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register(DeferredExampleTool)
        .expect("register deferred tool");
    let search = ToolSearchTool::new(Arc::downgrade(&registry));
    registry.register(search).expect("register ToolSearch");

    assert_eq!(registry.deferred_names(), ["DeferredExample"]);
    assert!(
        !registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredExample")
    );

    let discovered = registry
        .get("ToolSearch")
        .expect("ToolSearch exists")
        .execute(&context, json!({ "query": "select:DeferredExample" }))
        .await;
    assert!(!discovered.is_error, "{}", discovered.output);
    assert!(discovered.output.contains("Found 1 tool(s)"));
    assert!(discovered.output.contains("DeferredExample"));
    assert!(
        registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredExample")
    );
    assert!(registry.deferred_names().is_empty());

    let repeated = registry
        .get("ToolSearch")
        .expect("ToolSearch exists")
        .execute(&context, json!({ "query": "select:DeferredExample" }))
        .await;
    assert!(!repeated.is_error, "{}", repeated.output);
    assert!(repeated.output.contains("Found 1 tool(s)"));
    assert!(registry.deferred_names().is_empty());
}

#[tokio::test]
async fn tool_search_keyword_matching_reports_available_deferred_tools() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context =
        ToolContext::new(work.path(), "tool-search-keyword").expect("create tool context");
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register(DeferredExampleTool)
        .expect("register deferred tool");
    let search = ToolSearchTool::new(Arc::downgrade(&registry));
    registry.register(search).expect("register ToolSearch");

    let no_match = registry
        .get("ToolSearch")
        .expect("ToolSearch exists")
        .execute(&context, json!({ "query": "does-not-exist" }))
        .await;
    assert!(!no_match.is_error);
    assert!(no_match.output.contains("No matching deferred tools"));
    assert!(no_match.output.contains("DeferredExample"));
    assert_eq!(registry.deferred_names(), ["DeferredExample"]);
    assert!(
        !registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredExample")
    );

    let keyword = registry
        .get("ToolSearch")
        .expect("ToolSearch exists")
        .execute(&context, json!({ "query": "example", "max_results": 5 }))
        .await;
    assert!(!keyword.is_error, "{}", keyword.output);
    assert!(keyword.output.contains("DeferredExample"));
    assert!(
        registry
            .schemas()
            .iter()
            .any(|schema| schema["name"] == "DeferredExample")
    );
}

#[tokio::test]
async fn tool_search_rejects_invalid_arguments() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context =
        ToolContext::new(work.path(), "tool-search-invalid").expect("create tool context");
    let registry = Arc::new(ToolRegistry::new());
    let search = ToolSearchTool::new(Arc::downgrade(&registry));

    let invalid = search.execute(&context, json!({ "query": 42 })).await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("query"));
}
