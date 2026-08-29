use std::sync::Weak;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::ToolContext,
    registry::ToolRegistry,
    tool::PermissionSubject,
    tool::{Tool, ToolCategory, ToolResult},
};

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS: usize = 20;

#[derive(Debug, Deserialize)]
struct ToolSearchRequest {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug)]
pub struct ToolSearchTool {
    registry: Weak<ToolRegistry>,
}

impl ToolSearchTool {
    pub fn new(registry: Weak<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &'static str {
        "ToolSearch"
    }

    fn description(&self) -> &'static str {
        "Searches for and loads deferred Tools that are not initially visible."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword search or select:ToolName,OtherTool"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum keyword-search results",
                        "default": DEFAULT_MAX_RESULTS
                    }
                },
                "required": ["query"]
            }
        })
    }

    fn permission_subject(&self, arguments: &Value) -> Option<PermissionSubject> {
        arguments
            .get("query")?
            .as_str()
            .map(|query| PermissionSubject::Query(query.to_string()))
    }

    async fn execute(&self, _context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, ToolSearchRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid ToolSearch arguments: {source}"));
            }
        };
        let Some(registry) = self.registry.upgrade() else {
            return ToolResult::error("the Tool Registry is no longer available");
        };
        let max_results = request
            .max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS);

        let tools = if let Some(selection) = request.query.strip_prefix("select:") {
            let names = selection
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            registry.find_deferred_by_names(&names)
        } else {
            registry.search_deferred(&request.query, max_results)
        };

        if tools.is_empty() {
            let deferred_names = registry.deferred_names().join(", ");
            if deferred_names.is_empty() {
                return ToolResult::success(format!(
                    "No deferred tools are available for query {:?}.",
                    request.query
                ));
            }
            return ToolResult::success(format!(
                "No matching deferred tools were found for query {:?}. Available deferred tools: {deferred_names}",
                request.query
            ));
        }

        for tool in &tools {
            registry.mark_discovered(tool.name());
        }
        let schemas = tools.iter().map(|tool| tool.schema()).collect::<Vec<_>>();
        match serde_json::to_string_pretty(&schemas) {
            Ok(rendered) => ToolResult::success(format!(
                "Found {} tool(s). Their schemas are loaded for subsequent requests:\n{rendered}",
                tools.len()
            )),
            Err(source) => {
                ToolResult::error(format!("failed to render ToolSearch results: {source}"))
            }
        }
    }
}
