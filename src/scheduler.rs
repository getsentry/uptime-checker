use std::time::Duration;

use chrono::Utc;
use reqwest::ClientBuilder;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

use crate::checker::check_url;

pub async fn run_scheduler() -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let client = ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to build checker client");

    let checker_job = Job::new_async("0 */5 * * * *", move |_uuid, mut _l| {
        let job_client = client.clone();

        Box::pin(async move {
            println!("Executing job at {:?}", Utc::now());

            let check_result = check_url(&job_client, "https://sentry.io").await;

            println!("checked sentry.io, got {:?}", check_result)
        })
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
