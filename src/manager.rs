use futures::Future;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use std::collections::hash_map::Entry::Vacant;
use std::pin::Pin;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::check_executor::{run_executor, CheckSender};
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
        tokio::spawn(async move { wait_for_partition_boot(waiter_config_store, partition).await });

        let shutdown_signal = CancellationToken::new();
        let scheduler_join_handle = run_scheduler(
            partition,
            config_store.clone(),
            executor_sender,
            shutdown_signal.clone(),
            build_progress_key(partition),
            config.redis_host.clone(),
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

    pub fn stop(&self) {
        self.shutdown_signal.cancel();
        // TODO: We shuld change this method to mut and async
        // self.scheduler_join_handle.await.unwrap();
    }
}

#[derive(Debug)]
pub struct Manager {
    config: Arc<Config>,
    services: RwLock<HashMap<u16, Arc<PartitionedService>>>,
    executor_sender: CheckSender,
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
        let checker = Arc::new(HttpChecker::new());

        let producer = Arc::new(KafkaResultsProducer::new(
            &config.results_kafka_topic,
            KafkaConfig::new_config(config.results_kafka_cluster.to_owned(), None),
        ));

        // XXX: Executor will shutdown once the sender goes out of scope. This will happen once all
        // referneces of the Sender (executor_sender) are dropped.
        let (executor_sender, executor_join_handle) =
            run_executor(config.checker_concurrency, checker, producer);

        let manager = Arc::new(Self {
            config,
            services: RwLock::new(HashMap::new()),
            executor_sender,
            shutdown_signal: CancellationToken::new(),
        });

        let consumer_join_handle = run_config_consumer(
            &manager.config,
            manager.clone(),
            manager.shutdown_signal.clone(),
        );

        let shutdown_signal = manager.shutdown_signal.clone();

        move || {
            Box::pin(async move {
                shutdown_signal.cancel();
                consumer_join_handle.await.expect("Failed to stop consumer");
                executor_join_handle.await.expect("Failed to stop executor");
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

        // TODO(epurkhiser): When `stop` becomes async we'll need a task and queue that we can put
        // this stop call in to shutdown the service and wait for it to complete async
        service.stop();
    }
}

#[cfg(test)]
mod tests {
    use crate::app::config::Config;
    use crate::manager::{Manager, PartitionedService};
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use tracing_test::traced_test;

    impl Manager {
        pub fn start_without_consumer(config: Arc<Config>) -> Arc<Self> {
            let (executor_sender, _) = mpsc::unbounded_channel();
            Arc::new(Self {
                config,
                services: RwLock::new(HashMap::new()),
                executor_sender,
                shutdown_signal: CancellationToken::new(),
            })
        }
    }

    #[tokio::test]
    async fn test_partitioned_service_get_config_store() {
        let (executor_sender, _) = mpsc::unbounded_channel();
        let service = PartitionedService::start(Arc::new(Config::default()), executor_sender, 0);
        service.get_config_store();
        service.stop();
    }

    #[tokio::test]
    async fn test_start_stop() {
        let (executor_sender, _) = mpsc::unbounded_channel();
        let service = PartitionedService::start(Arc::new(Config::default()), executor_sender, 0);
        service.stop();
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
