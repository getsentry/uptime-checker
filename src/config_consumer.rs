use std::{collections::HashMap, sync::Arc, thread, time::Duration};

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
use tokio::{
    sync::oneshot::{self, Receiver},
    task::JoinHandle,
    time::{interval, Instant},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::{app::config::Config, config_store::RwConfigStore, types::check_config::CheckConfig};

#[tracing::instrument(skip_all)]
fn update_config_store(
    config_store: Arc<RwConfigStore>,
    message: Message<KafkaPayload>,
) -> Result<Message<KafkaPayload>, InvalidMessage> {
    let InnerMessage::BrokerMessage(ref broker_message) = message.inner_message else {
        panic!("Expected BrokerMessage, got {:?}", message.inner_message);
    };

    let key = broker_message.payload.key().ok_or_else(|| {
        error!("Missing message key (subscription_id)");
        InvalidMessage::from(broker_message)
    })?;
    let subscription_id = Uuid::from_slice(key).map_err(|err| {
        error!(?err, got_key = ?key, "Key must be a valid subscription_id UUID.");
        InvalidMessage::from(broker_message)
    })?;

    // TODO(epurkhiser): We may want to use a batching strategy here to avoid lock contention on
    // the ConfigStore.

    match broker_message.payload.payload() {
        // Register new configuration
        Some(payload) => {
            let mut config: CheckConfig = rmp_serde::from_slice(payload).map_err(|err| {
                error!(?err, "Failed to decode config message");
                InvalidMessage::from(broker_message)
            })?;

            if config.subscription_id != subscription_id {
                error!(?key, ?config.subscription_id, "Config key mismatch");
                return Err(InvalidMessage::from(broker_message));
            }

            config.partition = broker_message.partition.index;

            // Store configuration
            debug!(config = ?config, "Consumed configuration");
            config_store
                .write()
                .expect("Lock poisoned")
                .add_config(Arc::new(config));
        }
        // Remove existing configuration
        None => {
            config_store
                .write()
                .expect("Lock poisoned")
                .remove_config(subscription_id);
            debug!(%subscription_id, "Removed configuration");
        }
    }

    Ok(message)
}

struct ConfigConsumerFactory {
    config_store: Arc<RwConfigStore>,
}

impl ProcessingStrategyFactory<KafkaPayload> for ConfigConsumerFactory {
    fn create(&self) -> Box<dyn ProcessingStrategy<KafkaPayload>> {
        let config_store = self.config_store.clone();

        Box::new(RunTask::new(
            move |message| update_config_store(config_store.clone(), message),
            Noop {},
        ))
    }

    fn update_partitions(&self, partitions: &HashMap<Partition, u64>) {
        // Revoke configurations for partitions that are no longer assigned to this consumer.
        self.config_store
            .write()
            .expect("Lock poisoned")
            .update_partitions(&partitions.keys().map(|p| p.index).collect())
    }
}

/// Runs a kafka consumer to read configurations from the uptime-configs topic. Each new config is
/// added to the ['ConfigStore'] as it is recieved. Null mesages will revoke configs.
pub fn run_config_consumer(
    config: &Config,
    config_store: Arc<RwConfigStore>,
    shutdown: CancellationToken,
) -> (JoinHandle<()>, Receiver<()>) {
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

    let stream_processor = StreamProcessor::with_kafka(
        consumer_config,
        ConfigConsumerFactory {
            config_store: config_store.clone(),
        },
        Topic::new(&config.configs_kafka_topic),
        None,
    );

    let wait_booted = wait_for_boot(config_store.clone());

    let mut processing_handle = stream_processor.get_handle();

    let join_handle = thread::spawn(move || {
        info!("Starting config consumer");
        stream_processor
            .run()
            .expect("Failed to run config consumer");
    });

    let shutdown_handle = tokio::spawn(async move {
        shutdown.cancelled().await;
        processing_handle.signal_shutdown();
        join_handle
            .join()
            .expect("Failed to join config consumer consumer thread");

        info!("Config consuemr shutdown");
    });

    (shutdown_handle, wait_booted)
}

/// How long does the consumer need to be idle before we consider it to have "finished" reading the
/// backlog of configs.
const BOOT_IDLE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long will we wait while the consumer has not consumed *anything* before we consider it to
/// be ready. This handles the case where there is simply nothing in the config topic backlog.
const BOOT_MAX_IDLE: Duration = Duration::from_secs(10);

/// This function waits for the config_store to have completed the "boot-up" of loading a backlog
/// of configs.
///
/// This works by waiting for the ConfigStore to not have been updated for more than BOOT_MAX_IDLE
/// time. In practice this means that a new config was not produced into the topic for more than
/// the number of milliseconds configured. Since new configs are added into the configs topic at a
/// slow rate, we can be sure we've read all of the backlog when we start idling on updates.
///
/// To handle the case where there are NO configs in the topic, we will wait BOOT_MAX_IDLE duration
/// while the last_update is empty.
///
/// XXX: This makes the assumption that the there will NOT be a large volume of configs being
/// produced at all times, such that the cons
///
/// The returned Receiver can be awaited
fn wait_for_boot(config_store: Arc<RwConfigStore>) -> Receiver<()> {
    let start = Instant::now();
    let (boot_finished, boot_finished_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut interval = interval(Duration::from_millis(100));

        loop {
            let tick = interval.tick().await;
            let elapsed = tick - start;

            let last_update = config_store
                .read()
                .expect("Lock poisoned")
                .get_last_update();

            // If it's been longer than the BOOT_MAX_IDLE and we haven't updated the config store
            // we can assume there was nothing in the backlog to read.
            if last_update.is_none() && elapsed >= BOOT_MAX_IDLE {
                break;
            }

            if last_update.is_some_and(|t| (tick - t) >= BOOT_IDLE_TIMEOUT) {
                break;
            }
        }

        let boot_time_ms = start.elapsed().as_millis();
        let total_configs = config_store
            .read()
            .expect("Lock poisoned")
            .all_configs()
            .len();

        tracing::info!(boot_time_ms, total_configs, "bootup_complete");
        metrics::gauge!("config_consumer.boot_time_ms").set(boot_time_ms as f64);
        metrics::gauge!("config_consumer.total_configs").set(total_configs as f64);

        boot_finished.send(()).expect("Failed to report bo");
    });

    boot_finished_rx
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, task::Poll, time::Duration};

    use chrono::Utc;
    use futures::poll;
    use rust_arroyo::{
        backends::kafka::types::KafkaPayload,
        types::{BrokerMessage, InnerMessage, Message, Partition, Topic},
    };
    use similar_asserts::assert_eq;
    use tokio::time::sleep;
    use uuid::uuid;

    use crate::{
        config_consumer::{BOOT_IDLE_TIMEOUT, BOOT_MAX_IDLE},
        config_store::ConfigStore,
        types::check_config::CheckConfig,
    };

    use super::update_config_store;

    #[test]
    fn test_update_config_store() {
        let config_store = Arc::new(ConfigStore::new_rw());

        // Example msgpack taken from
        // sentry-kafka-schemas/examples/uptime-configs/1/example.msgpack
        let example = vec![
            0x84, 0xaf, 0x73, 0x75, 0x62, 0x73, 0x63, 0x72, 0x69, 0x70, 0x74, 0x69, 0x6f, 0x6e,
            0x5f, 0x69, 0x64, 0xd9, 0x20, 0x64, 0x37, 0x36, 0x32, 0x39, 0x63, 0x36, 0x63, 0x38,
            0x32, 0x62, 0x65, 0x34, 0x66, 0x36, 0x37, 0x39, 0x65, 0x65, 0x37, 0x38, 0x61, 0x30,
            0x64, 0x39, 0x37, 0x37, 0x38, 0x35, 0x36, 0x64, 0x32, 0xa3, 0x75, 0x72, 0x6c, 0xb0,
            0x68, 0x74, 0x74, 0x70, 0x3a, 0x2f, 0x2f, 0x73, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x2e,
            0x69, 0x6f, 0xb0, 0x69, 0x6e, 0x74, 0x65, 0x72, 0x76, 0x61, 0x6c, 0x5f, 0x73, 0x65,
            0x63, 0x6f, 0x6e, 0x64, 0x73, 0xcd, 0x01, 0x2c, 0xaa, 0x74, 0x69, 0x6d, 0x65, 0x6f,
            0x75, 0x74, 0x5f, 0x6d, 0x73, 0xcd, 0x01, 0xf4,
        ];

        let example_uuid = uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2");

        let message = Message {
            inner_message: InnerMessage::BrokerMessage(BrokerMessage {
                partition: Partition {
                    index: 2,
                    topic: Topic::new("uptime-configs"),
                },
                payload: KafkaPayload::new(Some(example_uuid.into()), None, Some(example)),
                offset: 0,
                timestamp: Utc::now(),
            }),
        };
        let _ = update_config_store(config_store.clone(), message);

        let configs = config_store.read().unwrap().all_configs();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].partition, 2);
        assert_eq!(
            configs[0].subscription_id,
            uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2")
        );
    }

    #[test]
    fn test_drop_config() {
        let config_store = Arc::new(ConfigStore::new_rw());

        let example_config = Arc::new(CheckConfig::default());

        config_store
            .write()
            .unwrap()
            .add_config(example_config.clone());

        // Empty message which will be ready for log compaction
        let message = Message {
            inner_message: InnerMessage::BrokerMessage(BrokerMessage {
                partition: Partition {
                    index: 2,
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
        let _ = update_config_store(config_store.clone(), message);

        let configs = config_store.read().unwrap().all_configs();

        // example_config was removed
        assert_eq!(configs.len(), 0);
        assert_eq!(Arc::strong_count(&example_config), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn test_wait_for_boot() {
        let config_store = Arc::new(ConfigStore::new_rw());

        let wait_booted = super::wait_for_boot(config_store.clone());
        tokio::pin!(wait_booted);

        // nothing produced yet. Move time right before to the BOOT_MAX_IDLE.
        sleep(BOOT_MAX_IDLE - Duration::from_millis(100)).await;

        // We haven't marked the boot as complete
        assert_eq!(poll!(wait_booted.as_mut()), Poll::Pending);

        // Add a check config
        config_store
            .write()
            .unwrap()
            .add_config(Arc::new(CheckConfig::default()));

        // Move time forward to the BOOT_MAX_IDLE. This will NOT mark the boot as complete sicne we
        // just produced a config. We will need to wait another 400ms for it to complete
        sleep(Duration::from_millis(200)).await;

        assert_eq!(poll!(wait_booted.as_mut()), Poll::Pending);

        // Add a check config again (does not matter that it's the same)
        config_store
            .write()
            .unwrap()
            .add_config(Arc::new(CheckConfig::default()));

        // Advance past the BOOT_IDLE_TIMEOUT, we will now have finished
        sleep(BOOT_IDLE_TIMEOUT + Duration::from_millis(100)).await;

        assert_eq!(poll!(wait_booted.as_mut()), Poll::Ready(Ok(())));
    }

    // TODO(epurkhiser): We probably want to test the update_partition callback
}
