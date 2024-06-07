use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    Failure,
}

/// The type of HTTP request used for the check
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequestType {
    Head,
    Get,
}

/// Captures the reason for a check's given status
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckStatusReason {
    /// The type of the status reason
    #[serde(rename = "type")]
    pub status_type: CheckStatusReasonType,

    /// A human readable description of the status reason
    pub description: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RequestInfo {
    /// The type of HTTP method used for the check
    pub request_type: RequestType,

    /// The status code of the response. May be empty when the request did not receive a response
    /// whatsoever.
    pub http_status_code: Option<u16>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckResult {
    /// Unique identifier of the uptime check
    #[serde(serialize_with = "uuid_simple")]
    pub guid: Uuid,

    /// The identifier of the uptime monitor
    pub monitor_id: u64,

    /// The identifier of the uptime monitors environment
    pub monitor_environment_id: u64,

    /// The status of the check
    pub status: CheckStatus,

    /// Reason for the status, primairly used for failure
    pub status_reason: Option<CheckStatusReason>,

    /// Trace ID associated with the check-in made
    #[serde(serialize_with = "uuid_simple")]
    pub trace_id: Uuid,

    /// Timestamp in milliseconds of when the check was schedule to run
    pub scheduled_check_time: u64,

    /// Timestamp in milliseconds of when the check was actually ran
    pub actual_check_time: u64,

    /// Duration of the check in ms. Will be null when the status is missed_window
    pub duration_ms: Option<u128>,

    /// Information about the check request made. Will be empty if the check was missed
    pub request_info: Option<RequestInfo>,
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    #[test]
    fn serialize_json_roundtrip_failure_example() {
        let json = r#"{
  "guid": "54afc7ed9c53491481919c931f75bae1",
  "monitor_id": 1,
  "monitor_environment_id": 1,
  "status": "failure",
  "status_reason": {
    "type": "dns_error",
    "description": "Unable to resolve hostname example.xyz"
  },
  "trace_id": "947efba02dac463b9c1d886a44bafc94",
  "scheduled_check_time": 1717614062978,
  "actual_check_time": 1717614068008,
  "duration_ms": 100,
  "request_info": {
    "request_type": "HEAD",
    "http_status_code": 500
  }
}"#;

        let check_result = serde_json::from_str::<CheckResult>(json).unwrap();
        let serialized = serde_json::to_string_pretty(&check_result).unwrap();

        assert_eq!(json, serialized);
    }

    #[test]
    fn serialize_json_roundtrip_success_example() {
        let json = r#"{
  "guid": "54afc7ed9c53491481919c931f75bae1",
  "monitor_id": 1,
  "monitor_environment_id": 1,
  "status": "success",
  "status_reason": null,
  "trace_id": "947efba02dac463b9c1d886a44bafc94",
  "scheduled_check_time": 1717614062978,
  "actual_check_time": 1717614068008,
  "duration_ms": 50,
  "request_info": {
    "request_type": "HEAD",
    "http_status_code": 200
  }
}"#;

        let check_result = serde_json::from_str::<CheckResult>(json).unwrap();
        let serialized = serde_json::to_string_pretty(&check_result).unwrap();

        assert_eq!(json, serialized);
    }
}
