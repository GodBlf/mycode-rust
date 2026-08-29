use std::{process::Stdio, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{
    context::ToolContext,
    tool::PermissionSubject,
    tool::{Tool, ToolCategory, ToolResult},
};

const MAX_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Deserialize)]
struct BashRequest {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Debug)]
pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "Bash"
    }

    fn description(&self) -> &'static str {
        "Runs a shell command in the workspace with a timeout and cancellation support."
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Command
    }

    fn schema(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Timeout in seconds",
                        "default": 120,
                        "maximum": 600
                    }
                },
                "required": ["command"]
            }
        })
    }

    fn permission_subject(&self, arguments: &Value) -> Option<PermissionSubject> {
        arguments
            .get("command")?
            .as_str()
            .map(|command| PermissionSubject::Command(command.to_string()))
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> ToolResult {
        let request = match serde_path_to_error::deserialize::<_, BashRequest>(arguments) {
            Ok(request) => request,
            Err(source) => {
                return ToolResult::error(format!("invalid Bash arguments: {source}"));
            }
        };
        let timeout_seconds = request
            .timeout_seconds
            .unwrap_or(120)
            .min(MAX_TIMEOUT_SECONDS);
        if timeout_seconds == 0 {
            return ToolResult::error("Bash timeout_seconds must be greater than zero");
        }

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(&request.command)
            .current_dir(context.workspace_root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(source) => {
                return ToolResult::error(format!("failed to start command: {source}"));
            }
        };

        let completion = tokio::select! {
            output = child.wait_with_output() => Completion::Output(output),
            () = context.cancellation().cancelled() => Completion::Cancelled,
            () = tokio::time::sleep(Duration::from_secs(timeout_seconds)) => Completion::TimedOut,
        };

        match completion {
            Completion::Cancelled => ToolResult::error("command was cancelled"),
            Completion::TimedOut => {
                ToolResult::error(format!("command timed out after {timeout_seconds} seconds"))
            }
            Completion::Output(output) => {
                let output = match output {
                    Ok(output) => output,
                    Err(source) => {
                        return ToolResult::error(format!("failed to wait for command: {source}"));
                    }
                };
                let exit_code = output.status.code().unwrap_or(-1);
                let mut rendered = format!("$ {}\n", request.command);
                if !output.stdout.is_empty() {
                    rendered.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !rendered.ends_with('\n') {
                        rendered.push('\n');
                    }
                }
                if !output.stderr.is_empty() {
                    rendered.push_str("STDERR: ");
                    rendered.push_str(&String::from_utf8_lossy(&output.stderr));
                    if !rendered.ends_with('\n') {
                        rendered.push('\n');
                    }
                }
                rendered.push_str(&format!("(exit code {exit_code})"));
                ToolResult {
                    output: rendered,
                    is_error: !output.status.success(),
                }
            }
        }
    }
}

enum Completion {
    Output(std::io::Result<std::process::Output>),
    Cancelled,
    TimedOut,
}
