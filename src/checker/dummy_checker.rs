use crate::checker::Checker;
use crate::config_store::Tick;
use crate::types::check_config::CheckConfig;
use crate::types::result::{CheckResult, CheckStatus};
use chrono::Utc;
use sentry::protocol::{SpanId, TraceId};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct DummyChecker {}
impl DummyChecker {
    pub fn new() -> Self {
        Self {}
    }
}
impl Checker for DummyChecker {
    async fn check_url(&self, config: &CheckConfig, tick: &Tick) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();
        let trace_id = TraceId::default();
        let span_id = SpanId::default();
        let duration = None;
        let status = CheckStatus::Success;
        let status_reason = None;
        let request_info = None;

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

