use std::sync::RwLock;
use std::{collections::HashMap, fmt, sync::Arc};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::check_config::{CheckConfig, MAX_CHECK_INTERVAL_SECS};

// Represents a bucket of checks at a given tick.
pub type TickBucket = Vec<Arc<CheckConfig>>;

/// TickBuckets are used to determine which checks should be executed at which ticks along the
/// interval pattern. Each tick contains the set of checks that are assigned to that second.
pub type TickBuckets = Vec<TickBucket>;

pub type PartitionedTickBuckets = HashMap<u16, TickBuckets>;

/// Ticks represent a location within the TickBuckets. They are guaranteed to be within the
/// MAX_CHECK_INTERVAL_SECS range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    index: usize,
    time: DateTime<Utc>,
}

/// Represents a slot within the TickBuckets. Ticks are used to determine which checks should be
/// executed at which ticks along the interval pattern.
impl Tick {
    /// Used primarily in tests to create a tick. Does not grantee invariants.
    #[doc(hidden)]
    fn new(index: usize, time: DateTime<Utc>) -> Tick {
        Self { index, time }
    }

    /// Construct a tick for a given time.
    pub fn from_time(time: DateTime<Utc>) -> Tick {
        let tick: usize = time.timestamp() as usize % MAX_CHECK_INTERVAL_SECS;

        Self { index: tick, time }
    }

    /// Get the wallclock time of the tick.
    pub fn time(&self) -> DateTime<Utc> {
        self.time
    }
}

impl From<DateTime<Utc>> for Tick {
    fn from(time: DateTime<Utc>) -> Self {
        Tick::from_time(time)
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ts = self.time.timestamp();
        write!(f, "Tick(slot: {}, ts: {})", self.index, ts)
    }
}

/// The ConfigStore maintains the state of all check configurations and provides an efficient way
/// to retrieve the checks that are scheduled for a given tick.
///
/// CheckConfigs are stored into TickBuckets based on their interval. Each bucket contains the
/// checks that should be executed at that tick. Each configuration includes the partition it
/// belongs to, the ConfigStore will group configurations by partition and supports dropping entire
/// partitions of CheckConfigs.
#[derive(Debug)]
pub struct ConfigStore {
    /// A ConfigStore is derived from configurations loaded from Kafka, when the assigned
    /// partitions for the uptime-checker are re-balanced, partitions will be added and removed.
    /// Storing TickBuckets for each partition makes dropping an entire partition of configs during
    /// removal simple.
    partitioned_buckets: HashMap<u16, TickBuckets>,

    /// Mapping of each subscription_id's config
    configs: HashMap<Uuid, Arc<CheckConfig>>,
}

/// A RwLockable ConfigStore.
pub type RwConfigStore = RwLock<ConfigStore>;

impl ConfigStore {
    pub fn new() -> ConfigStore {
        ConfigStore {
            partitioned_buckets: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    pub fn new_rw() -> RwConfigStore {
        RwLock::new(ConfigStore::new())
    }

    /// Insert a new Check Configuration into the store.
    pub fn add_config(&mut self, config: Arc<CheckConfig>) {
        self.configs.insert(config.subscription_id, config.clone());

        let bucket = self
            .partitioned_buckets
            .entry(config.partition)
            .or_insert_with(|| (0..MAX_CHECK_INTERVAL_SECS).map(|_| vec![]).collect());

        // Insert the configuration into the appropriate slots
        for slot in config.slots() {
            bucket[slot].push(config.clone());
        }
    }

    /// Remove a Check Configuration from the store.
    pub fn remove_config(&mut self, subscription_id: Uuid) {
        let Some(config) = self.configs.remove(&subscription_id) else {
            return;
        };
        let Some(buckets) = self.partitioned_buckets.get_mut(&config.partition) else {
            return;
        };

        for slot in config.slots() {
            buckets[slot].retain(|c| c.subscription_id != subscription_id);
        }
    }

    /// Drop an entire partition of check configurations.
    pub fn drop_partition(&mut self, partition: u16) {
        let Some(buckets) = self.partitioned_buckets.remove(&partition) else {
            return;
        };

        buckets
            .iter()
            .flat_map(|b| b.iter().cloned())
            .for_each(|config| {
                self.configs.remove(&config.subscription_id);
            });
    }

    /// Get all check configs that are scheduled for a given tick.
    pub fn get_configs(&self, tick: Tick) -> TickBucket {
        self.partitioned_buckets
            .values()
            .flat_map(move |b| b[tick.index].iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use crate::{
        config_store::{ConfigStore, Tick},
        types::check_config::{CheckConfig, CheckInterval, MAX_CHECK_INTERVAL_SECS},
    };

    #[test]
    fn test_tick_for_time() {
        let time = DateTime::from_timestamp(0, 0).unwrap();
        assert_eq!(Tick::from_time(time).index, 0);

        let time = DateTime::from_timestamp(60, 0).unwrap();
        assert_eq!(Tick::from_time(time).index, 60);

        let time = DateTime::from_timestamp(60 * 60, 0).unwrap();
        assert_eq!(Tick::from_time(time).index, 0);

        let time = DateTime::from_timestamp(60 * 60 * 24, 0).unwrap();
        assert_eq!(Tick::from_time(time).index, 0);
    }

    #[test]
    fn test_add_config() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        assert_eq!(store.configs.len(), 1);
        assert_eq!(store.partitioned_buckets[&0][0].len(), 1);
        assert_eq!(store.partitioned_buckets[&0][60].len(), 1);

        // Another config with a 5 min interval should be in every 5th slot
        let five_minute_config = Arc::new(CheckConfig {
            // Cannot have multiple configs with the same subscription_id, to test configs that
            // exist in the same bucket manually create a uuid that slots into the 0th bucket after
            // wrapping around the max value.
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        let second_partition_config = Arc::new(CheckConfig {
            partition: 2,
            ..Default::default()
        });
        store.add_config(second_partition_config.clone());

        assert_eq!(store.configs.len(), 2);
        assert_eq!(store.partitioned_buckets[&0][0].len(), 2);
        assert_eq!(store.partitioned_buckets[&0][60].len(), 1);
        assert_eq!(store.partitioned_buckets[&0][60 * 5].len(), 2);
        assert_eq!(store.partitioned_buckets[&2][0].len(), 1);
    }

    #[test]
    pub fn test_remove_config() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        store.remove_config(config.subscription_id);
        assert_eq!(store.configs.len(), 1);
        assert_eq!(store.partitioned_buckets[&0][0].len(), 1);
        assert_eq!(store.partitioned_buckets[&0][60].len(), 0);
        assert_eq!(store.partitioned_buckets[&0][60 * 5].len(), 1);

        store.remove_config(five_minute_config.subscription_id);
        assert_eq!(store.configs.len(), 0);
        assert_eq!(store.partitioned_buckets[&0][0].len(), 0);
        assert_eq!(store.partitioned_buckets[&0][60 * 5].len(), 0);
    }

    #[test]
    fn test_get_configs() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        let second_partition_config = Arc::new(CheckConfig {
            partition: 2,
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128 * 2),
            ..Default::default()
        });
        store.add_config(second_partition_config.clone());

        let configs = store.get_configs(Tick::new(0, Utc::now()));
        assert_eq!(configs.len(), 3);
        assert!(configs.contains(&config));
        assert!(configs.contains(&five_minute_config));
        assert!(configs.contains(&second_partition_config));

        let no_configs = store.get_configs(Tick::new(1, Utc::now()));
        assert!(no_configs.is_empty());
    }

    #[test]
    fn test_drop_partition() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        let second_partition_config = Arc::new(CheckConfig {
            partition: 2,
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128 * 2),
            ..Default::default()
        });
        store.add_config(second_partition_config.clone());

        assert_eq!(store.configs.len(), 3);
        assert_eq!(store.partitioned_buckets.len(), 2);

        store.drop_partition(2);
        assert_eq!(store.configs.len(), 2);
        assert_eq!(store.partitioned_buckets.len(), 1);
        assert!(store.partitioned_buckets.contains_key(&0));
    }
}
