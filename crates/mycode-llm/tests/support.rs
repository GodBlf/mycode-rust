#![allow(dead_code)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub async fn serve_once(response: String) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind");
    let address = listener
        .local_addr()
        .expect("test server should have an address");
    let (request_sender, request_receiver) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("test server should accept");
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 4096];
            let read = socket.read(&mut chunk).await.expect("request should read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request_is_complete(&request) {
                break;
            }
        }
        socket
            .write_all(response.as_bytes())
            .await
            .expect("test response should write");
        let _ = socket.shutdown().await;
        let _ = request_sender
            .send(String::from_utf8(request).expect("test request should be valid UTF-8"));
    });

    (format!("http://{address}"), request_receiver)
}

pub async fn serve_hanging() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test server should bind");
    let address = listener
        .local_addr()
        .expect("test server should have an address");

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0_u8; 4096];
            let _ = socket.read(&mut buffer).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    format!("http://{address}")
}

pub fn sse_response(events: &[&str]) -> String {
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn http_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn request_body(request: &str) -> serde_json::Value {
    let (_, body) = request
        .split_once("\r\n\r\n")
        .expect("request should have headers and body");
    serde_json::from_str(body).expect("request body should be JSON")
}

pub fn request_headers(request: &str) -> Vec<(String, String)> {
    let (headers, _) = request
        .split_once("\r\n\r\n")
        .expect("request should have headers and body");
    headers
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_string()))
        .collect()
}

fn request_is_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    request.len() >= header_end + 4 + content_length
}
