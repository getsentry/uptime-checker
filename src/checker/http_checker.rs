use chrono::{TimeDelta, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, ClientBuilder, Response};
use sentry::protocol::{SpanId, TraceId};
use std::error::Error;
use tokio::time::Instant;
use uuid::Uuid;

use crate::config_store::Tick;
use crate::types::{
    check_config::CheckConfig,
    result::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo},
};

use super::ip_filter::is_external_ip;
use super::Checker;

const UPTIME_USER_AGENT: &str =
    "SentryUptimeBot/1.0 (+http://docs.sentry.io/product/alerts/uptime-monitoring/)";

/// Responsible for making HTTP requests to check if a domain is up.
#[derive(Clone, Debug)]
pub struct HttpChecker {
    client: Client,
}

struct Options {
    /// When set to true (the default) resolution to internal network addresses will be restricted.
    /// This should primarily be disabled for tests.
    validate_url: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { validate_url: true }
    }
}

/// Fetches the response from a URL.
///
/// First attempts to fetch just the head, and if not supported falls back to fetching the entire body.
async fn do_request(
    client: &Client,
    check_config: &CheckConfig,
    sentry_trace: &str,
) -> Result<Response, reqwest::Error> {
    let timeout = check_config
        .timeout
        .to_std()
        .expect("Timeout duration could not be converted to std::time::Duration");

    let url = check_config.url.as_str();

    let headers: HeaderMap = check_config
        .request_headers
        .clone()
        .into_iter()
        .filter_map(|(key, value)| {
            // Try to convert key and value to HeaderName and HeaderValue
            let header_name = HeaderName::try_from(key).ok()?;
            let header_value = HeaderValue::from_str(&value).ok()?;
            Some((header_name, header_value))
        })
        .collect();

    client
        .request(check_config.request_method.into(), url)
        .timeout(timeout)
        .headers(headers)
        .header("sentry-trace", sentry_trace.to_owned())
        .body(check_config.request_body.to_owned())
        .send()
        .await
}

/// Check if the request error is a DNS error.
fn dns_error(err: &reqwest::Error) -> Option<String> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        inner = source;

        if let Some(inner_err) = source.downcast_ref::<hickory_resolver::error::ResolveError>() {
            return Some(format!("{}", inner_err));
        }
    }
    None
}

impl HttpChecker {
    fn new_internal(options: Options) -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert("User-Agent", UPTIME_USER_AGENT.to_string().parse().unwrap());

        let mut builder = ClientBuilder::new()
            .hickory_dns(true)
            .default_headers(default_headers);

        if options.validate_url {
            builder = builder.ip_filter(is_external_ip);
        }

        let client = builder.build().expect("Failed to build checker client");

        Self { client }
    }

    pub fn new() -> Self {
        Self::new_internal(Default::default())
    }
}

fn make_trace_header(config: &CheckConfig, trace_id: TraceId, span_id: SpanId) -> String {
    /// Format the 'sentry-trace' header. if we append a 0 to the header,
    /// we're indicating the trace spans will not be sampled.
    /// if we don't append a 0, then the default behavior is to sample the trace spans
    /// according to the service's sampling policy. see
    /// https://develop.sentry.dev/sdk/telemetry/traces/#header-sentry-trace
    /// for more information.
    if config.trace_sampling {
        format!("{}-{}", trace_id, span_id)
    } else {
        format!("{}-{}-{}", trace_id, span_id, '0')
    }
}

impl Checker for HttpChecker {
    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    #[tracing::instrument]
    async fn check_url(&self, config: &CheckConfig, tick: &Tick) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();
        let trace_id = TraceId::default();
        let span_id = SpanId::default();


        
        let trace_header = make_trace_header(config, trace_id, span_id);

        let start = Instant::now();
        let response = do_request(&self.client, config, &trace_header).await;
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
            request_type: config.request_method,
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
                        description: format!("{:?}", e.without_url()),
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
    use crate::checker::http_checker::make_trace_header;
    use crate::checker::Checker;
    use crate::config_store::Tick;
    use crate::types::check_config::CheckConfig;
    use crate::types::result::{CheckStatus, CheckStatusReasonType};
    use crate::types::shared::RequestMethod;

    use super::{HttpChecker, Options, UPTIME_USER_AGENT};
    use chrono::{TimeDelta, Utc};
    use httpmock::prelude::*;
    use httpmock::Method;
    use sentry::protocol::{SpanId, TraceId};

    fn make_tick() -> Tick {
        Tick::from_time(Utc::now() - TimeDelta::seconds(60))
    }

    #[tokio::test]
    async fn test_default_get() {
        let server = MockServer::start();
        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });

        let get_mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/no-head")
                .header_exists("sentry-trace")
                .header("User-Agent", UPTIME_USER_AGENT.to_string());
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
            Some(RequestMethod::Get)
        );

        get_mock.assert();
    }

    #[tokio::test]
    async fn test_configured_post() {
        let server = MockServer::start();
        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });

        let get_mock = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/no-head")
                .header_exists("sentry-trace")
                .body("{\"key\":\"value\"}")
                .header("User-Agent", UPTIME_USER_AGENT.to_string())
                .header("Authorization", "Bearer my-token".to_string())
                .header("X-My-Custom-Header", "value".to_string());
            then.status(200);
        });

        let config = CheckConfig {
            url: server.url("/no-head").to_string(),
            request_method: RequestMethod::Post,
            request_headers: vec![
                ("Authorization".to_string(), "Bearer my-token".to_string()),
                ("X-My-Custom-Header".to_string(), "value".to_string()),
            ],
            request_body: "{\"key\":\"value\"}".to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&config, &tick).await;

        assert_eq!(result.status, CheckStatus::Success);
        assert_eq!(
            result.request_info.as_ref().map(|i| i.request_type),
            Some(RequestMethod::Post)
        );

        get_mock.assert();
    }
    #[tokio::test]
    async fn test_simple_timeout() {
        static TIMEOUT: i64 = 200;

        let server = MockServer::start();

        let timeout = TimeDelta::milliseconds(TIMEOUT);
        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });

        let timeout_mock = server.mock(|when, then| {
            when.method(Method::GET)
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
        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });

        let head_mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/get")
                .header_exists("sentry-trace");
            then.status(400);
        });

        let config = CheckConfig {
            url: server.url("/get").to_string(),
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

    #[tokio::test]
    async fn test_restricted_resolution() {
        let checker = HttpChecker::new_internal(Options { validate_url: true });

        let localhost_config = CheckConfig {
            url: "http://localhost/whatever".to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let result = checker.check_url(&localhost_config, &tick).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);

        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::DnsError)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("destination is restricted".to_string())
        );
    }

    #[tokio::test]
    async fn test_validate_url() {
        let checker = HttpChecker::new_internal(Options { validate_url: true });

        // Private address space
        let restricted_ip_config = CheckConfig {
            url: "http://10.0.0.1/".to_string(),
            ..Default::default()
        };
        let tick = make_tick();
        let result = checker.check_url(&restricted_ip_config, &tick).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);

        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::DnsError)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("destination is restricted".to_string())
        );

        // Unique Local Address
        let restricted_ipv6_config = CheckConfig {
            url: "http://[fd12:3456:789a:1::1]/".to_string(),
            ..Default::default()
        };
        let tick = make_tick();
        let result = checker.check_url(&restricted_ipv6_config, &tick).await;
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("destination is restricted".to_string())
        );
    }

    #[tokio::test]
    async fn test_trace_sampling() {
        let trace_id = TraceId::default();
        let span_id = SpanId::default();
    
        // Test with sampling disabled
        let config = CheckConfig {
            url: "http://localhost/".to_string(),
            trace_sampling: false,
            ..Default::default()
        };
        let trace_header = make_trace_header(&config, trace_id, span_id);
        assert_eq!(trace_header, format!("{}-{}-0", trace_id, span_id));
    
        // Test with sampling enabled
        let config_with_sampling = CheckConfig {
            url: "http://localhost/".to_string(),
            trace_sampling: true,
            ..Default::default()
        };
        let trace_header_sampling = make_trace_header(&config_with_sampling, trace_id, span_id);
        assert_eq!(trace_header_sampling, format!("{}-{}", trace_id, span_id));
    }
    // TODO: Figure out how to simulate a DNS failure
    // assert_eq!(check_domain(&client, "https://hjkhjkljkh.io/".to_string()).await, CheckResult::FAILURE(FailureReason::DnsError("failed to lookup address information: nodename nor servname provided, or not known".to_string())));
}
