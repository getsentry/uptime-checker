use std::sync::Arc;

use chrono::Utc;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

use crate::checker::{Checker, CheckerConfig};

pub async fn run_scheduler() -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let checker = Arc::new(Checker::new(CheckerConfig::default()));

    let checker_job = Job::new_async("0 */5 * * * *", move |_uuid, mut _l| {
        let job_checker = checker.clone();

        Box::pin(async move {
            println!("Executing job at {:?}", Utc::now());

            let check_result = job_checker.check_url("https://sentry.io").await;

            println!("checked sentry.io, got {:?}", check_result)
        })
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
