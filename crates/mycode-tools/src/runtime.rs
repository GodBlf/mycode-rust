use std::sync::Arc;

use mycode_core::config::PermissionMode;

use crate::{
    bash::BashTool,
    context::ToolContext,
    edit_file::EditFileTool,
    glob::GlobTool,
    grep::GrepTool,
    permission::{
        PermissionChecker, PermissionDecision, PermissionDecisionEffect, PermissionRuleEngine,
    },
    read_file::ReadFileTool,
    registry::ToolRegistry,
    tool::ToolResult,
    tool_search::ToolSearchTool,
    write_file::WriteFileTool,
};

pub fn default_registry() -> Arc<ToolRegistry> {
    let registry = ToolRegistry::new();
    registry
        .register(ReadFileTool::new())
        .expect("default tool names are unique");
    registry
        .register(WriteFileTool::new())
        .expect("default tool names are unique");
    registry
        .register(EditFileTool::new())
        .expect("default tool names are unique");
    registry
        .register(BashTool::new())
        .expect("default tool names are unique");
    registry
        .register(GlobTool::new())
        .expect("default tool names are unique");
    registry
        .register(GrepTool::new())
        .expect("default tool names are unique");

    let registry = Arc::new(registry);
    let tool_search = ToolSearchTool::new(Arc::downgrade(&registry));
    registry
        .register(tool_search)
        .expect("ToolSearch is unique");
    registry
}

pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    checker: PermissionChecker,
}

impl ToolExecutor {
    pub fn new(registry: Arc<ToolRegistry>, checker: PermissionChecker) -> Self {
        Self { registry, checker }
    }

    pub fn registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.registry)
    }

    pub fn permission_decision(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<PermissionDecision> {
        self.registry
            .get(tool_name)
            .map(|tool| self.checker.decision(tool.as_ref(), arguments))
    }

    pub async fn execute(
        &self,
        context: &ToolContext,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> ToolResult {
        let Some(tool) = self.lookup_tool(tool_name) else {
            return ToolResult::error(format!("unknown tool: {tool_name}"));
        };
        let decision = self.checker.decision(tool.as_ref(), &arguments);
        match decision.effect {
            PermissionDecisionEffect::Allow => tool.execute(context, arguments).await,
            PermissionDecisionEffect::Deny => {
                ToolResult::error(format!("tool denied: {}", decision.reason))
            }
            PermissionDecisionEffect::Ask => ToolResult::error(format!(
                "tool requires user confirmation: {}",
                decision.reason
            )),
        }
    }

    pub async fn execute_with_permission_decision(
        &self,
        context: &ToolContext,
        tool_name: &str,
        arguments: serde_json::Value,
        permission_decision: PermissionDecision,
    ) -> ToolResult {
        if permission_decision.effect != PermissionDecisionEffect::Allow {
            return ToolResult::error(format!(
                "tool Permission Decision is not allow: {}",
                permission_decision.reason
            ));
        }
        let Some(tool) = self.lookup_tool(tool_name) else {
            return ToolResult::error(format!("unknown tool: {tool_name}"));
        };
        tool.execute(context, arguments).await
    }

    fn lookup_tool(&self, tool_name: &str) -> Option<Arc<dyn crate::tool::Tool>> {
        self.registry.get(tool_name)
    }
}

pub fn default_checker(
    mode: PermissionMode,
    home_dir: impl AsRef<std::path::Path>,
    work_dir: impl AsRef<std::path::Path>,
) -> Result<PermissionChecker, crate::permission::PermissionError> {
    let home_dir = home_dir.as_ref();
    let work_dir = work_dir.as_ref();
    let workspace = mycode_core::workspace::WorkspacePaths::new(work_dir);
    let user_rules = mycode_core::workspace::WorkspacePaths::home_permissions_file(home_dir);
    let project_rules = workspace.permissions_file();
    let local_rules = workspace.local_permissions_file();
    let rules = PermissionRuleEngine::from_paths([&user_rules, &project_rules, &local_rules])?;
    Ok(PermissionChecker::new(mode, work_dir, rules, None))
}
