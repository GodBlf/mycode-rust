use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn cli_lists_and_searches_sessions_as_json_lines() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_session(
        workspace.path(),
        "rust-session",
        [
            r#"{"role":"user","content":[{"type":"text","text":"refactor workspace"}],"timestamp_unix_seconds":1}"#,
        ],
    );
    write_session(
        workspace.path(),
        "database-session",
        [
            r#"{"role":"user","content":[{"type":"text","text":"migrate database"}],"timestamp_unix_seconds":2}"#,
        ],
    );

    let listed = run_session_command(workspace.path(), &["--list-sessions"]).await;
    assert!(listed.status.success(), "stderr: {:?}", listed.stderr);
    let events = parse_json_lines(&listed.stdout);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["id"], "database-session");
    assert_eq!(events[1]["id"], "rust-session");

    let searched = run_session_command(workspace.path(), &["--search-sessions", "MIGRATE"]).await;
    assert!(searched.status.success(), "stderr: {:?}", searched.stderr);
    let events = parse_json_lines(&searched.stdout);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], "database-session");
    assert_eq!(events[0]["first_user_message"], "migrate database");
}

#[tokio::test]
async fn cli_manually_compacts_an_existing_session_with_a_local_provider() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let recent = "y".repeat(7_000);
    let records = vec![
        r#"{"role":"user","content":[{"type":"text","text":"old request"}],"timestamp_unix_seconds":1}"#.to_string(),
    ]
    .into_iter()
    .chain((0..5).map(|index| {
        format!(
            r#"{{"role":"user","content":[{{"type":"text","text":"recent {index} {recent}"}}],"timestamp_unix_seconds":{}}}"#,
            index + 2
        )
    }))
    .collect::<Vec<_>>();
    write_session(workspace.path(), "compact-me", &records);

    let response = sse_summary_response();
    let (base_url, _request) = serve_once(response).await;
    let config_path = workspace.path().join("config.yaml");
    std::fs::write(
        &config_path,
        format!(
            r#"
providers:
  - name: local-mock
    protocol: openai-compat
    base_url: {base_url}
    model: test-model
    api_key: test-key
    context_window: 128000
    max_output_tokens: 1024
permission_mode: bypassPermissions
"#
        ),
    )
    .expect("config should write");

    let output = run_session_command(
        workspace.path(),
        &[
            "--headless",
            "--compact",
            "--config",
            config_path.to_str().expect("config path is UTF-8"),
            "--session",
            "compact-me",
        ],
    )
    .await;
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let events = parse_json_lines(&output.stdout);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "compacted");
    assert_eq!(events[0]["summary"], "actionable summary");

    let session_path = workspace.path().join(".mycode/sessions/compact-me.jsonl");
    let transcript = std::fs::read_to_string(session_path).expect("session should exist");
    assert!(transcript.contains(r#""type":"compact_boundary""#));
}

fn write_session<I, S>(workspace: &std::path::Path, id: &str, records: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let directory = workspace.join(".mycode/sessions");
    std::fs::create_dir_all(&directory).expect("sessions directory should create");
    std::fs::write(
        directory.join(format!("{id}.jsonl")),
        records
            .into_iter()
            .map(|record| format!("{}\n", record.as_ref()))
            .collect::<String>(),
    )
    .expect("session should write");
}

async fn run_session_command(
    workspace: &std::path::Path,
    arguments: &[&str],
) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_mycode"));
    for argument in arguments {
        command.arg(argument);
    }
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start")
        .wait_with_output()
        .await
        .expect("CLI should finish")
}

fn parse_json_lines(output: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("output should be JSON Lines"))
        .collect()
}

fn sse_summary_response() -> String {
    let body = concat!(
        r#"data: {"choices":[{"delta":{"content":"<summary>actionable summary</summary>"}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        "\n\n",
        "data: [DONE]\n\n"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve_once(response: String) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("server should bind");
    let address = listener.local_addr().expect("server address");
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("server should accept");
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 4096];
            let read = socket.read(&mut chunk).await.expect("request should read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or_default();
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
        let _ = socket.shutdown().await;
        let _ = sender.send(request);
    });
    (format!("http://{address}"), receiver)
}
