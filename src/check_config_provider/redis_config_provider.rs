use redis::AsyncCommands;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{app::config::Config, manager::Manager, types::check_config::CheckConfig};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::time::Duration;
use tokio::time::interval;
use uuid::Uuid;

#[derive(Debug)]
pub struct RedisPartition {
    partition: u16,
    config_key: String,
    update_key: String,
}

impl RedisPartition {
    pub fn new(partition: u16) -> RedisPartition {
        RedisPartition {
            partition,
            config_key: format!("uptime:configs:{}", partition),
            update_key: format!("uptime:updates:{}", partition),
        }
    }
}

/// The action to perform when receiving a ConfigUpdate
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigUpdateAction {
    Add,
    Delete,
}

/// The CheckConfig represents a configuration for a single check.
#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigUpdate {
    /// Whether we're adding/updating or deleting a config.
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
    redis: redis::Client,
    partitions: HashSet<u16>,
    check_interval: Duration,
}

impl RedisConfigProvider {
    pub fn new(
        redis_url: &str,
        partitions: HashSet<u16>,
        check_interval: Duration,
    ) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis: redis::Client::open(redis_url)?,
            partitions,
            check_interval,
        })
    }

    async fn monitor_configs(&self, manager: Arc<Manager>, shutdown: CancellationToken) {
        // Start monitoring configs using this provider. Loads the initial configs and (todo)
        // monitors redis for updates
        let partitions = self.get_partition_keys();
        self.load_initial_configs(manager.clone(), &partitions)
            .await;
        self.monitor_updates(manager.clone(), &partitions, shutdown)
            .await;
    }

    fn get_partition_keys(&self) -> Vec<RedisPartition> {
        // Returns a RedisPartition for each managed partition.
        self.partitions
            .iter()
            .map(|p| RedisPartition::new(*p))
            .collect()
    }

    async fn load_initial_configs(&self, manager: Arc<Manager>, partitions: &[RedisPartition]) {
        // Fetch configs for all partitions from Redis and register them with the manager
        // TODO: Should we also register all the partitions here, or elsewhere?
        let mut conn = self
            .redis
            .get_multiplexed_tokio_connection()
            .await
            .expect("Unable to connect to Redis");

        // Initial load of all configs for all partitions
        for partition in partitions {
            let config_payloads: Vec<Vec<u8>> = conn
                .hvals(&partition.config_key)
                .await
                .expect("Unable to get configs");
            tracing::info!(
                partition = partition.partition,
                config_count = config_payloads.len(),
                "redis_config_provider.loading_initial_configs"
            );

            for config_payload in config_payloads {
                let config: CheckConfig = rmp_serde::from_slice(&config_payload)
                    .map_err(|err| {
                        tracing::error!(?err, "config_consumer.invalid_config_message");
                    })
                    .unwrap();
                manager
                    .get_service(partition.partition)
                    .get_config_store()
                    .write()
                    .unwrap()
                    .add_config(Arc::new(config));
            }
        }
    }

    async fn monitor_updates(
        &self,
        manager: Arc<Manager>,
        partitions: &[RedisPartition],
        shutdown: CancellationToken,
    ) {
        let mut conn = self
            .redis
            .get_multiplexed_tokio_connection()
            .await
            .expect("Unable to connect to Redis");
        let mut interval = interval(self.check_interval);

        while !shutdown.is_cancelled() {
            let _ = interval.tick().await;

            for partition in partitions.iter() {
                let mut pipe = redis::pipe();
                // We fetch all updates from the list and then delete the key. We do this
                // atomically so that there isn't any chance of a race
                let (config_adds, config_deletes): (Vec<_>, Vec<_>) = pipe
                    .atomic()
                    .hvals(&partition.update_key)
                    .del(&partition.update_key)
                    .query_async::<(Vec<Vec<u8>>, ())>(&mut conn)
                    .await
                    .unwrap()
                    .0 // Get just the LRANGE results
                    .iter()
                    .map(|payload| {
                        rmp_serde::from_slice::<ConfigUpdate>(payload).map_err(|err| {
                            tracing::error!(?err, "config_consumer.invalid_config_message");
                            err
                        })
                    })
                    .filter_map(Result::ok)
                    .partition(|cu| cu.action == ConfigUpdateAction::Add);

                config_deletes.into_iter().for_each(|config_delete| {
                    manager
                        .get_service(partition.partition)
                        .get_config_store()
                        .write()
                        .unwrap()
                        .remove_config(config_delete.subscription_id);
                    tracing::debug!(
                        %config_delete.subscription_id,
                        "config_consumer.config_removed"
                    );
                });

                if config_adds.is_empty() {
                    continue;
                }

                let config_payloads: Vec<Vec<u8>> = conn
                    .hget(
                        partition.config_key.clone(),
                        config_adds
                            .iter()
                            .map(|config| config.redis_key())
                            .collect::<Vec<_>>(),
                    )
                    .await
                    .unwrap();

                for config_payload in config_payloads {
                    let config: CheckConfig = rmp_serde::from_slice(&config_payload)
                        .map_err(|err| {
                            tracing::error!(?err, "config_consumer.invalid_config_message");
                        })
                        .unwrap();
                    tracing::debug!(
                        partition = partition.partition,
                        subscription_id = %config.subscription_id,
                        "redis_config_provider.adding_config"
                    );
                    manager
                        .get_service(partition.partition)
                        .get_config_store()
                        .write()
                        .unwrap()
                        .add_config(Arc::new(config));
                }
            }
        }
    }
}

pub fn run_config_provider(
    config: &Config,
    manager: Arc<Manager>,
    partitions: HashSet<u16>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    // Initializes the redis config provider and starts monitoring for config updates
    let provider = RedisConfigProvider::new(
        &config.redis_host,
        partitions,
        Duration::from_millis(config.config_provider_redis_update_ms),
    )
    .expect("Failed to create Redis config provider");

    tokio::spawn(async move {
        let monitor_shutdown = shutdown.clone();
        let monitor_task =
            tokio::spawn(async move { provider.monitor_configs(manager, monitor_shutdown).await });

        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("redis_config_provider.shutdown_requested");
            }
            _ = monitor_task => {
                tracing::error!("redis_config_provider.monitor_task_ended");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis_test_macro::redis_test;
    use std::time::Duration;
    use uuid::Uuid;

    async fn setup_test() -> (
        Config,
        HashSet<u16>,
        redis::aio::MultiplexedConnection,
        Vec<RedisPartition>,
        Arc<Manager>,
        CancellationToken,
    ) {
        let config = Config {
            config_provider_redis_update_ms: 10,
            ..Default::default()
        };
        let test_partitions: HashSet<u16> = vec![0, 1].into_iter().collect();
        let mut conn = redis::Client::open(config.redis_host.clone())
            .unwrap()
            .get_multiplexed_tokio_connection()
            .await
            .unwrap();

        let partitions = RedisConfigProvider::new(
            config.redis_host.as_str(),
            test_partitions.clone(),
            Duration::from_millis(10),
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
        manager.update_partitions(&test_partitions);
        let shutdown = CancellationToken::new();

        (config, test_partitions, conn, partitions, manager, shutdown)
    }

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_load_no_configs() {
        let (config, test_partitions, _, _, manager, shutdown) = setup_test().await;
        let _handle =
            run_config_provider(&config, manager.clone(), test_partitions, shutdown.clone());
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
        let (config, test_partitions, mut conn, partitions, manager, shutdown) = setup_test().await;
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

        let _handle =
            run_config_provider(&config, manager.clone(), test_partitions, shutdown.clone());

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify configs were added to both partitions
        for (partition, config) in partition_configs {
            let configs = manager
                .get_service(partition.partition)
                .get_config_store()
                .read()
                .unwrap()
                .all_configs();

            assert_eq!(configs, [Arc::new(config)]);
        }
    }

    async fn send_update(
        mut conn: redis::aio::MultiplexedConnection,
        partition: &RedisPartition,
        config: &CheckConfig,
    ) {
        let update = ConfigUpdate {
            action: ConfigUpdateAction::Add,
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
        mut conn: redis::aio::MultiplexedConnection,
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
        let (config, test_partitions, conn, partitions, manager, shutdown) = setup_test().await;

        let _handle =
            run_config_provider(&config, manager.clone(), test_partitions, shutdown.clone());

        tokio::time::sleep(Duration::from_millis(30)).await;

        for partition in &partitions {
            let configs = manager
                .get_service(partition.partition)
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
                .get_service(partition.partition)
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
            .get_service(removed_config.0.partition)
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
}
