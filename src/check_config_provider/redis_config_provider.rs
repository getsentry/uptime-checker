use anyhow::Result;
use std::sync::Arc;
use std::{collections::HashSet, time::Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    app::config::Config, manager::Manager, redis::RedisClient, types::check_config::CheckConfig,
};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

#[derive(Debug)]
pub struct RedisPartition {
    number: u16,
    config_key: String,
    update_key: String,
}

impl RedisPartition {
    pub fn new(number: u16) -> RedisPartition {
        RedisPartition {
            number,
            config_key: format!("uptime:configs:{number}"),
            update_key: format!("uptime:updates:{number}"),
        }
    }
}

/// The action to perform when receiving a ConfigUpdate
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigUpdateAction {
    Upsert,
    Delete,
}

/// The ConfigUpdate is a notification that a ConfigMessage was upserted or deleted.
#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigUpdate {
    /// Whether we're upserting or deleting a config.
    pub action: ConfigUpdateAction,

    /// The subscription this check configuration is associated to in sentry.
    #[serde(with = "uuid::serde::simple")]
    pub subscription_id: Uuid,
}

trait RedisKey {
    fn redis_key(&self) -> String;
}

impl RedisKey for ConfigUpdate {
    fn redis_key(&self) -> String {
        self.subscription_id.simple().to_string()
    }
}

impl RedisKey for CheckConfig {
    fn redis_key(&self) -> String {
        self.subscription_id.simple().to_string()
    }
}

pub struct RedisConfigProvider {
    redis: RedisClient,
    partitions: HashSet<u16>,
    check_interval: Duration,
}

impl RedisConfigProvider {
    pub fn new(
        redis_url: &str,
        partitions: HashSet<u16>,
        check_interval: Duration,
        enable_cluster: bool,
        redis_timeouts_ms: u64,
        readonly: bool,
    ) -> Result<Self> {
        let client = crate::redis::build_redis_client(
            redis_url,
            enable_cluster,
            redis_timeouts_ms,
            readonly,
        )?;
        Ok(Self {
            redis: client,
            partitions,
            check_interval,
        })
    }

    async fn monitor_configs(
        &self,
        manager: Arc<Manager>,
        shutdown: CancellationToken,
        region: &'static str,
    ) {
        // Start monitoring configs using this provider. Loads the initial configs and
        // monitors redis for updates
        let partitions = self.get_partition_keys();
        metrics::gauge!("config_provider.redis.boot.partitions", "uptime_region" => region)
            .set(partitions.len() as f64);
        self.load_initial_configs(manager.clone(), &partitions, region)
            .await;

        if !self.redis.is_readonly() {
            self.monitor_updates(manager.clone(), &partitions, shutdown, region)
                .await;
        }
    }

    fn get_partition_keys(&self) -> Vec<RedisPartition> {
        // Returns a RedisPartition for each managed partition.
        self.partitions
            .iter()
            .map(|p| RedisPartition::new(*p))
            .collect()
    }

    async fn load_initial_configs(
        &self,
        manager: Arc<Manager>,
        partitions: &[RedisPartition],
        region: &'static str,
    ) {
        // Fetch configs for all partitions from Redis and register them with the manager
        manager.update_partitions(&self.partitions);

        let mut ops = self
            .redis
            .get_async_connection()
            .await
            .expect("Redis should be available");
        metrics::gauge!("config_provider.initial_load.partitions", "uptime_region" => region)
            .set(partitions.len() as f64);

        let start_loading = Instant::now();

        // Initial load of all configs for all partitions
        for partition in partitions {
            let partition_start_loading = Instant::now();
            let config_payloads: Vec<Vec<u8>> = ops
                .read_configs(&partition.config_key)
                .await
                .expect("Config key should exist");
            tracing::info!(
                partition = partition.number,
                config_count = config_payloads.len(),
                "redis_config_provider.loading_initial_configs"
            );
            metrics::gauge!("config_provider.initial_load.partition_size", "uptime_region" => region, "partition" => partition.number.to_string())
                .set(config_payloads.len() as f64);

            for config_payload in config_payloads {
                let config: CheckConfig = rmp_serde::from_slice(&config_payload)
                    .map_err(|err| {
                        tracing::error!(?err, "config_consumer.invalid_config_message");
                    })
                    .unwrap();
                manager
                    .get_service(partition.number)
                    .get_config_store()
                    .write()
                    .unwrap()
                    .add_config(Arc::new(config));
            }
            let partition_loading_time = partition_start_loading.elapsed().as_secs_f64();
            metrics::histogram!(
                "config_provider.initial_load.partition.duration",
                "histogram" => "timer",
                "uptime_region" => region,
                "partition" => partition.number.to_string(),
            )
            .record(partition_loading_time);
        }

        let loading_time = start_loading.elapsed().as_secs_f64();

        metrics::histogram!(
            "config_provider.initial_load.duration",
            "histogram" => "timer",
            "uptime_region" => region,
        )
        .record(loading_time);
    }

    async fn monitor_updates(
        &self,
        manager: Arc<Manager>,
        partitions: &[RedisPartition],
        shutdown: CancellationToken,
        region: &'static str,
    ) {
        let mut ops = self
            .redis
            .get_async_connection()
            .await
            .expect("Redis should be available");
        let mut interval = interval(self.check_interval);

        while !shutdown.is_cancelled() {
            let _ = interval.tick().await;

            let update_start = Instant::now();

            metrics::gauge!("config_provider.updater.partitions", "uptime_region" => region)
                .set(partitions.len() as f64);

            for partition in partitions.iter() {
                let partition_update_start = Instant::now();
                // We fetch all updates from the list and then delete the key. We do this
                // atomically so that there isn't any chance of a race
                let (config_upserts, config_deletes) =
                    ops.consume_config_updates(&partition.update_key).await;

                metrics::counter!("config_provider.updater.upserts", "uptime_region" => region, "partition" => partition.number.to_string())
                    .increment(config_upserts.len() as u64);
                metrics::counter!("config_provider.updater.deletes", "uptime_region" => region, "partition" => partition.number.to_string())
                    .increment(config_deletes.len() as u64);

                config_deletes.into_iter().for_each(|config_delete| {
                    manager
                        .get_service(partition.number)
                        .get_config_store()
                        .write()
                        .expect("Lock should not be poisoned")
                        .remove_config(config_delete.subscription_id);
                    tracing::debug!(
                        %config_delete.subscription_id,
                        "config_consumer.config_removed"
                    );
                });

                if config_upserts.is_empty() {
                    continue;
                }

                let config_payloads: Vec<Vec<u8>> = ops
                    .get_config_key_payloads(
                        &partition.config_key,
                        config_upserts
                            .iter()
                            .map(|config| config.redis_key())
                            .collect::<Vec<_>>(),
                    )
                    .await;

                for config_payload in config_payloads {
                    let config: CheckConfig = rmp_serde::from_slice(&config_payload)
                        .map_err(|err| {
                            tracing::error!(?err, "config_consumer.invalid_config_message");
                        })
                        .unwrap();
                    tracing::debug!(
                        partition = partition.number,
                        subscription_id = %config.subscription_id,
                        "redis_config_provider.upserting_config"
                    );
                    manager
                        .get_service(partition.number)
                        .get_config_store()
                        .write()
                        .unwrap()
                        .add_config(Arc::new(config));
                }
                let partition_update_duration = partition_update_start.elapsed().as_secs_f64();
                metrics::histogram!(
                    "config_provider.updater.partition.duration",
                    "histogram" => "timer",
                    "uptime_region" => region,
                    "partition" => partition.number.to_string(),
                )
                .record(partition_update_duration);
            }
            let update_duration = update_start.elapsed().as_secs_f64();
            metrics::histogram!(
                "config_provider.updater.duration",
                "histogram" => "timer",
                "uptime_region" => region,
            )
            .record(update_duration);
        }
    }
}

pub fn run_config_provider(
    config: &Config,
    manager: Arc<Manager>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    // Initializes the redis config provider and starts monitoring for config updates
    let provider = RedisConfigProvider::new(
        &config.redis_host,
        determine_owned_partitions(config),
        Duration::from_millis(config.config_provider_redis_update_ms),
        config.redis_enable_cluster,
        config.redis_timeouts_ms,
        config.redis_readonly,
    )
    .expect("Config provider should be initializable");

    let region = config.region;
    tokio::spawn(async move {
        let monitor_shutdown = shutdown.clone();
        let monitor_manager = manager.clone();
        let monitor_task = tokio::spawn(async move {
            provider
                .monitor_configs(monitor_manager, monitor_shutdown, region)
                .await
        });

        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("redis_config_provider.shutdown_requested");

                // Inform the manager that there are no longer any partitions
                manager.update_partitions(&HashSet::default());
            }
            _ = monitor_task => {
                tracing::error!("redis_config_provider.monitor_task_ended");
            }
        }
    })
}

pub fn determine_owned_partitions(config: &Config) -> HashSet<u16> {
    // Determines which partitions this checker owns based on number of partitions,
    // number of checkers and checker number
    if config.checker_number >= config.total_checkers {
        panic!(
            "checker_number {} must be less than total_checkers {}",
            config.checker_number, config.total_checkers
        );
    }

    (config.checker_number..config.config_provider_redis_total_partitions)
        .step_by(config.total_checkers.into())
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::redis::RedisAsyncConnection;

    use super::*;
    use redis::AsyncCommands;
    use redis_test_macro::redis_test;
    use std::time::Duration;

    async fn setup_test(
        readonly: bool,
    ) -> (
        Config,
        RedisAsyncConnection,
        Vec<RedisPartition>,
        Arc<Manager>,
        CancellationToken,
    ) {
        let config = Config {
            config_provider_redis_update_ms: 10,
            config_provider_redis_total_partitions: 2,
            checker_number: 0,
            total_checkers: 1,
            redis_readonly: readonly,
            ..Default::default()
        };
        let test_partitions: HashSet<u16> = vec![0, 1].into_iter().collect();
        let client = redis::Client::open(config.redis_host.clone()).unwrap();
        let mut conn =
            RedisAsyncConnection::Single(client.get_multiplexed_tokio_connection().await.unwrap());

        let partitions = RedisConfigProvider::new(
            config.redis_host.as_str(),
            test_partitions.clone(),
            Duration::from_millis(10),
            false,
            config.redis_timeouts_ms,
            config.redis_readonly,
        )
        .unwrap()
        .get_partition_keys();

        let all_keys: Vec<&String> = partitions
            .iter()
            .flat_map(|p| [&p.config_key, &p.update_key])
            .collect();
        let _: () = conn.del(&all_keys).await.unwrap();

        // Create manager and start provider
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        let shutdown = CancellationToken::new();

        (config, conn, partitions, manager, shutdown)
    }

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_load_no_configs() {
        let (config, _, _, manager, shutdown) = setup_test(false).await;
        let _handle = run_config_provider(&config, manager.clone(), shutdown.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify partitions were created but are empty
        for partition in [0, 1] {
            let configs = manager
                .get_service(partition)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs.len(), 0);
        }
        shutdown.cancel();
    }

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_load() {
        let (config, mut conn, partitions, manager, shutdown) = setup_test(false).await;
        assert_eq!(partitions.len(), 2);
        let partition_configs = partitions
            .iter()
            .map(|p| {
                (
                    p,
                    CheckConfig {
                        subscription_id: Uuid::new_v4(),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        // Test adding configs to different partitions
        for (partition, config) in partition_configs.iter() {
            let _: () = conn
                .hset(
                    &partition.config_key,
                    config.redis_key(),
                    rmp_serde::to_vec(&config).unwrap(),
                )
                .await
                .unwrap();
        }

        let _handle = run_config_provider(&config, manager.clone(), shutdown.clone());

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify configs were added to both partitions
        for (partition, config) in partition_configs {
            let configs = manager
                .get_service(partition.number)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs, [Arc::new(config)]);
        }
    }

    async fn send_update(
        mut conn: RedisAsyncConnection,
        partition: &RedisPartition,
        config: &CheckConfig,
    ) {
        let update = ConfigUpdate {
            action: ConfigUpdateAction::Upsert,
            subscription_id: config.subscription_id,
        };
        let config_msg = rmp_serde::to_vec(&config).unwrap();
        let update_msg = rmp_serde::to_vec(&update).unwrap();
        let _: () = conn
            .hset(&partition.config_key, config.redis_key(), &config_msg)
            .await
            .unwrap();
        let _: () = conn
            .hset(&partition.update_key, config.redis_key(), &update_msg)
            .await
            .unwrap();
    }

    async fn send_delete(
        mut conn: RedisAsyncConnection,
        partition: &RedisPartition,
        config: &CheckConfig,
    ) {
        let update = ConfigUpdate {
            action: ConfigUpdateAction::Delete,
            subscription_id: config.subscription_id,
        };
        let update_msg = rmp_serde::to_vec(&update).unwrap();
        let _: () = conn
            .hdel(&partition.config_key, config.redis_key())
            .await
            .unwrap();
        let _: () = conn
            .hset(&partition.update_key, config.redis_key(), &update_msg)
            .await
            .unwrap();
    }

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_updates() {
        let (config, conn, partitions, manager, shutdown) = setup_test(false).await;
        assert_eq!(partitions.len(), 2);
        let _handle = run_config_provider(&config, manager.clone(), shutdown.clone());

        tokio::time::sleep(Duration::from_millis(30)).await;

        for partition in &partitions {
            let configs = manager
                .get_service(partition.number)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs.len(), 0);
        }

        let partition_configs = partitions
            .iter()
            .map(|p| {
                (
                    p,
                    CheckConfig {
                        subscription_id: Uuid::new_v4(),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        // Test adding configs to different partitions
        for (partition, config) in partition_configs.iter() {
            send_update(conn.clone(), partition, config).await;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;

        // Verify configs were added to both partitions
        for (partition, config) in partition_configs.clone() {
            let configs = manager
                .get_service(partition.number)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs, [Arc::new(config)]);
        }

        let removed_config = partition_configs.first().unwrap();
        send_delete(conn.clone(), removed_config.0, &removed_config.1).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
        let configs = manager
            .get_service(removed_config.0.number)
            .get_config_store()
            .read()
            .unwrap()
            .all_configs();
        assert_eq!(0, configs.len());

        // Test deleting a non-existent config doesn't cause a problem
        send_delete(conn.clone(), removed_config.0, &removed_config.1).await;
        tokio::time::sleep(Duration::from_millis(15)).await;

        shutdown.cancel();
    }

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_updates_readonly() {
        let (config, mut conn, partitions, manager, shutdown) = setup_test(true).await;
        assert_eq!(partitions.len(), 2);

        let partition_configs = partitions
            .iter()
            .map(|p| {
                (
                    p,
                    CheckConfig {
                        subscription_id: Uuid::new_v4(),
                        interval: crate::types::check_config::CheckInterval::OneMinute,
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        for (partition, config) in partition_configs.iter() {
            let _: () = conn
                .hset(
                    &partition.config_key,
                    config.redis_key(),
                    rmp_serde::to_vec(&config).unwrap(),
                )
                .await
                .unwrap();
        }

        let _handle = run_config_provider(&config, manager.clone(), shutdown.clone());

        tokio::time::sleep(Duration::from_millis(30)).await;

        for partition in &partitions {
            let configs = manager
                .get_service(partition.number)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs.len(), 1);
        }

        let partition_configs = partitions
            .iter()
            .map(|p| {
                (
                    p,
                    CheckConfig {
                        subscription_id: Uuid::new_v4(),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        // Test adding configs to different partitions
        for (partition, config) in partition_configs.iter() {
            send_update(conn.clone(), partition, config).await;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;

        // Verify upserts do nothing.
        for (partition, _) in partition_configs.clone() {
            let configs = manager
                .get_service(partition.number)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs.len(), 1);
        }

        // Verify deletes do nothing.
        let removed_config = partition_configs.first().unwrap();
        send_delete(conn.clone(), removed_config.0, &removed_config.1).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
        let configs = manager
            .get_service(removed_config.0.number)
            .get_config_store()
            .read()
            .unwrap()
            .all_configs();
        assert_eq!(1, configs.len());

        shutdown.cancel();
    }

    fn run_determine_owned_partition_test(
        total_partitions: u16,
        checker_number: u16,
        total_pods: u16,
        expected_partitions: Vec<u16>,
    ) {
        let config = Config {
            config_provider_redis_total_partitions: total_partitions,
            checker_number,
            total_checkers: total_pods,
            ..Default::default()
        };
        assert_eq!(
            determine_owned_partitions(&config),
            expected_partitions.into_iter().collect()
        );
    }

    #[tokio::test]
    async fn test_determine_owned_partitions() {
        run_determine_owned_partition_test(2, 0, 1, vec![0, 1]);
        run_determine_owned_partition_test(2, 0, 2, vec![0]);
        run_determine_owned_partition_test(2, 1, 2, vec![1]);
        run_determine_owned_partition_test(100, 1, 10, vec![1, 11, 21, 31, 41, 51, 61, 71, 81, 91]);
        run_determine_owned_partition_test(100, 9, 10, vec![9, 19, 29, 39, 49, 59, 69, 79, 89, 99]);
    }

    #[tokio::test]
    #[should_panic(expected = "checker_number 1 must be less than total_checkers 1")]
    async fn test_determine_owned_partitions_checker_number_too_high() {
        run_determine_owned_partition_test(2, 1, 1, vec![0, 1]);
    }
}
