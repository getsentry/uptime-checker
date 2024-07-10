use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::app::config::Config;
use crate::checker::http_checker::HttpChecker;
use crate::checker::Checker;
use crate::config_store::{RwConfigStore, Tick};
use crate::producer::kafka_producer::KafkaResultsProducer;
use crate::producer::ResultsProducer;
use crate::types::result::{CheckResult, CheckStatus, CheckStatusReasonType};

pub fn run_scheduler(
    config: &Config,
    config_store: Arc<RwConfigStore>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    let checker = Arc::new(HttpChecker::new());

    let producer = Arc::new(KafkaResultsProducer::new(
        &config.results_kafka_topic,
        KafkaConfig::new_config(config.results_kafka_cluster.to_owned(), None),
    ));

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

        metrics::gauge!("scheduler.bucket_size").set(configs.len() as f64);

        // We maintain a join set for each partition, this way as each partition worth of
        // checks completes we can mark that partition as having completed for this tick
        let partitions: Vec<_> = configs.iter().map(|c| c.partition).collect();

        let mut partitioned_join_sets: HashMap<_, JoinSet<_>> = partitions
            .into_iter()
            .map(|p| (p, JoinSet::new()))
            .collect();

        // TODO(epurkhiser): Check if we skipped any ticks for each of the partition that's
        // being processed. If we did we should catch up on those.
        //
        // TODO(epurkhiser): In the future it may make more sense to know how many
        // partitions we are assigned and check skipped ticks for ALL partitions, not just
        // partitions that we have checks to execute for in this tick.

        // TODO: We should put schedule config executions into a worker using mpsc
        for config in configs {
            let job_checker = checker.clone();
            let job_producer = producer.clone();

            partitioned_join_sets
                .get_mut(&config.partition)
                .map(|join_set| {
                    join_set.spawn(async move {
                        let check_result = job_checker.check_url(&config, &tick).await;

                        if let Err(e) = job_producer.produce_checker_result(&check_result) {
                            error!(error = ?e, "Failed to produce check result");
                        }

                        info!(result = ?check_result, "Check complete");
                        record_result_metrics(&check_result);
                    })
                });
        }

        // Spawn tasks to wait for each partition to complete.
        //
        // TODO(epurkhiser): We'll want to record the tick timestamp for this partition in
        // redis or some other store so that we can resume processing if we fail to process
        // ticks (crash-loop, etc)
        for (partition, mut join_set) in partitioned_join_sets {
            tokio::spawn(async move {
                let checks_ran = join_set.len();
                while join_set.join_next().await.is_some() {}
                let execution_duration = tick_start.elapsed();

                debug!(
                    result = %tick,
                    duration = ?execution_duration,
                    partition,
                    checks_ran,
                    "Tick check execution complete"
                );
            });
        }
    };

    info!("Starting scheduler");
    while !shutdown.is_cancelled() {
        let interval_tick = interval.tick().await;
        let tick = Tick::from_time(start + interval_tick.duration_since(instant));

        debug!(tick = %tick, "Scheduler ticking");
        schedule_checks(tick);
    }
    info!("Scheduler shutdown");
}
