use chrono::TimeDelta;
use uuid::Uuid;

const ONE_MINUTE: isize = 60;

/// Valid intervals between the checks
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum CheckInterval {
    OneMinute = ONE_MINUTE,
    FiveMinutes = ONE_MINUTE * 5,
    TenMinutes = ONE_MINUTE * 10,
    TwentyMinutes = ONE_MINUTE * 20,
    ThirtyMintues = ONE_MINUTE * 30,
    SixtyMinutes = ONE_MINUTE * 60,
}

/// The largest check interval
pub const MAX_CHECK_INTERVAL_SECS: usize = CheckInterval::SixtyMinutes as usize;

/// The CheckConfig represents a configuration for a single check.
#[derive(Debug, PartialEq, Clone)]
pub struct CheckConfig {
    /// The subscription this check configuration is associated to in sentry.
    pub subscription_id: Uuid,

    /// The interval between each check run.
    pub interval: CheckInterval,

    // The total time we will allow to make the request in.
    pub timeout: TimeDelta,

    /// The actual HTTP URL to check.
    pub url: String,
}

impl CheckConfig {
    /// Compute at which seconds within the TickBuckets the CheckConfig belongs to. This is
    /// consistent on the subscription_id.
    pub fn slots(&self) -> Vec<usize> {
        let interval_secs = self.interval as usize;

        let first_slot = self
            .subscription_id
            .as_u128()
            .checked_rem(self.interval as u128)
            .unwrap() as usize;

        let pattern_count = MAX_CHECK_INTERVAL_SECS / interval_secs;

        (0..pattern_count)
            .map(|c| first_slot + (interval_secs * c))
            .collect()
    }
}

#[cfg(test)]
mod tests {

    use chrono::TimeDelta;
    use similar_asserts::assert_eq;
    use uuid::Uuid;

    use crate::types::check_config::MAX_CHECK_INTERVAL_SECS;

    use super::{CheckConfig, CheckInterval};

    impl Default for CheckConfig {
        fn default() -> Self {
            CheckConfig {
                subscription_id: Uuid::from_u128(0),
                interval: CheckInterval::OneMinute,
                timeout: TimeDelta::seconds(10),
                url: "https://example.com".to_string(),
            }
        }
    }

    #[test]
    fn test_slots() {
        let all_slots = 0..MAX_CHECK_INTERVAL_SECS;

        let minute_config = CheckConfig::default();

        // Every minute slot includes the config
        assert_eq!(
            minute_config.slots(),
            all_slots.clone().step_by(60).collect::<Vec<usize>>()
        );

        let hour_config = CheckConfig {
            subscription_id: Uuid::from_u128(1),
            interval: CheckInterval::SixtyMinutes,
            ..Default::default()
        };

        // The second slot includes the config because of the subscription_id
        assert_eq!(hour_config.slots(), vec![1]);

        let five_minute_config = CheckConfig {
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        };

        // Every 5th slot includes the config
        assert_eq!(
            five_minute_config.slots(),
            all_slots.clone().step_by(60 * 5).collect::<Vec<usize>>()
        );

        let five_minute_config_offset = CheckConfig {
            subscription_id: Uuid::from_u128(MAX_CHECK_INTERVAL_SECS as u128 + 15),
            interval: CheckInterval::FiveMinutes,
            ..Default::default()
        };

        // Every 5th slot includes the config starting at the 15th slot
        assert_eq!(
            five_minute_config_offset.slots(),
            all_slots.skip(15).step_by(60 * 5).collect::<Vec<usize>>()
        );
    }
}
