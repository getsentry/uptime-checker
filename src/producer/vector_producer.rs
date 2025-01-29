use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

pub struct VectorResultsProducer {
    schema: Schema,
    sender: UnboundedSender<Vec<u8>>,
}

impl VectorResultsProducer {
    pub fn new(topic_name: &str, vector_batch_size: usize) -> (Self, JoinHandle<()>) {
        Self::new_with_endpoint(
            topic_name,
            "http://localhost:8020".to_string(),
            vector_batch_size,
        )
    }

    pub fn new_with_endpoint(
        topic_name: &str,
        endpoint: String,
        vector_batch_size: usize,
    ) -> (Self, JoinHandle<()>) {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        let client = Client::new();

        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = Self::spawn_worker(vector_batch_size, client, endpoint, receiver);

        (Self { schema, sender }, worker)
    }

    async fn send_batch(
        batch: Vec<Vec<u8>>,
        client: Client,
        endpoint: String,
    ) -> Result<(), ExtractCodeError> {
        if batch.is_empty() {
            return Ok(());
        }

        let body: String = batch
            .iter()
            .filter_map(|json| String::from_utf8(json.clone()).ok())
            .map(|s| s + "\n")
            .collect();

        // Log the exact payload for debugging
        tracing::debug!(%body, "payload.sending");
        tracing::debug!(size = body.len(), "request.sending_to_vector");

        let response = client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;

        match response {
            Ok(_) => {
                tracing::debug!("request.sent_successfully");
                Ok(())
            }
            Err(e) => {
                tracing::error!(error = ?e, "request.failed_to_vector");
                Err(ExtractCodeError::VectorRequestStatusError(
                    reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                ))
            }
        }
    }

    fn spawn_worker(
        vector_batch_size: usize,
        client: Client,
        endpoint: String,
        mut receiver: UnboundedReceiver<Vec<u8>>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::debug!(%endpoint, "worker.started");
            let mut batch = Vec::with_capacity(vector_batch_size);

            while let Some(json) = receiver.recv().await {
                tracing::debug!(size = json.len(), "event.received");
                batch.push(json);
                tracing::debug!(size = batch.len(), "batch.size.updated");

                if batch.len() < vector_batch_size {
                    continue;
                }

                tracing::debug!(size = batch.len(), "batch.sending");
                let batch_to_send =
                    std::mem::replace(&mut batch, Vec::with_capacity(vector_batch_size));
                if let Err(e) =
                    Self::send_batch(batch_to_send, client.clone(), endpoint.clone()).await
                {
                    tracing::error!(error = ?e, "batch.send_failed");
                }
            }

            if !batch.is_empty() {
                tracing::debug!(size = batch.len(), "final_batch.sending");
                if let Err(e) = Self::send_batch(batch, client, endpoint).await {
                    tracing::error!(error = ?e, "final_batch.send_failed");
                }
            }

            tracing::info!("worker.shutdown");
        })
    }
}

impl ResultsProducer for VectorResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;

        // Send the serialized result to the worker task
        if self.sender.send(json).is_err() {
            tracing::error!("event.send_failed_channel_closed");
            return Err(ExtractCodeError::VectorRequestStatusError(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::VectorResultsProducer;
    use crate::{
        producer::ResultsProducer,
        types::{
            result::{
                CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo,
            },
            shared::RequestMethod,
        },
    };
    use chrono::{TimeDelta, Utc};
    use httpmock::prelude::*;
    use httpmock::Method;
    use sentry::protocol::SpanId;
    use std::time::Duration;
    use tokio::time::sleep;
    use uuid::{uuid, Uuid};

    const TEST_BATCH_SIZE: usize = 10;

    fn create_test_result() -> CheckResult {
        CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: uuid!("23d6048d67c948d9a19c0b47979e9a03"),
            status: CheckStatus::Success,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "hi".to_string(),
            }),
            trace_id: Uuid::new_v4(),
            span_id: SpanId::default(),
            scheduled_check_time: Utc::now(),
            actual_check_time: Utc::now(),
            duration: Some(TimeDelta::seconds(1)),
            request_info: Some(RequestInfo {
                request_type: RequestMethod::Get,
                http_status_code: Some(200),
            }),
            region: "us-west-1".to_string(),
        }
    }

    #[tokio::test]
    async fn test_single_event() {
        let mock_server = MockServer::start();
        tracing::debug!("Mock server started at {}", mock_server.url("/"));
        let test_result = create_test_result();

        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!(
                            "Received request body: {:?}",
                            String::from_utf8_lossy(body)
                        );
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        if lines.len() != 1 {
                            tracing::debug!("Expected 1 line, got {}", lines.len());
                            return false;
                        }
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(lines[0]) {
                            return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
                                && value["status"] == "success"
                                && value["region"] == "us-west-1";
                        }
                    }
                    false
                });
            then.status(200);
        });

        let (producer, worker) =
            VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"), 10);
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Wait a bit for the request to be processed
        sleep(Duration::from_millis(100)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;
        mock.assert();
    }

    #[tokio::test]
    async fn test_batch_events() {
        let mock_server = MockServer::start();
        tracing::debug!("Mock server started at {}", mock_server.url("/"));
        let (producer, worker) = VectorResultsProducer::new_with_endpoint(
            "uptime-results",
            mock_server.url("/"),
            TEST_BATCH_SIZE,
        );

        // Create and send BATCH_SIZE + 2 events
        for i in 0..(10 + 2) {
            tracing::debug!("Sending event {}", i + 1);
            let test_result = create_test_result();
            let result = producer.produce_checker_result(&test_result);
            assert!(result.is_ok());
        }

        // Mock for full batch and remainder
        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!(
                            "Received request body: {:?}",
                            String::from_utf8_lossy(body)
                        );
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        let len = lines.len();
                        if len != 10 && len != 2 {
                            tracing::debug!("Expected {} or 2 lines, got {}", 10, len);
                            return false;
                        }
                        // Verify each line is valid JSON with expected fields
                        lines.iter().all(|line| {
                            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
                                return value["subscription_id"]
                                    == "23d6048d67c948d9a19c0b47979e9a03"
                                    && value["status"] == "success"
                                    && value["region"] == "us-west-1";
                            }
                            false
                        })
                    } else {
                        false
                    }
                });
            then.status(200);
        });

        // Wait a bit for the requests to be processed
        sleep(Duration::from_millis(100)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;

        // We expect 2 requests: one with BATCH_SIZE events and one with 2 events
        mock.assert_hits(2);
    }

    #[tokio::test]
    async fn test_server_error() {
        let mock_server = MockServer::start();
        tracing::debug!("Mock server started at {}", mock_server.url("/"));
        let test_result = create_test_result();

        let error_mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!(
                            "Received request body: {:?}",
                            String::from_utf8_lossy(body)
                        );
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        if lines.len() != 1 {
                            tracing::debug!("Expected 1 line, got {}", lines.len());
                            return false;
                        }
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(lines[0]) {
                            return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
                                && value["status"] == "success"
                                && value["region"] == "us-west-1";
                        }
                    }
                    false
                });
            then.status(500).body("Internal Server Error");
        });

        let (producer, worker) = VectorResultsProducer::new_with_endpoint(
            "uptime-results",
            mock_server.url("/"),
            TEST_BATCH_SIZE,
        );
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Wait a bit for the request to be processed
        sleep(Duration::from_millis(100)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;
        error_mock.assert();
    }

    #[tokio::test]
    async fn test_flush_on_shutdown() {
        let mock_server = MockServer::start();
        tracing::debug!("Mock server started at {}", mock_server.url("/"));
        // Create a mock that expects a single request with less than BATCH_SIZE events
        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!(
                            "Received request body: {:?}",
                            String::from_utf8_lossy(body)
                        );
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        // We expect only 1 event, which is less than BATCH_SIZE
                        if lines.len() != 1 {
                            tracing::debug!("Expected 1 line, got {}", lines.len());
                            return false;
                        }
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(lines[0]) {
                            return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
                                && value["status"] == "success"
                                && value["region"] == "us-west-1";
                        }
                    }
                    false
                });
            then.status(200);
        });

        let (producer, worker) = VectorResultsProducer::new_with_endpoint(
            "uptime-results",
            mock_server.url("/"),
            TEST_BATCH_SIZE,
        );

        // Send a single event (less than BATCH_SIZE)
        let test_result = create_test_result();
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Give the worker a moment to process the event
        sleep(Duration::from_millis(50)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;

        // Verify that the event was sent despite not reaching BATCH_SIZE
        mock.assert();
    }
}
