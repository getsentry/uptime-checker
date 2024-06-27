use std::sync::Arc;

use chrono::Utc;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, Instant};
use tracing::{debug, error, info};

use crate::checker::Checker;
use crate::config::Config;
use crate::config_store::{ConfigStore, Tick};
use crate::producer::ResultProducer;

pub async fn run_scheduler(
    config: &Config,
    config_store: Arc<ConfigStore>,
) -> Result<JoinHandle<()>, ()> {
    let checker = Arc::new(Checker::new(Default::default()));

    let producer = Arc::new(ResultProducer::new(
        &config.results_kafka_topic,
        KafkaConfig::new_config(config.results_kafka_cluster.to_owned(), None),
    ));

    let schduler = tokio::spawn(async move {
        let mut interval = time::interval(time::Duration::from_secs(1));

        let start = Utc::now();
        let instant = Instant::now();

        let schedule_checks = |tick: Tick| {
            let configs = config_store.get_configs(tick);

            if configs.is_empty() {
                debug!(tick = %tick, "No checks scheduled for tick");
                return;
            }

            let tick_start = Instant::now();
            let mut join_set = JoinSet::new();

            // TODO: We should put schedule config exections into a worker using mpsc
            for config in configs {
                let job_checker = checker.clone();
                let job_producer = producer.clone();

                join_set.spawn(async move {
                    let check_result = job_checker.check_url(&config, &tick).await;

                    if let Err(e) = job_producer.produce_checker_result(&check_result).await {
                        error!(error = ?e, "Failed to produce check result");
                    }

                    info!(result = ?check_result, "Check complete");
                });
            }

            tokio::spawn(async move {
                while join_set.join_next().await.is_some() {}

                let execution_duration = tick_start.elapsed();
                debug!(result = %tick, duration = ?execution_duration, "Tick check execution complete");
            });
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

    Ok(schduler)
}
