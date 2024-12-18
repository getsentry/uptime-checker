use chrono::TimeDelta;
use rust_arroyo::{
    backends::kafka::{config::KafkaConfig, types::KafkaPayload, InitialOffset},
    processing::{
        strategies::{
            noop::Noop, run_task::RunTask, InvalidMessage, ProcessingStrategy,
            ProcessingStrategyFactory,
        },
        StreamProcessor,
    },
    types::{InnerMessage, Message, Partition, Topic},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::task::{self, JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{app::config::Config, manager::Manager, types::check_config::CheckConfig};

#[tracing::instrument(skip_all)]
fn register_config(
    manager: Arc<Manager>,
    message: Message<KafkaPayload>,
) -> Result<Message<KafkaPayload>, InvalidMessage> {
    let InnerMessage::BrokerMessage(ref broker_message) = message.inner_message else {
        panic!("Expected BrokerMessage, got {:?}", message.inner_message);
    };

    let partition = broker_message.partition.index;

    let key = broker_message.payload.key().ok_or_else(|| {
        tracing::error!("config_consumer.missing_payload_key");
        InvalidMessage::from(broker_message)
    })?;
    let subscription_id = Uuid::from_slice(key).map_err(|err| {
        tracing::error!(?err, got_key = ?key, "config_consumer.invalid_uuid_message_key");
        InvalidMessage::from(broker_message)
    })?;

    // TODO(epurkhiser): We may want to use a batching strategy here to avoid lock contention on
    // the ConfigStore.

    match broker_message.payload.payload() {
        // Register new configuration
        Some(payload) => {
            let config: CheckConfig = rmp_serde::from_slice(payload).map_err(|err| {
                tracing::error!(?err, "config_consumer.invalid_config_message");
                InvalidMessage::from(broker_message)
            })?;

            if config.subscription_id != subscription_id {
                tracing::error!(?key, ?config.subscription_id, "config_consumer.key_mismatch");
                return Err(InvalidMessage::from(broker_message));
            }

            // Store configuration
            tracing::debug!(config = ?config, "config_consumer.config_added");
            manager
                .get_service(partition)
                .get_config_store()
                .write()
                .expect("Lock poisoned")
                .add_config(Arc::new(config));
        }
        // Remove existing configuration
        None => {
            manager
                .get_service(partition)
                .get_config_store()
                .write()
                .expect("Lock poisoned")
                .remove_config(subscription_id);
            tracing::debug!(%subscription_id, "config_consumer.config_removed");
        }
    }

    Ok(message)
}

struct ConfigConsumerFactory {
    manager: Arc<Manager>,
}

impl ProcessingStrategyFactory<KafkaPayload> for ConfigConsumerFactory {
    fn create(&self) -> Box<dyn ProcessingStrategy<KafkaPayload>> {
        tracing::info!("config_consumer.creating_processing_strategy");
        let manager = self.manager.clone();

        Box::new(RunTask::new(
            move |message| register_config(manager.clone(), message),
            Noop {},
        ))
    }

    fn update_partitions(&self, partitions: &HashMap<Partition, u64>) {
        self.manager
            .update_partitions(&partitions.keys().map(|p| p.index).collect())
    }
}

/// Runs a kafka consumer to read configurations from the uptime-configs topic. Each new config is
/// added to the ['ConfigStore'] as it is recieved. Null mesages will revoke configs.
pub fn run_config_consumer(
    config: &Config,
    manager: Arc<Manager>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    // XXX: In the future the consumer group should be derrived from the generation of
    // the deployment. This will allow old deployments to continue consuming
    // configurations until they are shut-down
    let consumer_config = KafkaConfig::new_consumer_config(
        config.configs_kafka_cluster.to_owned(),
        "uptime-configs-consumer-group-0".to_string(),
        InitialOffset::Earliest,
        false,
        TimeDelta::seconds(60).num_milliseconds() as usize,
        None,
    );

    let local_manager = manager.clone();

    let stream_processor = StreamProcessor::with_kafka(
        consumer_config,
        ConfigConsumerFactory { manager },
        Topic::new(&config.configs_kafka_topic),
        None,
    );

    let mut processing_handle = stream_processor.get_handle();

    let join_handle = task::spawn_blocking(|| {
        tracing::info!("config_consumer.started");
        stream_processor
            .run()
            .expect("Failed to run config consumer");
    });

    tokio::spawn(async move {
        shutdown.cancelled().await;
        tracing::info!("config_consumer.shutdown_starting");
        processing_handle.signal_shutdown();
        join_handle
            .await
            .expect("Failed to join config consumer consumer thread");

        // XXX: Ensure all partitions are revoked once the consumer has shutdown. I would expect
        // the conumser to do this, but it doesn't seem like it always does?
        local_manager.update_partitions(&HashSet::new());

        tracing::info!("config_consumer.shutdown_complete");
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rust_arroyo::processing::strategies::ProcessingStrategyFactory;
    use rust_arroyo::{
        backends::kafka::types::KafkaPayload,
        types::{BrokerMessage, InnerMessage, Message, Partition, Topic},
    };
    use similar_asserts::assert_eq;
    use std::collections::HashMap;
    use std::{sync::Arc, vec};
    use uuid::uuid;

    use super::{register_config, ConfigConsumerFactory};
    use crate::app::config::Config;
    use crate::{manager::Manager, types::check_config::CheckConfig};

    #[tokio::test]
    async fn test_update_config_store() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.update_partitions(&vec![0u16].into_iter().collect());

        // Example msgpack taken from
        // sentry-kafka-schemas/examples/uptime-configs/1/example.msgpack
        let example = vec![
            0x86, 0xaf, 0x73, 0x75, 0x62, 0x73, 0x63, 0x72, 0x69, 0x70, 0x74, 0x69, 0x6f, 0x6e,
            0x5f, 0x69, 0x64, 0xd9, 0x20, 0x64, 0x37, 0x36, 0x32, 0x39, 0x63, 0x36, 0x63, 0x38,
            0x32, 0x62, 0x65, 0x34, 0x66, 0x36, 0x37, 0x39, 0x65, 0x65, 0x37, 0x38, 0x61, 0x30,
            0x64, 0x39, 0x37, 0x37, 0x38, 0x35, 0x36, 0x64, 0x32, 0xa3, 0x75, 0x72, 0x6c, 0xb0,
            0x68, 0x74, 0x74, 0x70, 0x3a, 0x2f, 0x2f, 0x73, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x2e,
            0x69, 0x6f, 0xb0, 0x69, 0x6e, 0x74, 0x65, 0x72, 0x76, 0x61, 0x6c, 0x5f, 0x73, 0x65,
            0x63, 0x6f, 0x6e, 0x64, 0x73, 0xcd, 0x01, 0x2c, 0xaa, 0x74, 0x69, 0x6d, 0x65, 0x6f,
            0x75, 0x74, 0x5f, 0x6d, 0x73, 0xcd, 0x01, 0xf4, 0xae, 0x61, 0x63, 0x74, 0x69, 0x76,
            0x65, 0x5f, 0x72, 0x65, 0x67, 0x69, 0x6f, 0x6e, 0x73, 0x92, 0xa7, 0x75, 0x73, 0x2d,
            0x77, 0x65, 0x73, 0x74, 0xa6, 0x65, 0x75, 0x72, 0x6f, 0x70, 0x65, 0xb4, 0x72, 0x65,
            0x67, 0x69, 0x6f, 0x6e, 0x5f, 0x73, 0x63, 0x68, 0x65, 0x64, 0x75, 0x6c, 0x65, 0x5f,
            0x6d, 0x6f, 0x64, 0x65, 0xab, 0x72, 0x6f, 0x75, 0x6e, 0x64, 0x5f, 0x72, 0x6f, 0x62,
            0x69, 0x6e,
        ];

        let example_uuid = uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2");

        let message = Message {
            inner_message: InnerMessage::BrokerMessage(BrokerMessage {
                partition: Partition {
                    index: 0,
                    topic: Topic::new("uptime-configs"),
                },
                payload: KafkaPayload::new(Some(example_uuid.into()), None, Some(example)),
                offset: 0,
                timestamp: Utc::now(),
            }),
        };
        let _ = register_config(manager.clone(), message);

        let configs = manager
            .get_service(0)
            .get_config_store()
            .read()
            .unwrap()
            .all_configs();

        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].subscription_id,
            uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2")
        );
    }

    #[tokio::test]
    async fn test_drop_config() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        manager.update_partitions(&vec![0u16].into_iter().collect());

        let example_config = Arc::new(CheckConfig::default());
        manager
            .get_service(0)
            .get_config_store()
            .write()
            .unwrap()
            .add_config(example_config.clone());

        // Empty message which will be ready for log compaction
        let message = Message {
            inner_message: InnerMessage::BrokerMessage(BrokerMessage {
                partition: Partition {
                    index: 0,
                    topic: Topic::new("uptime-configs"),
                },
                payload: KafkaPayload::new(
                    Some(example_config.clone().subscription_id.into()),
                    None,
                    None,
                ),
                offset: 0,
                timestamp: Utc::now(),
            }),
        };
        let _ = register_config(manager.clone(), message);

        let configs = manager
            .get_service(0)
            .get_config_store()
            .read()
            .unwrap()
            .all_configs();

        // example_config was removed
        assert_eq!(configs.len(), 0);
        assert_eq!(Arc::strong_count(&example_config), 1);
    }

    #[tokio::test]
    async fn test_update_partition() {
        let manager = Manager::start_without_consumer(Arc::new(Config::default()));
        let factory = ConfigConsumerFactory { manager };
        let mut partitions: HashMap<Partition, u64> = HashMap::new();
        partitions.insert(
            Partition {
                index: 0,
                topic: Topic::new("uptime-configs"),
            },
            0,
        );
        factory.update_partitions(&partitions);
        assert_eq!(factory.manager.get_service(0).get_partition(), 0);
        partitions.remove(&Partition {
            index: 0,
            topic: Topic::new("uptime-configs"),
        });
        partitions.insert(
            Partition {
                index: 1,
                topic: Topic::new("uptime-configs"),
            },
            1,
        );
        factory.update_partitions(&partitions);
        assert!(!factory.manager.has_service(0));
        assert!(factory.manager.has_service(1));
    }
}
