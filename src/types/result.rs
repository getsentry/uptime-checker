use chrono::{DateTime, TimeDelta, Utc};
use hyper::rt::ConnectionStats;
use hyper::stats::RequestStats;
use openssl::asn1::Asn1Time;
use openssl::x509::X509;
use sentry::protocol::SpanId;
use serde::{Deserialize, Serialize};
use serde_with::chrono;
use serde_with::serde_as;
use std::time::Instant;
use uuid::Uuid;

use super::shared::RequestMethod;

fn uuid_simple<S>(uuid: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    uuid.as_simple().serialize(serializer)
}

/// The status result of a check
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Success,
    Failure,
    MissedWindow,
}

/// The status reason result of a failed check
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatusReasonType {
    Timeout,
    DnsError,
    TlsError,
    ConnectionError,
    RedirectError,
    Failure,
}

/// Captures the reason for a check's given status
#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckStatusReason {
    /// The type of the status reason
    #[serde(rename = "type")]
    pub status_type: CheckStatusReasonType,

    /// A human readable description of the status reason
    pub description: String,
}

fn to_timing(
    reference_ts: &u128,
    reference_instant: &Instant,
    start: &Instant,
    end: &Instant,
) -> Timing {
    Timing {
        start_us: reference_ts + start.duration_since(*reference_instant).as_micros(),
        duration_us: (*end - *start).as_micros() as u64,
    }
}

pub fn to_request_info_list(stats: &RequestStats, method: RequestMethod) -> Vec<RequestInfo> {
    stats
        .redirects()
        .iter()
        .map(|rs| {
            let conn_stats = if let Some(cstats) = rs.get_http_stats().get_connection_stats() {
                *cstats
            } else {
                // If we don't have a connection stats, then it's a pooled connection.  Just invent a new connection stats, which
                // start up a connection at the moment we see the request go out.
                ConnectionStats::new_pooled(
                    rs.get_request_start(),
                    rs.get_request_start_timestamp(),
                )
            };

            // It's pretty hard to find out when "request goes on the wire" precisely happens,
            // so for now, we can pretend it happens right after the end of tcp connection or
            // (if it exists) tls negotiation.
            let latest_connection_stat = conn_stats
                .get_tls_end()
                .unwrap_or(conn_stats.get_connect_end());

            let certificate_info = rs
                .get_certificate_bytes()
                .and_then(|cert| X509::from_der(cert).ok())
                .map(|cert| {
                    let epoch_start = Asn1Time::from_unix(0).unwrap();

                    let not_after_diff = epoch_start.diff(cert.not_after()).unwrap();
                    let not_before_diff = epoch_start.diff(cert.not_before()).unwrap();

                    CertificateInfo {
                        not_after_timestamp_s: not_after_diff.secs as u64
                            + (not_after_diff.days as u64 * 24 * 60 * 60),
                        not_before_timestamp_s: not_before_diff.secs as u64
                            + (not_before_diff.days as u64 * 24 * 60 * 60),
                    }
                });

            RequestInfo {
                certificate_info,
                http_status_code: Some(rs.get_status_code()),
                request_type: method,
                request_body_size_bytes: rs.get_request_body_size(),
                url: rs.get_url().to_string(),
                response_body_size_bytes: 0,
                request_duration_us: rs
                    .get_request_end()
                    .duration_since(conn_stats.get_start_instant())
                    .as_micros() as u64,
                durations: RequestDurations {
                    dns_lookup: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats.get_dns_resolve_start(),
                        &conn_stats.get_dns_resolve_end(),
                    ),
                    tcp_connection: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats.get_connect_start(),
                        &conn_stats.get_connect_end(),
                    ),
                    tls_handshake: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats
                            .get_tls_start()
                            .unwrap_or(conn_stats.get_start_instant()),
                        &conn_stats
                            .get_tls_end()
                            .unwrap_or(conn_stats.get_start_instant()),
                    ),
                    time_to_first_byte: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats.get_start_instant(),
                        &rs.get_header_ttfb()
                            .unwrap_or(conn_stats.get_start_instant()),
                    ),
                    send_request: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats.get_start_instant(),
                        &latest_connection_stat,
                    ),
                    receive_response: to_timing(
                        &conn_stats.get_start_timestamp(),
                        &conn_stats.get_start_instant(),
                        &conn_stats.get_start_instant(),
                        &rs.get_request_end(),
                    ),
                },
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct RequestInfo {
    /// The type of HTTP method used for the check
    pub request_type: RequestMethod,

    /// The status code of the response. May be empty when the request did not receive a response
    /// whatsoever.
    pub http_status_code: Option<u16>,

    pub url: String,

    pub request_body_size_bytes: u32,

    pub response_body_size_bytes: u32,

    pub request_duration_us: u64,

    pub durations: RequestDurations,

    // Information about the leaf certificate retrieved as part of this
    // request, if any.
    pub certificate_info: Option<CertificateInfo>,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub struct RequestDurations {
    pub dns_lookup: Timing,

    pub tcp_connection: Timing,

    pub tls_handshake: Timing,

    pub time_to_first_byte: Timing,

    pub send_request: Timing,

    pub receive_response: Timing,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Copy, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub struct Timing {
    pub start_us: u128,
    pub duration_us: u64,
}

#[derive(Debug, PartialEq, Deserialize, Serialize, Copy, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub struct CertificateInfo {
    pub not_before_timestamp_s: u64,
    pub not_after_timestamp_s: u64,
}

#[serde_as]
#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckResult {
    /// Unique identifier of the uptime check
    #[serde(serialize_with = "uuid_simple")]
    pub guid: Uuid,

    /// The identifier of the subscription
    #[serde(serialize_with = "uuid_simple")]
    pub subscription_id: Uuid,

    /// The status of the check
    pub status: CheckStatus,

    /// Reason for the status, primarily used for failure
    pub status_reason: Option<CheckStatusReason>,

    /// Trace ID associated with the check-in made
    #[serde(serialize_with = "uuid_simple")]
    pub trace_id: Uuid,

    /// Span ID associated with the check-in made
    pub span_id: SpanId,

    /// Timestamp in milliseconds of when the check was schedule to run
    #[serde(rename = "scheduled_check_time_ms")]
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub scheduled_check_time: DateTime<Utc>,

    /// Timestamp in milliseconds of when the check was actually ran
    #[serde(rename = "actual_check_time_ms")]
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub actual_check_time: DateTime<Utc>,

    /// Duration of the check in ms. Will be null when the status is missed_window
    #[serde(rename = "duration_ms")]
    #[serde_as(as = "Option<serde_with::DurationMilliSeconds<i64>>")]
    pub duration: Option<TimeDelta>,

    /// Information about the check request made. Will be empty if the check was missed
    pub request_info: Option<RequestInfo>,

    // Information about all check requests made (as a result of redirections,) including
    // the final request.
    pub request_info_list: Vec<RequestInfo>,

    /// Region slug that produced the check result
    pub region: &'static str,
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    #[test]
    fn serialize_json_roundtrip() {
        let json = r#"{
  "guid": "54afc7ed9c53491481919c931f75bae1",
  "subscription_id": "23d6048d67c948d9a19c0b47979e9a03",
  "status": "failure",
  "status_reason": {
    "type": "dns_error",
    "description": "Unable to resolve hostname example.xyz"
  },
  "trace_id": "947efba02dac463b9c1d886a44bafc94",
  "span_id": "9c1d886a44bafc94",
  "scheduled_check_time_ms": 1717614062978,
  "actual_check_time_ms": 1717614068008,
  "duration_ms": 100,
  "request_info": {
    "request_type": "HEAD",
    "http_status_code": 500,
    "url": "http://www.santry.ayo",
    "request_body_size_bytes": 0,
    "response_body_size_bytes": 0,
    "request_duration_us": 0,
    "durations": {
      "dns_lookup": {
        "start_us": 0,
        "duration_us": 0
      },
      "tcp_connection": {
        "start_us": 1,
        "duration_us": 1
      },
      "tls_handshake": {
        "start_us": 2,
        "duration_us": 2
      },
      "time_to_first_byte": {
        "start_us": 3,
        "duration_us": 3
      },
      "send_request": {
        "start_us": 4,
        "duration_us": 4
      },
      "receive_response": {
        "start_us": 5,
        "duration_us": 5
      }
    }
  },
  "request_info_list": [],
  "region": "us-west-1"
}"#;

        let check_result = serde_json::from_str::<CheckResult>(json).unwrap();
        let serialized = serde_json::to_string_pretty(&check_result).unwrap();

        assert_eq!(json, serialized);
    }

    #[test]
    fn serialize_json_roundtrip_success_example() {
        let json = r#"{
  "guid": "54afc7ed9c53491481919c931f75bae1",
  "subscription_id": "23d6048d67c948d9a19c0b47979e9a03",
  "status": "success",
  "status_reason": null,
  "trace_id": "947efba02dac463b9c1d886a44bafc94",
  "span_id": "9c1d886a44bafc94",
  "scheduled_check_time_ms": 1717614062978,
  "actual_check_time_ms": 1717614068008,
  "duration_ms": 50,
  "request_info": {
    "request_type": "HEAD",
    "http_status_code": 200,
    "url": "http://www.santry.ayo",
    "request_body_size_bytes": 0,
    "response_body_size_bytes": 0,
    "request_duration_us": 0,
    "durations": {
      "dns_lookup": {
        "start_us": 0,
        "duration_us": 0
      },
      "tcp_connection": {
        "start_us": 1,
        "duration_us": 1
      },
      "tls_handshake": {
        "start_us": 2,
        "duration_us": 2
      },
      "time_to_first_byte": {
        "start_us": 3,
        "duration_us": 3
      },
      "send_request": {
        "start_us": 4,
        "duration_us": 4
      },
      "receive_response": {
        "start_us": 5,
        "duration_us": 5
      }
    }
  },
  "request_info_list": [],
  "region": "us-west-1"
}"#;

        let check_result = serde_json::from_str::<CheckResult>(json).unwrap();
        let serialized = serde_json::to_string_pretty(&check_result).unwrap();

        assert_eq!(json, serialized);
    }
}
