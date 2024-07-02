use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, Instant};
use tracing::{debug, error, info};

use crate::app::config::Config;
use crate::checker::Checker;
use crate::config_store::{RwConfigStore, Tick};
use crate::producer::ResultProducer;

pub async fn run_scheduler(
    config: &Config,
    config_store: Arc<RwConfigStore>,
) -> Result<JoinHandle<()>, ()> {
    let checker = Arc::new(Checker::new());

    let producer = Arc::new(ResultProducer::new(
        &config.results_kafka_topic,
        KafkaConfig::new_config(config.results_kafka_cluster.to_owned(), None),
    ));

    let scheduler = tokio::spawn(async move {
        let mut interval = time::interval(time::Duration::from_secs(1));

        let start = Utc::now();
        let instant = Instant::now();

        let schedule_checks = |tick| {
            let tick_start = Instant::now();
            let configs = config_store
                .read()
                .expect("Lock poisoned")
                .get_configs(tick);

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

                            if let Err(e) = job_producer.produce_checker_result(&check_result).await
                            {
                                error!(error = ?e, "Failed to produce check result");
                            }

                            info!(result = ?check_result, "Check complete");
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
                    while join_set.join_next().await.is_some() {}
                    let execution_duration = tick_start.elapsed();

                    debug!(
                        result = %tick,
                        duration = ?execution_duration,
                        partition,
                        "Tick check execution complete"
                    );
                });
            }
        };

        info!("Starting scheduler");
        loop {
            // TODO: Probably need graceful shutdown via a CancellationToken

            let interval_tick = interval.tick().await;
            let tick = Tick::from_time(start + interval_tick.duration_since(instant));

            debug!(tick = %tick, "Scheduler ticking");
            schedule_checks(tick);
        }
    });

    Ok(scheduler)
}
