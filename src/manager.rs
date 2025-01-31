use futures::{Future, StreamExt};
use rust_arroyo::backends::kafka::config::KafkaConfig;
use std::collections::hash_map::Entry::Vacant;
use std::pin::Pin;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::app::config::{ConfigProviderMode, ProducerMode};
use crate::check_config_provider::redis_config_provider::run_config_provider;
use crate::check_executor::{run_executor, CheckSender};
use crate::config_waiter::wait_for_partition_boot;
use crate::producer::kafka_producer::KafkaResultsProducer;
use crate::{
    app::config::Config,
    checker::http_checker::HttpChecker,
    config_store::{ConfigStore, RwConfigStore},
    producer::vector_producer::VectorResultsProducer,
    scheduler::run_scheduler,
};

/// Represents the set of services that run per partition.
#[derive(Debug)]
pub struct PartitionedService {
    partition: u16,
    config: Arc<Config>,
    config_store: Arc<RwConfigStore>,
    shutdown_signal: CancellationToken,
    scheduler_join_handle: JoinHandle<()>,
}

pub fn build_progress_key(partition: u16) -> String {
    format!("scheduler_process::{}", partition).to_string()
}

impl PartitionedService {
    pub fn start(config: Arc<Config>, executor_sender: CheckSender, partition: u16) -> Self {
        let config_store = Arc::new(ConfigStore::new_rw());

        let waiter_config_store = config_store.clone();

        // TODO(epurkhiser): We may want to wait to start the scheduler until "booting" completes,
        // otherwise we may execute checks for old configs in a partition that are removed later in
        // the log.
        let shutdown_signal = CancellationToken::new();
        let config_loaded = wait_for_partition_boot(
            waiter_config_store,
            partition,
            shutdown_signal.clone(),
            config.region.clone(),
        );

        let scheduler_join_handle = run_scheduler(
            partition,
            config_store.clone(),
            executor_sender,
            shutdown_signal.clone(),
            build_progress_key(partition),
            config.redis_host.clone(),
            config_loaded,
            config.region.clone(),
            config.redis_enable_cluster,
        );

        Self {
            config,
            partition,
            config_store,
            shutdown_signal,
            scheduler_join_handle,
        }
    }

    pub fn get_partition(&self) -> u16 {
        self.partition
    }

    pub fn get_config_store(&self) -> Arc<RwConfigStore> {
        self.config_store.clone()
    }

    pub async fn stop(self) {
        self.shutdown_signal.cancel();
        self.scheduler_join_handle.await.unwrap();
        tracing::info!(partition = self.partition, "partitioned_service.shutdown");
    }
}

#[derive(Debug)]
pub struct Manager {
    config: Arc<Config>,
    services: RwLock<HashMap<u16, Arc<PartitionedService>>>,
    executor_sender: CheckSender,
    shutdown_sender: UnboundedSender<PartitionedService>,
    shutdown_signal: CancellationToken,
}

impl Manager {
    /// Starts the config consumer. When uptime-config partitions are assigned PartitionedService's
    /// will be started for each partition automatically. Each PartitionedService is responsible for
    /// scheduling configs belonging to that partition.
    ///
    /// The returned shutdown function may be called to stop the consumer and thus shutdown all
    /// PartitionedService's, stopping check execution.
    pub fn start(config: Arc<Config>) -> impl FnOnce() -> Pin<Box<dyn Future<Output = ()>>> {
        let checker = Arc::new(HttpChecker::new(
            !config.allow_internal_ips,
            config.disable_connection_reuse,
        ));

        let (executor_sender, (executor_join_handle, results_worker)) = match &config.producer_mode
        {
            ProducerMode::Vector => {
                let (results_producer, results_worker) = VectorResultsProducer::new(
                    &config.results_kafka_topic,
                    config.vector_endpoint.clone(),
                    config.vector_batch_size,
                    config.vector_max_retries,
                );
                // XXX: Executor will shutdown once the sender goes out of scope. This will happen once all
                // referneces of the Sender (executor_sender) are dropped.
                let (executor_sender, executor_handle) = run_executor(
                    config.checker_concurrency,
                    checker.clone(),
                    Arc::new(results_producer),
                    config.region.clone(),
                );
                (executor_sender, (executor_handle, results_worker))
            }
            ProducerMode::Kafka => {
                let kafka_overrides =
                    HashMap::from([("compression.type".to_string(), "lz4".to_string())]);
                let kafka_config = KafkaConfig::new_config(
                    config.results_kafka_cluster.to_owned(),
                    Some(kafka_overrides),
                );
                let producer = Arc::new(KafkaResultsProducer::new(
                    &config.results_kafka_topic,
                    kafka_config,
                ));
                // XXX: Executor will shutdown once the sender goes out of scope. This will happen once all
                // referneces of the Sender (executor_sender) are dropped.
                let (sender, handle) = run_executor(
                    config.checker_concurrency,
                    checker.clone(),
                    producer,
                    config.region.clone(),
                );
                let dummy_worker = tokio::spawn(async {});
                (sender, (handle, dummy_worker))
            }
        };

        let (shutdown_sender, shutdown_service_rx) = mpsc::unbounded_channel();

        let manager = Arc::new(Self {
            config,
            services: RwLock::new(HashMap::new()),
            executor_sender,
            shutdown_sender,
            shutdown_signal: CancellationToken::new(),
        });

        let consumer_join_handle = match &manager.config.config_provider_mode {
            ConfigProviderMode::Redis => run_config_provider(
                &manager.config,
                manager.clone(),
                manager.shutdown_signal.clone(),
            ),
        };

        let shutdown_signal = manager.shutdown_signal.clone();

        // process shutdown services as they are shutdown. All services must be shutdown before
        // the manager is completely stopped
        //
        // Shutdowns are processed concurrently so each shutdown will be started immedieatly.
        let services_join_handle = tokio::spawn(async move {
            let shutdown_stream: UnboundedReceiverStream<_> = shutdown_service_rx.into();

            shutdown_stream
                .for_each_concurrent(None, |service| service.stop())
                .await
        });

        move || {
            Box::pin(async move {
                shutdown_signal.cancel();
                consumer_join_handle.await.expect("Failed to stop consumer");

                results_worker.await.expect("Failed to stop vector worker");

                executor_join_handle.await.expect("Failed to stop executor");
                services_join_handle
                    .await
                    .expect("Failed to stop partitioned services");
            })
        }
    }

    pub fn get_service(&self, partition: u16) -> Arc<PartitionedService> {
        self.services
            .read()
            .unwrap()
            .get(&partition)
            .expect("Cannot access unregistered partition")
            .clone()
    }

    pub fn has_service(&self, partition: u16) -> bool {
        self.services.read().unwrap().contains_key(&partition)
    }

    /// Notify the manager for which parititions it is responsible for.
    ///
    /// Partitions that were previously known will have their services dropped. New partitions will
    /// register a new PartitionedService.
    pub fn update_partitions(&self, new_partitions: &HashSet<u16>) {
        let known_partitions: HashSet<_> = self.services.read().unwrap().keys().cloned().collect();

        // Drop partitions that we are no longer responsible for
        for removed_part in known_partitions.difference(new_partitions) {
            self.unregister_partition(*removed_part);
        }

        // Add new partitions and start partition services
        for new_partition in new_partitions.difference(&known_partitions) {
            self.register_partition(*new_partition);
        }
    }

    fn register_partition(&self, partition: u16) {
        tracing::info!(partition, "partition_update.registered_new");
        let mut services = self.services.write().unwrap();

        let Vacant(entry) = services.entry(partition) else {
            tracing::error!(partition, "partition_update.already_registered");
            return;
        };

        let service =
            PartitionedService::start(self.config.clone(), self.executor_sender.clone(), partition);
        entry.insert(Arc::new(service));
    }

    fn unregister_partition(&self, partition: u16) {
        tracing::info!(partition, "partition_update.unregistering");
        let mut services = self.services.write().unwrap();

        let Some(service) = services.remove(&partition).take() else {
            tracing::error!(partition, "partition_update.not_registered");
            return;
        };

        self.shutdown_sender
            .send(Arc::into_inner(service).expect("Cannot take ownership of service"))
            .expect("Cannot queue service for shutdown");
    }
}

#[cfg(test)]
mod tests {
    use crate::app::config::{Config, ConfigProviderMode};
    use crate::check_executor::CheckSender;
    use crate::manager::{Manager, PartitionedService};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc::{self};
    use tokio_util::sync::CancellationToken;
    use tracing_test::traced_test;

    impl Manager {
        pub fn start_without_consumer(config: Arc<Config>) -> Arc<Self> {
            let (executor_sender, _) = CheckSender::new();
            let (shutdown_sender, mut shutdown_service_rx) = mpsc::unbounded_channel();

            let manager = Arc::new(Self {
                config,
                services: RwLock::new(HashMap::new()),
                executor_sender,
                shutdown_sender,
                shutdown_signal: CancellationToken::new(),
            });

            // For mocking purposes just recieve shutdowns, no need to do anything with them
            tokio::spawn(async move { while shutdown_service_rx.recv().await.is_some() {} });

            // return the service shutdown reciever. If ownership of it is not given to the
            // caller it will be dropped and we won't be able to register services for shutdown
            manager
        }
    }

    #[tokio::test]
    async fn test_partitioned_service_get_config_store() {
        let (executor_sender, _) = CheckSender::new();
        let service = PartitionedService::start(Arc::new(Config::default()), executor_sender, 0);
        service.get_config_store();
        service.stop().await;
    }

    #[tokio::test]
    async fn test_start_stop() {
        let (executor_sender, _) = CheckSender::new();
        let service = PartitionedService::start(Arc::new(Config::default()), executor_sender, 0);
        service.stop().await;
    }

    #[tokio::test]
    async fn test_start_stop_redis() {
        let (executor_sender, _) = CheckSender::new();
        let config = Config {
            config_provider_mode: ConfigProviderMode::Redis,
            ..Default::default()
        };
        let service = PartitionedService::start(Arc::new(config), executor_sender, 0);
        service.stop().await;
    }

    #[tokio::test]
    async fn test_manager_get_service() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(0);
        assert_eq!(manager.get_service(0).partition, 0);
    }

    #[tokio::test]
    #[should_panic(expected = "Cannot access unregistered partition")]
    async fn test_manager_get_service_fail() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(0);
        manager.get_service(1);
    }

    #[tokio::test]
    async fn test_manager_register_partition() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(1);
        assert_eq!(manager.get_service(1).partition, 1);
    }

    #[tokio::test]
    #[traced_test]
    async fn test_manager_double_register_partition() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(1);
        manager.register_partition(1);
        assert!(logs_contain(
            "partition_update.already_registered partition=1"
        ));
    }

    #[tokio::test]
    #[should_panic(expected = "Cannot access unregistered partition")]
    async fn test_manager_unregister_partition() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(0);
        manager.unregister_partition(0);

        manager.get_service(0);
    }

    #[tokio::test]
    #[traced_test]
    async fn test_manager_unregister_unregistered_partition() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.unregister_partition(1);
        assert!(logs_contain("partition_update.not_registered partition=1"));
    }

    #[tokio::test]
    async fn test_manager_update_partitions() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(0);

        let new_partitions: HashSet<u16> = [0, 1, 2, 3].into_iter().collect();
        manager.update_partitions(&new_partitions);
        assert!(
            new_partitions.iter().all(|&partition| {
                let service = manager.get_service(partition);
                service.partition == partition
            }),
            "One or more partitions did not match"
        );

        let updated_partitions: HashSet<u16> = [1, 3].into_iter().collect();
        manager.update_partitions(&updated_partitions);
        assert!(
            updated_partitions
                .iter()
                .all(|&partition| { manager.get_service(partition).partition == partition }),
            "One or more partitions did not match"
        );

        assert!(
            new_partitions
                .difference(&updated_partitions)
                .all(|&partition| { !manager.has_service(partition) }),
            "Partition still exists"
        );
    }
}
