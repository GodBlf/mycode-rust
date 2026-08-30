use std::sync::Mutex;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, ProviderRequest, ProviderStream};
use crate::events::{ProviderError, ProviderEvent};

#[derive(Debug, Default)]
pub struct MockClient {
    results: Vec<Result<ProviderEvent, ProviderError>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl MockClient {
    pub fn new(events: Vec<ProviderEvent>) -> Self {
        Self::with_results(events.into_iter().map(Ok).collect())
    }

    pub fn with_results(results: Vec<Result<ProviderEvent, ProviderError>>) -> Self {
        Self {
            results,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<ProviderRequest> {
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
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests
            .lock()
            .expect("mock request lock should not be poisoned")
            .push(request.clone());

        let (event_sender, event_receiver) = mpsc::channel(1);
        let results = self.results.clone();
        tokio::spawn(async move {
            if cancellation.is_cancelled() {
                let _ = event_sender.send(Err(ProviderError::Cancelled)).await;
                return;
            }
            for result in results {
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        let _ = event_sender.send(Err(ProviderError::Cancelled)).await;
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
