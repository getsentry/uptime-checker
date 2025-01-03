use chrono::{DurationRound, TimeDelta, TimeZone, Utc};
use std::sync::Arc;

use tokio::sync::mpsc;

use std::time::Duration;
use tokio::sync::oneshot::Receiver;
use tokio::task::JoinHandle;
use tokio::time::{interval, Instant};
use tokio_util::sync::CancellationToken;

use crate::check_executor::{queue_check, CheckSender};
use crate::config_store::{RwConfigStore, Tick};
use crate::config_waiter::BootResult;
use redis::{AsyncCommands, Client};

#[allow(clippy::too_many_arguments)]
pub fn run_scheduler(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    executor_sender: CheckSender,
    shutdown: CancellationToken,
    progress_key: String,
    redis_host: String,
    config_loaded_receiver: Receiver<BootResult>,
    region: String,
) -> JoinHandle<()> {
    tracing::info!(partition, "scheduler.starting");
    tokio::spawn(async move {
        let _ = config_loaded_receiver.await;
        scheduler_loop(
            config_store,
            executor_sender,
            shutdown,
            progress_key,
            redis_host,
            region,
        )
        .await
    })
}

async fn scheduler_loop(
    config_store: Arc<RwConfigStore>,
    executor_sender: CheckSender,
    shutdown: CancellationToken,
    progress_key: String,
    redis_host: String,
    region: String,
) {
    let client = Client::open(redis_host.clone()).unwrap();
    let mut connection = client
        .get_multiplexed_tokio_connection()
        .await
        .expect("Unable to connect to Redis");
    let result: Option<String> = connection
        .get(&progress_key)
        .await
        .expect("Unable to get last tick key");
    let tick_frequency = Duration::from_secs(1);
    tracing::debug!(progress_key, result, "scheduler.redis_stored_tick_value");
    let start = match result {
        Some(result) => {
            let last_completed_check_nanos: i64 = result.parse().unwrap_or(Utc::now().timestamp());
            Utc.timestamp_nanos(last_completed_check_nanos) + tick_frequency
        }
        // We truncate the initial date to the nearest second so that we're aligned to the second
        // boundary here
        None => Utc::now().duration_trunc(TimeDelta::seconds(1)).unwrap(),
    };
    tracing::info!(%start, "scheduler.starting_at");

    let instant = Instant::now()
        .checked_sub((Utc::now() - start).to_std().unwrap())
        .unwrap();
    let mut interval = interval(tick_frequency);
    interval.reset_at(instant);

    let (tick_complete_tx, mut tick_complete_rx) = mpsc::unbounded_channel();

    let schedule_checks = |tick| {
        let tick_start = Instant::now();
        let configs = config_store
            .read()
            .expect("Lock poisoned")
            .get_configs(tick);
        tracing::debug!(%tick, bucket_size = configs.len(), "scheduler.tick_scheduled");

        metrics::gauge!("scheduler.bucket_size").set(configs.len() as f64);

        let mut results = vec![];

        for config in configs {
            if config.should_run(tick, &region) {
                results.push(queue_check(&executor_sender, tick, config));
            } else {
                tracing::debug!(%config.subscription_id, %tick, "scheduler.skipped_config");

                metrics::counter!(
                    "scheduler.skipped_region",
                    "checker_region" => region.clone(),
                )
                .increment(1);
            }
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
            tracing::debug!("scheduler.tick_awaiting_join_handle");
            let tick = tick_complete_join_handle
                .await
                .expect("Failed to recieve completed tick");

            let progress = tick.time().timestamp_nanos_opt().unwrap();
            tracing::debug!(tick = %tick, progress, "scheduler.tick_complete_join_handle");

            let _: () = connection
                .set(&progress_key, progress.to_string())
                .await
                .expect("Couldn't save progress of scheduler");
            tracing::debug!(tick = %tick, "scheduler.tick_execution_complete_in_order");
        }
        tracing::debug!("scheduler.tick_complete_join_handle_finished")
    });

    while !shutdown.is_cancelled() {
        let interval_tick = interval.tick().await;
        let tick = Tick::from_time(start + interval_tick.duration_since(instant));
        schedule_checks(tick);
    }
    tracing::info!("scheduler.begin_shutdown");

    // Wait for all dispatch ticks to have completed
    drop(tick_complete_tx);
    let _ = in_order_tick_processor_join_handle.await;
    tracing::info!("scheduler.shutdown");
}

#[cfg(test)]
mod tests {
    use crate::app::config::Config;
    use chrono::{Duration, Utc};
    use redis::{Client, Commands};
    use similar_asserts::assert_eq;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    use tracing_test::traced_test;
    use uuid::Uuid;

    use super::run_scheduler;

    use crate::config_waiter::BootResult;
    use crate::manager::build_progress_key;
    use crate::types::shared::RegionScheduleMode;
    use crate::{
        config_store::ConfigStore,
        types::{
            check_config::CheckConfig,
            result::{CheckResult, CheckStatus},
        },
    };
    static TEST_MUTEX: Mutex<()> = Mutex::const_new(());

    #[traced_test]
    #[tokio::test(start_paused = true)]
    async fn test_scheduler() {
                // We don't want these tests to run at the same time, since they access the external redis.
        let _guard = TEST_MUTEX.lock().await;

        let config = Config::default();
        let partition = 0;

        let progress_key = build_progress_key(partition);
        let client = Client::open(config.redis_host.clone()).unwrap();
        let mut connection = client.get_connection().expect("Unable to connect to Redis");
        let _: () = connection
            .set(progress_key.clone(), 0)
            .expect("Couldn't save progress of scheduler");

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
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            executor_tx,
            shutdown_token.clone(),
            build_progress_key(0),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
        );
        let _ = boot_tx.send(BootResult::Started);

        // // Wait and execute both ticks
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

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 60000000000);
    }

    #[traced_test]
    #[tokio::test(start_paused = true)]
    async fn test_scheduler_multi_region() {
        // We don't want these tests to run at the same time, since they access the external redis.
        let _guard = TEST_MUTEX.lock().await;
        let config = Config {
            region: "us_west".to_string(),
            ..Default::default()
        };
        let partition = 0;

        let progress_key = build_progress_key(partition);
        let client = Client::open(config.redis_host.clone()).unwrap();
        let mut connection = client.get_connection().expect("Unable to connect to Redis");
        let _: () = connection
            .set(progress_key.clone(), 0)
            .expect("Couldn't save progress of scheduler");

        let config_store = Arc::new(ConfigStore::new_rw());

        let config1 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            active_regions: Some(vec!["us_west".to_string(), "us_east".to_string()]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        });
        let config2 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(3),
            active_regions: Some(vec!["us_east".to_string(), "us_west".to_string()]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
            rw_store.add_config(config2.clone());
        }

        let (executor_tx, mut executor_rx) = mpsc::unbounded_channel();
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            executor_tx,
            shutdown_token.clone(),
            build_progress_key(0),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
        );
        let _ = boot_tx.send(BootResult::Started);

        // // Wait and execute both ticks
        let scheduled_check1 = executor_rx.recv().await.unwrap();
        let scheduled_check1_time = scheduled_check1.get_tick().time();
        assert_eq!(scheduled_check1.get_config().clone(), config1);

        let scheduled_check2 = executor_rx.recv().await.unwrap();
        let scheduled_check2_time = scheduled_check2.get_tick().time();
        assert_eq!(scheduled_check2.get_config().clone(), config2);

        // The second tick should have been scheduled 62 seconds after the first tick since it is
        // two buckets after the first tick, but in an alternating region
        assert_eq!(
            scheduled_check2_time - scheduled_check1_time,
            Duration::seconds(62)
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

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 120000000000);
    }

    #[traced_test]
    #[tokio::test(start_paused = true)]
    async fn test_scheduler_start() {
        // We don't want these tests to run at the same time, since they access the external redis.
        let _guard = TEST_MUTEX.lock().await;
        // TODO: Better abstraction here
        let config = Config::default();
        let partition = 1;

        let progress_key = build_progress_key(partition);
        let client = Client::open(config.redis_host.clone()).unwrap();
        let mut connection = client.get_connection().expect("Unable to connect to Redis");
        let _: () = connection
            .set(
                progress_key.clone(),
                std::time::Duration::from_secs(2).as_nanos().to_string(),
            )
            .expect("Couldn't save progress of scheduler");

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
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            executor_tx,
            shutdown_token.clone(),
            progress_key.clone(),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
        );

        let _ = boot_tx.send(BootResult::Started);

        // // Wait and execute both ticks
        let scheduled_check1 = executor_rx.recv().await.unwrap();
        let scheduled_check1_time = scheduled_check1.get_tick().time();
        assert_eq!(scheduled_check1.get_config().clone(), config2);

        let scheduled_check2 = executor_rx.recv().await.unwrap();
        let scheduled_check2_time = scheduled_check2.get_tick().time();
        assert_eq!(scheduled_check2.get_config().clone(), config1);

        // The second tick should have been scheduled 58 seconds after the first tick since it is
        // 58 buckets after the first tick.
        assert_eq!(
            scheduled_check2_time - scheduled_check1_time,
            Duration::seconds(58)
        );

        // Record results for both to complete both ticks
        scheduled_check1.record_result(CheckResult {
            guid: Uuid::new_v4(),
            subscription_id: config2.subscription_id,
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
            subscription_id: config1.subscription_id,
            status: CheckStatus::Success,
            status_reason: None,
            trace_id: Default::default(),
            span_id: Default::default(),
            scheduled_check_time: scheduled_check2_time,
            actual_check_time: Utc::now(),
            duration: Some(Duration::seconds(1)),
            request_info: None,
        });

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 62000000000);
    }
}
