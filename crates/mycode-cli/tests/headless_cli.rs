use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn headless_cli_completes_a_local_mock_provider_run() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let (base_url, request_receiver) = serve_once().await;
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

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mycode"))
        .arg("--headless")
        .arg("--config")
        .arg(&config_path)
        .arg("--session")
        .arg("new")
        .current_dir(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start");
    let mut child_stdin = child.stdin.take().expect("stdin should be piped");
    child_stdin
        .write_all(b"hello\n")
        .await
        .expect("prompt should write");
    drop(child_stdin);
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("CLI should finish within timeout")
        .expect("CLI output should be captured");
    let _ = tokio::time::timeout(Duration::from_secs(1), request_receiver).await;

    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let events = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("event should be JSON"))
        .collect::<Vec<_>>();
    assert_eq!(events[0]["type"], "session_started");
    assert_eq!(events[1]["type"], "text_delta");
    assert_eq!(events[1]["text"], "hello");
    assert_eq!(events.last().unwrap()["type"], "run_completed");

    let sessions = std::fs::read_dir(workspace.path().join(".mycode/sessions"))
        .expect("sessions directory should exist")
        .filter_map(std::io::Result::ok)
        .count();
    assert_eq!(sessions, 1);
}

#[tokio::test]
async fn headless_cli_answers_structured_permission_requests_from_stdin() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let tool_response = sse_response(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"WriteFile","arguments":"{\"file_path\":\"created.txt\",\"content\":\"created\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        "[DONE]",
    ]);
    let final_response = sse_response(&[
        r#"{"choices":[{"delta":{"content":"done"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        "[DONE]",
    ]);
    let (base_url, _requests) = serve_responses(vec![tool_response, final_response]).await;
    let config_path = workspace.path().join("config.yaml");
    write_config(&config_path, &base_url, "default");

    let output = run_cli(&config_path, workspace.path(), "create it\ny\n").await;
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""type":"permission_request""#));
    assert!(stdout.contains(r#""type":"permission_decision""#));
    assert!(workspace.path().join("created.txt").exists());
}

#[tokio::test]
async fn headless_cli_denies_permission_on_invalid_stdin_response() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let tool_response = sse_response(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"WriteFile","arguments":"{\"file_path\":\"denied.txt\",\"content\":\"denied\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        "[DONE]",
    ]);
    let final_response = sse_response(&[
        r#"{"choices":[{"delta":{"content":"done"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        "[DONE]",
    ]);
    let (base_url, _requests) = serve_responses(vec![tool_response, final_response]).await;
    let config_path = workspace.path().join("config.yaml");
    write_config(&config_path, &base_url, "default");

    let output = run_cli(&config_path, workspace.path(), "create it\nmaybe\n").await;
    assert_eq!(output.status.code(), Some(3));
    assert!(!workspace.path().join("denied.txt").exists());
}

fn write_config(path: &std::path::Path, base_url: &str, permission_mode: &str) {
    std::fs::write(
        path,
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
permission_mode: {permission_mode}
"#
        ),
    )
    .expect("config should write");
}

async fn run_cli(
    config_path: &std::path::Path,
    work_dir: &std::path::Path,
    stdin: &str,
) -> std::process::Output {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_mycode"))
        .arg("--headless")
        .arg("--config")
        .arg(config_path)
        .arg("--session")
        .arg("new")
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start");
    let mut child_stdin = child.stdin.take().expect("stdin should be piped");
    child_stdin
        .write_all(stdin.as_bytes())
        .await
        .expect("stdin should write");
    drop(child_stdin);
    tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("CLI should finish within timeout")
        .expect("CLI output should be captured")
}

fn sse_response(events: &[&str]) -> String {
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<Vec<_>>()
        .concat();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve_responses(
    responses: Vec<String>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind");
    let address = listener
        .local_addr()
        .expect("test server should have an address");
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("server should accept");
            let request = read_request(&mut socket).await;
            socket
                .write_all(response.as_bytes())
                .await
                .expect("response should write");
            let _ = socket.shutdown().await;
            let _ = sender.send(String::from_utf8(request).expect("request should be UTF-8"));
        }
    });
    (format!("http://{address}"), receiver)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
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
    request
}

async fn serve_once() -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind");
    let address = listener
        .local_addr()
        .expect("test server should have an address");
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
        let body = concat!(
            r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            "\n\n",
            "data: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response should write");
        let _ = socket.shutdown().await;
        let _ = sender.send(String::from_utf8(request).expect("request should be UTF-8"));
    });
    (format!("http://{address}"), receiver)
}
