mod checker;
mod command_safety;
mod rules;
mod sandbox;

pub use checker::{PermissionChecker, mode_decision};
pub use command_safety::{detect_dangerous_command, is_safe_command};
pub use rules::{PermissionError, PermissionRuleEngine, PermissionRuleParseError};
pub use sandbox::PathSandbox;

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
