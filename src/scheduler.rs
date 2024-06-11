use std::sync::Arc;

use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};
use tracing::{error, info};

use crate::checker::Checker;
use crate::config::Config;
use crate::producer::ResultProducer;

pub async fn run_scheduler(config: &Config) -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let checker = Arc::new(Checker::new(Default::default()));

    let producer = Arc::new(ResultProducer::new(
        "uptime-results",
        KafkaConfig::new_config(config.results_kafka_cluster.to_owned(), None),
    ));

    let checker_job = Job::new_async("*/10 * * * * *", move |_uuid, mut _l| {
        let job_checker = checker.clone();
        let job_producer = producer.clone();

        Box::pin(async move {
            info!("Executing check");

            let check_result = job_checker
                .check_url("https://downtime-simulator-test1.vercel.app")
                .await;

            if let Err(e) = job_producer.produce_checker_result(&check_result).await {
                error!(error = ?e, "Failed to produce check result");
            }

            info!(result = ?check_result, "Check complete");
        })
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
