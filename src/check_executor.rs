use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// The number of times this scheduled check has been retried
    retry_count: u16,
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
    num_running: Arc<AtomicU64>,
}

impl CheckSender {
    pub fn new() -> (Self, UnboundedReceiver<ScheduledCheck>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let queue_size = Arc::new(AtomicU64::new(0));
        let num_running = Arc::new(AtomicU64::new(0));
        let check_sender = Self {
            sender,
            queue_size,
            num_running,
        };

        (check_sender, receiver)
    }

    /// Queues a check for execution, returning a oneshot receiver that will be fired once the check
    /// has resolved to a CheckResult.
    pub fn queue_check(&self, tick: Tick, config: Arc<CheckConfig>) -> Receiver<CheckResult> {
        let (resolve_tx, resolve_rx) = oneshot::channel();

        let scheduled_check = ScheduledCheck {
            tick,
            config,
            resolve_tx,
            retry_count: 0,
        };

        self.queue_size.fetch_add(1, Ordering::SeqCst);
        self.sender
            .send(scheduled_check)
            .expect("Failed to queue ScheduledCheck");

        resolve_rx
    }

    /// Requeues the check to be executed again, increasing the number of retries by 1
    fn queue_check_for_retry(&self, mut check: ScheduledCheck) {
        check.retry_count += 1;
        self.queue_size.fetch_add(1, Ordering::SeqCst);
        self.sender
            .send(check)
            .expect("Failed to queue ScheduledCheck");
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

pub struct ExecutorConfig {
    /// Number of checks that will be executed at the same time.
    pub concurrency: usize,

    /// Number of times a check will be retred when the execution of the check results in a
    /// failure.
    pub failure_retries: u16,

    /// The region the checker checker is running as
    pub region: String,

    /// Track metrics about executed tasks
    pub record_task_metrics: bool,
}

pub fn run_executor(
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    conf: ExecutorConfig,
) -> (Arc<CheckSender>, JoinHandle<()>) {
    tracing::info!("executor.starting");

    let (sender, reciever) = CheckSender::new();
    let queue_size = sender.queue_size.clone();
    let num_running = sender.num_running.clone();

    let check_sender = Arc::new(sender);
    let executor_check_sender = check_sender.clone();

    let executor_handle = tokio::spawn(async move {
        executor_loop(
            conf,
            queue_size,
            num_running,
            checker,
            producer,
            executor_check_sender,
            reciever,
        )
        .await
    });

    (check_sender, executor_handle)
}

async fn executor_loop(
    conf: ExecutorConfig,
    queue_size: Arc<AtomicU64>,
    num_running: Arc<AtomicU64>,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    check_sender: Arc<CheckSender>,
    check_receiver: UnboundedReceiver<ScheduledCheck>,
) {
    let schedule_check_stream: UnboundedReceiverStream<_> = check_receiver.into();

    // construct a metrics taskmonitor
    let metrics_monitor = tokio_metrics::TaskMonitor::new();

    // record metrics to datadog every 10 seconds
    if conf.record_task_metrics {
        let metrics_region = conf.region.clone();
        let metrics_monitor = metrics_monitor.clone();
        tokio::spawn(async move {
            for interval in metrics_monitor.intervals() {
                record_task_metrics(interval, metrics_region.clone());
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
    }

    schedule_check_stream
        .for_each_concurrent(conf.concurrency, |scheduled_check| {
            let job_checker = checker.clone();
            let job_producer = producer.clone();
            let job_region = conf.region.clone();
            let job_num_running = num_running.clone();
            let job_check_sender = check_sender.clone();

            let job_metrics_monitor = metrics_monitor.clone();

            num_running.fetch_add(1, Ordering::SeqCst);
            queue_size.fetch_sub(1, Ordering::SeqCst);
            metrics::gauge!("executor.queue_size", "uptime_region" => conf.region.clone())
                .set(queue_size.load(Ordering::SeqCst) as f64);
            metrics::gauge!("executor.num_running", "uptime_region" => conf.region.clone())
                .set(num_running.load(Ordering::SeqCst) as f64);

            async move {
                let check_task = async move {
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

                    let will_retry = check_result.status == CheckStatus::Failure
                        && scheduled_check.retry_count < conf.failure_retries;

                    record_result_metrics(
                        &check_result,
                        scheduled_check.retry_count > 0,
                        will_retry,
                    );

                    // re-queue for execution again
                    if will_retry {
                        tracing::debug!(result = ?check_result, "executor.check_will_retry");
                        job_check_sender.queue_check_for_retry(scheduled_check);
                        job_num_running.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }

                    if let Err(e) = job_producer.produce_checker_result(&check_result) {
                        tracing::error!(error = ?e, "executor.failed_to_produce");
                    }

                    tracing::debug!(result = ?check_result, "executor.check_complete");

                    scheduled_check.record_result(check_result);
                    job_num_running.fetch_sub(1, Ordering::SeqCst);
                };

                if conf.record_task_metrics {
                    let _ = job_metrics_monitor.instrument(check_task).await;
                } else {
                    let _ = tokio::spawn(check_task).await;
                }
            }
        })
        .await;
    tracing::info!("executor.shutdown");
}

fn record_result_metrics(result: &CheckResult, is_retry: bool, will_retry: bool) {
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
        Some(CheckStatusReasonType::TlsError) => Some("tls_error"),
        Some(CheckStatusReasonType::ConnectionError) => Some("connection_error"),
        None => None,
    };
    let status_code = match request_info.as_ref().and_then(|a| a.http_status_code) {
        None => "none".to_string(),
        Some(status) => status.to_string(),
    };

    let retry_label = if is_retry { "true" } else { "false" };
    let will_retry_label = if will_retry { "true" } else { "false" };

    // Record duration of check
    if let Some(duration) = duration {
        metrics::histogram!(
            "check_result.duration",
            "histogram" => "timer",
            "status" => status_label,
            "failure_reason" => failure_reason.unwrap_or("ok"),
            "status_code" => status_code.clone(),
            "uptime_region" => result.region.clone(),
            "is_retry" => retry_label,
            "will_retry" => will_retry_label,
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
        "is_retry" => retry_label,
        "will_retry" => will_retry_label,
    )
    .record(delay);

    // Record status of the check
    metrics::counter!(
        "check_result.processed",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
        "status_code" => status_code,
        "uptime_region" => result.region.clone(),
        "is_retry" => retry_label,
        "will_retry" => will_retry_label,
    )
    .increment(1);
}

fn record_task_metrics(interval: tokio_metrics::TaskMetrics, region: String) {
    metrics::gauge!(
        "executor_task.mean_first_poll_delay",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_first_poll_delay());
    metrics::gauge!(
        "executor_task.mean_idle_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_idle_duration());
    metrics::gauge!(
        "executor_task.mean_poll_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_poll_duration());
    metrics::gauge!(
        "executor_task.mean_scheduled_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_scheduled_duration());
    metrics::gauge!(
        "executor_task.mean_fast_poll_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_fast_poll_duration());
    metrics::gauge!(
        "executor_task.mean_long_delay_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_long_delay_duration());
    metrics::gauge!(
        "executor_task.mean_short_delay_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_short_delay_duration());
    metrics::gauge!(
        "executor_task.mean_long_delay_duration",
        "uptime_region" => region.clone(),
    )
    .set(interval.mean_long_delay_duration());
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
        checker::dummy_checker::{DummyChecker, DummyResult},
        producer::dummy_producer::DummyResultsProducer,
        types::check_config::CheckInterval,
    };

    #[tokio::test(start_paused = true)]
    async fn test_executor_simple() {
        let delayed_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Success,
        };

        let dummy_checker = DummyChecker::new();
        dummy_checker.queue_result(delayed_result);

        let checker = Arc::new(dummy_checker);
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        let conf = ExecutorConfig {
            concurrency: 1,
            failure_retries: 0,
            region: "us-west".to_string(),
            record_task_metrics: false,
        };
        let (sender, _) = run_executor(checker, producer, conf);

        let tick = Tick::from_time(Utc::now());
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });

        let resolve_rx = sender.queue_check(tick, config.clone());
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
        let delayed_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Success,
        };

        let dummy_checker = DummyChecker::new();
        dummy_checker.queue_result(delayed_result.clone());
        dummy_checker.queue_result(delayed_result.clone());
        dummy_checker.queue_result(delayed_result.clone());
        dummy_checker.queue_result(delayed_result.clone());

        let checker = Arc::new(dummy_checker);
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        // Only allow 2 configs to execute concurrently
        let conf = ExecutorConfig {
            concurrency: 2,
            failure_retries: 0,
            region: "us-west".to_string(),
            record_task_metrics: false,
        };
        let (sender, _) = run_executor(checker, producer, conf);

        // Send 4 configs into the executor
        let mut configs: Vec<Receiver<CheckResult>> = (0..4)
            .map(|i| {
                let tick = Tick::from_time(Utc::now());
                let config = Arc::new(CheckConfig {
                    subscription_id: Uuid::from_u128(i),
                    ..Default::default()
                });
                sender.queue_check(tick, config)
            })
            .collect();

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 4);
        assert_eq!(sender.num_running.load(Ordering::SeqCst), 0);

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
        // Move time forward less than one second. Two will start running
        time::sleep(Duration::from_millis(999)).await;
        // No task has completed yet, some are running, some are queued
        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 2);
        assert_eq!(sender.num_running.load(Ordering::SeqCst), 2);
        assert_eq!(poll!(resolve_rx_1.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_2.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_3.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_4.as_mut()), Poll::Pending);

        // Move time forward over the second boundary. The first two will complete,
        // but the last two will not yet
        time::sleep(Duration::from_millis(2)).await;

        assert!(matches!(poll!(resolve_rx_1.as_mut()), Poll::Ready(_)));
        assert!(matches!(poll!(resolve_rx_2.as_mut()), Poll::Ready(_)));
        assert_eq!(poll!(resolve_rx_3.as_mut()), Poll::Pending);
        assert_eq!(poll!(resolve_rx_4.as_mut()), Poll::Pending);

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 0);
        assert_eq!(sender.num_running.load(Ordering::SeqCst), 2);

        // Move forward another second, the last two tasks should now be complete
        time::sleep(Duration::from_millis(1001)).await;
        assert!(matches!(poll!(resolve_rx_3.as_mut()), Poll::Ready(_)));
        assert!(matches!(poll!(resolve_rx_4.as_mut()), Poll::Ready(_)));

        assert_eq!(sender.queue_size.load(Ordering::SeqCst), 0);
        assert_eq!(sender.num_running.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn test_executor_missed() {
        let delayed_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Success,
        };

        let dummy_checker = DummyChecker::new();
        dummy_checker.queue_result(delayed_result);

        let checker = Arc::new(dummy_checker);
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        let conf = ExecutorConfig {
            concurrency: 1,
            failure_retries: 0,
            region: "us-west".to_string(),
            record_task_metrics: false,
        };
        let (sender, _) = run_executor(checker, producer, conf);

        let tick = Tick::from_time(Utc::now() - TimeDelta::minutes(2));
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            interval: CheckInterval::OneMinute,
            ..Default::default()
        });

        let result = sender.queue_check(tick, config.clone()).await.unwrap();

        assert_eq!(result.subscription_id, config.subscription_id);
        assert_eq!(result.status, CheckStatus::MissedWindow);
    }

    #[tokio::test(start_paused = true)]
    async fn test_executor_retry() {
        let failed_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Failure,
        };
        let success_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Success,
        };

        // One failure then one success
        let dummy_checker = DummyChecker::new();
        dummy_checker.queue_result(failed_result);
        dummy_checker.queue_result(success_result);

        let checker = Arc::new(dummy_checker);
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        // Allow one retry
        let conf = ExecutorConfig {
            concurrency: 1,
            failure_retries: 1,
            region: "us-west".to_string(),
            record_task_metrics: false,
        };
        let (sender, _) = run_executor(checker, producer, conf);

        let tick = Tick::from_time(Utc::now());
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });

        let resolve_rx = sender.queue_check(tick, config.clone());
        tokio::pin!(resolve_rx);

        // Resolves as success since we will retry
        let result = resolve_rx.await.unwrap();
        assert_eq!(result.subscription_id, config.subscription_id);
        assert_eq!(result.status, CheckStatus::Success);
    }

    #[tokio::test(start_paused = true)]
    async fn test_executor_retry_failed() {
        let failed_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Failure,
        };
        let success_result = DummyResult {
            delay: Some(Duration::from_secs(1)),
            status: CheckStatus::Success,
        };

        // Three failure then one success, we won't get the success since our retry limit is 2, so
        // we'll fail once, retry twice, and report the last failure
        let dummy_checker = DummyChecker::new();
        dummy_checker.queue_result(failed_result.clone());
        dummy_checker.queue_result(failed_result.clone());
        dummy_checker.queue_result(failed_result.clone());
        dummy_checker.queue_result(success_result);

        let checker = Arc::new(dummy_checker);
        let producer = Arc::new(DummyResultsProducer::new("uptime-results"));

        // Allow two retries
        let conf = ExecutorConfig {
            concurrency: 1,
            failure_retries: 2,
            region: "us-west".to_string(),
            record_task_metrics: false,
        };
        let (sender, _) = run_executor(checker, producer, conf);

        let tick = Tick::from_time(Utc::now());
        let config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });

        let resolve_rx = sender.queue_check(tick, config.clone());
        tokio::pin!(resolve_rx);

        // Resolves as failure after the two retries
        let result = resolve_rx.await.unwrap();
        assert_eq!(result.subscription_id, config.subscription_id);
        assert_eq!(result.status, CheckStatus::Failure);
    }
}
