use std::time::Duration;

use chrono::Utc;
use sentry::protocol::SpanId;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::RwLock;
use tokio::time;
use uuid::Uuid;

use crate::checker::Checker;
use crate::config_store::Tick;
use crate::types::check_config::CheckConfig;
use crate::types::result::{CheckResult, CheckStatus};

/// A DummyReuslt can be used to configure the DummyChecker with results to produce
#[derive(Clone, Debug)]
pub struct DummyResult {
    pub delay: Option<Duration>,
    pub status: CheckStatus,
}

impl Default for DummyResult {
    fn default() -> Self {
        Self {
            delay: None,
            status: CheckStatus::Success,
        }
    }
}

#[derive(Debug)]
pub struct DummyChecker {
    sender: UnboundedSender<DummyResult>,
    results: RwLock<UnboundedReceiver<DummyResult>>,
}

impl DummyChecker {
    pub fn new() -> Self {
        let (sender, reciever) = mpsc::unbounded_channel();
        Self {
            sender,
            results: RwLock::new(reciever),
        }
    }

    /// Add a result to the queue for when
    pub fn queue_result(&self, result: DummyResult) {
        self.sender
            .send(result)
            .expect("Failed to queue dummy result");
    }
}

impl Checker for DummyChecker {
    async fn check_url(
        &self,
        config: &CheckConfig,
        tick: &Tick,
        region: &str,
        _retry_num: u16,
    ) -> CheckResult {
        let scheduled_check_time = tick.time();
        let actual_check_time = Utc::now();
        let trace_id = Uuid::new_v4();
        let span_id = SpanId::default();
        let duration = None;
        let status_reason = None;
        let request_info = None;

        // Get queued results to yield
        let result = self.results.write().await.recv().await.unwrap_or_default();

        if let Some(delay) = result.delay {
            time::sleep(delay).await;
        }

        CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: config.subscription_id,
            status: result.status,
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
