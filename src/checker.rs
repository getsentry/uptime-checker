pub mod dummy_checker;
pub mod ip_filter;
pub mod reqwest_checker;

use std::future::Future;

use chrono::Timelike;
use reqwest_checker::ReqwestChecker;
use sentry::protocol::SpanId;
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
    /// Up is defined as returning a 2xx within a specific timeframe.
    fn check_url(
        &self,
        check: &ScheduledCheck,
        region: &'static str,
        check_robots: bool,
    ) -> impl Future<Output = CheckResult> + Send;
}

#[derive(Debug)]
pub enum HttpChecker {
    ReqwestChecker(ReqwestChecker),
    DummyChecker(DummyChecker),
}

// Check the robots.txt file every day, per subscription.
const ROBOTS_TXT_CHECK_INTERVAL_MINUTES: u128 = 60 * 24;

impl HttpChecker {
    pub async fn check_url(&self, check: &ScheduledCheck, region: &'static str) -> CheckResult {
        // Trigger a check of the robots.txt every 24 hrs at some particular minute of the day (per subscription).
        let daily_minute_to_check = (check.get_config().subscription_id.as_u128()
            % ROBOTS_TXT_CHECK_INTERVAL_MINUTES) as i32;
        let current_minute =
            (check.get_tick().time().minute() + check.get_tick().time().hour() * 60) as i32;
        let interval_in_minutes = check.get_config().interval as i32 / 60;

        // If the checker is really running slow/behind, we could wind up missing a check interval; this should
        // be rare, and so probably okay.
        let check_robots = current_minute - interval_in_minutes < daily_minute_to_check
            && current_minute >= daily_minute_to_check;

        match self {
            HttpChecker::ReqwestChecker(c) => c.check_url(check, region, check_robots).await,
            HttpChecker::DummyChecker(c) => c.check_url(check, region, check_robots).await,
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
