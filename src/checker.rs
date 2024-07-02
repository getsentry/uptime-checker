use chrono::{TimeDelta, Utc};
use reqwest::{Client, ClientBuilder, Response, StatusCode};
use sentry::protocol::{SpanId, TraceId};
use std::error::Error;
use tokio::time::Instant;
use uuid::Uuid;

use crate::config_store::Tick;
use crate::types::{
    check_config::CheckConfig,
    result::{
        CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo,
        RequestType,
    },
};

/// Responsible for making HTTP requests to check if a domain is up.
#[derive(Clone, Debug)]
pub struct Checker {
    client: Client,
}

/// Fetches the response from a URL.
///
/// First attempts to fetch just the head, and if not supported falls back to fetching the entire body.
async fn do_request(
    client: &Client,
    check_config: &CheckConfig,
    sentry_trace: &str,
) -> (RequestType, Result<Response, reqwest::Error>) {
    let timeout = check_config
        .timeout
        .to_std()
        .expect("Timeout duration could not be converted to std::time::Duration");

    let head_response = match client
        .head(&check_config.url)
        .timeout(timeout)
        .header("sentry-trace", sentry_trace.to_owned())
        .send()
        .await
    {
        Ok(response) => response,
        Err(e) => return (RequestType::Head, Err(e)),
    };

    // Successful head request
    if head_response.status() != StatusCode::METHOD_NOT_ALLOWED {
        return (RequestType::Head, Ok(head_response));
    }

    let result = client
        .get(check_config.url.as_str())
        .timeout(timeout)
        .header("sentry-trace", sentry_trace.to_owned())
        .send()
        .await;

    (RequestType::Get, result)
}

/// Check if the request error is a DNS error.
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
    pub fn new() -> Self {
        let client = ClientBuilder::new()
            .build()
            .expect("Failed to build checker client");

        Self { client }
    }

    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    pub async fn check_url(&self, config: &CheckConfig, tick: &Tick) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();

        let trace_id = TraceId::default();
        let span_id = SpanId::default();

        // Format the 'sentry-trace' header. The last byte indicates that we are NOT forcing
        // sampling on child spans. If we were to set this to '1' it would mean that every
        // check request made for that customer would be sampled,
        let trace_header = format!("{}-{}-{}", trace_id, span_id, '0');

        let start = Instant::now();
        let (request_type, response) = do_request(&self.client, config, &trace_header).await;
        let duration = Some(TimeDelta::from_std(start.elapsed()).unwrap());

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
            subscription_id: config.subscription_id,
            status,
            status_reason,
            trace_id,
            span_id,
            scheduled_check_time,
            actual_check_time,
            duration,
            request_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config_store::Tick;
    use crate::types::check_config::CheckConfig;
    use crate::types::result::{CheckStatus, CheckStatusReasonType, RequestType};

    use super::Checker;
    use chrono::{TimeDelta, Utc};
    use httpmock::prelude::*;
    use httpmock::Method;
    use reqwest::StatusCode;

    fn make_tick() -> Tick {
        Tick::from_time(Utc::now() - TimeDelta::seconds(60))
    }

    #[tokio::test]
    async fn test_simple_head() {
        let server = MockServer::start();
        let checker = Checker::new();

        let duration = TimeDelta::milliseconds(50);

        let head_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/head");
            then.delay(duration.to_std().unwrap()).status(200);
        });

        let config = CheckConfig {
            url: server.url("/head").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&config, &tick).await;

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
        assert_eq!(result.scheduled_check_time, tick.time());
        assert!(result.actual_check_time > tick.time());
        assert!(result.duration.is_some_and(|d| d >= duration));

        head_mock.assert();
    }

    #[tokio::test]
    async fn test_simple_get() {
        let server = MockServer::start();
        let checker = Checker::new();

        let head_disallowed_mock = server.mock(|when, then| {
            when.method(Method::HEAD).path("/no-head");
            then.status(StatusCode::METHOD_NOT_ALLOWED.as_u16());
        });
        let get_mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/no-head")
                .header_exists("sentry-trace");
            then.status(200);
        });

        let config = CheckConfig {
            url: server.url("/no-head").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&config, &tick).await;

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
        static TIMEOUT: i64 = 200;

        let server = MockServer::start();

        let timeout = TimeDelta::milliseconds(TIMEOUT);
        let checker = Checker::new();

        let timeout_mock = server.mock(|when, then| {
            when.method(Method::HEAD)
                .path("/timeout")
                .header_exists("sentry-trace");
            then.delay((timeout + TimeDelta::milliseconds(200)).to_std().unwrap())
                .status(200);
        });

        let config = CheckConfig {
            url: server.url("/timeout").to_string(),
            timeout,
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&config, &tick).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert!(result.duration.is_some_and(|d| d > timeout));
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
        let checker = Checker::new();

        let head_mock = server.mock(|when, then| {
            when.method(Method::HEAD)
                .path("/head")
                .header_exists("sentry-trace");
            then.status(400);
        });

        let config = CheckConfig {
            url: server.url("/head").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&config, &tick).await;

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
