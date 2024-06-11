use std::sync::Arc;

use chrono::Utc;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

use crate::checker::Checker;
use crate::producer::ResultProducer;

pub async fn run_scheduler() -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let checker = Arc::new(Checker::new(Default::default()));

    let checker_job = Job::new_async("0 */5 * * * *", move |_uuid, mut _l| {
        let job_checker = checker.clone();

        Box::pin(async move {
            println!("Executing job at {:?}", Utc::now());

            let check_result = job_checker
                .check_url("https://downtime-simulator-test1.vercel.app")
                .await;
            // TODO: Get this from configuration.
            // TODO: Producer should be instantiated with these values and shared
            let config = KafkaConfig::new_config(["0.0.0.0".to_string()].to_vec(), None);
            let producer = ResultProducer::new("uptime-checker-results", config);
            let _ = producer.produce_checker_result(&check_result).await;

            println!("checked sentry.io, got {:?}", check_result)
        })
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
