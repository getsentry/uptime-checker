use super::ip_filter::is_external_ip;
use super::{make_trace_header, make_trace_id, Checker};
use crate::check_executor::ScheduledCheck;
use crate::types::result::{to_request_info_list, RequestDurations, Timing};
use crate::types::{
    check_config::CheckConfig,
    result::{CheckResult, CheckStatus, CheckStatusReason, CheckStatusReasonType, RequestInfo},
};
use chrono::{TimeDelta, Utc};
use openssl::error::ErrorStack;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, ClientBuilder, Response, Url};
use sentry::protocol::SpanId;
use std::error::Error;
use std::net::IpAddr;
use std::time::Duration;
use texting_robots::Robot;
use tokio::time::{timeout, Instant};
const UPTIME_USER_AGENT: &str =
    "SentryUptimeBot/1.0 (+http://docs.sentry.io/product/alerts/uptime-monitoring/)";

/// Responsible for making HTTP requests to check if a domain is up.
#[derive(Clone, Debug)]
pub struct ReqwestChecker {
    client: Client,
}

struct Options {
    /// When set to true (the default) resolution to internal network addresses will be restricted.
    /// This should primarily be disabled for tests.
    validate_url: bool,

    /// When set to true sets the pool_max_idle_per_host to 0. Effectively removing connection
    /// pooling and forcing a new connection for each new request. This may help reduce connection
    /// errors due to connections being held open too long.
    disable_connection_reuse: bool,

    pool_idle_timeout: Duration,
    dns_nameservers: Option<Vec<IpAddr>>,

    /// Specifies the network interface to bind the client to.
    interface: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            validate_url: true,
            disable_connection_reuse: false,
            pool_idle_timeout: Duration::from_secs(90),
            dns_nameservers: None,
            interface: None,
        }
    }
}

/// Fetches the response from a URL.
async fn do_request(
    client: &Client,
    check_config: &CheckConfig,
    sentry_trace: &str,
) -> Result<Response, reqwest::Error> {
    let timeout = check_config
        .timeout
        .to_std()
        .expect("Timeout duration should be representable as a duration");

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
            return Some(format!("{inner_err}"));
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
fn hyper_util_error(err: &reqwest::Error) -> Option<(CheckStatusReasonType, String)> {
    let mut inner = &err as &dyn Error;
    while let Some(source) = inner.source() {
        if let Some(hyper_util_error) = source.downcast_ref::<hyper_util::client::legacy::Error>() {
            if hyper_util_error.is_connect() {
                return Some((
                    CheckStatusReasonType::ConnectionError,
                    hyper_util_error.to_string(),
                ));
            }
            return Some((CheckStatusReasonType::Failure, hyper_util_error.to_string()));
        }
        inner = source;
    }
    None
}

impl ReqwestChecker {
    fn new_internal(options: Options) -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            "User-Agent",
            UPTIME_USER_AGENT
                .to_string()
                .parse()
                .expect("Valid by construction"),
        );

        let mut builder = ClientBuilder::new()
            .hickory_dns(true)
            .default_headers(default_headers)
            .pool_idle_timeout(options.pool_idle_timeout);

        builder = builder.tls_info(true);

        if options.validate_url {
            builder = builder.ip_filter(is_external_ip);
        }
        if let Some(dns_nameservers) = options.dns_nameservers {
            builder = builder.dns_nameservers(dns_nameservers)
        }
        if options.disable_connection_reuse {
            builder = builder.pool_max_idle_per_host(0);
        }
        #[cfg(not(target_os = "linux"))]
        if options.interface.is_some() {
            tracing::info!("HTTP Client interface can only be configured for the linux platform");
        }
        #[cfg(target_os = "linux")]
        if let Some(nic) = options.interface {
            builder = builder.interface(&nic);
        }

        let client = builder.build().expect("builder should be buildable");

        Self { client }
    }

    pub fn new(
        validate_url: bool,
        disable_connection_reuse: bool,
        pool_idle_timeout: Duration,
        dns_nameservers: Option<Vec<IpAddr>>,
        interface: Option<String>,
    ) -> Self {
        Self::new_internal(Options {
            validate_url,
            disable_connection_reuse,
            pool_idle_timeout,
            dns_nameservers,
            interface,
        })
    }
}

impl Checker for ReqwestChecker {
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
        let duration = TimeDelta::from_std(start.elapsed()).unwrap();

        let status = if response.as_ref().is_ok_and(|r| r.status().is_success()) {
            CheckStatus::Success
        } else {
            CheckStatus::Failure
        };

        // TODO: this is how to extract the leaf cert from the request we run.

        // let cert = response
        //     .as_ref()
        //     .map(|resp| resp.extensions().get::<reqwest::tls::TlsInfo>());

        // if let Ok(Some(cert)) = cert {
        //     eprintln!(
        //         "got a cert: {}",
        //         cert.peer_certificate().map_or(12345, |bytes| bytes.len())
        //     );
        // }

        let http_status_code = match &response {
            Ok(r) => Some(r.status().as_u16()),
            Err(e) => e.status().map(|s| s.as_u16()),
        };

        let rinfos = match &response {
            Ok(resp) => to_request_info_list(resp.stats(), check.get_config().request_method),
            Err(err) => {
                // This is a best-effort at getting timings for the individual bits of a connection-oriented error.
                // Surfacing the timings for each part of DNS/TCP connect/TLS negotiation _in the event of a
                // connection error_ will require some effort, so for now, just bill the full time to the part that
                // we failed on, leaving the others at zero.
                let zero_timing = Timing {
                    start_us: actual_check_time.timestamp_micros() as u128,
                    duration_us: 0,
                };

                let full_duration = Timing {
                    start_us: actual_check_time.timestamp_micros() as u128,
                    duration_us: duration.num_microseconds().unwrap() as u64,
                };

                let mut dns_timing = zero_timing;
                let mut connection_timing = zero_timing;
                let mut tls_timing = zero_timing;
                let mut send_request_timing = zero_timing;

                if dns_error(err).is_some() {
                    dns_timing = full_duration
                } else if connection_error(err).is_some() {
                    connection_timing = full_duration
                } else if tls_error(err).is_some() {
                    tls_timing = full_duration
                } else {
                    send_request_timing = full_duration
                };

                vec![RequestInfo {
                    http_status_code,
                    request_type: check.get_config().request_method,
                    request_body_size_bytes: check.get_config().request_body.len() as u32,
                    url: check.get_config().url.clone(),
                    response_body_size_bytes: 0,
                    request_duration_us: duration.num_microseconds().unwrap() as u64,
                    durations: RequestDurations {
                        dns_lookup: dns_timing,
                        tcp_connection: connection_timing,
                        tls_handshake: tls_timing,
                        time_to_first_byte: zero_timing,
                        send_request: send_request_timing,
                        receive_response: zero_timing,
                    },
                    certificate_info: None,
                }]
            }
        };

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
                } else if e.is_redirect() {
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::RedirectError,
                        description: "Too many redirects".to_string(),
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
                } else if let Some((status_type, message)) = hyper_util_error(&e) {
                    CheckStatusReason {
                        status_type,
                        description: message,
                    }
                } else {
                    // if any error falls through we should log it,
                    // none should fall through.
                    let error_msg = e.without_url();
                    tracing::info!("check_url.error: {:?}", error_msg);
                    CheckStatusReason {
                        status_type: CheckStatusReasonType::Failure,
                        description: format!("{error_msg:?}"),
                    }
                }
            }),
        };

        let final_req = rinfos.last().unwrap().clone();

        CheckResult {
            guid: trace_id,
            subscription_id: check.get_config().subscription_id,
            status,
            status_reason,
            trace_id,
            span_id,
            scheduled_check_time,
            scheduled_check_time_us: scheduled_check_time,
            actual_check_time,
            actual_check_time_us: actual_check_time,
            duration: Some(duration),
            duration_us: Some(duration),
            request_info: Some(final_req),
            region,
            request_info_list: rinfos,
        }
    }

    async fn check_robots(
        &self,
        check: &ScheduledCheck,
        region: &'static str,
    ) -> Option<CheckResult> {
        let Ok(url) = check.get_config().url.parse::<Url>() else {
            return None;
        };

        let mut robots_url = url.clone();
        robots_url.set_path("robots.txt");
        let robots_txt = {
            // Request a robots.txt, bounding both the get call as well as the body-stream to a 10 second
            // window.
            let start_time = Instant::now();
            let time_allotment = Duration::from_secs(10);
            let res = self
                .client
                .get(robots_url)
                .timeout(time_allotment)
                .send()
                .await;

            let Ok(mut res) = res else {
                tracing::debug!("could not retrieve robots.txt");
                return None;
            };

            let mut all_bytes = vec![];
            loop {
                let maybe_timeout =
                    timeout(time_allotment - start_time.elapsed(), res.chunk()).await;
                let Ok(maybe_conn_err) = maybe_timeout else {
                    tracing::info!("waited too long for robots.txt");
                    break all_bytes;
                };
                let Ok(maybe_chunk) = maybe_conn_err else {
                    tracing::info!("connection error during robots.txt");
                    break all_bytes;
                };
                let Some(chunk) = maybe_chunk else {
                    break all_bytes;
                };
                all_bytes.extend_from_slice(&chunk);

                if all_bytes.len() > 100_000 {
                    tracing::info!(url = %url, "aborting huge robots.txt");
                    break all_bytes;
                }
            }
        };

        let Ok(r) = Robot::new("SentryUptimeBot", &robots_txt) else {
            tracing::info!("Could not create Robot");
            return None;
        };

        if r.allowed(url.as_str()) {
            return None;
        }

        let scheduled_check_time = check.get_tick().time();
        let actual_check_time = Utc::now();
        let span_id = SpanId::default();
        let trace_id = make_trace_id(check.get_config(), check.get_tick(), check.get_retry());

        Some(CheckResult {
            guid: trace_id,
            subscription_id: check.get_config().subscription_id,
            status: CheckStatus::DisallowedByRobots,
            status_reason: None,
            trace_id,
            span_id,
            scheduled_check_time,
            scheduled_check_time_us: scheduled_check_time,
            actual_check_time,
            actual_check_time_us: actual_check_time,
            duration: None,
            duration_us: None,
            request_info: None,
            region,
            request_info_list: Vec::new(),
        })
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
    use std::net::IpAddr;
    use std::time::Duration;

    use super::{make_trace_header, Options, ReqwestChecker, UPTIME_USER_AGENT};
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
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
    async fn test_restricted_resolution() {
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: true,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
        });

        let localhost_config = CheckConfig {
            url: "http://localhost/whatever".to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, localhost_config);
        let result = checker.check_url(&check, "us-west").await;

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
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: true,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
        });

        // Private address space
        let restricted_ip_config = CheckConfig {
            url: "http://10.0.0.1/".to_string(),
            ..Default::default()
        };
        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, restricted_ip_config);
        let result = checker.check_url(&check, "us-west").await;

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
        let check = ScheduledCheck::new_for_test(tick, restricted_ipv6_config);
        let result = checker.check_url(&check, "us-west").await;

        assert_eq!(
            result.status_reason.map(|r| r.description),
            Some("destination is restricted".to_string())
        );
    }

    #[tokio::test]
    async fn test_same_check_same_id() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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

        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
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
            let check = ScheduledCheck::new_for_test(tick, config);
            let result = checker.check_url(&check, "us-west").await;

            assert_eq!(
                result.status,
                CheckStatus::Failure,
                "Test case: {cert_type:?}",
            );
            assert_eq!(
                result.request_info.and_then(|i| i.http_status_code),
                None,
                "Test case: {cert_type:?}",
            );
            assert_eq!(
                result.status_reason.as_ref().map(|r| r.status_type),
                Some(CheckStatusReasonType::TlsError),
                "Test case: {cert_type:?}",
            );
            assert_eq!(
                result.status_reason.map(|r| r.description).unwrap(),
                expected_msg,
                "Test case: {cert_type:?}",
            );
        }
    }

    #[tokio::test]
    async fn test_connection_refused() {
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
            Some("Connection refused".to_string())
        );
    }

    #[tokio::test]
    async fn test_too_many_redirects() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
    async fn test_robots_deny_sentry_site() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
        });

        let redirect_mock = server.mock(|when, then| {
            when.method(Method::GET).path("/robots.txt");

            then.status(200)
                .body("User-agent: SentryUptimeBot\nDisallow: *\n");
        });

        let config = CheckConfig {
            url: server.url("/foo").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_robots(&check, "us-west").await.unwrap();

        assert_eq!(result.status, CheckStatus::DisallowedByRobots);

        // Verify that the mock was called once
        redirect_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn test_robots_deny_all_site() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
        });

        let redirect_mock = server.mock(|when, then| {
            when.method(Method::GET).path("/robots.txt");

            then.status(200).body("User-agent: *\nDisallow: *\n");
        });

        let config = CheckConfig {
            url: server.url("/foo").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_robots(&check, "us-west").await.unwrap();

        assert_eq!(result.status, CheckStatus::DisallowedByRobots);

        // Verify that the mock was called once
        redirect_mock.assert_hits(1);
    }

    #[tokio::test]
    async fn test_robots_allow_site() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
            interface: None,
        });

        // Create a redirect loop where each request redirects back to itself
        let redirect_mock = server.mock(|when, then| {
            when.method(Method::GET).path("/robots.txt");

            then.status(200)
                .body("User-agent: SentryUptimeBot\nDisallow: /bar/\n");
        });

        let config = CheckConfig {
            url: server.url("/foo").to_string(),
            ..Default::default()
        };

        let tick = make_tick();
        let check = ScheduledCheck::new_for_test(tick, config);
        let result = checker.check_robots(&check, "us-west").await;

        assert!(result.is_none());

        // Verify that the mock was called once
        redirect_mock.assert_hits(1);
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

        let checker = ReqwestChecker::new_internal(Options {
            validate_url: false,
            disable_connection_reuse: true,
            dns_nameservers: None,
            pool_idle_timeout: Duration::from_secs(90),
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
        assert!(
            result.status_reason.as_ref().map(|r| r.status_type)
                == Some(CheckStatusReasonType::ConnectionError)
                || result.status_reason.as_ref().map(|r| r.status_type)
                    == Some(CheckStatusReasonType::Failure)
        );
    }

    #[tokio::test]
    async fn test_dns_nameservers() {
        let server = MockServer::start();
        let checker = ReqwestChecker::new_internal(Options {
            dns_nameservers: Some(vec![IpAddr::from([42, 55, 6, 8])]),
            validate_url: false,
            ..Default::default()
        });

        let get_mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/no-head")
                .header_exists("sentry-trace")
                .header("User-Agent", UPTIME_USER_AGENT.to_string());
            then.status(200);
        });

        let config = CheckConfig {
            // We explicitly use localhost to trigger dns resolution
            url: format!("http://localhost:{}/no-head", server.port()),
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
}
