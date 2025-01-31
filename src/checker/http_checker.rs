use super::ip_filter::is_external_ip;
use super::Checker;
use crate::config_store::Tick;
use crate::types::{
    check_config::CheckConfig,
    result::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo},
};
use chrono::{TimeDelta, Utc};
use openssl::error::ErrorStack;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, ClientBuilder, Response};
use sentry::protocol::SpanId;
use std::error::Error;
use tokio::time::Instant;
use uuid::Uuid;

pub const CHECKER_RESULT_NAMESPACE: Uuid = Uuid::from_u128(0x67f0b2d5_e476_4f00_9b99_9e6b95c3b7e3);

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
fn tls_error(err: &reqwest::Error) -> Option<String> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        if let Some(e) = source.downcast_ref::<ErrorStack>() {
            return Some(
                e.errors()
                    .iter()
                    .map(|e| e.reason().unwrap_or("unknown error"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        inner = source;
    }
    None
}

fn connection_error(err: &reqwest::Error) -> Option<String> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        if let Some(io_err) = source.downcast_ref::<std::io::Error>() {
            // TODO: should we return the OS code error as well?
            if io_err.kind() == std::io::ErrorKind::ConnectionRefused {
                return Some("Connection refused".to_string());
            } else if io_err.kind() == std::io::ErrorKind::ConnectionReset {
                return Some("Connection reset".to_string());
            }
        }
        inner = source;
    }
    None
}

fn hyper_error(err: &reqwest::Error) -> Option<(CheckStatusReasonType, String)> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        if let Some(hyper_error) = source.downcast_ref::<hyper::Error>() {
            if hyper_error.is_incomplete_message() {
                return Some((
                    CheckStatusReasonType::ConnectionError,
                    hyper_error.to_string(),
                ));
            }
            return Some((CheckStatusReasonType::Failure, hyper_error.to_string()));
        }
        inner = source;
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

        let client = builder
            .pool_max_idle_per_host(0)
            .build()
            .expect("Failed to build checker client");

        Self { client }
    }

    pub fn new(validate_url: bool) -> Self {
        Self::new_internal(Options { validate_url })
    }
}

fn make_trace_header(config: &CheckConfig, trace_id: &Uuid, span_id: SpanId) -> String {
    // Format the 'sentry-trace' header. if we append a 0 to the header,
    // we're indicating the trace spans will not be sampled.
    // if we don't append a 0, then the default behavior is to sample the trace spans
    // according to the service's sampling policy. see
    // https://develop.sentry.dev/sdk/telemetry/traces/#header-sentry-trace
    // for more information.
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
    async fn check_url(&self, config: &CheckConfig, tick: &Tick, region: &str) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();
        let span_id = SpanId::default();
        let unique_key = format!("{}-{}", config.subscription_id, scheduled_check_time);
        let guid = Uuid::new_v5(&CHECKER_RESULT_NAMESPACE, unique_key.as_bytes());
        let trace_header = make_trace_header(config, &guid, span_id);

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
                        description: "Request timed out".to_string(),
                    }
                } else if let Some(message) = dns_error(&e) {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::DnsError,
                        description: message,
                    }
                } else if let Some(message) = tls_error(&e) {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::TlsError,
                        description: message,
                    }
                } else if let Some(message) = connection_error(&e) {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::ConnectionError,
                        description: message,
                    }
                } else if let Some((status_type, message)) = hyper_error(&e) {
                    CheckStatusReason {
                        status_type,
                        description: message,
                    }
                } else {
                    // if any error falls through we should log it,
                    // none should be.
                    let error_msg = e.without_url();
                    tracing::info!("check_url.error: {:?}", error_msg);
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::Failure,
                        description: format!("{:?}", error_msg),
                    }
                }
            }),
        };

        CheckResult {
            guid,
            subscription_id: config.subscription_id,
            status,
            status_reason,
            trace_id: guid,
            span_id,
            scheduled_check_time,
            actual_check_time,
            duration,
            request_info,
            region: region.to_string(),
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

    use sentry::protocol::SpanId;
    use uuid::Uuid;
    #[cfg(target_os = "linux")]
    use {
        rcgen::{Certificate, CertificateParams},
        rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        rustls::ServerConfig,
        std::sync::Arc,
        tokio::io::AsyncWriteExt,
        tokio::net::TcpListener,
        tokio_rustls::TlsAcceptor,
    };

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
        let result = checker.check_url(&config, &tick, "us-west").await;

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
        let result = checker.check_url(&config, &tick, "us-west").await;

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
        let result = checker.check_url(&config, &tick, "us-west").await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert!(result.duration.is_some_and(|d| d > timeout));
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.description.clone()),
            Some("Request timed out".to_string())
        );
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
        let result = checker.check_url(&config, &tick, "us-west").await;

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
        let result = checker.check_url(&localhost_config, &tick, "us-west").await;

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
        let result = checker
            .check_url(&restricted_ip_config, &tick, "us-west")
            .await;

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
        let result = checker
            .check_url(&restricted_ipv6_config, &tick, "us-west")
            .await;
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("destination is restricted".to_string())
        );
    }

    #[tokio::test]
    async fn test_same_check_same_id() {
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
        let result = checker.check_url(&config, &tick, "us-west").await;
        let result_2 = checker.check_url(&config, &tick, "us-west").await;

        assert_eq!(result.status, CheckStatus::Success);
        assert_eq!(result_2.status, CheckStatus::Success);
        assert_eq!(result.guid, result_2.guid);
        assert_eq!(result.trace_id, result_2.trace_id);
        get_mock.assert_hits(2);
    }

    #[tokio::test]
    async fn test_trace_sampling() {
        let trace_id = Uuid::new_v4();
        let span_id = SpanId::default();

        // Test with sampling disabled
        let config = CheckConfig {
            url: "http://localhost/".to_string(),
            trace_sampling: false,
            ..Default::default()
        };
        let trace_header = make_trace_header(&config, &trace_id, span_id);
        assert_eq!(trace_header, format!("{}-{}-0", trace_id, span_id));

        // Test with sampling enabled
        let config_with_sampling = CheckConfig {
            url: "http://localhost/".to_string(),
            trace_sampling: true,
            ..Default::default()
        };
        let trace_header_sampling = make_trace_header(&config_with_sampling, &trace_id, span_id);
        assert_eq!(trace_header_sampling, format!("{}-{}", trace_id, span_id));
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn test_ssl_errors_linux() {
        #[derive(Debug, Copy, Clone)]
        enum TestCertType {
            Expired,
            WrongHost,
            SelfSigned,
        }
        // Helper function to create various bad certificates
        fn create_bad_cert(cert_type: TestCertType) -> (Vec<u8>, Vec<u8>) {
            let mut params = CertificateParams::new(vec!["localhost".to_string()]);
            match cert_type {
                TestCertType::Expired => {
                    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(30);
                    params.not_after = time::OffsetDateTime::now_utc() - time::Duration::days(1);
                }
                TestCertType::WrongHost => {
                    params = CertificateParams::new(vec!["wronghost.com".to_string()]);
                }
                TestCertType::SelfSigned => {
                    // Default params are self-signed
                }
            }

            let cert = Certificate::from_params(params).unwrap();
            (
                cert.serialize_private_key_der(),
                cert.serialize_der().unwrap(),
            )
        }

        // Set up mock HTTPS server
        async fn setup_test_server(cert_type: TestCertType) -> String {
            let (key_der, cert_der) = create_bad_cert(cert_type);

            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
            let cert = vec![CertificateDer::from(cert_der)];

            let server_config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert, key)
                .unwrap();

            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    tokio::spawn(async move {
                        if let Ok(mut tls_stream) = acceptor.accept(stream).await {
                            // Simple HTTP response
                            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                            let _ = tls_stream.write_all(response).await;
                        }
                    });
                }
            });

            format!("https://localhost:{}", addr.port())
        }

        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });
        let tick = make_tick();

        // Test various SSL certificate errors
        let test_cases = vec![
            (TestCertType::Expired, "certificate verify failed"),
            (TestCertType::WrongHost, "certificate verify failed"),
            (TestCertType::SelfSigned, "certificate verify failed"),
        ];

        for (cert_type, expected_msg) in test_cases {
            let server_url = setup_test_server(cert_type).await;

            let config = CheckConfig {
                url: server_url,
                ..Default::default()
            };

            let result = checker.check_url(&config, &tick, "us-west").await;

            assert_eq!(
                result.status,
                CheckStatus::Failure,
                "Test case: {:?}",
                &cert_type
            );
            assert_eq!(
                result.request_info.and_then(|i| i.http_status_code),
                None,
                "Test case: {:?}",
                cert_type
            );
            assert_eq!(
                result.status_reason.as_ref().map(|r| r.status_type),
                Some(CheckStatusReasonType::TlsError),
                "Test case: {:?}",
                cert_type
            );
            assert_eq!(
                result.status_reason.map(|r| r.description).unwrap(),
                expected_msg,
                "Test case: {:?}",
                cert_type
            );
        }
    }

    #[tokio::test]
    async fn test_connection_refused() {
        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });
        let tick = make_tick();
        let config = CheckConfig {
            url: "http://localhost:12345/".to_string(),
            ..Default::default()
        };
        let result = checker.check_url(&config, &tick, "us-west").await;
        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("Connection refused".to_string())
        );
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn test_connection_reset() {
        // Set up a TCP listener that sends an incomplete response
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                // Send an incomplete HTTP response - just the headers
                let partial_response: &[u8; 38] = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n";
                let _ = stream.write_all(partial_response).await;
                // Close connection immediately without sending body or final \r\n
            }
        });

        let checker = HttpChecker::new_internal(Options {
            validate_url: false,
        });
        let tick = make_tick();
        let config = CheckConfig {
            url: format!("http://localhost:{}", addr.port()),
            ..Default::default()
        };
        let result = checker.check_url(&config, &tick, "us-west").await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
        let result_description = result.status_reason.map(|r| r.description).unwrap();
        assert_eq!(
            result_description, "connection closed before message completed",
            "Expected error message about closed connection: {}",
            result_description
        );
    }
}
