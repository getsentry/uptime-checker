use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::sync::oneshot;

const VECTOR_BATCH_SIZE: usize = 10;

pub struct VectorResultsProducer {
    schema: Schema,
    sender: UnboundedSender<Vec<u8>>,
    shutdown_sender: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl VectorResultsProducer {
    pub fn new(topic_name: &str) -> Self {
        Self::new_with_endpoint(topic_name, "http://localhost:8020".to_string())
    }

    pub fn new_with_endpoint(topic_name: &str, endpoint: String) -> Self {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        let client = Client::new();
        
        let (sender, receiver) = mpsc::unbounded_channel();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let worker = Self::spawn_worker(client, endpoint, receiver, shutdown_receiver);

        Self { 
            schema,
            sender,
            shutdown_sender: Some(shutdown_sender),
            worker: Some(worker),
        }
    }

    fn spawn_worker(
        client: Client, 
        endpoint: String, 
        mut receiver: UnboundedReceiver<Vec<u8>>,
        mut shutdown_receiver: oneshot::Receiver<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            tracing::debug!("Vector producer worker started with endpoint {}", endpoint);
            let mut batch = Vec::with_capacity(VECTOR_BATCH_SIZE);
            
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => {
                        tracing::debug!("Received shutdown signal");
                        break;
                    }
                    msg = receiver.recv() => {
                        match msg {
                            Some(json) => {
                                tracing::debug!("Received event of size {}", json.len());
                                batch.push(json);
                                tracing::debug!("Current batch size: {}", batch.len());
                                
                                // Send batch when it reaches BATCH_SIZE
                                if batch.len() >= VECTOR_BATCH_SIZE {
                                    tracing::debug!("Sending batch of {} events", batch.len());
                                    // Convert each JSON bytes to string and ensure each line ends with a newline
                                    let body = batch.iter()
                                        .filter_map(|json| String::from_utf8(json.clone()).ok())
                                        .map(|s| s + "\n")
                                        .collect::<String>();
                                    
                                    // Log the exact payload for debugging
                                    tracing::debug!("Payload being sent:\n{}", body);
                                    
                                    tracing::debug!("Sending request to Vector with body size {}", body.len());
                                    if let Err(e) = client
                                        .post(&endpoint)
                                        .header("Content-Type", "application/x-ndjson")
                                        .body(body)
                                        .send()
                                        .await
                                    {
                                        tracing::error!(error = ?e, "Failed to send batch request to Vector");
                                    } else {
                                        tracing::debug!("Successfully sent batch request");
                                    }
                                    batch.clear();
                                }
                            }
                            None => {
                                tracing::debug!("Channel closed");
                                break;
                            }
                        }
                    }
                }
            }
            
            // Send any remaining events in the batch
            if !batch.is_empty() {
                tracing::debug!("Sending final batch of {} events", batch.len());
                let body = batch.iter()
                    .filter_map(|json| String::from_utf8(json.clone()).ok())
                    .collect::<Vec<String>>()
                    .join("\n");
                
                // Log the exact payload for debugging
                tracing::debug!("Final payload being sent:\n{}", body);
                
                tracing::debug!("Sending final request to Vector with body size {}", body.len());
                if let Err(e) = client
                    .post(&endpoint)
                    .header("Content-Type", "application/x-ndjson")
                    .body(body)
                    .send()
                    .await
                {
                    tracing::error!(error = ?e, "Failed to send final batch request to Vector");
                } else {
                    tracing::debug!("Successfully sent final batch request");
                }
            }
            
            tracing::info!("Vector producer worker shutting down");
        })
    }

    pub async fn shutdown(&mut self) {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
            if let Some(worker) = self.worker.take() {
                let _ = worker.await;
            }
        }
    }
}

impl Drop for VectorResultsProducer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
    }
}

impl ResultsProducer for VectorResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;
        
        // Send the serialized result to the worker task
        if self.sender.send(json).is_err() {
            tracing::error!("Failed to send result to Vector worker - channel closed");
            return Err(ExtractCodeError::VectorRequestStatusError(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{VectorResultsProducer, VECTOR_BATCH_SIZE};
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
    use sentry::protocol::SpanId;
    use uuid::{uuid, Uuid};
    use httpmock::prelude::*;
    use httpmock::Method;
    use tokio::time::sleep;
    use std::time::Duration;

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
                .header("Content-Type", "application/x-ndjson")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!("Received request body: {:?}", String::from_utf8_lossy(body));
                        let lines: Vec<_> = body.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
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

        let mut producer = VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"));
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Wait a bit and then shut down
        sleep(Duration::from_millis(100)).await;
        producer.shutdown().await;
        mock.assert();
    }

    #[tokio::test]
    async fn test_batch_events() {
        let mock_server = MockServer::start();
        tracing::debug!("Mock server started at {}", mock_server.url("/"));
        let mut producer = VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"));
        
        // Create and send BATCH_SIZE + 2 events
        for i in 0..(VECTOR_BATCH_SIZE + 2) {
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
                        tracing::debug!("Received request body: {:?}", String::from_utf8_lossy(body));
                        let lines: Vec<_> = body.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
                        let len = lines.len();
                        if len != VECTOR_BATCH_SIZE && len != 2 {
                            tracing::debug!("Expected {} or 2 lines, got {}", VECTOR_BATCH_SIZE, len);
                            return false;
                        }
                        // Verify each line is valid JSON with expected fields
                        lines.iter().all(|line| {
                            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
                                return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
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

        // Wait a bit and then shut down
        sleep(Duration::from_millis(100)).await;
        producer.shutdown().await;
        
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
                .header("Content-Type", "application/x-ndjson")
                .matches(|req| {
                    if let Some(body) = &req.body {
                        tracing::debug!("Received request body: {:?}", String::from_utf8_lossy(body));
                        let lines: Vec<_> = body.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
                        if lines.len() != 1 {
                            tracing::debug!("Expected 1 line, got {}", lines.len());
                            return false;
                        }
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&lines[0]) {
                            return value["subscription_id"] == "23d6048d67c948d9a19c0b47979e9a03"
                                && value["status"] == "success"
                                && value["region"] == "us-west-1";
                        }
                    }
                    false
                });
            then.status(500)
                .body("Internal Server Error");
        });

        let mut producer = VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"));
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Wait a bit and then shut down
        sleep(Duration::from_millis(100)).await;
        producer.shutdown().await;
        error_mock.assert();
    }
}
