use crate::producer::{ExtractCodeError, ResultsProducer};
use crate::types::result::CheckResult;
use reqwest::Client;
use sentry_kafka_schemas::Schema;

pub struct VectorResultsProducer {
    schema: Schema,
    client: Client,
    endpoint: String,
}

impl VectorResultsProducer {
    pub fn new(topic_name: &str) -> Self {
        let schema = sentry_kafka_schemas::get_schema(topic_name, None).unwrap();
        let client = Client::new();
        Self { 
            schema, 
            client,
            endpoint: "http://localhost:8020".to_string()
        }
    }
}



impl ResultsProducer for VectorResultsProducer {
    fn produce_checker_result(&self, result: &CheckResult) -> Result<(), ExtractCodeError> {
        let json = serde_json::to_vec(result)?;
        self.schema.validate_json(&json)?;
        
        // Spawn a tokio task to make the async request
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();

        tokio::spawn(async move {
            // Make POST request to Vector
            let response = client
                .post(&endpoint)
                .body(json)
                .send()
                .await
                .map_err(ExtractCodeError::VectorRequestError)?;

            if !response.status().is_success() {
                return Err(ExtractCodeError::VectorRequestStatusError(response.status()));
            }

            Ok(())
        });

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

        let mut producer = VectorResultsProducer::new("uptime-results");
        producer.endpoint = mock_server.url("/");

        let result = producer.produce_checker_result(&expected_result);
        assert!(result.is_ok());

        // Give the async task time to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        
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

        let mut producer = VectorResultsProducer::new("uptime-results");
        producer.endpoint = mock_server.url("/");

        let result = producer.produce_checker_result(&test_result);
        
        // The initial result should be Ok since the error happens in the spawned task
        assert!(result.is_ok());

        // Give the async task time to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        
        error_mock.assert();
    }
}
