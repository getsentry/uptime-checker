use reqwest::{Client, ClientBuilder, Response, StatusCode};
use std::{error::Error, time::Duration};
use tokio::time::Instant;
use uuid::Uuid;

use crate::types::{
    CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo, RequestType,
};

pub struct CheckerConfig {
    /// How long will we wait before we consider the request to have timed out.
    pub timeout: Duration,
}

impl Default for CheckerConfig {
    fn default() -> CheckerConfig {
        CheckerConfig {
            timeout: Duration::from_secs(5),
        }
    }
}

/// Responsible for making HTTP requests to check if a domain is up.
#[derive(Clone, Debug)]
pub struct Checker {
    client: Client,
}

/// Fetches the response from a URL.
///
/// First attempts to fetch just the head, and if not supported falls back to fetching the entire body.
async fn do_request(client: &Client, url: &str) -> (RequestType, Result<Response, reqwest::Error>) {
    let head_response = match client.head(url).send().await {
        Ok(response) => response,
        Err(e) => return (RequestType::Head, Err(e)),
    };

    // Successful head request
    if head_response.status() != StatusCode::METHOD_NOT_ALLOWED {
        return (RequestType::Head, Ok(head_response));
    }

    let result = client.get(url).send().await;
    (RequestType::Get, result)
}

/// Check if the reqwest error is a DNS error.
fn dns_error(err: &reqwest::Error) -> Option<String> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        inner = source;

        // TODO: Would be better to get specific errors without string matching like this
        // Not sure if there's a better way
        let inner_message = inner.to_string();
        if inner_message.contains("dns error") {
            return Some(inner.source().unwrap().to_string());
        }
    }
    None
}

impl Checker {
    pub fn new(config: CheckerConfig) -> Self {
        let client = ClientBuilder::new()
            .timeout(config.timeout)
            .build()
            .expect("Failed to build checker client");

        Self { client }
    }

    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    pub async fn check_url(&self, url: &str) -> CheckResult {
        let trace_id = Uuid::new_v4();

        let start = Instant::now();
        let (request_type, response) = do_request(&self.client, url).await;
        let duration_ms = Some(start.elapsed().as_millis());

        let status = if response.as_ref().is_ok_and(|r| r.status().is_success()) {
            CheckStatus::Success
        } else {
            CheckStatus::Failure
        };

        let http_status_code = match &response {
            Ok(r) => Some(r.status().as_u16()),
            Err(e) => e.status().map(|s| s.as_u16()),
        };

        let request_info = Some(RequestInfo {
            http_status_code,
            request_type,
        });

        let status_reason = match response {
            Ok(r) if r.status().is_success() => None,
            Ok(r) => Some(CheckStatusReason {
                status_type: CheckStatusReasonType::Failure,
                description: format!("Got non 2xx status: {}", r.status()),
            }),
            Err(e) => Some({
                if e.is_timeout() {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::Timeout,
                        description: format!("{:?}", e),
                    }
                } else if let Some(message) = dns_error(&e) {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::DnsError,
                        description: message,
                    }
                } else {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::Failure,
                        description: format!("{:?}", e),
                    }
                }
            }),
        };

        CheckResult {
            guid: Uuid::new_v4(),
            monitor_id: 0,
            monitor_environment_id: 0,
            status,
            status_reason,
            trace_id,
            scheduled_check_time: 0,
            actual_check_time: 0,
            duration_ms,
            request_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{CheckStatus, CheckStatusReasonType, RequestType};

    use super::{Checker, CheckerConfig};
    use httpmock::prelude::*;
    use httpmock::Method;
    use reqwest::StatusCode;
    use std::time::Duration;
    // use crate::checker::FailureReason;

    #[tokio::test]
    async fn test_simple_head() {
        let server = MockServer::start();
        let checker = Checker::new(CheckerConfig::default());

        let head_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/head");
            then.delay(Duration::from_millis(50)).status(200);
        });

        let result = checker.check_url(&server.url("/head")).await;

        assert_eq!(result.status, CheckStatus::Success);
        assert!(result.status_reason.is_none());
        assert_eq!(
            result.request_info.as_ref().map(|i| i.request_type),
            Some(RequestType::Head)
        );
        assert_eq!(
            result.request_info.and_then(|i| i.http_status_code),
            Some(200)
        );
        assert!(result.duration_ms.unwrap_or(0) >= 50);

        head_mock.assert();
    }

    #[tokio::test]
    async fn test_simple_get() {
        let server = MockServer::start();
        let checker = Checker::new(CheckerConfig::default());

        let head_disallowed_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/no-head");
            then.status(StatusCode::METHOD_NOT_ALLOWED.as_u16());
        });
        let get_mock = server.mock(|when, then| {
            when.method(Method::GET).path("/no-head");
            then.status(200);
        });

        let result = checker.check_url(&server.url("/no-head")).await;

        assert_eq!(result.status, CheckStatus::Success);
        assert_eq!(
            result.request_info.as_ref().map(|i| i.request_type),
            Some(RequestType::Get)
        );

        head_disallowed_mock.assert();
        get_mock.assert();
    }

    #[tokio::test]
    async fn test_simple_timeout() {
        static TIMEOUT: u64 = 200;

        let server = MockServer::start();

        let timeout = Duration::from_millis(TIMEOUT);
        let checker = Checker::new(CheckerConfig { timeout });

        let timeout_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/timeout");
            then.delay(Duration::from_millis(TIMEOUT + 100)).status(200);
        });

        let result = checker.check_url(&server.url("/timeout")).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert!(result.duration_ms.unwrap_or(0) >= TIMEOUT as u128);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.map(|r| r.status_type),
            Some(CheckStatusReasonType::Timeout)
        );

        timeout_mock.assert();
    }

    #[tokio::test]
    async fn test_simple_400() {
        let server = MockServer::start();
        let checker = Checker::new(CheckerConfig::default());

        let head_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/head");
            then.status(400);
        });

        let result = checker.check_url(&server.url("/head")).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(
            result.request_info.and_then(|i| i.http_status_code),
            Some(400)
        );
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::Failure)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("Got non 2xx status: 400 Bad Request".to_string())
        );

        head_mock.assert();
    }

    // TODO: Figure out how to simulate a DNS failure
    // assert_eq!(check_domain(&client, "https://hjkhjkljkh.io/".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
}
