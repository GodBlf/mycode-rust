use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::workspace::WorkspacePaths;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("no config file found; expected one of: {}", paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "))]
    NoConfig { paths: Vec<PathBuf> },
    #[error("invalid configuration: {message}")]
    Validation { message: String },
    #[error("the home directory could not be determined")]
    HomeDirectoryMissing,
    #[error("the current working directory could not be determined")]
    WorkingDirectoryMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderProtocol {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openai-compat")]
    OpenAiCompat,
}

impl ProviderProtocol {
    fn environment_variable(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi | Self::OpenAiCompat => "OPENAI_API_KEY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

impl ProviderConfig {
    pub fn resolve_api_key_with(&self, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
        if self.api_key.is_empty() {
            lookup(self.protocol.environment_variable())
        } else {
            Some(self.api_key.clone())
        }
    }

    pub fn resolve_api_key_from_env(&self) -> Option<String> {
        self.resolve_api_key_with(|variable| env::var(variable).ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    TurnStart,
    TurnEnd,
    PreSend,
    PostReceive,
    PreToolUse,
    PostToolUse,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookErrorPolicy {
    #[default]
    Fail,
    Ignore,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HookAction {
    Command {
        command: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    Prompt {
        message: String,
    },
    Http {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        body: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    Agent {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        command: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(default)]
    pub id: Option<String>,
    pub event: HookEvent,
    #[serde(default)]
    pub condition: Option<String>,
    pub action: HookAction,
    #[serde(default)]
    pub reject: bool,
    #[serde(default)]
    pub once: bool,
    #[serde(rename = "async", default)]
    pub asynchronous: bool,
    #[serde(default)]
    pub on_error: HookErrorPolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigDocument {
    #[serde(default)]
    providers: Option<Vec<ProviderDocument>>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    mcp_servers: Option<Vec<McpServerConfig>>,
    #[serde(default)]
    hooks: Option<Vec<HookConfig>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProviderDocument {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    thinking: bool,
    #[serde(default)]
    context_window: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

impl ProviderDocument {
    fn into_provider(self, index: usize) -> Result<ProviderConfig, ConfigError> {
        let mut missing_fields = Vec::new();
        if self.name.is_none() {
            missing_fields.push("name");
        }
        if self.protocol.is_none() {
            missing_fields.push("protocol");
        }
        if self.base_url.is_none() {
            missing_fields.push("base_url");
        }
        if self.model.is_none() {
            missing_fields.push("model");
        }
        if !missing_fields.is_empty() {
            return Err(ConfigError::Validation {
                message: format!(
                    "provider {}: missing required field(s): {}",
                    index + 1,
                    missing_fields.join(", ")
                ),
            });
        }

        let protocol =
            parse_provider_protocol(self.protocol.as_deref().unwrap_or_default(), index)?;
        Ok(ProviderConfig {
            name: self.name.unwrap_or_default(),
            protocol,
            base_url: self.base_url.unwrap_or_default(),
            model: self.model.unwrap_or_default(),
            api_key: self.api_key,
            thinking: self.thinking,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub providers: Vec<ProviderConfig>,
    pub permission_mode: PermissionMode,
    pub mcp_servers: Vec<McpServerConfig>,
    pub hooks: Vec<HookConfig>,
}

impl Config {
    pub fn load(home_dir: &Path, work_dir: &Path) -> Result<Self, ConfigError> {
        let workspace = WorkspacePaths::new(work_dir);
        let paths = vec![
            WorkspacePaths::home_config_file(home_dir),
            workspace.config_file(),
            workspace.local_config_file(),
        ];
        let mut merged = ConfigDocument::default();
        let mut found_config = false;

        for path in &paths {
            match fs::metadata(path) {
                Ok(_) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(ConfigError::Read {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
            found_config = true;
            let document = read_document(path)?;
            merged = merge_documents(merged, document);
        }

        if !found_config {
            return Err(ConfigError::NoConfig { paths });
        }

        let config = merged.into_config()?;
        config.validate()?;
        Ok(config)
    }

    pub fn discover() -> Result<Self, ConfigError> {
        let home_dir = home::home_dir().ok_or(ConfigError::HomeDirectoryMissing)?;
        let work_dir = env::current_dir().map_err(|_| ConfigError::WorkingDirectoryMissing)?;
        Self::load(&home_dir, &work_dir)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.providers.is_empty() {
            return Err(ConfigError::Validation {
                message: "at least one provider must be configured".into(),
            });
        }

        for (index, provider) in self.providers.iter().enumerate() {
            if provider.name.trim().is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("provider {}: name must be non-empty", index + 1),
                });
            }
            if provider.model.trim().is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("provider {}: model must be non-empty", index + 1),
                });
            }
            if !is_http_url(&provider.base_url) {
                return Err(ConfigError::Validation {
                    message: format!(
                        "provider {}: base_url must be a valid HTTP(S) URL",
                        index + 1
                    ),
                });
            }
            if provider.context_window.is_some_and(|value| value == 0) {
                return Err(ConfigError::Validation {
                    message: format!(
                        "provider {}: context_window must be greater than zero",
                        index + 1
                    ),
                });
            }
            if provider.max_output_tokens.is_some_and(|value| value == 0) {
                return Err(ConfigError::Validation {
                    message: format!(
                        "provider {}: max_output_tokens must be greater than zero",
                        index + 1
                    ),
                });
            }
        }

        let provider_names = self.providers.iter().map(|provider| provider.name.as_str());
        if has_duplicate(provider_names) {
            return Err(ConfigError::Validation {
                message: "provider names must be unique".into(),
            });
        }

        for (index, server) in self.mcp_servers.iter().enumerate() {
            if server.name.trim().is_empty() {
                return Err(ConfigError::Validation {
                    message: format!("mcp server {}: name must be non-empty", index + 1),
                });
            }
            match server.transport {
                McpTransport::Stdio => {
                    if server
                        .command
                        .as_deref()
                        .is_none_or(|command| command.trim().is_empty())
                    {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "mcp server {}: command must be non-empty for stdio transport",
                                index + 1
                            ),
                        });
                    }
                }
                McpTransport::Http => {
                    let Some(url) = server.url.as_deref() else {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "mcp server {}: url must be set for HTTP transport",
                                index + 1
                            ),
                        });
                    };
                    if !is_http_url(url) {
                        return Err(ConfigError::Validation {
                            message: format!(
                                "mcp server {}: url must be a valid HTTP(S) URL",
                                index + 1
                            ),
                        });
                    }
                }
            }
        }

        for (index, hook) in self.hooks.iter().enumerate() {
            let label = hook
                .id
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| format!("hook {}", index + 1));
            match &hook.action {
                HookAction::Command { command, .. } => {
                    if command.trim().is_empty() {
                        return Err(ConfigError::Validation {
                            message: format!("{label}: action command must be non-empty"),
                        });
                    }
                }
                HookAction::Prompt { message } => {
                    if message.trim().is_empty() {
                        return Err(ConfigError::Validation {
                            message: format!("{label}: action message must be non-empty"),
                        });
                    }
                }
                HookAction::Http { url, .. } => {
                    if !is_http_url(url) {
                        return Err(ConfigError::Validation {
                            message: format!("{label}: action url must be a valid HTTP(S) URL"),
                        });
                    }
                }
                HookAction::Agent { message, command } => {
                    if message.as_deref().is_none_or(str::is_empty)
                        && command.as_deref().is_none_or(str::is_empty)
                    {
                        return Err(ConfigError::Validation {
                            message: format!("{label}: agent action requires message or command"),
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

fn read_document(path: &Path) -> Result<ConfigDocument, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

impl ConfigDocument {
    fn into_config(self) -> Result<Config, ConfigError> {
        let providers = self
            .providers
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, provider)| provider.into_provider(index))
            .collect::<Result<Vec<_>, _>>()?;
        let permission_mode = parse_permission_mode(self.permission_mode.as_deref())?;
        Ok(Config {
            providers,
            permission_mode,
            mcp_servers: self.mcp_servers.unwrap_or_default(),
            hooks: self.hooks.unwrap_or_default(),
        })
    }
}

fn merge_documents(base: ConfigDocument, override_document: ConfigDocument) -> ConfigDocument {
    let mut merged = base;
    if override_document.providers.is_some() {
        merged.providers = override_document.providers;
    }
    if override_document.permission_mode.is_some() {
        merged.permission_mode = override_document.permission_mode;
    }
    if let Some(override_servers) = override_document.mcp_servers {
        let mut servers = merged.mcp_servers.unwrap_or_default();
        for server in override_servers {
            if let Some(index) = servers
                .iter()
                .position(|existing| existing.name == server.name)
            {
                servers[index] = server;
            } else {
                servers.push(server);
            }
        }
        merged.mcp_servers = Some(servers);
    }
    if let Some(override_hooks) = override_document.hooks {
        let mut hooks = merged.hooks.unwrap_or_default();
        hooks.extend(override_hooks);
        merged.hooks = Some(hooks);
    }
    merged
}

fn parse_provider_protocol(value: &str, index: usize) -> Result<ProviderProtocol, ConfigError> {
    match value {
        "anthropic" => Ok(ProviderProtocol::Anthropic),
        "openai" => Ok(ProviderProtocol::OpenAi),
        "openai-compat" => Ok(ProviderProtocol::OpenAiCompat),
        _ => Err(ConfigError::Validation {
            message: format!(
                "provider {}: invalid protocol {value:?}; expected anthropic, openai, or openai-compat",
                index + 1
            ),
        }),
    }
}

fn parse_permission_mode(value: Option<&str>) -> Result<PermissionMode, ConfigError> {
    match value {
        None => Ok(PermissionMode::Default),
        Some("default") => Ok(PermissionMode::Default),
        Some("acceptEdits") => Ok(PermissionMode::AcceptEdits),
        Some("plan") => Ok(PermissionMode::Plan),
        Some("bypassPermissions") => Ok(PermissionMode::BypassPermissions),
        Some(value) => Err(ConfigError::Validation {
            message: format!(
                "invalid permission_mode {value:?}; expected default, acceptEdits, plan, or bypassPermissions"
            ),
        }),
    }
}

fn is_http_url(value: &str) -> bool {
    Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
        .unwrap_or(false)
}

fn has_duplicate<'a, I>(values: I) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = std::collections::BTreeSet::new();
    values.into_iter().any(|value| !seen.insert(value))
}
