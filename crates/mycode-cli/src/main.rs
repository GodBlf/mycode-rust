use std::path::PathBuf;

use mycode_agent::{Agent, AgentConfig, AgentEvent};
use mycode_core::config::{Config, PermissionMode};
use mycode_core::session::{SessionId, SessionStore};
use mycode_llm::build_provider_client;
use mycode_tools::context::ToolContext;
use mycode_tools::runtime::{ToolExecutor, default_checker, default_registry};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let status = match run(arguments).await {
        Ok(status) => status,
        Err(message) => {
            eprintln!("mycode: {message}");
            2
        }
    };
    std::process::exit(status);
}

async fn run(arguments: Vec<String>) -> Result<i32, String> {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print_usage();
        return Ok(0);
    }
    if arguments
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("mycode {VERSION}");
        return Ok(0);
    }

    let options = parse_arguments(arguments)?;
    if !options.headless {
        print_usage();
        return Ok(0);
    }
    let Some(config_path) = options.config else {
        return Err("--headless requires --config <path>".into());
    };
    let Some(session_argument) = options.session else {
        return Err("--headless requires --session <id|new>".into());
    };

    let config = Config::load_file(&config_path)
        .map_err(|error| format!("failed to load config: {error}"))?;
    let provider = config
        .providers
        .first()
        .ok_or_else(|| "configuration has no providers".to_string())?;
    let permission_mode = match options.permission_mode.as_deref() {
        Some(value) => PermissionMode::parse(value).map_err(|error| error.to_string())?,
        None => config.permission_mode,
    };

    let session_id = if session_argument == "new" {
        SessionId::generate()
    } else {
        SessionId::new(session_argument).map_err(|error| error.to_string())?
    };
    let work_dir = std::env::current_dir().map_err(|error| error.to_string())?;
    let home_dir = home::home_dir().ok_or_else(|| "home directory not found".to_string())?;
    let cancellation = CancellationToken::new();
    spawn_ctrl_c_handler(cancellation.clone());

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut prompt = String::new();
    stdin
        .read_line(&mut prompt)
        .await
        .map_err(|error| format!("failed to read prompt: {error}"))?;
    if prompt.trim().is_empty() {
        return Err("prompt is required on stdin".into());
    }

    let registry = default_registry();
    let checker = default_checker(permission_mode, &home_dir, &work_dir)
        .map_err(|error| error.to_string())?;
    let context =
        ToolContext::with_cancellation(&work_dir, session_id.as_str(), cancellation.clone())
            .map_err(|error| error.to_string())?;
    let provider_client = build_provider_client(provider)
        .await
        .map_err(|error| error.to_string())?;

    println!(
        "{}",
        serde_json::json!({"type": "session_started", "session_id": session_id.as_str()})
    );
    let agent = Agent::new(
        provider_client.client,
        ToolExecutor::new(registry, checker),
        context,
        SessionStore::new(&work_dir),
        session_id,
        AgentConfig::default(),
    );
    let mut run = agent.run(prompt);
    let mut permission_denied = false;
    let mut tool_error = false;
    let mut final_event = None;

    while let Some(event) = run.events.recv().await {
        final_event = Some(event.clone());
        println!(
            "{}",
            serde_json::to_string(&event).map_err(|error| error.to_string())?
        );

        if let AgentEvent::PermissionRequest { request_id, .. } = &event
            && !cancellation.is_cancelled()
        {
            let mut response = String::new();
            let read = tokio::select! {
                _ = cancellation.cancelled() => 0,
                read = stdin.read_line(&mut response) => {
                    read.map_err(|error| format!("failed to read permission response: {error}"))?
                }
            };
            if cancellation.is_cancelled() {
                continue;
            }
            let allowed = read > 0 && response.trim() == "y";
            permission_denied |= !allowed;
            run.respond(request_id.clone(), allowed);
        }
        if let AgentEvent::ToolResult { is_error, .. } = &event {
            tool_error |= *is_error;
        }
    }

    Ok(match final_event {
        Some(AgentEvent::RunCompleted { .. }) if permission_denied => 3,
        Some(AgentEvent::RunCompleted { .. }) if tool_error => 4,
        Some(AgentEvent::RunCompleted { .. }) => 0,
        Some(AgentEvent::MaxIterationsReached { .. }) => 5,
        Some(AgentEvent::RunCancelled) => 130,
        Some(AgentEvent::RunError { .. }) => 1,
        Some(_) => 1,
        None => 1,
    })
}

#[derive(Default)]
struct Options {
    headless: bool,
    config: Option<PathBuf>,
    session: Option<String>,
    permission_mode: Option<String>,
}

fn parse_arguments(arguments: Vec<String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iterator = arguments.into_iter();
    while let Some(argument) = iterator.next() {
        match argument.as_str() {
            "--headless" => options.headless = true,
            "--config" => {
                options.config = Some(PathBuf::from(take_value(&mut iterator, "--config")?));
            }
            "--session" => {
                options.session = Some(take_value(&mut iterator, "--session")?);
            }
            "--permission-mode" => {
                options.permission_mode = Some(take_value(&mut iterator, "--permission-mode")?);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(options)
}

fn take_value(iterator: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iterator
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn spawn_ctrl_c_handler(cancellation: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancellation.cancel();
        }
    });
}

fn print_usage() {
    println!("MyCode terminal coding agent");
    println!();
    println!("Usage: mycode [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -h, --help                    Print help");
    println!("  -V, --version                 Print version");
    println!("      --headless                Run one headless Agent turn");
    println!("      --config <path>           Use an explicit configuration file");
    println!("      --session <id|new>        Load a Session or create a new one");
    println!("      --permission-mode <mode>  Override the configured Permission mode");
}
