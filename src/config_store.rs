use crate::types::check_config::{CheckConfig, MAX_CHECK_INTERVAL_SECS};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::RwLock;
use std::{collections::HashMap, fmt, sync::Arc};
use tokio::time::Instant;
use uuid::Uuid;

// Represents a bucket of checks at a given tick.
pub type TickBucket = HashSet<Arc<CheckConfig>>;

/// TickBuckets are used to determine which checks should be executed at which ticks along the
/// interval pattern. Each tick contains the set of checks that are assigned to that second.
pub type TickBuckets = Vec<TickBucket>;

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
/// checks that should be executed at that tick.
#[derive(Debug)]
pub struct ConfigStore {
    /// A vector of buckets. Each bucket represents a single tick worth of configs.
    tick_buckets: TickBuckets,

    /// Mapping of each subscription_id's config
    configs: HashMap<Uuid, Arc<CheckConfig>>,

    /// Tracks the last instant the config store was updated.
    last_update: Option<Instant>,
}

/// A RwLockable ConfigStore.
pub type RwConfigStore = RwLock<ConfigStore>;

fn make_empty_tick_buckets() -> TickBuckets {
    (0..MAX_CHECK_INTERVAL_SECS)
        .map(|_| HashSet::new())
        .collect()
}

impl ConfigStore {
    pub fn new() -> ConfigStore {
        ConfigStore {
            tick_buckets: make_empty_tick_buckets(),
            configs: HashMap::new(),
            last_update: None,
        }
    }

    pub fn new_rw() -> RwConfigStore {
        RwLock::new(ConfigStore::new())
    }

    /// Insert a new Check Configuration into the store.
    pub fn add_config(&mut self, config: Arc<CheckConfig>) {
        if self.configs.contains_key(&config.subscription_id) {
            self.remove_config(config.subscription_id);
        }
        self.configs.insert(config.subscription_id, config.clone());

        // Insert the configuration into the appropriate slots
        for slot in config.slots() {
            self.tick_buckets[slot].insert(config.clone());
        }

        self.last_update = Some(Instant::now());
    }

    /// Remove a Check Configuration from the store.
    pub fn remove_config(&mut self, subscription_id: Uuid) {
        let Some(config) = self.configs.remove(&subscription_id) else {
            return;
        };

        for slot in config.slots() {
            self.tick_buckets[slot].remove(&config.clone());
        }

        self.last_update = Some(Instant::now());
    }

    /// Get all check configs across all buckets.
    pub fn all_configs(&self) -> Vec<Arc<CheckConfig>> {
        self.configs.values().cloned().collect()
    }

    /// Get all check configs that are scheduled for a given tick.
    pub fn get_configs(&self, tick: Tick) -> TickBucket {
        self.tick_buckets[tick.index].clone()
    }

    /// Retrives the last instant that the config store was updated.
    pub fn get_last_update(&self) -> Option<Instant> {
        self.last_update
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use tokio::time::{self};
    use uuid::Uuid;

    use crate::{
        config_store::{ConfigStore, Tick},
        types::check_config::{CheckConfig, CheckInterval},
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

        let config = Arc::new(CheckConfig {
            // 64 maps to 0 after converting to a uuid5 with our namespace
            subscription_id: Uuid::from_u128(64),
            ..Default::default()
        });

        store.add_config(config.clone());

        assert_eq!(store.configs.len(), 1);
        assert_eq!(store.tick_buckets[0].len(), 1);
        assert_eq!(store.tick_buckets[60].len(), 1);

        // Another config with a 5 min interval should be in every 5th slot
        let five_minute_config = Arc::new(CheckConfig {
            // Cannot have multiple configs with the same subscription_id, to test configs that
            // exist in the same bucket manually create a uuid that slots into the 0th bucket after
            // wrapping around the max value.
            subscription_id: Uuid::from_u128(9382),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        assert_eq!(store.configs.len(), 2);
        for slot in (0..60).map(|c| (60 * c)).collect::<Vec<_>>() {
            assert!(store.tick_buckets[slot].contains(&config));
            if slot % 300 == 0 {
                assert_eq!(store.tick_buckets[slot].len(), 2);
                assert!(store.tick_buckets[slot].contains(&five_minute_config));
            } else {
                assert_eq!(store.tick_buckets[slot].len(), 1);
            }
        }
    }

    #[test]
    fn test_add_duplicate_config() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig {
            interval: CheckInterval::OneMinute,
            subscription_id: Uuid::from_u128(174),
            ..Default::default()
        });
        store.add_config(config.clone());

        for slot in (0..60).map(|c| (60 * c)).collect::<Vec<_>>() {
            assert_eq!(store.tick_buckets[slot].len(), 1);
            assert!(store.tick_buckets[slot].contains(&config));
        }

        let config = Arc::new(CheckConfig {
            interval: CheckInterval::FiveMinutes,
            subscription_id: Uuid::from_u128(174),
            ..Default::default()
        });
        store.add_config(config.clone());

        for slot in (0..60).map(|c| (60 * c)).collect::<Vec<_>>() {
            if slot % 300 == 0 {
                assert_eq!(store.tick_buckets[slot].len(), 1);
                assert!(store.tick_buckets[slot].contains(&config));
            } else {
                assert_eq!(
                    store.tick_buckets[slot].len(),
                    0,
                    "slot {} contained values {:#?}",
                    slot,
                    store.tick_buckets[slot]
                );
            }
        }
    }

    #[test]
    pub fn test_remove_config() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(9382),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        store.remove_config(config.subscription_id);
        assert_eq!(store.configs.len(), 1);
        assert_eq!(store.tick_buckets[0].len(), 1);
        assert_eq!(store.tick_buckets[60].len(), 0);
        assert_eq!(store.tick_buckets[60 * 5].len(), 1);

        store.remove_config(five_minute_config.subscription_id);
        assert_eq!(store.configs.len(), 0);
        assert_eq!(store.tick_buckets[0].len(), 0);
        assert_eq!(store.tick_buckets[60 * 5].len(), 0);
    }

    #[test]
    fn test_get_configs() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig {
            // 64 maps to 0 after converting to a uuid5 with our namespace
            subscription_id: Uuid::from_u128(64),
            ..Default::default()
        });

        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(9382),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        let configs = store.get_configs(Tick::new(0, Utc::now()));
        assert_eq!(configs.len(), 2);
        assert!(configs.contains(&config));
        assert!(configs.contains(&five_minute_config));

        let no_configs = store.get_configs(Tick::new(1, Utc::now()));
        assert!(no_configs.is_empty());
    }

    #[tokio::test]
    async fn test_last_update() {
        time::pause();
        let now = time::Instant::now();

        let mut store = ConfigStore::new();
        assert!(store.get_last_update().is_none());

        // Add config
        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());
        assert_eq!(store.get_last_update(), Some(now));

        time::advance(Duration::from_secs(1)).await;
        let now = time::Instant::now();

        // Remove config
        store.remove_config(config.subscription_id);
        assert_eq!(store.get_last_update(), Some(now));
    }
}
