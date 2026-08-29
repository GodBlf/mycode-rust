use std::error::Error as ErrorTrait;
use std::fs;
use std::path::Path;

use mycode_core::config::{
    Config, HookAction, HookEvent, McpTransport, PermissionMode, ProviderProtocol,
};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("config has a parent")).expect("create config dir");
    fs::write(path, contents).expect("write config");
}

fn write_provider_config(work: &Path, provider_yaml: &str) {
    let normalized_provider = provider_yaml
        .trim()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n    ");
    write(
        &work.join(".mycode/config.yaml"),
        &format!("providers:\n  - {normalized_provider}\n"),
    );
}

#[test]
fn config_loads_and_merges_layers_with_defined_precedence() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    write(
        &home.path().join(".mycode/config.yaml"),
        r#"
providers:
  - name: home-provider
    protocol: anthropic
    base_url: https://home.example.test
    model: home-model
permission_mode: default
mcp_servers:
  - name: search
    transport: stdio
    command: home-search
hooks:
  - id: home-hook
    event: session_start
    action:
      type: prompt
      message: home
"#,
    );
    write(
        &work.path().join(".mycode/config.yaml"),
        r#"
providers:
  - name: project-provider
    protocol: openai
    base_url: https://project.example.test
    model: project-model
permission_mode: acceptEdits
mcp_servers:
  - name: search
    transport: http
    url: https://project-search.example.test
hooks:
  - id: project-hook
    event: turn_start
    action:
      type: command
      command: project-hook
"#,
    );
    write(
        &work.path().join(".mycode/config.local.yaml"),
        r#"
providers:
  - name: local-provider
    protocol: openai-compat
    base_url: https://local.example.test
    model: local-model
permission_mode: plan
mcp_servers:
  - name: local-search
    transport: stdio
    command: local-search
hooks:
  - id: local-hook
    event: shutdown
    action:
      type: http
      url: https://local-hook.example.test
unknown_field: ignored
"#,
    );

    let config = Config::load(home.path(), work.path()).expect("config should load");

    assert_eq!(config.providers.len(), 1);
    assert_eq!(config.providers[0].name, "local-provider");
    assert_eq!(config.providers[0].protocol, ProviderProtocol::OpenAiCompat);
    assert_eq!(config.permission_mode, PermissionMode::Plan);
    assert_eq!(config.mcp_servers.len(), 2);
    assert_eq!(config.mcp_servers[0].name, "search");
    assert_eq!(config.mcp_servers[0].transport, McpTransport::Http);
    assert_eq!(config.mcp_servers[1].name, "local-search");
    assert_eq!(config.hooks.len(), 3);
    assert_eq!(config.hooks[2].event, HookEvent::Shutdown);
    assert!(matches!(config.hooks[2].action, HookAction::Http { .. }));
}

#[test]
fn invalid_yaml_and_provider_settings_fail_with_config_errors() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    write(&work.path().join(".mycode/config.yaml"), "providers: [");

    let error = Config::load(home.path(), work.path())
        .expect_err("invalid YAML should fail")
        .to_string();
    assert!(
        error.contains("config.yaml"),
        "error should name the file: {error}"
    );
    assert!(
        !error.contains("EOF while parsing"),
        "error should not leak parser details: {error}"
    );
    assert!(
        Config::load(home.path(), work.path())
            .expect_err("invalid YAML should fail")
            .source()
            .is_some(),
        "parser source should remain available to the CLI boundary"
    );

    write(
        &work.path().join(".mycode/config.yaml"),
        r#"
providers:
  - name: ""
    protocol: invalid
    base_url: https://provider.example.test
    model: model
"#,
    );
    let error = Config::load(home.path(), work.path())
        .expect_err("invalid provider should fail")
        .to_string();
    assert!(
        error.contains("provider"),
        "error should identify provider: {error}"
    );
    assert!(
        error.contains("protocol"),
        "error should identify the invalid field: {error}"
    );
}

#[test]
fn unreadable_config_errors_do_not_leak_io_details() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    let config_path = work.path().join(".mycode/config.yaml");
    fs::create_dir_all(&config_path).expect("create config path as a directory");

    let error = Config::load(home.path(), work.path())
        .expect_err("unreadable config should fail")
        .to_string();

    assert!(
        error.contains("failed to read config file"),
        "error should describe the domain failure: {error}"
    );
    assert!(
        !error.contains("Is a directory"),
        "error should not leak IO details: {error}"
    );
    assert!(
        Config::load(home.path(), work.path())
            .expect_err("unreadable config should fail")
            .source()
            .is_some(),
        "IO source should remain available to the CLI boundary"
    );
}

#[test]
fn missing_provider_fields_are_reported_individually() {
    let cases = [
        (
            r#"
  protocol: anthropic
  base_url: https://provider.example.test
  model: model
"#,
            "name",
        ),
        (
            r#"
  name: provider
  base_url: https://provider.example.test
  model: model
"#,
            "protocol",
        ),
        (
            r#"
  name: provider
  protocol: anthropic
  model: model
"#,
            "base_url",
        ),
        (
            r#"
  name: provider
  protocol: anthropic
  base_url: https://provider.example.test
"#,
            "model",
        ),
    ];

    for (provider_yaml, field) in cases {
        let home = tempfile::tempdir().expect("home tempdir");
        let work = tempfile::tempdir().expect("work tempdir");
        write_provider_config(work.path(), provider_yaml.trim());

        let error = Config::load(home.path(), work.path())
            .expect_err("missing provider field should fail")
            .to_string();
        assert!(
            error.contains("provider 1"),
            "error should identify the provider: {error}"
        );
        assert!(
            error.contains(field),
            "error should identify missing field {field}: {error}"
        );
    }
}

#[test]
fn invalid_permission_mode_fails() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    write(
        &work.path().join(".mycode/config.yaml"),
        r#"
providers:
  - name: provider
    protocol: anthropic
    base_url: https://provider.example.test
    model: model
permission_mode: invalid
"#,
    );

    let error = Config::load(home.path(), work.path())
        .expect_err("invalid permission mode should fail")
        .to_string();
    assert!(
        error.contains("permission_mode"),
        "error should identify the field: {error}"
    );
}

#[test]
fn provider_api_key_falls_back_to_protocol_environment_variable() {
    let cases = [
        (ProviderProtocol::Anthropic, "ANTHROPIC_API_KEY"),
        (ProviderProtocol::OpenAi, "OPENAI_API_KEY"),
        (ProviderProtocol::OpenAiCompat, "OPENAI_API_KEY"),
    ];

    for (protocol, variable) in cases {
        let provider = mycode_core::config::ProviderConfig {
            name: "provider".into(),
            protocol,
            base_url: "https://provider.example.test".into(),
            model: "model".into(),
            api_key: String::new(),
            thinking: false,
            context_window: None,
            max_output_tokens: None,
        };
        assert_eq!(
            provider
                .resolve_api_key_with(|name| (name == variable).then(|| "environment-key".into())),
            Some("environment-key".to_string())
        );
    }
}
