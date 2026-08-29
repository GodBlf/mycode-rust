use std::{
    fs,
    path::{Path, PathBuf},
};

use globset::Glob;
use mycode_core::config::PermissionMode;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    context::normalize_path,
    tool::{Tool, ToolCategory},
};

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error("failed to read permission rules from {path}")]
    ReadRules {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse permission rules in {path}")]
    ParseRules {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid permission rule {rule:?}")]
    InvalidRule {
        rule: String,
        #[source]
        source: PermissionRuleParseError,
    },
}

#[derive(Debug, Error)]
pub enum PermissionRuleParseError {
    #[error("expected ToolName(pattern)")]
    Syntax,
    #[error("invalid glob pattern")]
    Glob(#[source] globset::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecisionEffect {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub effect: PermissionDecisionEffect,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PermissionRuleEffect {
    Allow,
    Deny,
}

#[derive(Debug, Deserialize)]
struct PermissionRuleEntry {
    rule: String,
    effect: PermissionRuleEffect,
}

#[derive(Debug)]
struct LoadedPermissionRule {
    tool_name: String,
    pattern: globset::GlobMatcher,
    effect: PermissionRuleEffect,
}

#[derive(Debug, Default)]
pub struct PermissionRuleEngine {
    rules: Vec<LoadedPermissionRule>,
}

impl PermissionRuleEngine {
    pub fn from_paths<I, P>(paths: I) -> Result<Self, PermissionError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut rules = Vec::new();
        for path in paths {
            let path = path.as_ref();
            if !path.exists() {
                continue;
            }
            let contents =
                fs::read_to_string(path).map_err(|source| PermissionError::ReadRules {
                    path: path.to_path_buf(),
                    source,
                })?;
            if contents.trim().is_empty() {
                continue;
            }
            let entries =
                serde_yaml::from_str::<Vec<PermissionRuleEntry>>(&contents).map_err(|source| {
                    PermissionError::ParseRules {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            for entry in entries {
                let rule = parse_rule(&entry.rule, entry.effect).map_err(|source| {
                    PermissionError::InvalidRule {
                        rule: entry.rule,
                        source,
                    }
                })?;
                rules.push(rule);
            }
        }
        Ok(Self { rules })
    }

    fn evaluate(&self, tool_name: &str, content: &str) -> Option<PermissionRuleEffect> {
        let mut matched = None;
        for rule in &self.rules {
            if rule.tool_name == tool_name && rule.pattern.is_match(content) {
                matched = Some(rule.effect);
            }
        }
        matched
    }
}

#[derive(Debug)]
pub struct PathSandbox {
    workspace_root: PathBuf,
    allowed_roots: Vec<PathBuf>,
}

impl PathSandbox {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        let root = workspace_root.as_ref();
        let workspace_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let allowed_roots = vec![workspace_root.clone()];
        Self {
            workspace_root,
            allowed_roots,
        }
    }

    pub fn check(&self, path: &str) -> Result<(), String> {
        let normalized = normalize_path(Path::new(path), &self.workspace_root);
        if self
            .allowed_roots
            .iter()
            .any(|root| normalized.strip_prefix(root).is_ok())
        {
            Ok(())
        } else {
            Err(format!(
                "path {path} is outside the permitted workspace roots"
            ))
        }
    }
}

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
        workspace_root: impl AsRef<Path>,
        rules: PermissionRuleEngine,
        plan_file_path: Option<&Path>,
    ) -> Self {
        let workspace_root = fs::canonicalize(&workspace_root)
            .unwrap_or_else(|_| workspace_root.as_ref().to_path_buf());
        Self {
            sandbox: PathSandbox::new(&workspace_root),
            rules,
            mode,
            plan_file_path: plan_file_path.map(|path| normalize_path(path, &workspace_root)),
            workspace_root,
        }
    }

    pub fn decision(&self, tool: &dyn Tool, arguments: &Value) -> PermissionDecision {
        let category = tool.category();
        let content = tool.permission_argument(arguments);

        if category == ToolCategory::Command
            && let Some(command) = content.as_deref()
        {
            if is_safe_command(command) {
                return self.allow("safe read-only command");
            }
            if let Some(reason) = detect_dangerous_command(command) {
                return self.deny(format!("dangerous command blocked: {reason}"));
            }
        }

        if matches!(category, ToolCategory::Read | ToolCategory::Write)
            && let Some(path) = content.as_deref()
            && let Err(reason) = self.sandbox.check(path)
        {
            return self.deny(format!("path sandbox: {reason}"));
        }

        if self.mode == PermissionMode::Plan
            && category == ToolCategory::Write
            && content
                .as_deref()
                .is_some_and(|path| self.is_plan_file(path))
        {
            return self.allow("Plan mode: selected Plan file write allowed");
        }

        if let Some(content) = content.as_deref()
            && let Some(effect) = self.rules.evaluate(tool.name(), content)
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
            normalize_path(Path::new(target), &self.workspace_root) == plan_path
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

pub fn is_safe_command(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty()
        || command.contains('\n')
        || ['>', '|', ';', '`']
            .iter()
            .any(|character| command.contains(*character))
        || command.contains("&&")
        || command.contains("$(")
    {
        return false;
    }

    if command
        .split_whitespace()
        .any(|argument| argument == "-delete" || argument == "--delete" || argument == "--force")
    {
        return false;
    }

    const SAFE_PREFIXES: &[&str] = &[
        "ls",
        "dir",
        "pwd",
        "echo",
        "cat",
        "head",
        "tail",
        "wc",
        "which",
        "whereis",
        "whoami",
        "hostname",
        "uname",
        "date",
        "cal",
        "uptime",
        "df",
        "du",
        "free",
        "env",
        "printenv",
        "file",
        "stat",
        "readlink",
        "realpath",
        "basename",
        "dirname",
        "sort",
        "uniq",
        "tr",
        "cut",
        "diff",
        "comm",
        "true",
        "false",
        "test",
        "git status",
        "git log",
        "git diff",
        "git show",
        "git branch",
        "git tag",
        "git remote",
        "git rev-parse",
        "git ls-files",
        "git blame",
        "git stash list",
        "go version",
        "go env",
        "node -v",
        "npm -v",
        "python --version",
        "pip list",
        "cargo --version",
        "rustc --version",
    ];
    SAFE_PREFIXES.iter().any(|prefix| {
        command == *prefix
            || (command.starts_with(prefix)
                && command[prefix.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace))
    })
}

pub fn detect_dangerous_command(command: &str) -> Option<&'static str> {
    const DANGEROUS: &[(&str, &str)] = &[
        (
            r"rm\s+(-[a-z]*r[a-z]*f[a-z]*|-[a-z]*f[a-z]*r[a-z]*)\s+/\s*$",
            "recursive force delete of root",
        ),
        (r"mkfs\.", "format disk"),
        (r"dd\s+if=.*of=/dev/", "direct write to a disk device"),
        (r"chmod\s+-R\s+777\s+/", "recursive root permission change"),
        (r":\(\)\{\s*:\|:&\s*\};:", "fork bomb"),
        (
            r"curl\s+.*\|\s*(ba)?sh",
            "pipe a remote script into a shell",
        ),
        (
            r"wget\s+.*\|\s*(ba)?sh",
            "pipe a remote script into a shell",
        ),
        (r">\s*/dev/sd", "overwrite a disk device"),
    ];
    for (pattern, reason) in DANGEROUS {
        if Regex::new(pattern)
            .expect("dangerous command regex")
            .is_match(command)
        {
            return Some(reason);
        }
    }
    None
}

fn parse_rule(
    rule: &str,
    effect: PermissionRuleEffect,
) -> Result<LoadedPermissionRule, PermissionRuleParseError> {
    let rule = rule.trim();
    let open = rule.find('(').ok_or(PermissionRuleParseError::Syntax)?;
    let close = rule.rfind(')').ok_or(PermissionRuleParseError::Syntax)?;
    if close <= open || !rule.ends_with(')') {
        return Err(PermissionRuleParseError::Syntax);
    }
    let tool_name = &rule[..open];
    let pattern = &rule[open + 1..close];
    if tool_name.is_empty()
        || !tool_name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        || pattern.is_empty()
    {
        return Err(PermissionRuleParseError::Syntax);
    }
    let pattern = Glob::new(pattern).map_err(PermissionRuleParseError::Glob)?;
    Ok(LoadedPermissionRule {
        tool_name: tool_name.to_string(),
        pattern: pattern.compile_matcher(),
        effect,
    })
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::Plan => "plan",
        PermissionMode::BypassPermissions => "bypassPermissions",
    }
}
