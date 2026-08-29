use std::time::Duration;

use mycode_core::config::PermissionMode;
use mycode_tools::{
    bash::BashTool,
    context::ToolContext,
    permission::{PermissionChecker, PermissionRuleEngine},
    runtime::{ToolExecutor, default_registry},
    tool::Tool,
};
use serde_json::json;

#[tokio::test]
async fn bash_reports_success_failure_and_invalid_arguments() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "bash-session").expect("create tool context");
    let bash = BashTool::new();

    let success = bash
        .execute(
            &context,
            json!({ "command": "printf 'stdout'", "timeout_seconds": 5 }),
        )
        .await;
    assert!(!success.is_error, "{}", success.output);
    assert!(success.output.contains("stdout"));
    assert!(success.output.contains("exit code 0"));

    let failure = bash
        .execute(
            &context,
            json!({ "command": "printf 'stderr' >&2; exit 7", "timeout_seconds": 5 }),
        )
        .await;
    assert!(failure.is_error);
    assert!(
        failure.output.contains("STDERR: stderr"),
        "{}",
        failure.output
    );
    assert!(failure.output.contains("exit code 7"));

    let invalid = bash.execute(&context, json!({ "command": 42 })).await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("command"));
}

#[tokio::test]
async fn bash_times_out_and_cancellation_stops_execution() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "bash-timeout").expect("create tool context");
    let bash = BashTool::new();

    let timeout = tokio::time::timeout(
        Duration::from_secs(3),
        bash.execute(
            &context,
            json!({ "command": "sleep 5", "timeout_seconds": 1 }),
        ),
    )
    .await
    .expect("timeout result should return");
    assert!(timeout.is_error);
    assert!(timeout.output.contains("timed out"));

    context.cancellation().cancel();
    let cancelled = bash
        .execute(
            &context,
            json!({ "command": "sleep 5", "timeout_seconds": 10 }),
        )
        .await;
    assert!(cancelled.is_error);
    assert!(cancelled.output.contains("cancelled"));
}

#[tokio::test]
async fn permission_is_checked_before_process_creation() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "bash-permission").expect("create tool context");
    let rules_path = work.path().join("rules.yaml");
    std::fs::write(
        &rules_path,
        r#"
- rule: "Bash(touch *)"
  effect: deny
"#,
    )
    .expect("write rules");
    let rules = PermissionRuleEngine::from_paths([&rules_path]).expect("load rules");
    let checker =
        PermissionChecker::new(PermissionMode::BypassPermissions, work.path(), rules, None);
    let executor = ToolExecutor::new(default_registry(), checker);

    let denied = executor
        .execute(
            &context,
            "Bash",
            json!({ "command": "touch created.txt", "timeout_seconds": 5 }),
        )
        .await;
    assert!(denied.is_error);
    assert!(denied.output.contains("denied"));
    assert!(!work.path().join("created.txt").exists());
}
