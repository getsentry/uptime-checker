use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

pub struct VectorResultsProducer {
    schema: Schema,
    sender: UnboundedSender<Vec<u8>>,
}

impl VectorResultsProducer {
    pub fn new(
        topic_name: &str,
        endpoint: String,
        vector_batch_size: usize,
        max_retries: u32,
    ) -> (Self, JoinHandle<()>) {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        let client = Client::new();

        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = Self::spawn_worker(vector_batch_size, client, endpoint, max_retries, receiver);

        (Self { schema, sender }, worker)
    }

    fn spawn_worker(
        vector_batch_size: usize,
        client: Client,
        endpoint: String,
        max_retries: u32,
        mut receiver: UnboundedReceiver<Vec<u8>>,
    ) -> JoinHandle<()> {
        tracing::info!("vector_worker.starting");

        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(vector_batch_size);

            while let Some(json) = receiver.recv().await {
                batch.push(json);

                if batch.len() < vector_batch_size {
                    continue;
                }

                let batch_to_send =
                    std::mem::replace(&mut batch, Vec::with_capacity(vector_batch_size));
                if let Err(e) =
                    send_batch(batch_to_send, client.clone(), endpoint.clone(), max_retries).await
                {
                    tracing::error!(error = ?e, "vector_batch.send_failed");
                }
            }

            if !batch.is_empty() {
                if let Err(e) = send_batch(batch, client, endpoint, max_retries).await {
                    tracing::error!(error = ?e, "final_batch.send_failed");
                }
            }

            tracing::info!("vector_worker.shutdown");
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
            return Err(ExtractCodeError::VectorError);
        }

        Ok(())
    }
}

async fn send_batch(
    batch: Vec<Vec<u8>>,
    client: Client,
    endpoint: String,
    max_retries: u32,
) -> Result<(), ExtractCodeError> {
    if batch.is_empty() {
        return Ok(());
    }

    let body: String = batch
        .iter()
        .filter_map(|json| String::from_utf8(json.clone()).ok())
        .map(|s| s + "\n")
        .collect();

    const BASE_DELAY_MS: u64 = 100;
    const MAX_DELAY_MS: u64 = 30_000; // Cap maximum delay at 30 seconds

    for retry in 0..=max_retries {
        let response = client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
            .await;
        
        // Calculate delay with a maximum cap
        let delay = Duration::from_millis(
            (BASE_DELAY_MS * (2_u64.pow(retry))).min(MAX_DELAY_MS)
        );
        
        match response {
            Ok(resp) if !resp.status().is_server_error() => return Ok(()),
            Ok(resp) => {
                if retry == max_retries {
                    // Consider implementing a dead letter queue or persistent storage here
                    tracing::error!(
                        status = ?resp.status(),
                        batch_size = batch.len(),
                        "request.failed_to_vector_after_retries"
                    );
                    return Err(ExtractCodeError::VectorError);
                }
                
                tracing::warn!(
                    status = ?resp.status(),
                    retry = retry + 1,
                    delay_ms = ?delay.as_millis(),
                    batch_size = batch.len(),
                    "request.failed_to_vector_retrying"
                );
                sleep(delay).await;
            }
            Err(e) => {
                if retry == max_retries {
                    tracing::error!(error = ?e, "request.failed_to_vector_after_retries");
                    return Err(ExtractCodeError::VectorError);
                }

                tracing::warn!(
                    error = ?e,
                    retry = retry + 1,
                    delay_ms = ?delay.as_millis(),
                    "request.failed_to_vector_retrying"
                );
                sleep(delay).await;
            }
        }
    }

    Err(ExtractCodeError::VectorError)
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
    const TEST_MAX_RETRIES: u32 = 2;

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
    async fn test_batch_events() {
        let mock_server = MockServer::start();
        let (producer, worker) = VectorResultsProducer::new(
            "uptime-results",
            mock_server.url("/").to_string(),
            TEST_BATCH_SIZE,
            TEST_MAX_RETRIES,
        );

        // Create and send BATCH_SIZE + 2 events
        for _ in 0..(TEST_BATCH_SIZE + 2) {
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
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        let len = lines.len();
                        if len != 10 && len != 2 {
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
    async fn test_flush_on_shutdown() {
        let mock_server = MockServer::start();
        // Create a mock that expects a single request with less than BATCH_SIZE events
        let mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        // We expect only 1 event, which is less than BATCH_SIZE
                        if lines.len() != 1 {
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

        let (producer, worker) = VectorResultsProducer::new(
            "uptime-results",
            mock_server.url("/").to_string(),
            TEST_BATCH_SIZE,
            TEST_MAX_RETRIES,
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

    #[tokio::test]
    async fn test_server_error_retries() {
        let mock_server = MockServer::start();
        let test_result = create_test_result();

        let error_mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .header("Content-Type", "application/json")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        let lines: Vec<_> = body
                            .split(|&b| b == b'\n')
                            .filter(|l| !l.is_empty())
                            .collect();
                        if lines.len() != 1 {
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

        let (producer, worker) = VectorResultsProducer::new(
            "uptime-results",
            mock_server.url("/").to_string(),
            TEST_BATCH_SIZE,
            TEST_MAX_RETRIES,
        );
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Wait a bit for the requests to be processed
        sleep(Duration::from_millis(300)).await;

        // Drop the producer which will close the channel
        drop(producer);

        // Now we can await the worker
        let _ = worker.await;

        // Verify that the server was called 3 times
        error_mock.assert_hits(3); // 1 initial attempt + 2 retries
    }
}
