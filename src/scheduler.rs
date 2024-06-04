use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

pub async fn run_scheduler() -> Result<(), JobSchedulerError> {
    let scheduler = JobScheduler::new().await?;

    let checker_job = Job::new("1/10 * * * * *", |_uuid, _l| {
        println!("I run every 10 seconds");
    })?;

    scheduler.add(checker_job).await?;
    scheduler.start().await?;

    Ok(())
}
