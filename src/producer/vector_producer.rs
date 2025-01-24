use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

pub struct VectorResultsProducer {
    schema: Schema,
    sender: UnboundedSender<Vec<u8>>,
    _worker: JoinHandle<()>,
}

impl VectorResultsProducer {
    pub fn new(topic_name: &str) -> Self {
        Self::new_with_endpoint(topic_name, "http://localhost:8020".to_string())
    }

    pub fn new_with_endpoint(topic_name: &str, endpoint: String) -> Self {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        let client = Client::new();
        
        let (sender, receiver) = mpsc::unbounded_channel();
        let worker = Self::spawn_worker(client, endpoint, receiver);

        Self { 
            schema,
            sender,
            _worker: worker,
        }
    }

    fn spawn_worker(client: Client, endpoint: String, mut receiver: UnboundedReceiver<Vec<u8>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(json) = receiver.recv().await {
                // Make POST request to Vector
                if let Err(e) = client
                    .post(&endpoint)
                    .body(json)
                    .send()
                    .await
                {
                    tracing::error!(error = ?e, "Failed to send request to Vector");
                    continue;
                }
            }
            tracing::info!("Vector producer worker shutting down");
        })
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
    use sentry::protocol::SpanId;
    use uuid::{uuid, Uuid};
    use httpmock::prelude::*;
    use httpmock::Method;
    use tokio::time::sleep;
    use std::time::Duration;

    #[tokio::test]
    async fn test() {
        // Start a mock server
        let mock_server = MockServer::start();

        // Create expected result to compare against
        let expected_result = CheckResult {
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
        };
        let json = serde_json::to_value(&expected_result).unwrap();
        let get_mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/")
                .json_body(json);
            then.status(200);
        });

        let producer = VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"));
        let result = producer.produce_checker_result(&expected_result);
        assert!(result.is_ok());

        // Give the worker task time to process the message
        sleep(Duration::from_millis(100)).await;
        
        get_mock.assert();
    }

    #[tokio::test]
    async fn test_server_error() {
        // Start a mock server
        let mock_server = MockServer::start();

        // Create test result
        let test_result = CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: uuid!("23d6048d67c948d9a19c0b47979e9a03"),
            status: CheckStatus::Success,
            status_reason: Some(CheckStatusReason {
                status_type: CheckStatusReasonType::DnsError,
                description: "test error".to_string(),
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
        };

        // Mock server returns 500 error
        let error_mock = mock_server.mock(|when, then| {
            when.method(Method::POST)
                .path("/");
            then.status(500)
                .body("Internal Server Error");
        });

        let producer = VectorResultsProducer::new_with_endpoint("uptime-results", mock_server.url("/"));
        let result = producer.produce_checker_result(&test_result);
        assert!(result.is_ok());

        // Give the worker task time to process the message
        sleep(Duration::from_millis(100)).await;
        
        error_mock.assert();
    }
}
