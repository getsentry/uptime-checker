use chrono::TimeDelta;
use serde::Deserialize;
use serde_repr::Deserialize_repr;
use serde_with::serde_as;
use uuid::Uuid;

const ONE_MINUTE: isize = 60;

/// Valid intervals between the checks
#[repr(isize)]
#[derive(Debug, Copy, Clone, PartialEq, Deserialize_repr)]
pub enum CheckInterval {
    OneMinute = ONE_MINUTE,
    FiveMinutes = ONE_MINUTE * 5,
    TenMinutes = ONE_MINUTE * 10,
    TwentyMinutes = ONE_MINUTE * 20,
    ThirtyMinutes = ONE_MINUTE * 30,
    SixtyMinutes = ONE_MINUTE * 60,
}

/// The largest check interval
pub const MAX_CHECK_INTERVAL_SECS: usize = CheckInterval::SixtyMinutes as usize;

/// The CheckConfig represents a configuration for a single check.
#[serde_as]
#[derive(Debug, PartialEq, Clone, Deserialize)]
pub struct CheckConfig {
    /// The kafka partition this config is associated to.
    #[serde(skip)]
    pub partition: u16,

    /// The subscription this check configuration is associated to in sentry.
    #[serde(with = "uuid::serde::simple")]
    pub subscription_id: Uuid,

    /// The interval between each check run.
    #[serde(rename = "interval_seconds")]
    pub interval: CheckInterval,

    // The total time we will allow to make the request in.
    #[serde(rename = "timeout_ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<i64>")]
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
    use uuid::{uuid, Uuid};

    use crate::types::check_config::MAX_CHECK_INTERVAL_SECS;

    use super::{CheckConfig, CheckInterval};

    impl Default for CheckConfig {
        fn default() -> Self {
            CheckConfig {
                partition: 0,
                subscription_id: Uuid::from_u128(0),
                interval: CheckInterval::OneMinute,
                timeout: TimeDelta::seconds(10),
                url: "https://example.com".to_string(),
            }
        }
    }

    #[test]
    fn test_serialize_msgpack_roundtrip() {
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

        assert_eq!(
            rmp_serde::from_slice::<CheckConfig>(&example).unwrap(),
            CheckConfig {
                partition: 0,
                subscription_id: uuid!("d7629c6c-82be-4f67-9ee7-8a0d977856d2"),
                timeout: TimeDelta::milliseconds(500),
                interval: CheckInterval::FiveMinutes,
                url: "http://sentry.io".to_string(),
            }
        );
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
