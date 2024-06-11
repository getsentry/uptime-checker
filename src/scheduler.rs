use std::sync::Arc;

use chrono::Utc;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use rust_arroyo::types::Topic;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

use crate::checker::{Checker, CheckerConfig};
use crate::producer::produce_checker_result;

pub async fn run_scheduler() -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let checker = Arc::new(Checker::new(CheckerConfig::default()));

    let checker_job = Job::new_async("0 */5 * * * *", move |_uuid, mut _l| {
        let job_checker = checker.clone();

        Box::pin(async move {
            println!("Executing job at {:?}", Utc::now());

            let check_result = job_checker.check_url("https://sentry.io").await;
            // TODO: Get this from configuration.
            // TODO: Producer should be instantiated with these values and shared
            let config = KafkaConfig::new_config(["0.0.0.0".to_string()].to_vec(), None);
            let topic = Topic::new("uptime-checker-results");
            let _ = produce_checker_result(&check_result, topic, config).await;

            println!("checked sentry.io, got {:?}", check_result)
        })
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
