use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum PermissionRuleEffect {
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

    pub(super) fn evaluate(&self, tool_name: &str, subject: &str) -> Option<PermissionRuleEffect> {
        let mut matched = None;
        for rule in &self.rules {
            if rule.tool_name == tool_name && rule.pattern.is_match(subject) {
                matched = Some(rule.effect);
            }
        }
        matched
    }
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
    let pattern = globset::Glob::new(pattern).map_err(PermissionRuleParseError::Glob)?;
    Ok(LoadedPermissionRule {
        tool_name: tool_name.to_string(),
        pattern: pattern.compile_matcher(),
        effect,
    })
}
