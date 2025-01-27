use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

use crate::checker::Checker;
use crate::config_store::Tick;
use crate::producer::ResultsProducer;
use crate::types::check_config::CheckConfig;
use crate::types::result::{CheckResult, CheckStatus, CheckStatusReasonType};

#[derive(Debug)]
pub struct ScheduledCheck {
    tick: Tick,
    config: Arc<CheckConfig>,
    resolve_tx: Sender<CheckResult>,
}

impl ScheduledCheck {
    /// Get the scheduled CheckConfig.
    pub fn get_config(&self) -> &Arc<CheckConfig> {
        &self.config
    }

    /// Get the tick this check was scheduled at.
    pub fn get_tick(&self) -> &Tick {
        &self.tick
    }

    /// Report the completion of the scheduled check.
    pub fn record_result(self, result: CheckResult) {
        self.resolve_tx
            .send(result)
            .expect("Failed to resolve completed check");
    }
}

#[derive(Debug, Clone)]
pub struct CheckSender {
    sender: UnboundedSender<ScheduledCheck>,
    queue_size: Arc<AtomicU64>,
}

impl CheckSender {
    pub fn new() -> (Self, UnboundedReceiver<ScheduledCheck>) {
        let (sender, reciever) = mpsc::unbounded_channel();
        let queue_size = Arc::new(AtomicU64::new(0));
        let check_sender = Self { sender, queue_size };

        (check_sender, reciever)
    }
}

impl CheckResult {
    /// Produce a missed check result from a scheduled check.
    pub fn missed_from(config: &CheckConfig, tick: &Tick, region: &str) -> Self {
        Self {
            guid: Uuid::new_v4(),
            subscription_id: config.subscription_id,
            status: CheckStatus::MissedWindow,
            status_reason: None,
            trace_id: Default::default(),
            span_id: Default::default(),
            scheduled_check_time: tick.time(),
            actual_check_time: Utc::now(),
            duration: None,
            request_info: None,
            region: region.to_string(),
        }
    }
}

pub fn run_executor(
    concurrency: usize,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    region: String,
) -> (CheckSender, JoinHandle<()>) {
    tracing::info!("executor.starting");

    let (check_sender, receiver) = CheckSender::new();
    let queue_size = check_sender.queue_size.clone();

    let executor = tokio::spawn(async move {
        executor_loop(concurrency, queue_size, checker, producer, receiver, region).await
    });

    (check_sender, executor)
}

/// Queues a check for execution, returning a oneshot receiver that will be fired once the check
/// has resolved to a CheckResult.
pub fn queue_check(
    check_sender: &CheckSender,
    tick: Tick,
    config: Arc<CheckConfig>,
) -> Receiver<CheckResult> {
    let (resolve_tx, resolve_rx) = oneshot::channel();

    let scheduled_check = ScheduledCheck {
        tick,
        config,
        resolve_tx,
    };

    check_sender.queue_size.fetch_add(1, Ordering::SeqCst);
    check_sender
        .sender
        .send(scheduled_check)
        .expect("Failed to queue ScheduledCheck");

    resolve_rx
}

async fn executor_loop(
    concurrency: usize,
    queue_size: Arc<AtomicU64>,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    reciever: UnboundedReceiver<ScheduledCheck>,
    region: String,
) {
    let schedule_check_stream: UnboundedReceiverStream<_> = reciever.into();

    schedule_check_stream
        .for_each_concurrent(concurrency, |scheduled_check| {
            let job_checker = checker.clone();
            let job_producer = producer.clone();
            let job_region = region.clone();
            let job_queue_size = queue_size.clone();

            metrics::gauge!("executor.check_concurrency")
                .set(queue_size.load(Ordering::SeqCst) as f64);

            async move {
                let _ = tokio::spawn(async move {
                    let config = &scheduled_check.config;
                    let tick = &scheduled_check.tick;

                    // If a check execution is processed after more than the interval of the check
                    // config we skip the check, we were too late
                    let late_by = Utc::now() - tick.time();
                    let interval = TimeDelta::seconds(config.interval as i64);

                    let check_result = if late_by > interval {
                        CheckResult::missed_from(config, tick, &job_region)
                    } else {
                        job_checker.check_url(config, tick, &job_region).await
                    };

                    if let Err(e) = job_producer.produce_checker_result(&check_result) {
                        tracing::error!(error = ?e, "executor.failed_to_produce");
                    }

                    record_result_metrics(&check_result);
                    tracing::debug!(result = ?check_result, "executor.check_complete");

                    scheduled_check.record_result(check_result);
                    job_queue_size.fetch_sub(1, Ordering::SeqCst);
                })
                .await;
            }
        })
        .await;
    tracing::info!("executor.shutdown");
}

fn record_result_metrics(result: &CheckResult) {
    // Record metrics
    let CheckResult {
        status,
        scheduled_check_time,
        actual_check_time,
        duration,
        status_reason,
        request_info,
        ..
    } = result;

    let status_label = match status {
        CheckStatus::Success => "success",
        CheckStatus::Failure => "failure",
        CheckStatus::MissedWindow => "missed_window",
    };
    let failure_reason = match status_reason.as_ref().map(|r| r.status_type) {
        Some(CheckStatusReasonType::Failure) => Some("failure"),
        Some(CheckStatusReasonType::DnsError) => Some("dns_error"),
        Some(CheckStatusReasonType::Timeout) => Some("timeout"),
        None => None,
    };
    let status_code = match request_info.as_ref().and_then(|a| a.http_status_code) {
        None => "none".to_string(),
        Some(status) => status.to_string(),
    };

    // Record duration of check
    if let Some(duration) = duration {
        metrics::histogram!(
            "check_result.duration",
            "histogram" => "timer",
            "status" => status_label,
            "failure_reason" => failure_reason.unwrap_or("ok"),
            "status_code" => status_code.clone(),
            "uptime_region" => result.region.clone(),
        )
        .record(duration.to_std().unwrap().as_secs_f64());
    }

    // Record time between scheduled and actual check
    let delay = (*actual_check_time - *scheduled_check_time)
        .to_std()
        .unwrap()
        .as_secs_f64();

    metrics::histogram!(
        "check_result.delay",
        "histogram" => "timer",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
        "status_code" => status_code.clone(),
        "uptime_region" => result.region.clone(),
    )
    .record(delay);

    // Record status of the check
    metrics::counter!(
        "check_result.processed",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
        "status_code" => status_code,
        "uptime_region" => result.region.clone(),
    )
    .increment(1);
}

#[cfg(test)]
mod tests {
    use std::{task::Poll, time::Duration};

    use chrono::Utc;
    use futures::poll;
    use similar_asserts::assert_eq;
    use tokio::{sync::oneshot::Receiver, time};
    use uuid::Uuid;

    use super::*;
    use crate::{
        checker::dummy_checker::DummyChecker,
        producer::dummy_producer::DummyResultsProducer,
        types::check_config::CheckInterval,
    };

    #[tokio::test(start_paused = true)]
    async fn test_executor_simple() {
        let checker = Arc::new(DummyChecker::new(Duration::from_secs(1)));
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        let (sender, _) = run_executor(1, checker, producer, "us-west".to_string());

        let tick = Tick::from_time(Utc::now());
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });

        let resolve_rx = queue_check(&sender, tick, config.clone());
        tokio::pin!(resolve_rx);

        // Will not be resolved yet
        time::sleep(Duration::from_millis(100)).await;
        assert_eq!(poll!(resolve_rx.as_mut()), Poll::Pending);

        let result = resolve_rx.await.unwrap();
        assert_eq!(result.subscription_id, config.subscription_id);
        assert_eq!(result.status, CheckStatus::Success);
    }

    #[tokio::test(start_paused = true)]
    async fn test_executor_concurrent_limit() {
        let checker = Arc::new(DummyChecker::new(Duration::from_secs(1)));
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        // Only allow 2 configs to execute concurrently
        let (sender, _) = run_executor(2, checker, producer, "us-west".to_string());

        // Send 4 configs into the executor
        let mut configs: Vec<Receiver<CheckResult>> = (0..4)
            .map(|i| {
                let tick = Tick::from_time(Utc::now());
                let config = Arc::new(CheckConfig {
                    subscription_id: Uuid::from_u128(i),
                    ..Default::default()
                });
                queue_check(&sender, tick, config)
            })
            .collect();

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 4);

        let resolve_rx_4 = configs.pop().unwrap();
        tokio::pin!(resolve_rx_4);

        let resolve_rx_3 = configs.pop().unwrap();
        tokio::pin!(resolve_rx_3);

        let resolve_rx_2 = configs.pop().unwrap();
        tokio::pin!(resolve_rx_2);

        let resolve_rx_1 = configs.pop().unwrap();
        tokio::pin!(resolve_rx_1);

        // No task has completed yet
        assert_eq!(poll!(resolve_rx_1.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_2.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_3.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_4.as_mut()), Poll::Pending);

        // Move time forward one second. The first two will complete, but the last two will not yet
        time::sleep(Duration::from_millis(1001)).await;
        assert!(matches!(poll!(resolve_rx_1.as_mut()), Poll::Ready(_)));
        assert!(matches!(poll!(resolve_rx_2.as_mut()), Poll::Ready(_)));
        assert_eq!(poll!(resolve_rx_3.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_4.as_mut()), Poll::Pending);

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 2);

        // Move forward another second, the last two tasks should now be complete
        time::sleep(Duration::from_millis(1001)).await;
        assert!(matches!(poll!(resolve_rx_3.as_mut()), Poll::Ready(_)));
        assert!(matches!(poll!(resolve_rx_4.as_mut()), Poll::Ready(_)));

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn test_executor_missed() {
        let checker = Arc::new(DummyChecker::new(Duration::from_secs(1)));
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        let (sender, _) = run_executor(1, checker, producer, "us-west".to_string());

        let tick = Tick::from_time(Utc::now() - TimeDelta::minutes(2));
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            interval: CheckInterval::OneMinute,
            ..Default::default()
        });

        let result = queue_check(&sender, tick, config.clone()).await.unwrap();

        assert_eq!(result.subscription_id, config.subscription_id);
        assert_eq!(result.status, CheckStatus::MissedWindow);
    }
}
