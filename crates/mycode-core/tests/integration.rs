use std::fs;

use mycode_core::config::{Config, PermissionMode, ProviderProtocol};
use mycode_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
use mycode_core::plan::PlanFileManager;
use mycode_core::session::{SessionId, SessionStore};

#[test]
fn core_domain_and_persistence_work_together_in_one_workspace() {
    let home = tempfile::tempdir().expect("home tempdir");
    let work = tempfile::tempdir().expect("work tempdir");
    let config_path = work.path().join(".mycode/config.yaml");
    fs::create_dir_all(config_path.parent().expect("config parent"))
        .expect("create config directory");
    fs::write(
        &config_path,
        r#"
providers:
  - name: integration-provider
    protocol: anthropic
    base_url: https://provider.example.test
    model: integration-model
permission_mode: acceptEdits
"#,
    )
    .expect("write config");

    let config = Config::load(home.path(), work.path()).expect("load config");
    assert_eq!(config.providers[0].protocol, ProviderProtocol::Anthropic);
    assert_eq!(config.permission_mode, PermissionMode::AcceptEdits);

    let session_store = SessionStore::new(work.path());
    let session_id = SessionId::new("integration-session").expect("valid session ID");
    let user_message = ConversationMessage {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: "Create a plan".into(),
        }],
        timestamp_unix_seconds: 1,
    };
    session_store
        .append(&session_id, &user_message)
        .expect("append session message");

    let mut plan_manager = PlanFileManager::new(work.path());
    let plan_path = plan_manager.create().expect("create plan");
    plan_manager.save("# Integration plan").expect("save plan");

    let loaded_messages = session_store
        .load(&session_id)
        .expect("load session")
        .expect("session exists");
    assert_eq!(loaded_messages, vec![user_message]);
    assert_eq!(
        plan_manager.load().expect("load plan"),
        Some("# Integration plan".to_string())
    );
    assert!(plan_path.starts_with(work.path().join(".mycode/plans")));
}
