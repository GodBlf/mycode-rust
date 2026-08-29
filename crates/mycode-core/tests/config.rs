use std::fs;
use std::path::Path;

use mycode_core::config::{
    Config, HookAction, HookEvent, McpTransport, PermissionMode, ProviderProtocol,
};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("config has a parent")).expect("create config dir");
    fs::write(path, contents).expect("write config");
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

    write(
        &work.path().join(".mycode/config.yaml"),
        r#"
providers:
  - name: ""
    protocol: invalid
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
