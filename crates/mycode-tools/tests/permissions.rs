use mycode_core::config::PermissionMode;
use mycode_tools::permission::{PermissionChecker, PermissionDecisionEffect, PermissionRuleEngine};
use mycode_tools::runtime::default_registry;
use serde_json::json;

fn checker(mode: PermissionMode, rules: &str) -> PermissionChecker {
    let work = tempfile::tempdir().expect("work tempdir");
    let rules_path = work.path().join("rules.yaml");
    std::fs::write(&rules_path, rules).expect("write rules");
    let engine = PermissionRuleEngine::from_paths([&rules_path]).expect("load rules");
    PermissionChecker::new(mode, work.path(), engine, None)
}

fn decision(
    mode: PermissionMode,
    rules: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> PermissionDecisionEffect {
    let registry = default_registry();
    let tool = registry.get(tool_name).expect("tool exists");
    checker(mode, rules)
        .decision(tool.as_ref(), arguments)
        .effect
}

#[test]
fn permission_modes_control_categories() {
    let empty = "";
    let cases = [
        (
            PermissionMode::Default,
            "ReadFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::Default,
            "WriteFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Ask,
        ),
        (
            PermissionMode::Default,
            "Bash",
            json!({ "command": "custom-command" }),
            PermissionDecisionEffect::Ask,
        ),
        (
            PermissionMode::AcceptEdits,
            "ReadFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::AcceptEdits,
            "WriteFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::AcceptEdits,
            "Bash",
            json!({ "command": "custom-command" }),
            PermissionDecisionEffect::Ask,
        ),
        (
            PermissionMode::Plan,
            "ReadFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::Plan,
            "WriteFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Ask,
        ),
        (
            PermissionMode::Plan,
            "Bash",
            json!({ "command": "custom-command" }),
            PermissionDecisionEffect::Ask,
        ),
        (
            PermissionMode::BypassPermissions,
            "ReadFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::BypassPermissions,
            "WriteFile",
            json!({ "file_path": "file" }),
            PermissionDecisionEffect::Allow,
        ),
        (
            PermissionMode::BypassPermissions,
            "Bash",
            json!({ "command": "custom-command" }),
            PermissionDecisionEffect::Allow,
        ),
    ];

    for (mode, tool_name, arguments, expected) in cases {
        assert_eq!(
            decision(mode, empty, tool_name, &arguments),
            expected,
            "{tool_name} in {mode:?}"
        );
    }
}

#[test]
fn later_permission_rules_win_and_paths_stay_sandboxed() {
    let rules = r#"
- rule: "Bash(cargo *)"
  effect: deny
- rule: "Bash(cargo test*)"
  effect: allow
"#;
    assert_eq!(
        decision(
            PermissionMode::Default,
            rules,
            "Bash",
            &json!({ "command": "cargo test" })
        ),
        PermissionDecisionEffect::Allow
    );
    assert_eq!(
        decision(
            PermissionMode::BypassPermissions,
            rules,
            "ReadFile",
            &json!({ "file_path": "../outside.txt" })
        ),
        PermissionDecisionEffect::Deny
    );
    assert_eq!(
        decision(
            PermissionMode::Default,
            "",
            "Bash",
            &json!({ "command": "find . -delete" })
        ),
        PermissionDecisionEffect::Ask
    );
}

#[test]
fn plan_file_exception_does_not_bypass_the_path_sandbox() {
    let work = tempfile::tempdir().expect("work tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let plan_path = outside.path().join("plan.md");
    let checker = PermissionChecker::new(
        PermissionMode::Plan,
        work.path(),
        PermissionRuleEngine::default(),
        Some(&plan_path),
    );
    let registry = default_registry();
    let write = registry.get("WriteFile").expect("WriteFile exists");

    let decision = checker.decision(write.as_ref(), &json!({ "file_path": plan_path }));

    assert_eq!(decision.effect, PermissionDecisionEffect::Deny);
    assert!(decision.reason.contains("path sandbox"));
}

#[test]
fn dangerous_commands_are_denied_and_safe_commands_are_allowed() {
    assert_eq!(
        decision(
            PermissionMode::BypassPermissions,
            "",
            "Bash",
            &json!({ "command": "rm -rf /" })
        ),
        PermissionDecisionEffect::Deny
    );
    assert_eq!(
        decision(
            PermissionMode::Default,
            "",
            "Bash",
            &json!({ "command": "git status" })
        ),
        PermissionDecisionEffect::Allow
    );
}

#[test]
fn plan_mode_allows_only_the_selected_plan_file() {
    let work = tempfile::tempdir().expect("work tempdir");
    let plan_path = work.path().join(".mycode/plans/example.md");
    let engine = PermissionRuleEngine::default();
    let checker =
        PermissionChecker::new(PermissionMode::Plan, work.path(), engine, Some(&plan_path));
    let registry = default_registry();
    let write = registry.get("WriteFile").expect("WriteFile exists");

    let allowed = checker.decision(
        write.as_ref(),
        &json!({ "file_path": ".mycode/plans/example.md" }),
    );
    let denied = checker.decision(write.as_ref(), &json!({ "file_path": "source.rs" }));

    assert_eq!(allowed.effect, PermissionDecisionEffect::Allow);
    assert_eq!(denied.effect, PermissionDecisionEffect::Ask);
}
