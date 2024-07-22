use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use rust_arroyo::backends::kafka::config::KafkaConfig;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
}

impl PartitionedService {
    fn new(config: Arc<Config>, partition: u16) -> Self {
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
    pub fn start(&self) {
        let checker = Arc::new(HttpChecker::new());

        let producer = Arc::new(KafkaResultsProducer::new(
            &self.config.results_kafka_topic,
            KafkaConfig::new_config(self.config.results_kafka_cluster.to_owned(), None),
        ));

        let check_scheduler = run_scheduler(
            self.config_store.clone(),
            checker,
            producer,
            self.shutdown_signal.clone(),
        );
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

    pub fn start(&self, manager: Arc<Manager>) {
        let _config_consumer =
            run_config_consumer(&self.config, manager, self.shutdown_signal.clone());
    }

    pub async fn shutdown(&self) {
        self.shutdown_signal.cancel();
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
        for new_partiton in new_partitions.difference(&known_partitions) {
            self.register_partition(*new_partiton);
            self.get_service(*new_partiton).start();
        }
    }

    fn register_partition(&self, partition: u16) {
        info!(partition, "Registering new partition");
        self.services.write().unwrap().insert(
            partition,
            Arc::new(PartitionedService::new(self.config.clone(), partition)),
        );
    }

    fn unregister_partition(&self, partition: u16) {
        info!(partition, "Unregistering revoked partition");
        self.services.write().unwrap().remove(&partition);
    }
}
