use std::sync::Arc;

use chrono::Utc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use crate::check_executor::ScheduledCheck;
use crate::config_store::{RwConfigStore, Tick};

pub fn run_scheduler(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    executor: UnboundedSender<ScheduledCheck>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tracing::info!(partition, "scheduler.starting");
    tokio::spawn(async move { scheduler_loop(config_store, executor, shutdown).await })
}

async fn scheduler_loop(
    config_store: Arc<RwConfigStore>,
    executor: UnboundedSender<ScheduledCheck>,
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

        let mut results = vec![];

        // TODO(epurkhiser): Check if we skipped any ticks If we did we should catch up on those.

        for config in configs {
            let (resolve, resolve_rx) = oneshot::channel();

            let scheduled_check = ScheduledCheck {
                tick,
                config,
                resolve,
            };
            executor.send(scheduled_check);
            results.push(resolve_rx);
        }

        // Spawn a task to wait for checks to complete.
        //
        // TODO(epurkhiser): We'll want to record the tick timestamp  in redis or some other store
        // so that we can resume processing if we fail to process ticks (crash-loop, etc)
        tokio::spawn(async move {
            let checks_scheduled = results.len();
            while let Some(result) = results.pop() {
                result.await;
            }

            let execution_duration = tick_start.elapsed();

            tracing::debug!(
                result = %tick,
                duration = ?execution_duration,
                checks_scheduled,
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
