use chrono::{DurationRound, TimeDelta, TimeZone, Utc};
use std::sync::Arc;

use tokio::sync::mpsc;

use std::time::Duration;
use tokio::sync::oneshot::Receiver;
use tokio::task::JoinHandle;
use tokio::time::{interval, Instant};
use tokio_util::sync::CancellationToken;

use crate::check_executor::CheckSender;
use crate::config_store::{RwConfigStore, Tick};
use crate::config_waiter::BootResult;
use crate::redis::build_redis_client;
use redis::AsyncCommands;

#[allow(clippy::too_many_arguments)]
pub fn run_scheduler(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    executor_sender: Arc<CheckSender>,
    shutdown: CancellationToken,
    progress_key: String,
    redis_host: String,
    config_loaded_receiver: Receiver<BootResult>,
    region: String,
    redis_enable_cluster: bool,
    redis_timeouts_ms: u64,
) -> JoinHandle<()> {
    tracing::info!(partition, "scheduler.starting");
    tokio::spawn(async move {
        let _ = config_loaded_receiver.await;
        scheduler_loop(
            partition,
            config_store,
            executor_sender,
            shutdown,
            progress_key,
            redis_host,
            region,
            redis_enable_cluster,
            redis_timeouts_ms,
        )
        .await
    })
}

#[allow(clippy::too_many_arguments)]
async fn scheduler_loop(
    partition: u16,
    config_store: Arc<RwConfigStore>,
    executor_sender: Arc<CheckSender>,
    shutdown: CancellationToken,
    progress_key: String,
    redis_host: String,
    region: String,
    redis_enable_cluster: bool,
    redis_timeouts_ms: u64,
) {
    let client = build_redis_client(&redis_host, redis_enable_cluster);
    let mut connection = client
        .get_async_connection(redis_timeouts_ms)
        .await
        .expect("Unable to connect to redis");
    let last_progress: Option<String> = connection
        .get(&progress_key)
        .await
        .expect("Unable to get last tick key");
    let tick_frequency = Duration::from_secs(1);
    tracing::debug!(
        progress_key,
        last_progress,
        "scheduler.redis_stored_tick_value"
    );

    // Determine when to start the ticker from when we last made progress. This may be a number of
    // seconds ago when the checker is restarting
    let start = match last_progress {
        Some(result) => {
            let last_completed_check_nanos: i64 = result.parse().unwrap_or(Utc::now().timestamp());
            let next_check = Utc.timestamp_nanos(last_completed_check_nanos) + tick_frequency;
            // There's a race where a checker is running for a given progress_key, and another is starting.
            // Since we add `tick_frequency` to the date here, the date can end up in the future, which causes
            // a panic when we subtract it from `Utc::now()`. We avoid this by clamping to Utc::now().
            next_check.min(Utc::now().duration_trunc(TimeDelta::seconds(1)).unwrap())
        }
        // We truncate the initial date to the nearest second so that we're aligned to the second
        // boundary here
        None => Utc::now().duration_trunc(TimeDelta::seconds(1)).unwrap(),
    };
    tracing::info!(%start, "scheduler.starting_at");

    let start_at = Instant::now()
        .checked_sub((Utc::now() - start).to_std().unwrap())
        .unwrap();
    let mut interval = interval(tick_frequency);
    interval.reset_at(start_at);

    let (tick_complete_tx, mut tick_complete_rx) = mpsc::unbounded_channel();

    let schedule_checks = |tick| {
        let tick_start = Instant::now();
        let configs = config_store
            .read()
            .expect("Lock poisoned")
            .get_configs(tick);
        let mut results = vec![];
        let mut bucket_size: usize = 0;
        let total_configs = configs.len();

        for config in configs {
            // Stat to see if the cadence that configs are processed is spiky between regions
            metrics::counter!(
                "scheduler.config_might_run",
                "uptime_region" => region.clone(),
            )
            .increment(1);
            if config.should_run(tick, &region) {
                results.push(executor_sender.queue_check(tick, config));
                bucket_size += 1;
            } else {
                tracing::debug!(%config.subscription_id, %tick, "scheduler.skipped_config");

                metrics::counter!(
                    "scheduler.skipped_region",
                    "uptime_region" => region.clone(),
                )
                .increment(1);
            }
        }
        tracing::debug!(
            %tick, bucket_size = bucket_size, uptime_region = region, partition=partition.to_string(),
            total_configs=total_configs, skipped_configs=total_configs - bucket_size,
            "scheduler.tick_scheduled"
        );
        metrics::gauge!("scheduler.bucket_size", "uptime_region" => region.clone(), "partition" => partition.to_string())
        .set(bucket_size as f64);

        // Spawn a task to wait for checks to complete.
        //
        // TODO(epurkhiser): We'll want to record the tick timestamp  in redis or some other store
        // so that we can resume processing if we fail to process ticks (crash-loop, etc)
        let tick_complete_join_handle = tokio::spawn(async move {
            let checks_scheduled = results.len();
            while let Some(result) = results.pop() {
                // TODO(epurkhiser): Do we want to do something with the CheckResult here?
                let _check_result = result.await.expect("Failed to receive CheckResult");
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
        const HANDLE_BATCH_SIZE: usize = 1000;
        let mut tick_handles = Vec::with_capacity(HANDLE_BATCH_SIZE);

        while tick_complete_rx
            .recv_many(&mut tick_handles, HANDLE_BATCH_SIZE)
            .await
            > 0
        {
            // XXX: This task guarantees that we can process ticks IN ORDER upon tick execution.
            // This waits for the tick to complete before waiting on the next tick to complete.
            tracing::debug!("scheduler.tick_awaiting_join_handle");
            let num_read = tick_handles.len();

            // Only read the very last update's tick, since that will be the latest one we've successfully
            // processed.
            let tick = tick_handles.pop();
            tick_handles.clear();

            let tick = tick
                .unwrap()
                .await
                .expect("Failed to receive completed tick");

            let progress = tick.time().timestamp_nanos_opt().unwrap();
            tracing::info!(tick = %tick, progress, num_read, "scheduler.tick_complete_join_handle");

            let result: Result<(), redis::RedisError> =
                connection.set(&progress_key, progress.to_string()).await;

            // Right now (April 25th,) we're seeing the occasional hang here, seemingly due to a
            // stale connection.  Let's be more explicit with the error handling and logging here,
            // instead of just swallowing and reporting the error like we do elsewhere.
            if let Err(e) = result {
                if e.is_timeout() {
                    // Log it, but let it through.
                    tracing::error!(progress_key, "scheduler.progress_saving_timeout");
                } else if e.is_connection_dropped() {
                    // Log and try reconnecting.
                    tracing::error!(progress_key, "scheduler.progress_saving_connection_dropped");

                    connection = client
                        .get_async_connection(redis_timeouts_ms)
                        .await
                        .expect("Unable to connect to redis");
                } else {
                    tracing::error!(progress_key, error = %e, "scheduler.progress_fatal_error");

                    panic!("Could not save scheduler progress: {:?}", e);
                }
            }
            tracing::debug!(tick = %tick, "scheduler.tick_execution_complete_in_order");
        }
        tracing::debug!("scheduler.tick_complete_join_handle_finished")
    });

    while !shutdown.is_cancelled() {
        let interval_tick = interval.tick().await;
        let tick = Tick::from_time(start + interval_tick.duration_since(start_at));
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
    use crate::check_executor::CheckSender;
    use chrono::{Duration, TimeDelta, Utc};
    use redis::{Client, Commands};
    use redis_test_macro::redis_test;
    use similar_asserts::assert_eq;
    use std::ops::Add;
    use std::sync::Arc;
    use tokio::sync::oneshot;
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

    #[traced_test]
    #[redis_test(start_paused = true)]
    async fn test_scheduler() {
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
            subscription_id: Uuid::from_u128(435),
            ..Default::default()
        });
        let config2 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(14),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
            rw_store.add_config(config2.clone());
        }

        let (executor_tx, mut executor_rx) = CheckSender::new();
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            Arc::new(executor_tx),
            shutdown_token.clone(),
            build_progress_key(0),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
            false,
            0,
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
            region: config.region.clone(),
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
            region: config.region.clone(),
        });

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 128000000000);
    }

    #[traced_test]
    #[redis_test(start_paused = true)]
    async fn test_scheduler_multi_region() {
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
        // We choose the subscription id specifically here. We need the first to be
        // `<val> % 60 == 1`, and also that the subscription seed id we produce based
        // on it in `CheckConfig.should_run` is `<val> % active_regions.length() == 0`
        // so that it's in us_west.
        // For config 2, we need it to be `<val> % 60 == 3` and
        // `<val> % active_regions.length() == 1` so that it's in us_east.
        // Note: These numbers are chosen so that the uuid5 we generate for them modulos to 1/3
        // respectively, and places them in the same slot.
        let config1 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(2500),
            active_regions: Some(vec!["us_west".to_string(), "us_east".to_string()]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        });
        let config2 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1317),
            active_regions: Some(vec!["us_east".to_string(), "us_west".to_string()]),
            region_schedule_mode: Some(RegionScheduleMode::RoundRobin),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
            rw_store.add_config(config2.clone());
        }

        let (executor_tx, mut executor_rx) = CheckSender::new();
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            Arc::new(executor_tx),
            shutdown_token.clone(),
            build_progress_key(0),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
            false,
            0,
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
            region: config.region.clone(),
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
            region: config.region.clone(),
        });

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 128000000000);
    }

    #[traced_test]
    #[redis_test(start_paused = true)]
    async fn test_scheduler_start() {
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
            subscription_id: Uuid::from_u128(435),
            ..Default::default()
        });
        let config2 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(14),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
            rw_store.add_config(config2.clone());
        }

        let (executor_tx, mut executor_rx) = CheckSender::new();
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            Arc::new(executor_tx),
            shutdown_token.clone(),
            progress_key.clone(),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
            false,
            0,
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
            region: config.region.clone(),
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
            region: config.region.clone(),
        });

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
        let progress: u64 = connection
            .get(progress_key)
            .expect("Couldn't save progress of scheduler");
        assert_eq!(progress, 130000000000);
    }

    #[traced_test]
    #[redis_test(start_paused = true)]
    async fn test_scheduler_start_bad_start_val() {
        let config = Config::default();
        let partition = 1;
        let progress_key = build_progress_key(partition);
        let client = Client::open(config.redis_host.clone()).unwrap();
        let mut connection = client.get_connection().expect("Unable to connect to Redis");
        let _: () = connection
            .set(
                progress_key.clone(),
                Utc::now()
                    .add(TimeDelta::seconds(60))
                    .timestamp_nanos_opt()
                    .unwrap()
                    .to_string(),
            )
            .expect("Couldn't save progress of scheduler");

        let config_store = Arc::new(ConfigStore::new_rw());

        let config1 = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(1),
            ..Default::default()
        });

        {
            let mut rw_store = config_store.write().unwrap();
            rw_store.add_config(config1.clone());
        }

        let (executor_tx, mut executor_rx) = CheckSender::new();
        let (boot_tx, boot_rx) = oneshot::channel::<BootResult>();
        let shutdown_token = CancellationToken::new();

        let join_handle = run_scheduler(
            partition,
            config_store,
            Arc::new(executor_tx),
            shutdown_token.clone(),
            progress_key.clone(),
            config.redis_host.clone(),
            boot_rx,
            config.region.clone(),
            false,
            0,
        );

        let _ = boot_tx.send(BootResult::Started);

        // // Wait and execute both ticks
        let scheduled_check1 = executor_rx.recv().await.unwrap();
        let scheduled_check1_time = scheduled_check1.get_tick().time();
        assert_eq!(scheduled_check1.get_config().clone(), config1);

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
            region: config.region.clone(),
        });

        shutdown_token.cancel();
        // XXX: Without this loop we end up stuck forever, we should try to understand that better
        while executor_rx.recv().await.is_some() {}
        join_handle.await.unwrap();
        assert!(logs_contain("scheduler.tick_execution_complete_in_order"));
    }
}
