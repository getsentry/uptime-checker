use crate::config_store::RwConfigStore;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::oneshot::{self, Receiver},
    time::{interval, Instant},
};
use tokio_util::sync::CancellationToken;

/// How long does the consumer need to be idle before we consider it to have "finished" reading the
/// backlog of configs.
const BOOT_IDLE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long will we wait while the consumer has not consumed *anything* before we consider it to
/// be ready. This handles the case where there is simply nothing in the config topic backlog.
const BOOT_MAX_IDLE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BootResult {
    Started,
    Cancelled,
}

/// This function waits for the config_store to have completed the "boot-up" of loading a backlog
/// of configs in a partition.
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
/// produced at all times.
///
/// The returned Receiver can be awaited
pub fn wait_for_partition_boot(
    config_store: Arc<RwConfigStore>,
    partition: u16,
    shutdown: CancellationToken,
) -> Receiver<BootResult> {
    let start = Instant::now();
    let (boot_finished, boot_finished_rx) = oneshot::channel::<BootResult>();

    tokio::spawn(async move {
        let mut interval = interval(Duration::from_millis(100));

        while !shutdown.is_cancelled() {
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

        let boot_string = format!(
            "config_consumer.partition_boot_{}",
            if shutdown.is_cancelled() {
                "cancelled"
            } else {
                "complete"
            }
        )
        .to_string();
        tracing::info!(boot_time_ms, total_configs, partition, boot_string);
        if shutdown.is_cancelled() {
            boot_finished.send(BootResult::Cancelled).expect("Failed to report boot cancellation");
            return
        }

        tracing::info!(
            boot_time_ms,
            total_configs,
            partition,
            "config_consumer.partition_boot_complete",
        );

        metrics::gauge!("config_consumer.partition_boot_time_ms", "partition" => partition.to_string())
            .set(boot_time_ms as f64);
        metrics::gauge!("config_consumer.partition_total_configs", "partition" => partition.to_string())
            .set(total_configs as f64);

        boot_finished
            .send(BootResult::Started)
            .expect("Failed to report boot");
    });

    boot_finished_rx
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, task::Poll};

    use futures::poll;
    use tokio::time::sleep;

    use super::*;
    use crate::{config_store::ConfigStore, types::check_config::CheckConfig};

    #[tokio::test(start_paused = true)]
    async fn test_wait_for_boot() {
        let config_store = Arc::new(ConfigStore::new_rw());

        let shutdown_signal = CancellationToken::new();
        let wait_booted = wait_for_partition_boot(config_store.clone(), 0, shutdown_signal.clone());
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

        assert_eq!(poll!(wait_booted.as_mut()), Poll::Ready(Ok(BootResult::Started)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_wait_for_boot_cancel() {
        let config_store = Arc::new(ConfigStore::new_rw());

        let shutdown_signal = CancellationToken::new();
        let wait_booted = wait_for_partition_boot(config_store.clone(), 0, shutdown_signal.clone());
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

        shutdown_signal.cancel();
        // Move the time forward a little so that the loop iterates again and the cancel can take
        // place
        sleep(Duration::from_millis(1)).await;
        // Boot should be finished, but cancelled
        assert_eq!(poll!(wait_booted.as_mut()), Poll::Ready(Ok(BootResult::Cancelled)));
    }
}
