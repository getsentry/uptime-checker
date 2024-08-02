use std::sync::Arc;

use chrono::Utc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use crate::checker::Checker;
use crate::config_store::{RwConfigStore, Tick};
use crate::producer::ResultsProducer;
use crate::types::result::{CheckResult, CheckStatus, CheckStatusReasonType};

pub fn run_scheduler(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tracing::info!(partition, "scheduler.starting");
    tokio::spawn(async move { scheduler_loop(config_store, checker, producer, shutdown).await })
}

fn record_result_metrics(result: &CheckResult) {
    // Record metrics
    let CheckResult {
        status,
        scheduled_check_time,
        actual_check_time,
        duration,
        status_reason,
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

    // Record duration of check
    if let Some(duration) = duration {
        metrics::histogram!(
            "check_result.duration_ms",
            "histogram" => "timer",
            "status" => status_label,
            "failure_reason" => failure_reason.unwrap_or("ok"),
        )
        .record(duration.num_milliseconds() as f64);
    }

    // Record time between scheduled and actual check
    metrics::histogram!(
        "check_result.delay_ms",
        "histogram" => "timer",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
    )
    .record((*actual_check_time - *scheduled_check_time).num_milliseconds() as f64);

    // Record status of the check
    metrics::counter!(
        "check_result.processed",
        "status" => status_label,
        "failure_reason" => failure_reason.unwrap_or("ok"),
    )
    .increment(1);
}

async fn scheduler_loop(
    config_store: Arc<RwConfigStore>,
    checker: Arc<impl Checker + 'static>,
    producer: Arc<impl ResultsProducer + 'static>,
    shutdown: CancellationToken,
) {
    let mut interval = time::interval(time::Duration::from_secs(1));

    let start = Utc::now();
    let instant = Instant::now();

    let schedule_checks = |tick| {
        let tick_start = Instant::now();
        let configs = config_store
            .read()
            .expect("Lock poisoned")
            .get_configs(tick);
        tracing::info!(%tick, bucket_size = configs.len(), "scheduler.tick_scheduled");

        metrics::gauge!("scheduler.bucket_size").set(configs.len() as f64);

        let mut join_set = JoinSet::new();

        // TODO(epurkhiser): Check if we skipped any ticks If we did we should catch up on those.

        // TODO: We should put schedule config executions into a worker using mpsc
        for config in configs {
            let job_checker = checker.clone();
            let job_producer = producer.clone();

            join_set.spawn(async move {
                let check_result = job_checker.check_url(&config, &tick).await;

                if let Err(e) = job_producer.produce_checker_result(&check_result) {
                    tracing::error!(error = ?e, "executor.failed_to_produce_result");
                }

                tracing::info!(result = ?check_result, "executor.check_complete");
                record_result_metrics(&check_result);
            });
        }

        // Spawn a task to wait for checks to complete.
        //
        // TODO(epurkhiser): We'll want to record the tick timestamp  in redis or some other store
        // so that we can resume processing if we fail to process ticks (crash-loop, etc)
        tokio::spawn(async move {
            let checks_ran = join_set.len();
            while join_set.join_next().await.is_some() {}
            let execution_duration = tick_start.elapsed();

            tracing::debug!(
                result = %tick,
                duration = ?execution_duration,
                checks_ran,
                "scheduler.tick_execution_complete"
            );
        });
    };

    while !shutdown.is_cancelled() {
        let interval_tick = interval.tick().await;
        let tick = Tick::from_time(start + interval_tick.duration_since(instant));
        schedule_checks(tick);
    }
    tracing::info!("scheduler.shutdown");
}
