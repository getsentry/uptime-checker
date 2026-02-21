pub mod dummy_checker;
pub mod ip_filter;
pub mod reqwest_checker;

use reqwest_checker::ReqwestChecker;
use sentry::protocol::SpanId;
use std::future::Future;
use uuid::Uuid;

use crate::{
    check_executor::ScheduledCheck,
    checker::dummy_checker::DummyChecker,
    config_store::Tick,
    types::{check_config::CheckConfig, result::CheckResult},
};

const CHECKER_RESULT_NAMESPACE: Uuid = Uuid::from_u128(0x67f0b2d5_e476_4f00_9b99_9e6b95c3b7e3);

/// Generate the Trace ID (uuid) for a check at a specific tick. This ensures the ID is consistent
/// for the same tick, allowing us to deduplicate check results that are produced at the same
/// scheduled time.
pub fn make_trace_id(config: &CheckConfig, tick: &Tick, retry_num: u16) -> Uuid {
    let unique_key = format!("{}-{}-{}", config.subscription_id, tick.time(), retry_num);
    Uuid::new_v5(&CHECKER_RESULT_NAMESPACE, unique_key.as_bytes())
}

/// Produce the Sentry Trace header value.
fn make_trace_header(config: &CheckConfig, trace_id: &Uuid, span_id: SpanId) -> String {
    // Format the 'sentry-trace' header. if we append a 0 to the header,
    // we're indicating the trace spans will not be sampled.
    // if we don't append a 0, then the default behavior is to sample the trace spans
    // according to the service's sampling policy. see
    // https://develop.sentry.dev/sdk/telemetry/traces/#header-sentry-trace
    // for more information.

    if config.trace_sampling {
        format!("{}-{}", trace_id.simple(), span_id)
    } else {
        format!("{}-{}-{}", trace_id.simple(), span_id, '0')
    }
}

/// A Checker is responsible for actually making checks given a [`CheckConfig`] and the [`Tick`] at
/// which the check is being made.
pub trait Checker: Send + Sync {
    /// Makes a request to a url to determine whether it is up.
    /// Up is defined as responding within a specific timeframe, optionally validated by assertions.
    fn check_url(
        &self,
        check: &ScheduledCheck,
        region: &'static str,
    ) -> impl Future<Output = CheckResult> + Send;

    fn check_robots(
        &self,
        check: &ScheduledCheck,
        region: &'static str,
    ) -> impl Future<Output = Option<CheckResult>> + Send;
}

#[derive(Debug)]
pub enum HttpChecker {
    ReqwestChecker(ReqwestChecker),
    DummyChecker(DummyChecker),
}

impl HttpChecker {
    pub async fn check_url(&self, check: &ScheduledCheck, region: &'static str) -> CheckResult {
        match self {
            HttpChecker::ReqwestChecker(c) => c.check_url(check, region).await,
            HttpChecker::DummyChecker(c) => c.check_url(check, region).await,
        }
    }

    pub async fn check_robots(
        &self,
        check: &ScheduledCheck,
        region: &'static str,
    ) -> Option<CheckResult> {
        match self {
            HttpChecker::ReqwestChecker(c) => c.check_robots(check, region).await,
            HttpChecker::DummyChecker(c) => c.check_robots(check, region).await,
        }
    }
}

impl From<ReqwestChecker> for HttpChecker {
    fn from(checker: ReqwestChecker) -> Self {
        HttpChecker::ReqwestChecker(checker)
    }
}

impl From<DummyChecker> for HttpChecker {
    fn from(checker: DummyChecker) -> Self {
        HttpChecker::DummyChecker(checker)
    }
}
