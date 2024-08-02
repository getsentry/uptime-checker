use futures::Future;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use std::collections::hash_map::Entry::Vacant;
use std::pin::Pin;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config_waiter::wait_for_partition_boot;
use crate::{
    app::config::Config,
    checker::http_checker::HttpChecker,
    config_consumer::run_config_consumer,
    config_store::{ConfigStore, RwConfigStore},
    producer::kafka_producer::KafkaResultsProducer,
    scheduler::run_scheduler,
};

/// Represents the set of services that run per partition.
#[derive(Debug)]
pub struct PartitionedService {
    pub partition: u16,
    config: Arc<Config>,
    config_store: Arc<RwConfigStore>,
    shutdown_signal: CancellationToken,
}

impl PartitionedService {
    pub fn new(config: Arc<Config>, partition: u16) -> Self {
        Self {
            config,
            partition,
            config_store: Arc::new(ConfigStore::new_rw()),
            shutdown_signal: CancellationToken::new(),
        }
    }

    pub fn get_config_store(&self) -> Arc<RwConfigStore> {
        self.config_store.clone()
    }

    /// Begin scheduling checks for this partition.
    pub fn start(&self) -> CancellationToken {
        let checker = Arc::new(HttpChecker::new());

        let producer = Arc::new(KafkaResultsProducer::new(
            &self.config.results_kafka_topic,
            KafkaConfig::new_config(self.config.results_kafka_cluster.to_owned(), None),
        ));

        let config_store = self.get_config_store();
        let partition = self.partition;

        // TODO(epurkhiser): We may want to wait to start the scheduler until "booting" completes,
        // otherwise we may execute checks for old configs in a partition that are removed later in
        // the log.
        tokio::spawn(async move { wait_for_partition_boot(config_store, partition).await });

        run_scheduler(
            self.partition,
            self.get_config_store(),
            checker,
            producer,
            self.shutdown_signal.clone(),
        );
        self.shutdown_signal.clone()
    }

    pub fn stop(&self) {
        self.shutdown_signal.cancel()
    }
}

#[derive(Debug)]
pub struct Manager {
    config: Arc<Config>,
    services: RwLock<HashMap<u16, Arc<PartitionedService>>>,
    shutdown_signal: CancellationToken,
}

impl Manager {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            services: RwLock::new(HashMap::new()),
            shutdown_signal: CancellationToken::new(),
        }
    }

    /// Instantiates a new manager with a single partition and a default config
    pub fn new_simple() -> Self {
        let config = Arc::new(Config::default());
        let manager = Self::new(config);
        manager.register_partition(0);
        manager
    }

    /// Starts the config consumer. When uptime-config partitions are assigned PartitionedService's
    /// will be started for each partition automatically. Each PartitionedService is responsible for
    /// scheduling configs belonging to that partition.
    ///
    /// The returned shutdown function may be called to stop the consumer and thus shutdown all
    /// PartitionedService's, stopping check execution.
    pub fn start(self: &Arc<Self>) -> impl FnOnce() -> Pin<Box<dyn Future<Output = ()>>> {
        let consumer_join_handle =
            run_config_consumer(&self.config, self.clone(), self.shutdown_signal.clone());

        let shutdown_signal = self.shutdown_signal.clone();
        move || {
            Box::pin(async move {
                shutdown_signal.cancel();
                consumer_join_handle.await.expect("Failed to stop consumer");
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
        info!(partition, "Registering new partition: {}", partition);
        let mut services = self.services.write().unwrap();

        let Vacant(entry) = services.entry(partition) else {
            error!(
                "Attempted to register already registered partition: {}",
                partition
            );
            return;
        };
        let service = PartitionedService::new(self.config.clone(), partition);
        service.start();
        entry.insert(Arc::new(service));
    }

    fn unregister_partition(&self, partition: u16) {
        info!(partition, "Unregistering revoked partition: {}", partition);
        let mut services = self.services.write().unwrap();

        let Some(service) = services.remove(&partition) else {
            error!(
                "Attempted to unregister a partition that is not registered: {}",
                partition
            );
            return;
        };
        service.stop();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::config::Config;
    use crate::manager::{Manager, PartitionedService};
    use std::collections::HashSet;
    use std::sync::Arc;
    use tracing_test::traced_test;

    #[test]
    fn test_partitioned_service_get_config_store() {
        let service = PartitionedService::new(Arc::new(Config::default()), 0);
        service.get_config_store();
    }

    #[tokio::test]
    async fn test_start_stop() {
        let service = PartitionedService::new(Arc::new(Config::default()), 0);
        let shutdown_signal = service.start();
        assert!(!shutdown_signal.is_cancelled());
        service.stop();
        assert!(shutdown_signal.is_cancelled());
    }

    #[tokio::test]
    async fn test_manager_get_service() {
        let manager = Manager::new_simple();
        assert_eq!(manager.get_service(0).partition, 0);
    }

    #[tokio::test]
    #[should_panic(expected = "Cannot access unregistered partition")]
    async fn test_manager_get_service_fail() {
        let manager = Manager::new_simple();
        manager.get_service(1);
    }

    #[tokio::test]
    async fn test_manager_register_partition() {
        let manager = Manager::new_simple();
        manager.register_partition(1);
        assert_eq!(manager.get_service(1).partition, 1);
    }

    #[tokio::test]
    #[traced_test]
    async fn test_manager_double_register_partition() {
        let manager = Manager::new_simple();
        manager.register_partition(1);
        manager.register_partition(1);
        assert!(logs_contain(
            "Attempted to register already registered partition: 1"
        ));
    }

    #[tokio::test]
    #[should_panic(expected = "Cannot access unregistered partition")]
    async fn test_manager_unregister_partition() {
        let manager = Manager::new_simple();
        manager.unregister_partition(0);
        manager.get_service(0);
    }

    #[tokio::test]
    #[traced_test]
    async fn test_manager_unregister_unregistered_partition() {
        let manager = Manager::new_simple();
        manager.unregister_partition(1);
        assert!(logs_contain(
            "Attempted to unregister a partition that is not registered: 1"
        ));
    }

    #[tokio::test]
    async fn test_manager_update_partitions() {
        let manager = Manager::new_simple();

        let new_partitions: HashSet<u16> = [0, 1, 2, 3].iter().cloned().collect();
        manager.update_partitions(&new_partitions);
        assert!(
            new_partitions.iter().all(|&partition| {
                let service = manager.get_service(partition);
                service.partition == partition
            }),
            "One or more partitions did not match"
        );

        let updated_partitions: HashSet<u16> = [1, 3].iter().cloned().collect();
        manager.update_partitions(&updated_partitions);
        assert!(
            updated_partitions.iter().all(|&partition| {
                let service = manager.get_service(partition);
                service.partition == partition
            }),
            "One or more partitions did not match"
        );

        assert!(
            new_partitions
                .difference(&updated_partitions)
                .all(|&partition| {
                    std::panic::catch_unwind(|| manager.get_service(partition)).is_err()
                }),
            "Partition still exists"
        );
    }
}
