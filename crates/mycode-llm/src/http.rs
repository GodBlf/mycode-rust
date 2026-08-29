use std::time::Duration;

use futures_util::{Stream, StreamExt};
use reqwest::{Client, Method, Response, Url};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::events::ProviderError;
use crate::events::ProviderEvent;
use crate::sse::{SseEvent, SseParser};

pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub trait SseDecoder {
    fn decode(&mut self, event: &SseEvent) -> Result<Vec<ProviderEvent>, ProviderError>;

    fn finish(&mut self) -> Result<Vec<ProviderEvent>, ProviderError> {
        Ok(Vec::new())
    }
}

pub fn spawn_sse_stream(
    http: Client,
    endpoint: String,
    headers: Vec<(&'static str, String)>,
    body: serde_json::Value,
    cancellation: CancellationToken,
    decoder: impl SseDecoder + Send + 'static,
) -> mpsc::Receiver<Result<ProviderEvent, ProviderError>> {
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(async move {
        run_sse_stream(http, endpoint, headers, body, cancellation, sender, decoder).await;
    });
    receiver
}

pub async fn post_json_sse(
    client: &Client,
    url: &str,
    headers: &[(&'static str, String)],
    body: &serde_json::Value,
) -> Result<impl Stream<Item = Result<SseEvent, ProviderError>>, ProviderError> {
    let url = url
        .parse::<Url>()
        .map_err(|error| ProviderError::InvalidResponse {
            message: format!("invalid provider URL {url:?}: {error}"),
        })?;
    let mut request = client.request(Method::POST, url).json(body);
    for (name, value) in headers {
        request = request.header(*name, value);
    }

    let response = request
        .send()
        .await
        .map_err(|error| ProviderError::Network {
            message: error.to_string(),
        })?;
    check_response(response).await
}

pub async fn run_sse_stream(
    http: Client,
    endpoint: String,
    headers: Vec<(&'static str, String)>,
    body: serde_json::Value,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    mut decoder: impl SseDecoder,
) {
    let events = tokio::select! {
        _ = cancellation.cancelled() => {
            let _ = sender.send(Err(ProviderError::Cancelled)).await;
            return;
        }
        events = post_json_sse(&http, &endpoint, &headers, &body) => events,
    };

    let events = match events {
        Ok(events) => events,
        Err(error) => {
            let _ = sender.send(Err(error)).await;
            return;
        }
    };
    tokio::pin!(events);

    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = sender.send(Err(ProviderError::Cancelled)).await;
                return;
            }
            next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, events.next()) => next,
        };

        let next = match next {
            Ok(next) => next,
            Err(_) => {
                let _ = sender
                    .send(Err(ProviderError::Network {
                        message: format!(
                            "provider stream idle timeout: no SSE events for {STREAM_IDLE_TIMEOUT:?}"
                        ),
                    }))
                    .await;
                return;
            }
        };

        let Some(next) = next else { break };
        let next = match next {
            Ok(next) => next,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        };

        let decoded = match decoder.decode(&next) {
            Ok(decoded) => decoded,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        };
        for event in decoded {
            if sender.send(Ok(event)).await.is_err() {
                return;
            }
        }
    }

    let decoded = match decoder.finish() {
        Ok(decoded) => decoded,
        Err(error) => {
            let _ = sender.send(Err(error)).await;
            return;
        }
    };
    for event in decoded {
        if sender.send(Ok(event)).await.is_err() {
            return;
        }
    }
}

async fn check_response(
    response: Response,
) -> Result<impl Stream<Item = Result<SseEvent, ProviderError>>, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response_sse_events(response));
    }

    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let message = response
        .text()
        .await
        .map_err(|error| ProviderError::Network {
            message: error.to_string(),
        })?;

    Err(map_http_error(status.as_u16(), &message, retry_after))
}

pub fn map_http_error(status: u16, message: &str, retry_after: Option<String>) -> ProviderError {
    let normalized = message.to_lowercase();
    if status == 413
        || (status == 400
            && (normalized.contains("context length")
                || normalized.contains("prompt is too long")
                || normalized.contains("too many tokens")))
    {
        return ProviderError::ContextTooLong {
            message: format!("provider returned HTTP {status}: {message}"),
        };
    }

    match status {
        401 | 403 => ProviderError::Authentication {
            message: format!("provider rejected credentials (HTTP {status})"),
        },
        429 => ProviderError::RateLimit {
            message: format!("provider rate limit exceeded (HTTP {status})"),
            retry_after,
        },
        _ => ProviderError::Network {
            message: format!("provider returned HTTP {status}: {message}"),
        },
    }
}

fn response_sse_events(response: Response) -> impl Stream<Item = Result<SseEvent, ProviderError>> {
    async_stream::stream! {
        let mut parser = SseParser::new();
        let mut bytes = response.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|error| ProviderError::Network {
                message: error.to_string(),
            })?;
            for event in parser.push_bytes(&chunk)? {
                yield Ok(event);
            }
        }
        for event in parser.finish()? {
            yield Ok(event);
        }
    }
}
