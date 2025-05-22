use futures::StreamExt;
use rust_arroyo::backends::kafka::config::KafkaConfig;
use std::collections::hash_map::Entry::Vacant;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::app::config::{CheckerMode, ConfigProviderMode, ProducerMode};
use crate::check_config_provider::redis_config_provider::run_config_provider;
use crate::check_executor::{run_executor, CheckSender, ExecutorConfig};
use crate::checker::HttpChecker;
use crate::config_waiter::wait_for_partition_boot;
use crate::producer::kafka_producer::KafkaResultsProducer;
use crate::{
    app::config::Config,
    checker::isahc_checker::IsahcChecker,
    checker::reqwest_checker::ReqwestChecker,
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
    pub fn start(
        config: Arc<Config>,
        executor_sender: Arc<CheckSender>,
        partition: u16,
        tasks_finished_tx: mpsc::UnboundedSender<Result<(), anyhow::Error>>,
    ) -> Self {
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
            config.region,
        );

        let scheduler_join_handle = run_scheduler(
            partition,
            config_store.clone(),
            executor_sender,
            shutdown_signal.clone(),
            build_progress_key(partition),
            config.redis_host.clone(),
            config_loaded,
            config.region,
            config.redis_enable_cluster,
            config.redis_timeouts_ms,
            tasks_finished_tx,
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

        // Okay to unwrap here, since we're just shutting down.
        self.scheduler_join_handle.await.unwrap();
        tracing::info!(partition = self.partition, "partitioned_service.shutdown");
    }
}

#[derive(Debug)]
pub struct Manager {
    config: Arc<Config>,
    services: RwLock<HashMap<u16, Arc<PartitionedService>>>,
    executor_sender: Arc<CheckSender>,
    shutdown_sender: UnboundedSender<PartitionedService>,
    tasks_finished_tx: tokio::sync::mpsc::UnboundedSender<Result<(), anyhow::Error>>,
}

pub struct ManagerShutdown {
    tasks_finished_rx: mpsc::UnboundedReceiver<Result<(), anyhow::Error>>,
    shutdown_signal: CancellationToken,
    consumer_join_handle: JoinHandle<()>,
    results_worker: JoinHandle<()>,
    services_join_handle: JoinHandle<()>,
    executor_join_handle: JoinHandle<()>,
}

impl ManagerShutdown {
    pub async fn stop(self) {
        self.shutdown_signal.cancel();
        // Unwrapping here because we're just shutting down; it's okay to fail badly
        // at this point.
        self.consumer_join_handle.await.unwrap();

        self.results_worker.await.unwrap();

        self.services_join_handle.await.unwrap();

        self.executor_join_handle.await.unwrap();
    }

    pub async fn recv_task_finished(&mut self) -> Option<anyhow::Result<()>> {
        self.tasks_finished_rx.recv().await
    }
}

impl Manager {
    /// Starts the config consumer. When uptime-config partitions are assigned PartitionedService's
    /// will be started for each partition automatically. Each PartitionedService is responsible for
    /// scheduling configs belonging to that partition.
    ///
    /// The returned shutdown function may be called to stop the consumer and thus shutdown all
    /// PartitionedService's, stopping check execution.
    pub fn start(config: Arc<Config>) -> ManagerShutdown {
        let checker: Arc<HttpChecker> = Arc::new(match config.checker_mode {
            CheckerMode::Reqwest => ReqwestChecker::new(
                !config.allow_internal_ips,
                config.disable_connection_reuse,
                Duration::from_secs(config.pool_idle_timeout_secs),
                config.http_checker_dns_nameservers.clone(),
                config.interface.to_owned(),
            )
            .into(),
            CheckerMode::Isahc => IsahcChecker::new(
                config.disable_connection_reuse,
                Duration::from_secs(config.pool_idle_timeout_secs),
                config.interface.to_owned(),
            )
            .into(),
        });

        let executor_conf = ExecutorConfig {
            concurrency: config.checker_concurrency,
            failure_retries: config.failure_retries,
            region: config.region,
            record_task_metrics: config.record_task_metrics,
        };

        let cancel_token = CancellationToken::new();

        let (executor_sender, executor_join_handle, results_worker) = match &config.producer_mode {
            ProducerMode::Vector => {
                let (results_producer, results_worker) = VectorResultsProducer::new(
                    &config.results_kafka_topic,
                    config.vector_endpoint.clone(),
                    config.vector_batch_size,
                );
                let producer = Arc::new(results_producer);
                // XXX: Executor will shutdown once the sender goes out of scope. This will happen once all
                // referneces of the Sender (executor_sender) are dropped.
                let (sender, handle) = run_executor(
                    checker.clone(),
                    producer,
                    executor_conf,
                    cancel_token.clone(),
                );
                (sender, handle, results_worker)
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
                    checker.clone(),
                    producer,
                    executor_conf,
                    cancel_token.clone(),
                );
                let dummy_worker = tokio::spawn(async {});
                (sender, handle, dummy_worker)
            }
        };

        let (shutdown_sender, shutdown_service_rx) = mpsc::unbounded_channel();
        let (tasks_finished_tx, tasks_finished_rx) = mpsc::unbounded_channel();

        let manager = Arc::new(Self {
            config,
            services: RwLock::new(HashMap::new()),
            executor_sender,
            shutdown_sender,
            tasks_finished_tx,
        });

        let consumer_join_handle = match &manager.config.config_provider_mode {
            ConfigProviderMode::Redis => {
                run_config_provider(&manager.config, manager.clone(), cancel_token.clone())
            }
        };

        let shutdown_signal = cancel_token.clone();

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

        ManagerShutdown {
            tasks_finished_rx,
            shutdown_signal,
            consumer_join_handle,
            results_worker,
            services_join_handle,
            executor_join_handle,
        }
    }

    pub fn get_service(&self, partition: u16) -> Arc<PartitionedService> {
        self.services
            .read()
            .expect("Lock should not be poisoned")
            .get(&partition)
            .expect("Parition should have been registered")
            .clone()
    }

    pub fn has_service(&self, partition: u16) -> bool {
        self.services
            .read()
            .expect("Lock should not be poisoned")
            .contains_key(&partition)
    }

    /// Notify the manager for which parititions it is responsible for.
    ///
    /// Partitions that were previously known will have their services dropped. New partitions will
    /// register a new PartitionedService.
    pub fn update_partitions(&self, new_partitions: &HashSet<u16>) {
        let known_partitions: HashSet<_> = self
            .services
            .read()
            .expect("Lock should not be poisoned")
            .keys()
            .cloned()
            .collect();

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
        let mut services = self.services.write().expect("Lock should not be poisoned");

        let Vacant(entry) = services.entry(partition) else {
            tracing::error!(partition, "partition_update.already_registered");
            return;
        };

        let service = PartitionedService::start(
            self.config.clone(),
            self.executor_sender.clone(),
            partition,
            self.tasks_finished_tx.clone(),
        );
        entry.insert(Arc::new(service));
    }

    fn unregister_partition(&self, partition: u16) {
        tracing::info!(partition, "partition_update.unregistering");
        let mut services = self.services.write().expect("Lock should not be poisoned");

        let Some(service) = services.remove(&partition) else {
            tracing::error!(partition, "partition_update.not_registered");
            return;
        };

        self.shutdown_sender
            .send(Arc::into_inner(service).expect("Should be no outstanding references to arc"))
            .expect("Shutdown sender should still be open");
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
    use tracing_test::traced_test;

    impl Manager {
        pub fn start_without_consumer(config: Arc<Config>) -> Arc<Self> {
            let (executor_sender, _) = CheckSender::new();
            let (shutdown_sender, mut shutdown_service_rx) = mpsc::unbounded_channel();
            let (partition_finished_tx, _) = mpsc::unbounded_channel();

            let manager = Arc::new(Self {
                config,
                services: RwLock::new(HashMap::new()),
                executor_sender: Arc::new(executor_sender),
                shutdown_sender,
                tasks_finished_tx: partition_finished_tx,
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
        let (partition_finished_tx, _) = mpsc::unbounded_channel();

        let service = PartitionedService::start(
            Arc::new(Config::default()),
            Arc::new(executor_sender),
            0,
            partition_finished_tx,
        );
        service.get_config_store();
        service.stop().await;
    }

    #[tokio::test]
    async fn test_start_stop() {
        let (executor_sender, _) = CheckSender::new();
        let (partition_finished_tx, _) = mpsc::unbounded_channel();

        let service = PartitionedService::start(
            Arc::new(Config::default()),
            Arc::new(executor_sender),
            0,
            partition_finished_tx,
        );
        service.stop().await;
    }

    #[tokio::test]
    async fn test_start_stop_redis() {
        let (executor_sender, _) = CheckSender::new();
        let (partition_finished_tx, _) = mpsc::unbounded_channel();
        let config = Config {
            config_provider_mode: ConfigProviderMode::Redis,
            ..Default::default()
        };
        let service = PartitionedService::start(
            Arc::new(config),
            Arc::new(executor_sender),
            0,
            partition_finished_tx,
        );
        service.stop().await;
    }

    #[tokio::test]
    async fn test_manager_get_service() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.register_partition(0);
        assert_eq!(manager.get_service(0).partition, 0);
    }

    #[tokio::test]
    #[should_panic(expected = "Parition should have been registered")]
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
    #[should_panic(expected = "Parition should have been registered")]
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
