use std::sync::Arc;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use crate::check_executor::{queue_check, CheckSender};
use crate::config_store::{RwConfigStore, Tick};

pub fn run_scheduler(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    executor_sender: CheckSender,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tracing::info!(partition, "scheduler.starting");
    tokio::spawn(async move { scheduler_loop(config_store, executor_sender, shutdown).await })
}

async fn scheduler_loop(
    config_store: Arc<RwConfigStore>,
    executor_sender: CheckSender,
    shutdown: CancellationToken,
) {
    let mut interval = time::interval(time::Duration::from_secs(1));

    let start = Utc::now();
    let instant = Instant::now();

    let (tick_complete_tx, mut tick_complete_rx) = mpsc::unbounded_channel();

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
            results.push(queue_check(&executor_sender, tick, config));
        }

        // Spawn a task to wait for checks to complete.
        //
        // TODO(epurkhiser): We'll want to record the tick timestamp  in redis or some other store
        // so that we can resume processing if we fail to process ticks (crash-loop, etc)
        let tick_complete_join_handle = tokio::spawn(async move {
            let checks_scheduled = results.len();
            while let Some(result) = results.pop() {
                // TODO(epurkhiser): Do we want to do something with the CheckResult here?
                let _check_result = result.await.expect("Failed to recieve CheckResult");
            }

            let execution_duration = tick_start.elapsed();

            tracing::debug!(
                result = %tick,
                duration = ?execution_duration,
                checks_scheduled,
                "scheduler.tick_execution_complete"
            );
            tick
        });

        tick_complete_tx
            .send(tick_complete_join_handle)
            .expect("Failed to queue join handle for tick completion");
    };

    let in_order_tick_processor_join_handle = tokio::spawn(async move {
        while let Some(tick_complete_join_handle) = tick_complete_rx.recv().await {
            // XXX: This task guarantees that we can process ticks IN ORDER upon tick execution.
            // This waits for the tick to complete before waiting on the next tick to complete.
            let tick = tick_complete_join_handle
                .await
                .expect("Failed to recieve completed tick");

            tracing::debug!(tick = %tick, "scheduler.tick_execution_complete_in_order");
        }
    });

    while !shutdown.is_cancelled() {
        let interval_tick = interval.tick().await;
        let tick = Tick::from_time(start + interval_tick.duration_since(instant));
        schedule_checks(tick);
    }

    // Wait for all dispatch ticks to have completed
    drop(tick_complete_tx);
    in_order_tick_processor_join_handle.await.unwrap();

    tracing::info!("scheduler.shutdown");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use similar_asserts::assert_eq;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use tracing_test::traced_test;
    use uuid::Uuid;

    use crate::{
        config_store::ConfigStore,
        types::{
            check_config::CheckConfig,
            result::{CheckResult, CheckStatus},
        },
    };

    use super::run_scheduler;

    #[tokio::test(start_paused = true)]
    #[traced_test]
    async fn test_scheduler() {
        let config_store = Arc::new(ConfigStore::new_rw());

        let config1 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });
        let config2 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(3),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
            rw_store.add_config(config2.clone());
        }

        let (executor_tx, mut executor_rx) = mpsc::unbounded_channel();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(0, config_store, executor_tx, shutdown_token.clone());

        // Wait and execute both ticks
        let scheduled_check1 = executor_rx.recv().await.unwrap();
        let scheduled_check1_time = scheduled_check1.get_tick().time();
        assert_eq!(scheduled_check1.get_config().clone(), config1);

        let scheduled_check2 = executor_rx.recv().await.unwrap();
        let scheduled_check2_time = scheduled_check2.get_tick().time();
        assert_eq!(scheduled_check2.get_config().clone(), config2);

        // The second tick should have been scheduled two seconds after the first tick since it is
        // two buckets after the first tick.
        assert_eq!(
            scheduled_check2_time - scheduled_check1_time,
            Duration::seconds(2)
        );

        // Record results for both to complete both ticks
        scheduled_check1.record_result(CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: config1.subscription_id,
            status: CheckStatus::Success,
            status_reason: None,
            trace_id: Default::default(),
            span_id: Default::default(),
            scheduled_check_time: scheduled_check1_time,
            actual_check_time: Utc::now(),
            duration: Some(Duration::seconds(1)),
            request_info: None,
        });
        scheduled_check2.record_result(CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: config2.subscription_id,
            status: CheckStatus::Success,
            status_reason: None,
            trace_id: Default::default(),
            span_id: Default::default(),
            scheduled_check_time: scheduled_check2_time,
            actual_check_time: Utc::now(),
            duration: Some(Duration::seconds(1)),
            request_info: None,
        });

        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));

        shutdown_token.cancel();
        join_handle.await.unwrap();
    }
}
