use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
    time::SystemTime,
};

use uuid::Uuid;

use crate::types::check_config::{CheckConfig, MAX_CHECK_INTERVAL_SECS};

/// Maps the unique subscription_id of a CheckConfig to the configuration itself. This provides
/// O(1) access during configuration updates.
pub type CheckerConfigs = HashMap<Uuid, Arc<CheckConfig>>;

/// The TickBucket is used to determine which checks should be executed at which ticks along the
/// interval pattern. Each tick contains the set of checks that are assigned to that second.
pub type TickBuckets = Vec<HashSet<Uuid>>;

/// The ConfigStore maintains the state of all check configurations and provides an efficient way
/// to retrieve the checks that are scheduled for a given tick.
#[derive(Debug)]
pub struct ConfigStore {
    buckets: TickBuckets,
    configs: CheckerConfigs,
}

/// Ticks represnet a location within the TickBuckets. They are guaranteed to be within the
/// MAX_CHECK_INTERVAL_SECS range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    index: usize,
    time: SystemTime,
}

impl Tick {
    /// Used primarily in tests to create a tick. Does not grantee invariants.
    #[doc(hidden)]
    fn new(index: usize, time: SystemTime) -> Tick {
        Self { index, time }
    }

    /// Construct a tick for a given time.
    pub fn from_time(time: SystemTime) -> Tick {
        let tick: usize = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize
            % MAX_CHECK_INTERVAL_SECS;

        Self { index: tick, time }
    }

    /// Get the wallclock time of the tick.
    pub fn time(&self) -> SystemTime {
        self.time
    }
}

impl From<SystemTime> for Tick {
    fn from(time: SystemTime) -> Self {
        Tick::from_time(time)
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ts = self
            .time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        write!(f, "Tick({}, {})", self.index, ts)
    }
}

impl ConfigStore {
    pub fn new() -> ConfigStore {
        let buckets = (0..MAX_CHECK_INTERVAL_SECS)
            .map(|_| HashSet::new())
            .collect();

        ConfigStore {
            buckets,
            configs: HashMap::new(),
        }
    }

    /// Insert a new Check Configuration into the store.
    pub fn add_config(&mut self, config: Arc<CheckConfig>) {
        self.configs.insert(config.subscription_id, config.clone());

        // Insert the configuration into the appropriate slots
        for slot in config.slots() {
            self.buckets[slot].insert(config.subscription_id);
        }
    }

    /// Remove a Check Configuration from the store.
    pub fn remove_config(&mut self, subscription_id: Uuid) {
        if let Some(config) = self.configs.remove(&subscription_id) {
            for slot in config.slots() {
                self.buckets[slot].remove(&subscription_id);
            }
        }
    }

    /// Get all check configs that are scheduled for a given tick.
    pub fn get_configs(&self, tick: Tick) -> Vec<Arc<CheckConfig>> {
        self.buckets[tick.index]
            .iter()
            .filter_map(|id| self.configs.get(id).cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, SystemTime},
    };

    use uuid::Uuid;

    use crate::{
        config_store::{ConfigStore, Tick},
        types::check_config::{CheckConfig, CheckInterval, MAX_CHECK_INTERVAL_SECS},
    };

    #[test]
    pub fn test_tick_for_time() {
        let time = SystemTime::UNIX_EPOCH;
        assert_eq!(Tick::from_time(time).index, 0);

        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        assert_eq!(Tick::from_time(time).index, 60);

        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(60 * 60);
        assert_eq!(Tick::from_time(time).index, 0);

        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(60 * 60 * 24);
        assert_eq!(Tick::from_time(time).index, 0);
    }

    #[test]
    pub fn test_add_config() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        assert_eq!(store.configs.len(), 1);
        assert_eq!(store.buckets[0].len(), 1);
        assert_eq!(store.buckets[60].len(), 1);

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

        assert_eq!(store.configs.len(), 2);
        assert_eq!(store.buckets[0].len(), 2);
        assert_eq!(store.buckets[60].len(), 1);
        assert_eq!(store.buckets[60 * 5].len(), 2);
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
        assert_eq!(store.buckets[0].len(), 1);
        assert_eq!(store.buckets[60].len(), 0);
        assert_eq!(store.buckets[60 * 5].len(), 1);

        store.remove_config(five_minute_config.subscription_id);
        assert_eq!(store.configs.len(), 0);
        assert_eq!(store.buckets[0].len(), 0);
        assert_eq!(store.buckets[60 * 5].len(), 0);
    }

    #[test]
    pub fn test_get_configs() {
        let mut store = ConfigStore::new();

        let config = Arc::new(CheckConfig::default());
        store.add_config(config.clone());

        let five_minute_config = Arc::new(CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        });
        store.add_config(five_minute_config.clone());

        let configs = store.get_configs(Tick::new(0, SystemTime::now()));
        assert_eq!(configs.len(), 2);
        assert!(configs.contains(&config));
        assert!(configs.contains(&five_minute_config));

        let no_configs = store.get_configs(Tick::new(1, SystemTime::now()));
        assert!(no_configs.is_empty());
    }
}
