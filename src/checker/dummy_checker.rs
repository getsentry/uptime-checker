use std::time::Duration;

use chrono::Utc;
use sentry::protocol::SpanId;
use tokio::time;
use uuid::Uuid;

use crate::checker::Checker;
use crate::config_store::Tick;
use crate::types::check_config::CheckConfig;
use crate::types::result::{CheckResult, CheckStatus};

#[derive(Clone, Debug)]
pub struct DummyChecker {
    delay: Option<Duration>,
}

impl DummyChecker {
    pub fn new(delay: impl Into<Option<Duration>>) -> Self {
        Self {
            delay: delay.into(),
        }
    }
}

impl Checker for DummyChecker {
    async fn check_url(&self, config: &CheckConfig, tick: &Tick, region: &str) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();
        let trace_id = Uuid::new_v4();
        let span_id = SpanId::default();
        let duration = None;
        let status = CheckStatus::Success;
        let status_reason = None;
        let request_info = None;

        if let Some(delay) = self.delay {
            time::sleep(delay).await;
        }

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
            region: region.to_string(),
        }
    }
}
