use super::ip_filter::PRIVATE_RANGES;
use super::{make_trace_header, make_trace_id, Checker};
use crate::check_executor::ScheduledCheck;
use crate::types::{
    check_config::CheckConfig,
    result::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo},
};
use chrono::{TimeDelta, Utc};
use http::Method;
use ipnet::{Ipv4Net, Ipv6Net};
use iprange::IpRange;
use isahc::config::{Configurable, NetworkInterface, RedirectPolicy};
use isahc::error::ErrorKind;
use isahc::{AsyncBody, HttpClient, Request, Response};
use sentry::protocol::SpanId;
use std::time::Duration;
use tokio::time::Instant;

const UPTIME_USER_AGENT: &str =
    "SentryUptimeBot/1.0 (+http://docs.sentry.io/product/alerts/uptime-monitoring/)";

/// Responsible for making HTTP requests to check if a domain is up.
#[derive(Clone, Debug)]
pub struct IsahcChecker {
    client: HttpClient,
}

struct Options {
    /// When set to true (the default) resolution to internal network addresses will be restricted.
    /// This should primarily be disabled for tests.
    validate_url: bool,

    /// Specifies the network interface to bind the client to.
    interface: Option<String>,

    /// Set the maximum time-to-live (TTL) for connections to remain in the connection cache.
    connection_cache_ttl: Duration,

    /// When set to true sets the connection_cache_size to 0. Effectively removing connection
    /// pooling and forcing a new connection for each new request. This may help reduce connection
    /// errors due to connections being held open too long.
    disable_connection_reuse: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            validate_url: true,
            interface: None,
            connection_cache_ttl: Duration::from_secs(90),
            disable_connection_reuse: false,
        }
    }
}

/// Fetches the response from a URL.
async fn do_request(
    client: &HttpClient,
    check_config: &CheckConfig,
    sentry_trace: &str,
) -> Result<Response<AsyncBody>, isahc::Error> {
    let timeout = check_config
        .timeout
        .to_std()
        .expect("Timeout duration should be representable as a duration");

    let url = check_config.url.as_str();

    let mut request_builder = Request::builder()
        .timeout(timeout)
        .method(Method::from(check_config.request_method).as_str())
        .uri(url)
        .header("sentry-trace", sentry_trace.to_owned());

    for (key, value) in &check_config.request_headers {
        request_builder = request_builder.header(key, value);
    }

    let request = request_builder.body(check_config.request_body.to_owned())?;

    client.send_async(request).await
}

impl IsahcChecker {
    fn new_internal(options: Options) -> Self {
        let mut builder = HttpClient::builder()
            .default_headers(&[("User-Agent", UPTIME_USER_AGENT)])
            .connection_cache_ttl(options.connection_cache_ttl)
            .redirect_policy(RedirectPolicy::Limit(9));

        if options.disable_connection_reuse {
            builder = builder.connection_cache_size(0);
        }

        if options.validate_url {
            let ipv4_blacklist: IpRange<Ipv4Net> = PRIVATE_RANGES
                .iter()
                .filter_map(|r| match r.addr() {
                    std::net::IpAddr::V4(_) => Some(r.to_string().parse().unwrap()),
                    std::net::IpAddr::V6(_) => None,
                })
                .collect();

            let ipv6_blacklist: IpRange<Ipv6Net> = PRIVATE_RANGES
                .iter()
                .filter_map(|r| match r.addr() {
                    std::net::IpAddr::V4(_) => None,
                    std::net::IpAddr::V6(_) => Some(r.to_string().parse().unwrap()),
                })
                .collect();
            builder = builder.ip_blacklists(ipv4_blacklist, ipv6_blacklist);
        }

        if let Some(nic) = options.interface {
            builder = builder.interface(NetworkInterface::name(nic))
        }

        let client = builder.build().expect("Failed to build checker client");

        Self { client }
    }

    pub fn new(
        validate_url: bool,
        disable_connection_reuse: bool,
        connection_cache_ttl: Duration,
        interface: Option<String>,
    ) -> Self {
        Self::new_internal(Options {
            validate_url,
            disable_connection_reuse,
            connection_cache_ttl,
            interface,
        })
    }
}

impl Checker for IsahcChecker {
    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as returning a 2xx within a specific timeframe.
    #[tracing::instrument]
    async fn check_url(&self, check: &ScheduledCheck, region: &'static str) -> CheckResult {
        let scheduled_check_time = check.get_tick().time();
        let actual_check_time = Utc::now();
        let span_id = SpanId::default();
        let trace_id = make_trace_id(check.get_config(), check.get_tick(), check.get_retry());
        let trace_header = make_trace_header(check.get_config(), &trace_id, span_id);

        let start = Instant::now();
        let response = do_request(&self.client, check.get_config(), &trace_header).await;
        let duration = Some(TimeDelta::from_std(start.elapsed()).expect("Duration should be sane"));

        let status = if response.as_ref().is_ok_and(|r| r.status().is_success()) {
            CheckStatus::Success
        } else {
            CheckStatus::Failure
        };

        let http_status_code = match &response {
            Ok(r) => Some(r.status().as_u16()),
            Err(_) => None,
        };

        let request_info = Some(RequestInfo {
            http_status_code,
            request_type: check.get_config().request_method,
        });

        let status_reason = match response {
            Ok(r) if r.status().is_success() => None,
            Ok(r) => Some(CheckStatusReason {
                status_type: CheckStatusReasonType::Failure,
                description: format!("Got non 2xx status: {}", r.status()),
            }),
            Err(e) => Some({
                match e.kind() {
                    ErrorKind::Timeout => CheckStatusReason {
                        status_type: CheckStatusReasonType::Timeout,
                        description: e.to_string(),
                    },
                    ErrorKind::TooManyRedirects => CheckStatusReason {
                        status_type: CheckStatusReasonType::RedirectError,
                        description: "Too many redirects".to_string(),
                    },
                    ErrorKind::NameResolution => CheckStatusReason {
                        status_type: CheckStatusReasonType::DnsError,
                        description: e.to_string(),
                    },
                    ErrorKind::BadClientCertificate
                    | ErrorKind::BadServerCertificate
                    | ErrorKind::TlsEngine => CheckStatusReason {
                        status_type: CheckStatusReasonType::TlsError,
                        description: e.to_string(),
                    },
                    ErrorKind::ConnectionFailed | ErrorKind::Io => CheckStatusReason {
                        status_type: CheckStatusReasonType::ConnectionError,
                        description: e.to_string(),
                    },
                    _ => {
                        // if any error falls through we should log it,
                        // none should fall through.
                        let error_msg = e.to_string();
                        tracing::info!("check_url.error: {:?}", error_msg);
                        CheckStatusReason {
                            status_type: CheckStatusReasonType::Failure,
                            description: format!("{:?}", error_msg),
                        }
                    }
                }
            }),
        };

        CheckResult {
            guid: trace_id,
            subscription_id: check.get_config().subscription_id,
            status,
            status_reason,
            trace_id,
            span_id,
            scheduled_check_time,
            actual_check_time,
            duration,
            request_info,
            region,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::check_executor::ScheduledCheck;
    use crate::checker::Checker;
    use crate::config_store::Tick;
    use crate::types::check_config::CheckConfig;
    use crate::types::result::{CheckStatus, CheckStatusReasonType};
    use crate::types::shared::RequestMethod;
    use std::time::Duration;

    use super::{make_trace_header, IsahcChecker, Options, UPTIME_USER_AGENT};
    use chrono::{TimeDelta, Utc};
    use httpmock::prelude::*;
    use httpmock::Method;

    use sentry::protocol::SpanId;
    use tokio::io::AsyncReadExt;
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
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
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
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;

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
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
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
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;

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
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
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
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert!(result.duration.is_some_and(|d| d > timeout));
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.description.clone()),
            Some("request or operation took longer than the configured timeout time".to_string())
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
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
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
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;

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
    async fn test_blocked_ranges() {
        let checker = IsahcChecker::new_internal(Options {
            validate_url: true,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });

        let test_urls: Vec<_> = vec![
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "[::ffff:0:1]",
            "[::ffff:a00:1]",
            "[::ffff:6440:1]",
            "[::ffff:7f00:1]",
            "[::ffff:a9fe:1]",
            "[::ffff:ac10:1]",
            "[::ffff:c000:1]",
            "[::ffff:c000:200]",
            "[::ffff:c058:6300]",
            "[::ffff:c0a8:1]",
            "[::ffff:c612:1]",
            "[::ffff:c633:6400]",
            "[::ffff:e000:1]",
            "[::ffff:f000:1]",
            "[::ffff:ffff:ffff]",
            "[::1]",
            "[::ffff:0:0:1]",
            "[64:ff9b::1]",
            "[64:ff9b:1::1]",
            "[100::1]",
            "[2001:0000::1]",
            "[2001:20::1]",
            "[2001:db8::1]",
            "[2002::1]",
            "[fc00::1]",
            "[fe80::1]",
            "[ff00::1]",
        ]
        .iter()
        .map(|ip| format!("http://{ip}/get"))
        .collect();

        for url in test_urls {
            let tick = make_tick();
            let config = CheckConfig {
                url: url.to_owned(),
                ..Default::default()
            };
            let check = ScheduledCheck::new_for_test(tick, config);
            let result = checker.check_url(&check, "us-west".into()).await;

            assert_eq!(result.status, CheckStatus::Failure);
        }
    }

    #[tokio::test]
    async fn test_restricted_resolution() {
        let checker = IsahcChecker::new_internal(Options {
            validate_url: true,
            disable_connection_reuse: true,
            interface: None,
            connection_cache_ttl: Duration::from_secs(90),
        });

        let localhost_config = CheckConfig {
            url: "http://localhost/whatever".to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, localhost_config);
        let result = checker.check_url(&check, "us-west".into()).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);

        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
    }

    #[tokio::test]
    async fn test_validate_url() {
        let checker = IsahcChecker::new_internal(Options {
            validate_url: true,
            disable_connection_reuse: true,
            interface: None,
            connection_cache_ttl: Duration::from_secs(90),
        });

        // Private address space
        let restricted_ip_config = CheckConfig {
            url: "http://10.0.0.1/".to_string(),
            ..Default::default()
        };
        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, restricted_ip_config);
        let result = checker.check_url(&check, "us-west".into()).await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);

        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );

        // Unique Local Address
        let restricted_ipv6_config = CheckConfig {
            url: "http://[fd12:3456:789a:1::1]/".to_string(),
            ..Default::default()
        };
        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, restricted_ipv6_config);
        let result = checker.check_url(&check, "us-west".into()).await;
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
    }

    #[tokio::test]
    async fn test_same_check_same_id() {
        let server = MockServer::start();
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
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
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;
        let result_2 = checker.check_url(&check, "us-west").await;

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
        assert_eq!(trace_header, format!("{}-{}-0", trace_id.simple(), span_id));
        assert_eq!(trace_header.to_string().matches("-").count(), 2);

        // Test with sampling enabled
        let config_with_sampling = CheckConfig {
            url: "http://localhost/".to_string(),
            trace_sampling: true,
            ..Default::default()
        };
        let trace_header_sampling = make_trace_header(&config_with_sampling, &trace_id, span_id);
        assert_eq!(
            trace_header_sampling,
            format!("{}-{}", trace_id.simple(), span_id)
        );
        assert_eq!(trace_header_sampling.to_string().matches("-").count(), 1);
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

        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });
        let tick = make_tick();

        // Test various SSL certificate errors
        let test_cases = vec![
            (
                TestCertType::Expired,
                "the server certificate could not be validated",
            ),
            (
                TestCertType::WrongHost,
                "the server certificate could not be validated",
            ),
            (
                TestCertType::SelfSigned,
                "the server certificate could not be validated",
            ),
        ];

        for (cert_type, expected_msg) in test_cases {
            let server_url = setup_test_server(cert_type).await;

            let config = CheckConfig {
                url: server_url,
                ..Default::default()
            };

            let check = ScheduledCheck::new_for_test(tick, config);
            let result = checker.check_url(&check, "us-west").await;

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
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });
        let tick = make_tick();
        let config = CheckConfig {
            url: "http://localhost:12345/".to_string(),
            ..Default::default()
        };
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;
        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("failed to connect to the server".to_string())
        );
    }

    #[tokio::test]
    async fn test_too_many_redirects() {
        let server = MockServer::start();
        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });

        // Create a redirect loop where each request redirects back to itself
        let redirect_mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/redirect")
                .header_exists("sentry-trace");
            then.status(302)
                .header("Location", server.url("/redirect").as_str());
        });

        let config = CheckConfig {
            url: server.url("/redirect").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;
        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::RedirectError)
        );
        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("Too many redirects".to_string())
        );

        // Verify that the mock was called at least once
        // The reqwest client follows redirects multiple times before giving up
        redirect_mock.assert_hits(10);
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

        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });
        let tick = make_tick();
        let config = CheckConfig {
            url: format!("http://localhost:{}", addr.port()),
            ..Default::default()
        };
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west").await;

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
        let result_description = result.status_reason.map(|r| r.description).unwrap();
        assert_eq!(
            result_description, "unknown error",
            "Expected error message about closed connection: {}",
            result_description
        );
    }

    #[tokio::test]
    async fn test_ipv6_accept() {
        // Other tests implicitly test whether or not we can query an ipv4 address; given the
        // way ip blocking works, it's a good idea to also test that we can hit an ipv6 address
        // as well.
        let listener = tokio::net::TcpListener::bind("::1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buff = [0; 8];
            stream.read(&mut buff).await.unwrap();
        });

        let checker = IsahcChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            connection_cache_ttl: Duration::from_secs(90),
            interface: None,
        });
        let tick = make_tick();
        let config = CheckConfig {
            url: format!("http://[::1]:{}", addr.port()),
            ..Default::default()
        };
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_url(&check, "us-west".into()).await;
        eprintln!("{:?}", result);

        assert_eq!(result.status, CheckStatus::Failure);
        assert_eq!(result.request_info.and_then(|i| i.http_status_code), None);
        assert_eq!(
            result.status_reason.as_ref().map(|r| r.status_type),
            Some(CheckStatusReasonType::ConnectionError)
        );
        let result_description = result.status_reason.map(|r| r.description).unwrap();
        assert_eq!(
            result_description, "unknown error",
            "Expected error message about closed connection: {}",
            result_description
        );
    }
}
