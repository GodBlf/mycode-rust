use std::sync::Mutex;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, LlmRequest, LlmStream};
use crate::events::{LlmError, LlmEvent};

#[derive(Debug, Default)]
pub struct MockClient {
    results: Vec<Result<LlmEvent, LlmError>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl MockClient {
    pub fn new(events: Vec<LlmEvent>) -> Self {
        Self::with_results(events.into_iter().map(Ok).collect())
    }

    pub fn with_results(results: Vec<Result<LlmEvent, LlmError>>) -> Self {
        Self {
            results,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests
            .lock()
            .expect("mock request lock should not be poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmClient for MockClient {
    async fn stream(
        &self,
        request: LlmRequest,
        cancellation: CancellationToken,
    ) -> Result<LlmStream, LlmError> {
        self.requests
            .lock()
            .expect("mock request lock should not be poisoned")
            .push(request.clone());

        let (event_sender, event_receiver) = mpsc::channel(1);
        let results = self.results.clone();
        tokio::spawn(async move {
            if cancellation.is_cancelled() {
                let _ = event_sender.send(Err(LlmError::Cancelled)).await;
                return;
            }
            for result in results {
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        let _ = event_sender.send(Err(LlmError::Cancelled)).await;
                        return;
                    }
                    sent = event_sender.send(result) => {
                        if sent.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(event_receiver)
    }
}
