use mycode_core::config::PermissionMode;
use mycode_tools::{
    context::ToolContext,
    glob::GlobTool,
    grep::GrepTool,
    permission::{PermissionChecker, PermissionRuleEngine},
    runtime::{ToolExecutor, default_registry},
    tool::Tool,
};
use serde_json::json;

#[tokio::test]
async fn glob_matches_recursive_patterns_and_skips_ignored_directories() {
    let work = tempfile::tempdir().expect("work tempdir");
    std::fs::create_dir_all(work.path().join("src/nested")).expect("create src");
    std::fs::create_dir_all(work.path().join("node_modules")).expect("create node_modules");
    std::fs::write(work.path().join("src/top.rs"), "top").expect("write top");
    std::fs::write(work.path().join("src/nested/deep.rs"), "deep").expect("write deep");
    std::fs::write(work.path().join("node_modules/ignored.rs"), "ignored").expect("write ignored");
    let context = ToolContext::new(work.path(), "glob-session").expect("create tool context");
    let glob = GlobTool::new();

    let recursive = glob
        .execute(&context, json!({ "pattern": "**/*.rs" }))
        .await;
    assert!(!recursive.is_error, "{}", recursive.output);
    assert_eq!(recursive.output, "src/nested/deep.rs\nsrc/top.rs");

    let plain = glob
        .execute(&context, json!({ "pattern": "top.rs", "path": "src" }))
        .await;
    assert!(!plain.is_error, "{}", plain.output);
    assert_eq!(plain.output, "top.rs");
}

#[tokio::test]
async fn glob_rejects_invalid_arguments_patterns_and_outside_paths() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "glob-invalid").expect("create tool context");
    let glob = GlobTool::new();

    let invalid = glob.execute(&context, json!({ "pattern": 42 })).await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("pattern"));

    let bad_pattern = glob
        .execute(&context, json!({ "pattern": "[invalid" }))
        .await;
    assert!(bad_pattern.is_error);
    assert!(bad_pattern.output.contains("invalid Glob pattern"));

    let outside = glob
        .execute(&context, json!({ "pattern": "*.rs", "path": "../outside" }))
        .await;
    assert!(outside.is_error);
    assert!(outside.output.contains("outside the workspace"));

    let zero_limit = glob
        .execute(&context, json!({ "pattern": "*.rs", "limit": 0 }))
        .await;
    assert!(zero_limit.is_error);
    assert!(zero_limit.output.contains("limit"));
}

#[tokio::test]
async fn grep_matches_regex_lines_and_respects_limits_and_ignores() {
    let work = tempfile::tempdir().expect("work tempdir");
    std::fs::create_dir_all(work.path().join("src")).expect("create src");
    std::fs::create_dir_all(work.path().join(".git")).expect("create .git");
    std::fs::write(work.path().join("src/a.rs"), "alpha\nbeta\n").expect("write a");
    std::fs::write(work.path().join("src/b.txt"), "gamma alpha\n").expect("write b");
    std::fs::write(work.path().join(".git/ignored.rs"), "alpha ignored\n").expect("write ignored");
    let context = ToolContext::new(work.path(), "grep-session").expect("create tool context");
    let grep = GrepTool::new();

    let rust = grep
        .execute(&context, json!({ "pattern": "alpha", "include": "*.rs" }))
        .await;
    assert!(!rust.is_error, "{}", rust.output);
    assert_eq!(rust.output, "src/a.rs:1:alpha");

    let limited = grep
        .execute(&context, json!({ "pattern": "alpha", "limit": 1 }))
        .await;
    assert!(!limited.is_error, "{}", limited.output);
    assert_eq!(limited.output, "src/a.rs:1:alpha");

    let none = grep
        .execute(&context, json!({ "pattern": "missing-value" }))
        .await;
    assert!(!none.is_error);
    assert_eq!(none.output, "No matches found.");
}

#[tokio::test]
async fn grep_rejects_invalid_arguments_regex_and_outside_paths() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "grep-invalid").expect("create tool context");
    let grep = GrepTool::new();

    let invalid = grep.execute(&context, json!({ "pattern": 42 })).await;
    assert!(invalid.is_error, "{}", invalid.output);
    assert!(invalid.output.contains("pattern"));

    let bad_regex = grep
        .execute(&context, json!({ "pattern": "[invalid" }))
        .await;
    assert!(bad_regex.is_error);
    assert!(bad_regex.output.contains("invalid Grep regular expression"));

    let outside = grep
        .execute(
            &context,
            json!({ "pattern": "value", "path": "../outside" }),
        )
        .await;
    assert!(outside.is_error);
    assert!(outside.output.contains("outside the workspace"));
}

#[tokio::test]
async fn search_permission_denies_paths_outside_the_workspace() {
    let work = tempfile::tempdir().expect("work tempdir");
    let context = ToolContext::new(work.path(), "search-permission").expect("create tool context");
    let checker = PermissionChecker::new(
        PermissionMode::BypassPermissions,
        work.path(),
        PermissionRuleEngine::default(),
        None,
    );
    let executor = ToolExecutor::new(default_registry(), checker);

    let denied = executor
        .execute(
            &context,
            "Grep",
            json!({ "pattern": "value", "path": "../outside" }),
        )
        .await;
    assert!(denied.is_error);
    assert!(denied.output.contains("path sandbox"));
}
