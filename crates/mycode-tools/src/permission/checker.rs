use std::{fs, path::PathBuf};

use mycode_core::config::PermissionMode;
use serde_json::Value;

use super::{
    PathSandbox, PermissionDecision, PermissionDecisionEffect, PermissionRuleEngine,
    command_safety, rules::PermissionRuleEffect,
};
use crate::tool::{PermissionSubject, Tool, ToolCategory};

#[derive(Debug)]
pub struct PermissionChecker {
    sandbox: PathSandbox,
    rules: PermissionRuleEngine,
    mode: PermissionMode,
    plan_file_path: Option<PathBuf>,
    workspace_root: PathBuf,
}

impl PermissionChecker {
    pub fn new(
        mode: PermissionMode,
        workspace_root: impl AsRef<std::path::Path>,
        rules: PermissionRuleEngine,
        plan_file_path: Option<&std::path::Path>,
    ) -> Self {
        let workspace_root = fs::canonicalize(&workspace_root)
            .unwrap_or_else(|_| workspace_root.as_ref().to_path_buf());
        Self {
            sandbox: PathSandbox::new(&workspace_root),
            rules,
            mode,
            plan_file_path: plan_file_path
                .map(|path| crate::context::normalize_path(path, &workspace_root)),
            workspace_root,
        }
    }

    pub fn decision(&self, tool: &dyn Tool, arguments: &Value) -> PermissionDecision {
        let category = tool.category();
        let subject = tool.permission_subject(arguments);

        if let Some(PermissionSubject::Command(command)) = subject.as_ref()
            && command_safety::is_safe_command(command)
        {
            return self.allow("safe read-only command");
        }
        if let Some(PermissionSubject::Command(command)) = subject.as_ref()
            && let Some(reason) = command_safety::detect_dangerous_command(command)
        {
            return self.deny(format!("dangerous command blocked: {reason}"));
        }

        if let Some(path) = subject.as_ref().and_then(permission_path)
            && let Err(reason) = self.sandbox.check(path)
        {
            return self.deny(format!("path sandbox: {reason}"));
        }

        if self.mode == PermissionMode::Plan
            && category == ToolCategory::Write
            && let Some(PermissionSubject::Path(path)) = subject.as_ref()
            && self.is_plan_file(path)
        {
            return self.allow("Plan mode: selected Plan file write allowed");
        }

        if let Some(subject) = subject.as_ref()
            && let Some(effect) = self.rules.evaluate(tool.name(), subject.as_str())
        {
            return match effect {
                PermissionRuleEffect::Allow => self.allow("permission rule: allow"),
                PermissionRuleEffect::Deny => self.deny("permission rule: deny"),
            };
        }

        match mode_decision(self.mode, category) {
            PermissionDecisionEffect::Allow => {
                self.allow(format!("permission mode {}: allow", mode_name(self.mode)))
            }
            PermissionDecisionEffect::Deny => {
                self.deny(format!("permission mode {}: deny", mode_name(self.mode)))
            }
            PermissionDecisionEffect::Ask => self.ask("user confirmation required"),
        }
    }

    fn is_plan_file(&self, target: &str) -> bool {
        self.plan_file_path.as_deref().is_some_and(|plan_path| {
            crate::context::normalize_path(std::path::Path::new(target), &self.workspace_root)
                == plan_path
        })
    }

    fn allow(&self, reason: impl Into<String>) -> PermissionDecision {
        PermissionDecision {
            effect: PermissionDecisionEffect::Allow,
            reason: reason.into(),
        }
    }

    fn deny(&self, reason: impl Into<String>) -> PermissionDecision {
        PermissionDecision {
            effect: PermissionDecisionEffect::Deny,
            reason: reason.into(),
        }
    }

    fn ask(&self, reason: impl Into<String>) -> PermissionDecision {
        PermissionDecision {
            effect: PermissionDecisionEffect::Ask,
            reason: reason.into(),
        }
    }
}

fn permission_path(subject: &PermissionSubject) -> Option<&str> {
    match subject {
        PermissionSubject::Path(path) => Some(path),
        PermissionSubject::Search {
            path: Some(path), ..
        } => Some(path),
        PermissionSubject::Search { path: None, .. } => Some("."),
        PermissionSubject::Command(_) | PermissionSubject::Query(_) => None,
    }
}

pub fn mode_decision(mode: PermissionMode, category: ToolCategory) -> PermissionDecisionEffect {
    match (mode, category) {
        (PermissionMode::BypassPermissions, _) => PermissionDecisionEffect::Allow,
        (PermissionMode::AcceptEdits, ToolCategory::Read | ToolCategory::Write) => {
            PermissionDecisionEffect::Allow
        }
        (PermissionMode::AcceptEdits, ToolCategory::Command)
        | (PermissionMode::Default, ToolCategory::Write | ToolCategory::Command)
        | (PermissionMode::Plan, ToolCategory::Write | ToolCategory::Command) => {
            PermissionDecisionEffect::Ask
        }
        (_, ToolCategory::Read) => PermissionDecisionEffect::Allow,
    }
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::BypassPermissions => "bypassPermissions",
    }
}
