use redis::AsyncCommands;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{app::config::Config, manager::Manager, types::check_config::CheckConfig};

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

pub struct RedisConfigProvider {
    redis: redis::Client,
    partitions: HashSet<u16>,
}

impl RedisConfigProvider {
    pub fn new(redis_url: &str, partitions: HashSet<u16>) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis: redis::Client::open(redis_url)?,
            partitions,
        })
    }

    async fn monitor_configs(&self, manager: Arc<Manager>, _shutdown: CancellationToken) {
        // Start monitoring configs using this provider. Loads the initial configs and (todo)
        // monitors redis for updates
        let partitions = self.get_partition_keys();
        self.load_initial_configs(manager.clone(), &partitions)
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
}

pub fn run_config_provider(
    config: &Config,
    manager: Arc<Manager>,
    partitions: HashSet<u16>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    // Initializes the redis config provider and starts monitoring for config updates
    let provider = RedisConfigProvider::new(&config.redis_host, partitions)
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

    #[redis_test(start_paused = false)]
    async fn test_redis_config_provider_load_no_configs() {
        let config = Config {
            ..Default::default()
        };
        let test_partitions: HashSet<u16> = vec![0, 1].into_iter().collect();

        let redis = redis::Client::open(config.redis_host.clone()).unwrap();
        let mut conn = redis.get_multiplexed_tokio_connection().await.unwrap();

        let provider =
            RedisConfigProvider::new(config.redis_host.as_str(), test_partitions.clone()).unwrap();

        let partitions = provider.get_partition_keys();
        let all_keys: Vec<&String> = partitions
            .iter()
            .flat_map(|p| [&p.config_key, &p.update_key])
            .collect();
        let _: () = conn.del(&all_keys).await.unwrap();

        // Create manager and start provider
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.update_partitions(&test_partitions);

        let shutdown = CancellationToken::new();
        let _handle = run_config_provider(
            &Config::default(),
            manager.clone(),
            test_partitions,
            shutdown.clone(),
        );

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify configs were added to both partitions
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
        let config = Config {
            ..Default::default()
        };
        let test_partitions: HashSet<u16> = vec![0, 1].into_iter().collect();

        let redis = redis::Client::open(config.redis_host.clone()).unwrap();
        let mut conn = redis.get_multiplexed_tokio_connection().await.unwrap();

        let provider =
            RedisConfigProvider::new(config.redis_host.as_str(), test_partitions.clone()).unwrap();

        let partitions = provider.get_partition_keys();
        let all_keys: Vec<&String> = partitions
            .iter()
            .flat_map(|p| [&p.config_key, &p.update_key])
            .collect();
        let _: () = conn.del(&all_keys).await.unwrap();

        // Create manager and start provider
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.update_partitions(&test_partitions);

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
            let config_msg = rmp_serde::to_vec(&config).unwrap();
            let config_key = format!("uptime:configs:{}", partition.partition);
            let _: () = conn
                .hset(&config_key, config.subscription_id.to_string(), &config_msg)
                .await
                .unwrap();
        }

        let shutdown = CancellationToken::new();
        let _handle = run_config_provider(
            &Config::default(),
            manager.clone(),
            test_partitions,
            shutdown.clone(),
        );

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

        shutdown.cancel();
    }
}
